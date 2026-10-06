// SPDX-License-Identifier: Apache-2.0

//! Transactional personal learning, partitioned by authenticated tenant/project.
//!
//! The host supplies a dedicated, file-backed SQLite connection opened inside
//! its private, trusted storage boundary. This module does not authorize paths,
//! resolve accounts/entitlements, or authenticate administrator-edited databases.
//! Do not open this database from an untrusted workspace or share it across users.

use std::time::Duration;

use anyhow::{Result, ensure};
use lean_ctx_protocol::{ProjectId, TaskEnvelopeV1, TenantId};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

mod policy_evidence;
pub(crate) use policy_evidence::workload_of;
mod routing;
mod storage;
pub use routing::{MAX_ROUTING_HISTORY, RoutingHistoryEntry};

use super::{AdaptiveLearningState, AutopilotController, AutopilotInput, TaskAutopilotDecision};
use crate::core::{
    execution_protocol::ValidatedExecutionProtocolV1, provider_bandit::ProviderBandit,
};

/// Covers the maximum receipt set (10,000 identifiers of at most 256 bytes).
pub(super) const MAX_STATE_BYTES: usize = 4 * 1024 * 1024;
const SCHEMA_VERSION: i64 = 1;
pub const MAX_HISTORY: usize = 1_000;
const MAX_HISTORY_BYTES: usize = 16 * 1024;

/// Privacy-minimal explanation of a committed learning observation, not a full
/// execution audit or a history of unexecuted/Community planning decisions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LearningHistoryEntry {
    pub schema_version: u32,
    pub receipt_id: String,
    pub decision_id: String,
    pub mode: String,
    pub outcome: lean_ctx_protocol::AcceptanceState,
    pub explicit_override: bool,
    pub reason_codes: Vec<String>,
    pub confidence_milli: u16,
}

impl LearningHistoryEntry {
    fn validate(&self) -> Result<()> {
        ensure!(
            self.schema_version == 1,
            "unsupported learning history version"
        );
        lean_ctx_protocol::ReceiptId::new(self.receipt_id.clone())?;
        lean_ctx_protocol::DecisionId::new(self.decision_id.clone())?;
        ensure!(
            super::valid_learned_mode(&self.mode),
            "invalid history mode"
        );
        ensure!(
            self.outcome != lean_ctx_protocol::AcceptanceState::Unknown,
            "history outcome is not terminal"
        );
        ensure!(self.confidence_milli <= 1000, "invalid history confidence");
        ensure!(
            self.reason_codes.len() <= 32
                && self.reason_codes.iter().all(|code| !code.is_empty()
                    && code.len() <= 64
                    && code.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')),
            "invalid history reason codes"
        );
        Ok(())
    }
}

/// No cached mutable state: every update reloads under an immediate transaction.
pub struct AdaptiveLearningStore {
    connection: Connection,
    project_id: ProjectId,
    tenant_id: Option<TenantId>,
    scope: String,
}

impl AdaptiveLearningStore {
    /// Open personal storage outside the workspace. This is local filesystem
    /// ownership, not entitlement or organization-role authorization.
    pub fn open_default(project_id: ProjectId, tenant_id: Option<TenantId>) -> Result<Self> {
        let data_dir = crate::core::data_dir::resolve_data_dir().map_err(anyhow::Error::msg)?;
        Self::new(storage::open_private(&data_dir)?, project_id, tenant_id)
    }

    /// Own a dedicated host-opened database; in-memory stores are not durable.
    /// Lock contention fails after a bounded wait, never degrades to unlocked IO.
    pub fn new(
        connection: Connection,
        project_id: ProjectId,
        tenant_id: Option<TenantId>,
    ) -> Result<Self> {
        ensure!(
            connection.is_autocommit(),
            "learning database has an active transaction"
        );
        ensure!(
            connection.path().is_some_and(|path| !path.is_empty()),
            "learning database must be file-backed"
        );
        connection.busy_timeout(Duration::from_secs(2))?;
        connection.pragma_update(None, "trusted_schema", false)?;
        connection.pragma_update(None, "synchronous", "EXTRA")?;
        connection.pragma_update(None, "secure_delete", true)?;
        let journal: String =
            connection.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
        ensure!(
            matches!(journal.as_str(), "delete" | "truncate" | "persist" | "wal"),
            "learning database requires crash-safe journaling"
        );
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS autopilot_learning_v1 (
                scope TEXT PRIMARY KEY NOT NULL,
                version INTEGER NOT NULL,
                state_json TEXT NOT NULL CHECK(length(CAST(state_json AS BLOB)) <= 4194304)
            );
            CREATE TABLE IF NOT EXISTS autopilot_learning_history_v1 (
                scope TEXT NOT NULL,
                receipt_id TEXT NOT NULL,
                entry_json TEXT NOT NULL CHECK(length(CAST(entry_json AS BLOB)) <= 16384),
                UNIQUE(scope, receipt_id)
            );",
        )?;
        routing::initialize(&connection)?;
        policy_evidence::initialize(&connection)?;
        // Same bytes as before the shared definition: existing rows keep their scope.
        let scope = crate::core::context_store::task_scope(tenant_id.as_ref(), &project_id);
        Ok(Self {
            connection,
            project_id,
            tenant_id,
            scope,
        })
    }

