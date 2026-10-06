// SPDX-License-Identifier: Apache-2.0
use lean_ctx_protocol::context_gateway::{
    ContextDecisionReceiptV1, ContextDecisionV1, ContextDispositionV1, DeliveryOutcomeV1,
    DestinationLocalityV1, DestinationV1, GatewayModeV1, PrincipalV1, SecurityCountsV1,
    SourceCountsV1, TokenAccountV1,
};
use lean_ctx_protocol::{
    AcceptanceState, ProtocolReference, Sha256Digest, TaskId, V1_SCHEMA_VERSION,
};

use super::*;
use crate::core::context_admission::receipt_store;
use crate::core::execution_ledger::{ContextBalanceV1, ExecutionLedgerStore};

const TASK: &str = "task-lineage-1";

fn digest(fill: char) -> Sha256Digest {
    Sha256Digest::new(format!("sha256:{}", fill.to_string().repeat(64))).expect("digest")
}

fn receipt(task: Option<&str>, id: &str) -> ContextDecisionReceiptV1 {
    ContextDecisionReceiptV1 {
        schema_version: V1_SCHEMA_VERSION,
        receipt_id: ProtocolReference::new(id).expect("reference"),
        mode: GatewayModeV1::Developer,
        principal: PrincipalV1::unknown(),
        task: task.map(|task| TaskId::new(task).expect("task id")),
        destination: DestinationV1 {
            provider: ProtocolReference::new("mcp-host").expect("provider"),
            model: None,
            locality: DestinationLocalityV1::Unknown,
            organization_managed: false,
            account_ref: None,
            region: None,
        },
        policy: None,
        sources: SourceCountsV1 {
            inspected: 2,
            permitted: 1,
            selected: 1,
            blocked: 1,
        },
        decisions: vec![
            ContextDecisionV1 {
                object: digest('a'),
                disposition: ContextDispositionV1::Allow,
                reason_codes: Vec::new(),
                signals: Vec::new(),
                required_transformations: Vec::new(),
            },
            ContextDecisionV1 {
                object: digest('b'),
                disposition: ContextDispositionV1::Deny,
                reason_codes: vec![
                    lean_ctx_protocol::context_gateway::ReasonCodeV1::new("secret.blocked")
                        .expect("reason"),
                ],
                signals: Vec::new(),
                required_transformations: Vec::new(),
            },
        ],
        security: SecurityCountsV1 {
            redactions: 0,
            blocked_objects: 1,
            quarantined_objects: 0,
            injection_signals: 0,
            incomplete_coverage: 0,
        },
        tokens: TokenAccountV1 {
            original: 900,
            delivered: 120,
        },
        final_context: Some(digest('f')),
        outcome: DeliveryOutcomeV1::Delivered,
        duration_us: 10,
        quality: None,
    }
}

fn ledger_chain(store: &ExecutionLedgerStore) {
    store
        .append(ExecutionEvent::TaskStarted {
            task_id: TASK.to_owned(),
            trace_id: "trace-1".to_owned(),
            envelope_ref: "artifact://execution/envelope/1".to_owned(),
            timestamp: "2026-10-02T12:00:00Z".to_owned(),
            sequence_number: 0,
            prev_hash: String::new(),
        })
        .expect("task started");
    store
        .append(ExecutionEvent::PlanCreated {
            task_id: TASK.to_owned(),
            trace_id: "trace-1".to_owned(),
            plan_id: "plan_0123456789abcdef".to_owned(),
            plan_ref: "artifact://execution/plan/1".to_owned(),
            timestamp: "2026-10-02T12:00:01Z".to_owned(),
            sequence_number: 0,
            prev_hash: String::new(),
        })
        .expect("plan created");
    store
        .append(ExecutionEvent::ContextDelivered {
            task_id: TASK.to_owned(),
            trace_id: "trace-1".to_owned(),
            context_balance: ContextBalanceV1 {
                original_tokens: 900,
                materialized_tokens: 400,
                delivered_tokens: 120,
                provider_billed_tokens: 0,
            },
            timestamp: "2026-10-02T12:00:02Z".to_owned(),
            sequence_number: 0,
            prev_hash: String::new(),
        })
        .expect("context delivered");
}

