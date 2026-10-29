// SPDX-License-Identifier: Apache-2.0
//! Task proposals through the outbox (`tasks` channel): an agent task the
//! agent asks for, or a contradiction between two facts (lean-ctx-ae
//! queues those). A proposal is screened when it is queued; the scheduler
//! and `lean-ctx hive sync` send it, sealed in sealed projects. Hive
//! answers with the task's state: a member agent's task is open at once
//! (owners and maintainers may close it afterwards); a Hive that still
//! asks for approval answers `proposed`, and that is what is reported.

use std::path::Path;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{TaskContent, screened};
use crate::core::hive::HiveError;
use crate::core::hive::auth::Session;
use crate::core::hive::outbox::{CHANNEL_TASKS, Outbox};
use crate::core::hive::sealed::{self, SealingKey};
use crate::core::hive::vault::VaultMap;
use lean_ctx_protocol::hive::lease::HiveRight;
use lean_ctx_protocol::hive::tasks::{
    SEALED_TASK_CONTENT_SCHEMA, TASK_PROPOSE_SCHEMA, TaskOriginKind, TaskOriginV1,
    TaskProposeRequestV1,
};

/// What a proposal is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalKind {
    Agent,
    Contradiction,
}

/// One queued proposal (the outbox payload).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedProposal {
    pub kind: ProposalKind,
    /// `stable_identity` of the project (as knowledge drafts name it).
    pub stable_identity: String,
    pub root: String,
    /// The two contradicting facts; empty for an agent task.
    #[serde(default)]
    pub facts: Vec<String>,
    pub title: String,
    #[serde(default)]
    pub goal: Option<String>,
    pub acceptance: String,
    #[serde(default)]
    pub role: Option<String>,
    /// Opaque session reference (`knowledge::draft::session_ref`).
    #[serde(default)]
    pub session_ref: Option<String>,
}

impl QueuedProposal {
    fn origin(&self) -> TaskOriginV1 {
        let mut facts = self.facts.clone();
        facts.sort();
        TaskOriginV1 {
            kind: match self.kind {
                ProposalKind::Agent => TaskOriginKind::Agent,
                ProposalKind::Contradiction => TaskOriginKind::Contradiction,
            },
            facts,
            message: None,
        }
    }

    fn session_ref(&self) -> String {
        self.session_ref.clone().unwrap_or_else(|| {
            crate::core::hive::knowledge::draft::session_ref("hive-task-proposals")
        })
    }

    /// The request in the clear (validated before it is queued and sent).
    fn plain_request(&self) -> TaskProposeRequestV1 {
        TaskProposeRequestV1 {
            schema: TASK_PROPOSE_SCHEMA.into(),
            title: Some(self.title.clone()),
            goal: self.goal.clone(),
            acceptance: Some(self.acceptance.clone()),
            role: self.role.clone(),
            origin: self.origin(),
            session_ref: self.session_ref(),
            sealed: None,
        }
    }

    /// One proposal is one task, however often it is resent or requeued:
    /// the key covers the project, the kind, the facts and (for an agent
    /// task) the title.
    #[must_use]
    pub fn idempotency_key(&self) -> String {
        let origin = self.origin();
        let title = match self.kind {
            ProposalKind::Agent => self.title.trim(),
            ProposalKind::Contradiction => "",
        };
        let digest = Sha256::digest(
            format!(
                "leanctx.hive.task-proposal.v1\n{}\n{:?}\n{}\n{title}",
                self.stable_identity,
                self.kind,
                origin.facts.join(",")
            )
            .as_bytes(),
        );
        hex::encode(&digest[..16])
    }
}

/// Screen, validate and queue `proposal`; returns the outbox item id.
pub fn enqueue(data_dir: &Path, proposal: &QueuedProposal) -> Result<String, HiveError> {
    screened(&[
        ("title", Some(&proposal.title)),
        ("goal", proposal.goal.as_deref()),
        ("acceptance", Some(&proposal.acceptance)),
    ])?;
    proposal
        .plain_request()
        .validate()
        .map_err(|e| HiveError::Refused(format!("invalid task proposal: {e}")))?;
    let payload = serde_json::to_value(proposal).map_err(|e| HiveError::Local(e.to_string()))?;
    Outbox::at(data_dir).enqueue(CHANNEL_TASKS, payload, Utc::now())
}

/// An agent task for the project at `project_root`.
pub fn propose_agent_task(
    data_dir: &Path,
    project_root: &Path,
    title: &str,
    goal: Option<&str>,
    acceptance: &str,
    role: Option<&str>,
    session_id: &str,
) -> Result<String, HiveError> {
    let identity = crate::core::hive::vault::project_identity(project_root).ok_or_else(|| {
        HiveError::Config(
            "this repository has no identity Hive can link to a vault (no git remote)".into(),
        )
    })?;
    enqueue(
        data_dir,
        &QueuedProposal {
            kind: ProposalKind::Agent,
            stable_identity: lean_ctx_protocol::hive::vault::stable_identity(&identity),
            root: project_root.to_string_lossy().into_owned(),
            facts: Vec::new(),
            title: title.trim().to_owned(),
            goal: goal
                .map(str::trim)
                .filter(|g| !g.is_empty())
                .map(str::to_owned),
            acceptance: acceptance.trim().to_owned(),
            role: role.map(str::to_owned),
            session_ref: Some(crate::core::hive::knowledge::draft::session_ref(session_id)),
        },
    )
}