    /// Missing state is empty; corruption, unsupported versions and IO errors
    /// propagate instead of silently discarding replay protection.
    pub fn load(&self) -> Result<AdaptiveLearningState> {
        load_state(&self.connection, &self.scope)
    }

    /// Deterministic, privacy-minimal export: counters, modes and opaque receipt
    /// IDs only, never source text, prompts, paths or full receipts.
    pub fn export_json(&self) -> Result<String> {
        Ok(self.load()?.export_json()?)
    }

    /// A consistent read snapshot for status/export while another process learns.
    pub fn inspect(&self) -> Result<(AdaptiveLearningState, Vec<LearningHistoryEntry>)> {
        let transaction = self.connection.unchecked_transaction()?;
        let state = self.load()?;
        let history = self.history(MAX_HISTORY)?;
        transaction.commit()?;
        Ok((state, history))
    }

    /// Newest committed observations first, capped independently of replay IDs.
    /// Older stores can have replay IDs without explanations; none are fabricated.
    pub fn history(&self, limit: usize) -> Result<Vec<LearningHistoryEntry>> {
        let mut statement = self.connection.prepare(
            "SELECT CASE WHEN length(CAST(entry_json AS BLOB)) <= ?2 THEN entry_json ELSE NULL END
             FROM autopilot_learning_history_v1 WHERE scope = ?1 ORDER BY rowid DESC LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![
                self.scope,
                i64::try_from(MAX_HISTORY_BYTES)?,
                i64::try_from(limit.min(MAX_HISTORY))?
            ],
            |row| row.get::<_, Option<String>>(0),
        )?;
        rows.map(|row| decode_history(row?)).collect()
    }

    pub fn explain(&self, receipt_id: &str) -> Result<Option<LearningHistoryEntry>> {
        lean_ctx_protocol::ReceiptId::new(receipt_id.to_owned())?;
        let row = self.connection.query_row(
            "SELECT CASE WHEN length(CAST(entry_json AS BLOB)) <= ?3 THEN entry_json ELSE NULL END
             FROM autopilot_learning_history_v1 WHERE scope = ?1 AND receipt_id = ?2",
            params![self.scope, receipt_id, i64::try_from(MAX_HISTORY_BYTES)?],
            |row| row.get::<_, Option<String>>(0),
        ).optional()?;
        row.map(decode_history).transpose()
    }

    /// Plan from the durable snapshot using the same canonical controller.
    /// Entitlement, confidence and policy gates remain owned by that controller.
    pub fn plan_for_task(
        &self,
        controller: &AutopilotController,
        task: &TaskEnvelopeV1,
        input: &AutopilotInput,
        bandit: Option<&mut ProviderBandit>,
    ) -> Result<TaskAutopilotDecision> {
        task.validate()?;
        self.validate_scope(&task.project_id, task.tenant_id.as_ref())?;
        let mut input = input.clone();
        input.learning = self.load()?;
        input.context_policy = super::ScopeContextPolicy::load(&self.scope);
        controller.plan_for_task(task.task_id.clone(), &input, bandit)
    }