const SCOPE: &str = r#"[null,"project-a"]"#;

fn none() -> TaskReceipts {
    TaskReceipts {
        entries: Vec::new(),
        index_complete: true,
    }
}

/// Gate 5: one task reconstructs plan → what reached the model → cost, and
/// names the missing outcome instead of inventing one.
#[test]
fn a_task_reconstructs_plan_delivery_and_cost_from_real_stores() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ledger = ExecutionLedgerStore::new(dir.path().join("ledger.jsonl"));
    ledger_chain(&ledger);
    let gateway = dir.path().join("gateway");
    receipt_store::persist_in(&gateway, &receipt(Some(TASK), "rcpt-1"), "/p", Some(SCOPE))
        .expect("persist");
    receipt_store::persist_in(
        &gateway,
        &receipt(Some("task-other"), "rcpt-2"),
        "/p",
        Some(SCOPE),
    )
    .expect("persist");

    let lineage = build(
        TASK,
        ledger.by_task_verified(TASK).map_err(|e| e.to_string()),
        LedgerScope::Verified,
        &receipt_store::for_task_in(&gateway, SCOPE, TASK),
    );

    let kinds: Vec<_> = lineage.steps.iter().map(|step| step.kind).collect();
    assert_eq!(kinds, ["task_started", "plan_created", "context_delivered"]);
    assert_eq!(
        lineage.steps[1].fields.get("plan_id").map(String::as_str),
        Some("plan_0123456789abcdef")
    );
    assert_eq!(lineage.deliveries.len(), 1, "only this task's receipt");
    let summary = lineage.deliveries[0].summary.as_ref().expect("verified");
    assert_eq!(
        (summary.inspected, summary.delivered, summary.withheld),
        (2, 1, 1)
    );
    assert_eq!(
        (summary.tokens_original, summary.tokens_delivered),
        (900, 120)
    );
    assert_eq!(lineage.outcome, "unknown");
    assert_eq!(lineage.gaps, ["no_outcome_recorded"]);
}

/// Equal task ids in two projects never share receipts.
#[test]
fn another_projects_task_with_the_same_id_stays_apart() {
    let dir = tempfile::tempdir().expect("tempdir");
    let gateway = dir.path().join("gateway");
    receipt_store::persist_in(&gateway, &receipt(Some(TASK), "rcpt-1"), "/a", Some(SCOPE))
        .expect("persist");
    let other_scope = r#"[null,"project-b"]"#;
    assert!(
        receipt_store::for_task_in(&gateway, other_scope, TASK)
            .entries
            .is_empty()
    );
    assert_eq!(
        receipt_store::for_task_in(&gateway, SCOPE, TASK)
            .entries
            .len(),
        1
    );
}

#[test]
fn a_foreign_or_unproven_ledger_is_not_shown() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ledger = ExecutionLedgerStore::new(dir.path().join("ledger.jsonl"));
    ledger_chain(&ledger);
    let events = || ledger.by_task_verified(TASK).map_err(|e| e.to_string());
    for scope in [LedgerScope::Foreign, LedgerScope::Unverified] {
        let lineage = build(TASK, events(), scope, &none());
        assert!(lineage.steps.is_empty(), "{scope:?}");
        assert!(lineage.gaps.contains(&"ledger_unverified"), "{scope:?}");
    }
    // The ledger's envelope reference is not readable here: unproven.
    let loaded = events().expect("events");
    assert_eq!(ledger_scope(&loaded, SCOPE), LedgerScope::Unverified);
    assert_eq!(ledger_scope(&[], SCOPE), LedgerScope::Verified);
}