/// What one drain pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DrainReport {
    /// The tasks Hive created, with the state it gave each.
    pub sent: Vec<(u64, lean_ctx_protocol::hive::tasks::TaskState)>,
    pub pending: usize,
    pub failed: usize,
    pub problems: Vec<String>,
}

/// Send the due proposals (scheduler, `lean-ctx hive sync`; never inside
/// a tool call).
pub fn drain(session: &Session) -> DrainReport {
    let data = session.store.data_dir().to_path_buf();
    let outbox = Outbox::at(&data);
    let mut report = DrainReport::default();
    let items = outbox.list(CHANNEL_TASKS);
    if !items.is_empty() && !session.lease.lease.grants(HiveRight::TasksPropose) {
        report.pending = items.len();
        report
            .problems
            .push("the Hive lease does not allow task proposals".into());
        return report;
    }
    let map = VaultMap::load(&data);
    let now = Utc::now();
    for mut item in items {
        if !item.due(now) {
            continue;
        }
        let Ok(proposal) = serde_json::from_value::<QueuedProposal>(item.payload.clone()) else {
            let _ = outbox.fail(&mut item, "damaged task proposal");
            continue;
        };
        let Some(record) = map.vaults.get(&proposal.stable_identity) else {
            let _ = outbox.retry_later(
                &mut item,
                "this repository is not linked to a vault yet",
                None,
                now,
            );
            continue;
        };
        match send(session, &record.vault_id, record.sealed, &proposal) {
            Ok(created) => {
                report.sent.push(created);
                let _ = outbox.remove(&item);
            }
            Err(error) if error.is_transient() => {
                // 429: Retry-After / retry_after_s, or a quota's reset_at.
                let retry_after = error.retry_after_at(chrono::Utc::now());
                let _ = outbox.retry_later(&mut item, &error.to_string(), retry_after, now);
            }
            Err(error @ HiveError::Refused(_)) => {
                // Sealed key not here yet or rotating: try again later.
                let _ = outbox.retry_later(&mut item, &error.to_string(), None, now);
            }
            Err(error) => {
                report.problems.push(error.to_string());
                let _ = outbox.fail(&mut item, &error.to_string());
            }
        }
    }
    let depth = outbox.depth(CHANNEL_TASKS);
    report.pending = depth.pending;
    report.failed = depth.failed;
    report
}

fn send(
    session: &Session,
    vault_id: &str,
    sealed_project: bool,
    proposal: &QueuedProposal,
) -> Result<(u64, lean_ctx_protocol::hive::tasks::TaskState), HiveError> {
    let mut request = proposal.plain_request();
    if sealed_project {
        let (epoch, key) = match sealed::sealing_key(session, vault_id, 1)? {
            SealingKey::Ready(epoch, key) => (epoch, key),
            SealingKey::Waiting | SealingKey::Rotating { .. } => {
                return Err(HiveError::Refused(
                    "the sealed vault's key is not ready on this machine".into(),
                ));
            }
        };
        let content = TaskContent {
            title: proposal.title.clone(),
            goal: proposal.goal.clone(),
            acceptance: proposal.acceptance.clone(),
        };
        let bytes = zeroize::Zeroizing::new(
            serde_json::to_vec(&content).map_err(|e| HiveError::Local(e.to_string()))?,
        );
        request.title = None;
        request.goal = None;
        request.acceptance = None;
        request.sealed = Some(sealed::seal(
            &key,
            vault_id,
            0,
            epoch,
            SEALED_TASK_CONTENT_SCHEMA,
            &bytes,
        )?);
    }
    let key = proposal.idempotency_key();
    session
        .with_client(|client| super::client::TasksApi { client, vault_id }.propose(&request, &key))
        .map(|ack| (ack.number, ack.state))
}

/// What `lean-ctx hive sync` prints for a task Hive created from a
/// proposal: its real state, so an agent knows whether it can claim it.
#[must_use]
pub fn created_line(number: u64, state: lean_ctx_protocol::hive::tasks::TaskState) -> String {
    use lean_ctx_protocol::hive::tasks::TaskState;
    match state {
        TaskState::Open => format!("Created task #{number}, open now (any agent may claim it)."),
        TaskState::Proposed => format!("Proposed task #{number} (waits for a person's approval)."),
        other => format!(
            "Created task #{number} ({}).",
            format!("{other:?}").to_lowercase()
        ),
    }
}