    /// Commit learned counters and replay IDs together, only after canonical
    /// outcome admission. An error leaves no partially trained in-memory cache.
    pub fn observe_protocol_outcome(
        &mut self,
        decision: &TaskAutopilotDecision,
        protocol: &ValidatedExecutionProtocolV1<'_>,
    ) -> Result<bool> {
        self.validate_scope(protocol.project_id(), protocol.tenant_id())?;
        // The task's deliveries, read before the write lock: file reads never
        // hold the learning database. Scoped like the store itself.
        let task_id = protocol.execution_plan().task_id.as_str();
        let receipts =
            crate::core::context_admission::receipt_store::for_task(&self.scope, task_id);
        // Signals count only for a task whose deliveries were observed.
        let signals = (receipts.complete() && receipts.verified().next().is_some())
            .then(|| {
                crate::core::context_store::task_signals::load(&(
                    self.scope.clone(),
                    task_id.to_owned(),
                ))
            })
            .flatten();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut learning = load_state(&transaction, &self.scope)?;
        let changed = decision.observe_protocol_outcome(protocol, &mut learning)?;
        if changed {
            let json = learning.export_json()?;
            ensure!(
                json.len() <= MAX_STATE_BYTES,
                "learning state exceeds byte limit"
            );
            transaction.execute(
                "INSERT INTO autopilot_learning_v1(scope, version, state_json) VALUES (?1, ?2, ?3)
                 ON CONFLICT(scope) DO UPDATE SET version=excluded.version, state_json=excluded.state_json",
                params![self.scope, SCHEMA_VERSION, json],
            )?;
            let planned = decision.decision();
            let entry = LearningHistoryEntry {
                schema_version: 1,
                receipt_id: protocol.receipt_id().to_owned(),
                decision_id: planned.decision_id.clone(),
                mode: planned.read_policy.mode.clone(),
                outcome: protocol.outcome().accepted,
                explicit_override: planned.read_policy.explicit_override,
                reason_codes: planned
                    .reasons
                    .iter()
                    .map(|reason| reason.code.clone())
                    .collect(),
                confidence_milli: planned.confidence_milli,
            };
            entry.validate()?;
            let entry_json = serde_json::to_string(&entry)?;
            ensure!(
                entry_json.len() <= MAX_HISTORY_BYTES,
                "learning history exceeds byte limit"
            );
            transaction.execute(
                "INSERT INTO autopilot_learning_history_v1(scope, receipt_id, entry_json) VALUES (?1, ?2, ?3)",
                params![self.scope, entry.receipt_id, entry_json],
            )?;
            transaction.execute(
                "DELETE FROM autopilot_learning_history_v1 WHERE scope = ?1 AND rowid NOT IN
                 (SELECT rowid FROM autopilot_learning_history_v1 WHERE scope = ?1 ORDER BY rowid DESC LIMIT ?2)",
                params![self.scope, i64::try_from(MAX_HISTORY)?],
            )?;
            // Measured only when every delivery of the task is known and
            // verified; an outcome without a valid time is not evidence.
            if let Some(observed_day) = policy_evidence::utc_day(&protocol.outcome().observed_at) {
                let observation = policy_evidence::observe(
                    planned,
                    protocol.outcome().accepted,
                    observed_day,
                    &receipts,
                    signals,
                );
                policy_evidence::insert(
                    &transaction,
                    &self.scope,
                    &entry.receipt_id,
                    &observation,
                )?;
            }
        }
        transaction.commit()?;
        Ok(changed)
    }

    /// Explicit user reset deletes only this scope, including its replay memory.
    /// Old receipts can be learned again after a deliberate reset. This is logical
    /// deletion; filesystem snapshots/backups and SQLite journals are host-owned.
    pub fn reset(&mut self) -> Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "DELETE FROM autopilot_learning_v1 WHERE scope = ?1",
            [&self.scope],
        )?;
        transaction.execute(
            "DELETE FROM autopilot_learning_history_v1 WHERE scope = ?1",
            [&self.scope],
        )?;
        transaction.execute(
            "DELETE FROM routing_learning_history_v1 WHERE scope = ?1",
            [&self.scope],
        )?;
        policy_evidence::reset(&transaction, &self.scope)?;
        transaction.commit()?;
        Ok(())
    }

    fn validate_scope(&self, project: &ProjectId, tenant: Option<&TenantId>) -> Result<()> {
        ensure!(
            project == &self.project_id && tenant == self.tenant_id.as_ref(),
            "learning scope does not match authenticated task tenant/project"
        );
        Ok(())
    }
}