#[test]
fn a_recorded_outcome_closes_the_chain() {
    let dir = tempfile::tempdir().expect("tempdir");
    let gateway = dir.path().join("gateway");
    receipt_store::persist_in(&gateway, &receipt(Some(TASK), "rcpt-1"), "/p", Some(SCOPE))
        .expect("persist");
    let events = vec![
        ExecutionEvent::DecisionRecorded {
            task_id: TASK.to_owned(),
            trace_id: "trace-1".to_owned(),
            decision_id: "autopilot_0123456789abcdef".to_owned(),
            kind: "context_selection".to_owned(),
            selected: "map".to_owned(),
            timestamp: "2026-10-02T12:00:01Z".to_owned(),
            sequence_number: 1,
            prev_hash: String::new(),
        },
        ExecutionEvent::OutcomeRecorded {
            task_id: TASK.to_owned(),
            trace_id: "trace-1".to_owned(),
            outcome_id: "outcome-1".to_owned(),
            receipt_id: "receipt-1".to_owned(),
            accepted: AcceptanceState::Accepted,
            timestamp: "2026-10-02T12:00:03Z".to_owned(),
            sequence_number: 2,
            prev_hash: String::new(),
        },
    ];

    let lineage = build(
        TASK,
        Ok(events),
        LedgerScope::Verified,
        &receipt_store::for_task_in(&gateway, SCOPE, TASK),
    );

    assert_eq!(lineage.outcome, "accepted");
    assert!(lineage.gaps.is_empty(), "{:?}", lineage.gaps);
}

#[test]
fn every_missing_link_is_named() {
    let lineage = build(
        TASK,
        Err("chain broken".to_owned()),
        LedgerScope::Verified,
        &TaskReceipts {
            entries: Vec::new(),
            index_complete: false,
        },
    );
    assert_eq!(
        lineage.gaps,
        [
            "ledger_unverified",
            "no_plan_recorded",
            "no_delivery_recorded",
            "deliveries_incomplete",
            "no_outcome_recorded"
        ]
    );
    assert_eq!(lineage.ledger_error.as_deref(), Some("chain broken"));
}

#[test]
fn an_edited_receipt_is_reported_not_shown() {
    let dir = tempfile::tempdir().expect("tempdir");
    let gateway = dir.path().join("gateway");
    let digest =
        receipt_store::persist_in(&gateway, &receipt(Some(TASK), "rcpt-1"), "/p", Some(SCOPE))
            .expect("persist");
    let path = gateway
        .join("receipts")
        .join(format!("{}.json", digest.hex()));
    let edited = std::fs::read_to_string(&path)
        .expect("stored receipt")
        .replace("\"delivered\":120", "\"delivered\":1");
    std::fs::write(&path, edited).expect("edit");

    let receipts = receipt_store::for_task_in(&gateway, SCOPE, TASK);
    assert!(!receipts.complete());
    let lineage = build(TASK, Ok(Vec::new()), LedgerScope::Verified, &receipts);

    assert!(!lineage.deliveries[0].verified);
    assert_eq!(lineage.deliveries[0].error, Some("tampered"));
    assert!(lineage.deliveries[0].summary.is_none());
    assert!(lineage.gaps.contains(&"delivery_unverified"));
}

#[test]
fn a_damaged_task_index_is_reported_and_never_overwritten() {
    let dir = tempfile::tempdir().expect("tempdir");
    let gateway = dir.path().join("gateway");
    receipt_store::persist_in(&gateway, &receipt(Some(TASK), "rcpt-1"), "/p", Some(SCOPE))
        .expect("persist");
    let index = std::fs::read_dir(gateway.join("tasks"))
        .expect("tasks dir")
        .next()
        .expect("one index")
        .expect("entry")
        .path();
    std::fs::write(&index, "{not json").expect("damage");

    let receipts = receipt_store::for_task_in(&gateway, SCOPE, TASK);
    assert!(!receipts.index_complete && receipts.entries.is_empty());
    receipt_store::persist_in(&gateway, &receipt(Some(TASK), "rcpt-2"), "/p", Some(SCOPE))
        .expect("persist");
    assert_eq!(
        std::fs::read_to_string(&index).expect("index"),
        "{not json",
        "a damaged index is left for the reader to report"
    );

    std::fs::write(&index, "x".repeat(64 * 1024)).expect("oversize");
    assert!(!receipt_store::for_task_in(&gateway, SCOPE, TASK).index_complete);
}

#[test]
fn a_receipt_without_a_task_or_scope_is_not_indexed_under_one() {
    let dir = tempfile::tempdir().expect("tempdir");
    let gateway = dir.path().join("gateway");
    receipt_store::persist_in(&gateway, &receipt(None, "rcpt-1"), "/p", Some(SCOPE))
        .expect("persist");
    receipt_store::persist_in(&gateway, &receipt(Some(TASK), "rcpt-2"), "/p", None)
        .expect("persist");
    assert!(
        receipt_store::for_task_in(&gateway, SCOPE, TASK)
            .entries
            .is_empty()
    );
    assert!(!gateway.join("tasks").exists());
}