fn decode_history(json: Option<String>) -> Result<LearningHistoryEntry> {
    let json = json.ok_or_else(|| anyhow::anyhow!("learning history exceeds byte limit"))?;
    let entry: LearningHistoryEntry = serde_json::from_str(&json)?;
    entry.validate()?;
    Ok(entry)
}

fn load_state(connection: &Connection, scope: &str) -> Result<AdaptiveLearningState> {
    // Guard in SQL before materializing attacker-corrupted oversized data in Rust.
    let stored: Option<(i64, Option<String>)> = connection.query_row(
        "SELECT version, CASE WHEN length(CAST(state_json AS BLOB)) <= ?2 THEN state_json ELSE NULL END
         FROM autopilot_learning_v1 WHERE scope = ?1",
        params![scope, i64::try_from(MAX_STATE_BYTES)?],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional()?;
    let Some((version, json)) = stored else {
        return Ok(AdaptiveLearningState::default());
    };
    ensure!(
        version == SCHEMA_VERSION,
        "unsupported learning state version"
    );
    let json = json.ok_or_else(|| anyhow::anyhow!("learning state exceeds byte limit"))?;
    Ok(AdaptiveLearningState::import_json(&json)?)
}

#[cfg(test)]
mod tests {
    use super::super::tests::{controller, input, protocol_task};
    use super::*;
    use crate::core::execution_protocol::test_support::build_for_context;
    use lean_ctx_protocol::AcceptanceState;

    fn open(path: &std::path::Path, task: &TaskEnvelopeV1) -> AdaptiveLearningStore {
        AdaptiveLearningStore::new(
            Connection::open(path).unwrap(),
            task.project_id.clone(),
            task.tenant_id.clone(),
        )
        .unwrap()
    }

    #[test]
    fn authenticated_unknown_never_persists_learning_even_with_a_partial_score() {
        let directory = tempfile::tempdir().expect("learning directory");
        let path = directory.path().join("learning.db");
        let task = protocol_task();
        let mut store = open(&path, &task);
        let handoff = store
            .plan_for_task(&controller(), &task, &input(), None)
            .expect("initial plan");
        let before = store.export_json().expect("initial state");
        let mut fixture = build_for_context(
            &task,
            Some(handoff.context_projection()),
            Some(&handoff),
            AcceptanceState::Unknown,
        );
        // Unknown can carry an evaluator's partial score; no score value is
        // permission to learn when the signed receipt state remains Unknown.
        for score in [None, Some(0), Some(500), Some(1_000)] {
            fixture.protocol.accepted_outcome.quality_score_milli = score;
            let admitted = fixture.validated_for(&task).expect("operational Unknown");
            for _ in 0..2 {
                assert!(
                    !store
                        .observe_protocol_outcome(&handoff, &admitted)
                        .expect("Unknown observation")
                );
                assert_eq!(store.export_json().expect("unchanged state"), before);
            }
        }
        let rows: (i64, i64) = store
            .connection
            .query_row(
                "SELECT (SELECT count(*) FROM autopilot_learning_v1),
                        (SELECT count(*) FROM autopilot_learning_history_v1)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("persisted row counts");
        assert_eq!(rows, (0, 0));
        drop(store);
        let reopened = open(&path, &task);
        assert_eq!(reopened.export_json().expect("reopened state"), before);
        let next = reopened
            .plan_for_task(&controller(), &task, &input(), None)
            .expect("subsequent plan");
        assert_eq!(
            next.decision().read_policy.mode,
            handoff.decision().read_policy.mode
        );
    }

    #[test]
    fn durable_outcome_replay_plan_export_and_reset() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("learning.db");
        let task = protocol_task();
        let mut store = open(&path, &task);
        let initial = store
            .plan_for_task(&controller(), &task, &input(), None)
            .unwrap();
        assert_eq!(initial.decision().read_policy.mode, "map");
        let mut request = input();
        request.overrides.read_mode = Some("full".to_owned());
        let handoff = store
            .plan_for_task(&controller(), &task, &request, None)
            .unwrap();
        let fixture = build_for_context(
            &task,
            Some(handoff.context_projection()),
            Some(&handoff),
            AcceptanceState::Accepted,
        );
        let admitted = fixture.validated_for(&task).unwrap();
        assert!(store.observe_protocol_outcome(&handoff, &admitted).unwrap());
        let exported = store.export_json().unwrap();
        assert_eq!(store.load().unwrap().accepted, 1);
        drop(store);
        let mut reopened = open(&path, &task);
        assert_eq!(reopened.export_json().unwrap(), exported);
        assert!(
            !reopened
                .observe_protocol_outcome(&handoff, &admitted)
                .unwrap()
        );
        let next = reopened
            .plan_for_task(&controller(), &task, &input(), None)
            .unwrap();
        assert_eq!(next.decision().read_policy.mode, "full");
        request = input();
        request.entitled_to_adaptive = false;
        let community = reopened
            .plan_for_task(&controller(), &task, &request, None)
            .unwrap();
        assert_eq!(
            community.decision().tier,
            super::super::PlannerTier::Community
        );
        assert_eq!(community.decision().read_policy.mode, "map");
        reopened.reset().unwrap();
        drop(reopened);
        assert_eq!(
            open(&path, &task).load().unwrap(),
            AdaptiveLearningState::default()
        );
    }

    #[test]
    fn rejects_cross_project_and_tenant_without_writes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("learning.db");
        let task = protocol_task();
        let handoff = controller()
            .plan_for_task(task.task_id.clone(), &input(), None)
            .unwrap();
        let fixture = build_for_context(
            &task,
            Some(handoff.context_projection()),
            Some(&handoff),
            AcceptanceState::Accepted,
        );
        let admitted = fixture.validated_for(&task).unwrap();
        for other in [
            TaskEnvelopeV1 {
                project_id: ProjectId::new("other-project").unwrap(),
                ..task.clone()
            },
            TaskEnvelopeV1 {
                tenant_id: Some(TenantId::new("other-tenant").unwrap()),
                ..task.clone()
            },
        ] {
            let mut store = open(&path, &other);
            assert!(store.observe_protocol_outcome(&handoff, &admitted).is_err());
            assert!(
                store
                    .plan_for_task(&controller(), &task, &input(), None)
                    .is_err()
            );
            assert_eq!(store.load().unwrap(), AdaptiveLearningState::default());
            // Scope is committed by the signed task digest, not the caller label.
            assert!(fixture.validated_for(&other).is_err());
        }
    }

    #[test]
    fn concurrent_connections_commit_same_receipt_once() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("learning.db");
        let task = protocol_task();
        let handoff = controller()
            .plan_for_task(task.task_id.clone(), &input(), None)
            .unwrap();
        let fixture = build_for_context(
            &task,
            Some(handoff.context_projection()),
            Some(&handoff),
            AcceptanceState::Accepted,
        );
        let mut first = open(&path, &task);
        let mut second = open(&path, &task);
        let barrier = std::sync::Barrier::new(2);
        let run = |store: &mut AdaptiveLearningStore| {
            let admitted = fixture.validated_for(&task).unwrap();
            barrier.wait();
            store.observe_protocol_outcome(&handoff, &admitted).unwrap()
        };
        let results = std::thread::scope(|scope| {
            let a = scope.spawn(|| run(&mut first));
            let b = scope.spawn(|| run(&mut second));
            [a.join().unwrap(), b.join().unwrap()]
        });
        assert_eq!(results.into_iter().filter(|changed| *changed).count(), 1);
        assert_eq!(first.load().unwrap().accepted, 1);
        assert_eq!(second.load().unwrap().accepted, 1);
    }

    #[test]
    fn failed_write_rolls_back_and_retry_can_learn() {
        let directory = tempfile::tempdir().unwrap();
        let task = protocol_task();
        let mut store = open(&directory.path().join("learning.db"), &task);
        let handoff = controller()
            .plan_for_task(task.task_id.clone(), &input(), None)
            .unwrap();
        let fixture = build_for_context(
            &task,
            Some(handoff.context_projection()),
            Some(&handoff),
            AcceptanceState::Accepted,
        );
        let admitted = fixture.validated_for(&task).unwrap();
        store.connection.execute_batch("CREATE TRIGGER fail_write BEFORE INSERT ON autopilot_learning_v1 BEGIN SELECT RAISE(ABORT, 'injected failure'); END;").unwrap();
        assert!(store.observe_protocol_outcome(&handoff, &admitted).is_err());
        assert_eq!(store.load().unwrap(), AdaptiveLearningState::default());
        store
            .connection
            .execute_batch("DROP TRIGGER fail_write;")
            .unwrap();
        assert!(store.observe_protocol_outcome(&handoff, &admitted).unwrap());
    }

    #[test]
    fn corrupt_unknown_oversized_state_fails_closed() {
        let directory = tempfile::tempdir().unwrap();
        let task = protocol_task();
        let mut store = open(&directory.path().join("learning.db"), &task);
        for (version, json) in [
            (2, "{}".to_owned()),
            (1, "broken".to_owned()),
            (1, " ".repeat(MAX_STATE_BYTES + 1)),
        ] {
            store
                .connection
                .pragma_update(None, "ignore_check_constraints", true)
                .unwrap();
            store
                .connection
                .execute(
                    "INSERT OR REPLACE INTO autopilot_learning_v1 VALUES (?1, ?2, ?3)",
                    params![store.scope, version, json],
                )
                .unwrap();
            assert!(store.load().is_err());
            assert!(store.export_json().is_err());
            assert!(
                store
                    .plan_for_task(&controller(), &task, &input(), None)
                    .is_err()
            );
            store.reset().unwrap();
            assert_eq!(store.load().unwrap(), AdaptiveLearningState::default());
        }
        let mut state = serde_json::to_value(AdaptiveLearningState::default()).unwrap();
        state["unknown_private_code"] = serde_json::json!("must not be retained");
        assert!(AdaptiveLearningState::import_json(&state.to_string()).is_err());
        state
            .as_object_mut()
            .unwrap()
            .remove("unknown_private_code");
        state["processed_receipts"] = serde_json::json!(["x".repeat(257)]);
        assert!(AdaptiveLearningState::import_json(&state.to_string()).is_err());
    }

    #[test]
    fn in_memory_and_unsafe_journaling_are_rejected() {
        let task = protocol_task();
        assert!(
            AdaptiveLearningStore::new(
                Connection::open_in_memory().unwrap(),
                task.project_id.clone(),
                None
            )
            .is_err()
        );
        let directory = tempfile::tempdir().unwrap();
        let connection = Connection::open(directory.path().join("learning.db")).unwrap();
        connection
            .pragma_update(None, "journal_mode", "OFF")
            .unwrap();
        assert!(AdaptiveLearningStore::new(connection, task.project_id, None).is_err());
    }

    #[test]
    fn lock_contention_is_bounded_and_reset_preserves_other_scopes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("learning.db");
        let task = protocol_task();
        let other_task = TaskEnvelopeV1 {
            tenant_id: Some(TenantId::new("another-tenant").unwrap()),
            ..task.clone()
        };
        let mut store = open(&path, &task);
        let mut other = open(&path, &other_task);
        for (store, task) in [(&mut store, &task), (&mut other, &other_task)] {
            let handoff = controller()
                .plan_for_task(task.task_id.clone(), &input(), None)
                .unwrap();
            let fixture = build_for_context(
                task,
                Some(handoff.context_projection()),
                Some(&handoff),
                AcceptanceState::Rejected,
            );
            let admitted = fixture.validated_for(task).unwrap();
            assert!(store.observe_protocol_outcome(&handoff, &admitted).unwrap());
        }
        let mut blocker = Connection::open(&path).unwrap();
        let held = blocker
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let started = std::time::Instant::now();
        assert!(store.reset().is_err());
        assert!(started.elapsed() < Duration::from_secs(5));
        held.rollback().unwrap();
        assert_eq!(store.load().unwrap().rejected, 1);
        store.reset().unwrap();
        assert_eq!(store.load().unwrap(), AdaptiveLearningState::default());
        assert_eq!(other.load().unwrap().rejected, 1);
    }

    #[test]
    fn restart_recovers_committed_learning_after_abrupt_exit() {
        const CHILD: &str = "LEAN_CTX_LEARNING_CRASH_TEST_DB";
        if let Some(path) = std::env::var_os(CHILD) {
            let task = protocol_task();
            let mut store = open(std::path::Path::new(&path), &task);
            let handoff = controller()
                .plan_for_task(task.task_id.clone(), &input(), None)
                .unwrap();
            let fixture = build_for_context(
                &task,
                Some(handoff.context_projection()),
                Some(&handoff),
                AcceptanceState::Accepted,
            );
            let admitted = fixture.validated_for(&task).unwrap();
            assert!(store.observe_protocol_outcome(&handoff, &admitted).unwrap());
            // Exit without destructors after a real uncommitted destructive update.
            store.connection.execute_batch("BEGIN IMMEDIATE; UPDATE autopilot_learning_v1 SET state_json = 'invalid-uncommitted-state';").unwrap();
            std::process::exit(0);
        }
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("learning.db");
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "core::context_kernel::autopilot::learning_store::tests::restart_recovers_committed_learning_after_abrupt_exit", "--nocapture"])
            .env(CHILD, &path)
            .output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let store = open(&path, &protocol_task());
        let recovered = store.load().unwrap();
        assert_eq!(recovered.accepted, 1);
        assert_eq!(recovered.processed_receipts.len(), 1);
        assert!(
            !store
                .export_json()
                .unwrap()
                .contains("invalid-uncommitted-state")
        );
    }

    #[test]
    fn history_is_atomic_bounded_and_does_not_evict_replay_memory() {
        let directory = tempfile::tempdir().unwrap();
        let task = protocol_task();
        let mut store = open(&directory.path().join("history.db"), &task);
        let handoff = controller()
            .plan_for_task(task.task_id.clone(), &input(), None)
            .unwrap();
        let fixture = build_for_context(
            &task,
            Some(handoff.context_projection()),
            Some(&handoff),
            AcceptanceState::Accepted,
        );
        let admitted = fixture.validated_for(&task).unwrap();
        store.connection.execute_batch("CREATE TRIGGER reject_history BEFORE INSERT ON autopilot_learning_history_v1 BEGIN SELECT RAISE(ABORT, 'history failure'); END;").unwrap();
        assert!(store.observe_protocol_outcome(&handoff, &admitted).is_err());
        assert_eq!(store.load().unwrap(), AdaptiveLearningState::default());
        assert!(store.history(1).unwrap().is_empty());
        store
            .connection
            .execute_batch("DROP TRIGGER reject_history;")
            .unwrap();
        let mut state = AdaptiveLearningState::default();
        for index in 0..MAX_HISTORY {
            let receipt = format!("prior-{index}");
            state.processed_receipts.insert(receipt.clone());
            let entry = LearningHistoryEntry {
                schema_version: 1,
                receipt_id: receipt.clone(),
                decision_id: "prior-decision".to_owned(),
                mode: "full".to_owned(),
                outcome: AcceptanceState::Rejected,
                explicit_override: false,
                reason_codes: vec!["adaptive".to_owned()],
                confidence_milli: 900,
            };
            store
                .connection
                .execute(
                    "INSERT INTO autopilot_learning_history_v1 VALUES (?1, ?2, ?3)",
                    params![store.scope, receipt, serde_json::to_string(&entry).unwrap()],
                )
                .unwrap();
        }
        store
            .connection
            .execute(
                "INSERT INTO autopilot_learning_v1 VALUES (?1, 1, ?2)",
                params![store.scope, state.export_json().unwrap()],
            )
            .unwrap();
        assert!(store.observe_protocol_outcome(&handoff, &admitted).unwrap());
        let (state, history) = store.inspect().unwrap();
        assert_eq!(state.processed_receipts.len(), MAX_HISTORY + 1);
        assert!(state.processed_receipts.contains("prior-0"));
        assert_eq!(history.len(), MAX_HISTORY);
        assert_eq!(history[0].receipt_id, admitted.receipt_id());
        assert_eq!(
            history[0].reason_codes,
            handoff
                .decision()
                .reasons
                .iter()
                .map(|reason| reason.code.clone())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            store.explain(admitted.receipt_id()).unwrap(),
            Some(history[0].clone())
        );
        assert!(store.explain("prior-0").unwrap().is_none());
        assert!(!store.observe_protocol_outcome(&handoff, &admitted).unwrap());
        assert_eq!(store.history(MAX_HISTORY + 1).unwrap().len(), MAX_HISTORY);
        store.reset().unwrap();
        assert!(store.history(MAX_HISTORY).unwrap().is_empty());
    }

    #[test]
    fn corrupt_history_fails_closed() {
        let directory = tempfile::tempdir().unwrap();
        let task = protocol_task();
        let store = open(&directory.path().join("history.db"), &task);
        store
            .connection
            .execute(
                "INSERT INTO autopilot_learning_history_v1 VALUES (?1, 'broken', '{}')",
                [&store.scope],
            )
            .unwrap();
        assert!(store.history(1).is_err());
        assert!(store.explain("broken").is_err());
        assert!(store.inspect().is_err());
    }
}