/// A held index lock never blocks the tool call for long and never loses the
/// receipt; the task is marked incomplete instead.
#[test]
fn a_held_index_lock_keeps_the_receipt_and_marks_the_task_incomplete() {
    use fs2::FileExt;
    let dir = tempfile::tempdir().expect("tempdir");
    let gateway = dir.path().join("gateway");
    std::fs::create_dir_all(&gateway).expect("gateway dir");
    let holder = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(gateway.join(".index.lock"))
        .expect("lock file");
    holder.lock_exclusive().expect("hold lock");

    let started = std::time::Instant::now();
    let digest =
        receipt_store::persist_in(&gateway, &receipt(Some(TASK), "rcpt-1"), "/p", Some(SCOPE))
            .expect("the receipt is still stored");
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    assert!(
        gateway
            .join("receipts")
            .join(format!("{}.json", digest.hex()))
            .exists()
    );
    let receipts = receipt_store::for_task_in(&gateway, SCOPE, TASK);
    assert!(!receipts.index_complete && !receipts.complete());
    drop(holder);
}

#[test]
fn the_same_receipt_is_indexed_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    let gateway = dir.path().join("gateway");
    for _ in 0..3 {
        receipt_store::persist_in(&gateway, &receipt(Some(TASK), "rcpt-1"), "/p", Some(SCOPE))
            .expect("persist");
    }
    assert_eq!(
        receipt_store::for_task_in(&gateway, SCOPE, TASK)
            .entries
            .len(),
        1
    );
}

#[test]
fn lineage_serializes_deterministically() {
    let lineage = build(TASK, Ok(Vec::new()), LedgerScope::Verified, &none());
    let first = serde_json::to_string(&lineage).expect("json");
    let second =
        serde_json::to_string(&build(TASK, Ok(Vec::new()), LedgerScope::Verified, &none()))
            .expect("json");
    assert_eq!(first, second);
    assert!(first.contains("\"schema_version\":1"));
}

/// Store a real task envelope for `project` and return its `TaskStarted`.
fn task_started_in(project: &str) -> (ExecutionEvent, String) {
    let envelope = crate::core::task_spine::TaskSpine::create_envelope_in_project(
        "query",
        "session-1",
        "agent-1",
        Some(project),
    );
    let bytes = crate::core::canonical::canonical_serialize(&envelope);
    let digest = crate::core::execution_ledger::host::digest(&bytes).expect("digest");
    drop(
        crate::core::engine_interface::persist_engine_artifact_content(
            "execution/evidence",
            digest.hex(),
            "json",
            &bytes,
        )
        .expect("persist envelope"),
    );
    let scope = super::super::task_scope(envelope.tenant_id.as_ref(), &envelope.project_id);
    (
        ExecutionEvent::TaskStarted {
            task_id: TASK.to_owned(),
            trace_id: "trace-1".to_owned(),
            envelope_ref: digest.as_str().to_owned(),
            timestamp: "2026-10-03T12:00:00Z".to_owned(),
            sequence_number: 1,
            prev_hash: String::new(),
        },
        scope,
    )
}

/// The ledger is shared and keyed by task id alone: when two projects used
/// the same id, neither project is shown the mixed history.
#[test]
fn every_envelope_must_prove_the_scope() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let (started_a, scope_a) = task_started_in("/work/project-a");
    let (started_b, scope_b) = task_started_in("/work/project-b");

    assert_eq!(
        ledger_scope(std::slice::from_ref(&started_a), &scope_a),
        LedgerScope::Verified
    );
    assert_eq!(
        ledger_scope(std::slice::from_ref(&started_a), &scope_b),
        LedgerScope::Foreign
    );
    let mixed = [started_a, started_b];
    assert_eq!(ledger_scope(&mixed, &scope_a), LedgerScope::Foreign);
    assert_eq!(ledger_scope(&mixed, &scope_b), LedgerScope::Foreign);
}
