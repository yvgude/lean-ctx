// SPDX-License-Identifier: Apache-2.0
//! Task proposals through the outbox (`tasks` channel): an agent task the
//! agent asks for, or a contradiction between two facts (lean-ctx-ae
//! queues those). A proposal is screened when it is queued; `ctx_tasks
//! propose` sends it at once off the tool call ([`send_now`]), and the
//! scheduler and `lean-ctx hive sync` send what is still queued. Hive
//! answers with the task's state: a member agent's task is open at once
//! (owners and maintainers may close it afterwards); a Hive that still
//! asks for approval answers `proposed`, and that is what is reported.

use std::path::Path;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::screened;
use crate::HiveError;
use crate::auth::Session;
use crate::outbox::{CHANNEL_TASKS, Outbox};
use crate::vault::VaultMap;
use lean_ctx_protocol::hive::lease::HiveRight;
use lean_ctx_protocol::hive::tasks::{
    TASK_PROPOSE_SCHEMA, TaskOriginKind, TaskOriginV1, TaskProposeRequestV1,
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
    /// The local task this proposal brings into the vault on link
    /// (`hive link --bring-local`); part of the key, so two local tasks
    /// with the same title stay two vault tasks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_number: Option<u64>,
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
        self.session_ref
            .clone()
            .unwrap_or_else(|| crate::knowledge::draft::session_ref("hive-task-proposals"))
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
        }
    }

    /// One proposal is one task, however often it is resent or requeued:
    /// the key covers the project, the kind, the facts, (for an agent task)
    /// the title and, for a brought local task, its local number.
    #[must_use]
    pub fn idempotency_key(&self) -> String {
        let origin = self.origin();
        let title = match self.kind {
            ProposalKind::Agent => self.title.trim(),
            ProposalKind::Contradiction => "",
        };
        let mut text = format!(
            "leanctx.hive.task-proposal.v1\n{}\n{:?}\n{}\n{title}",
            self.stable_identity,
            self.kind,
            origin.facts.join(",")
        );
        // Absent for every other proposal: their keys stay as they were.
        if let Some(number) = self.local_number {
            text.push_str(&format!("\nlocal:{number}"));
        }
        let digest = Sha256::digest(text.as_bytes());
        hex::encode(&digest[..16])
    }
}

/// Secrets screened out, the request valid: before a proposal is queued
/// or sent.
fn checked(proposal: &QueuedProposal) -> Result<(), HiveError> {
    screened(&[
        ("title", Some(&proposal.title)),
        ("goal", proposal.goal.as_deref()),
        ("acceptance", Some(&proposal.acceptance)),
    ])?;
    proposal
        .plain_request()
        .validate()
        .map_err(|e| HiveError::Refused(format!("invalid task proposal: {e}")))?;
    Ok(())
}

/// Screen, validate and queue `proposal`; returns the outbox item id.
pub fn enqueue(data_dir: &Path, proposal: &QueuedProposal) -> Result<String, HiveError> {
    checked(proposal)?;
    let payload = serde_json::to_value(proposal).map_err(|e| HiveError::Local(e.to_string()))?;
    Outbox::at(data_dir).enqueue(CHANNEL_TASKS, payload, Utc::now())
}

/// Screen, validate and send `proposal` to `vault_id` now, without the
/// outbox: for a caller that needs the task number at once (bringing local
/// tasks into a vault on link). The idempotency key is the queued one, so
/// a repeat after an interrupted run gets the same task back.
pub fn propose_now(
    session: &Session,
    vault_id: &str,
    proposal: &QueuedProposal,
) -> Result<(u64, lean_ctx_protocol::hive::tasks::TaskState), HiveError> {
    checked(proposal)?;
    send(session, vault_id, proposal)
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
    let identity = crate::vault::project_identity(project_root).ok_or_else(|| {
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
            session_ref: Some(crate::knowledge::draft::session_ref(session_id)),
            local_number: None,
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

/// What became of one queued proposal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    /// Hive created the task.
    Sent(u64, lean_ctx_protocol::hive::tasks::TaskState),
    /// Kept in the outbox for a later pass (offline, 429, not linked yet).
    Waiting(String),
    /// Moved to the failed outbox; never sent.
    Failed(String),
}

const NO_PROPOSE_RIGHT: &str = "the Hive lease does not allow task proposals";

fn deliver(
    session: &Session,
    outbox: &Outbox,
    map: &VaultMap,
    mut item: crate::outbox::OutboxItem,
    now: chrono::DateTime<Utc>,
) -> Delivery {
    let Ok(proposal) = serde_json::from_value::<QueuedProposal>(item.payload.clone()) else {
        let _ = outbox.fail(&mut item, "damaged task proposal");
        return Delivery::Failed("damaged task proposal".into());
    };
    // The checkout's current vault first: a proposal queued under an
    // identity the checkout left goes where the checkout syncs now.
    let Some(record) = map
        .for_root(&proposal.root)
        .or_else(|| map.vaults.get(&proposal.stable_identity))
    else {
        let reason = "this repository is not linked to a vault yet";
        let _ = outbox.retry_later(&mut item, reason, None, now);
        return Delivery::Waiting(reason.into());
    };
    match send(session, &record.vault_id, &proposal) {
        Ok((number, state)) => {
            let _ = outbox.remove(&item);
            Delivery::Sent(number, state)
        }
        Err(error) if error.is_transient() => {
            // 429: Retry-After / retry_after_s, or a quota's reset_at.
            let retry_after = error.retry_after_at(chrono::Utc::now());
            let _ = outbox.retry_later(&mut item, &error.to_string(), retry_after, now);
            Delivery::Waiting(error.to_string())
        }
        Err(error @ HiveError::Refused(_)) => {
            // Refused for now: try again later.
            let _ = outbox.retry_later(&mut item, &error.to_string(), None, now);
            Delivery::Waiting(error.to_string())
        }
        Err(error) => {
            let _ = outbox.fail(&mut item, &error.to_string());
            Delivery::Failed(error.to_string())
        }
    }
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
        report.problems.push(NO_PROPOSE_RIGHT.into());
        return report;
    }
    // Never into a vault a checkout's link file or identity left.
    let map = match crate::resolution::load_current(&data) {
        Ok((map, _)) => map,
        Err(error) => {
            report.problems.push(error.to_string());
            VaultMap::default()
        }
    };
    let now = Utc::now();
    for item in items {
        if !item.due(now) {
            continue;
        }
        match deliver(session, &outbox, &map, item, now) {
            Delivery::Sent(number, state) => report.sent.push((number, state)),
            Delivery::Waiting(_) => {}
            Delivery::Failed(problem) => report.problems.push(problem),
        }
    }
    let depth = outbox.depth(CHANNEL_TASKS);
    report.pending = depth.pending;
    report.failed = depth.failed;
    report
}

/// Send the queued proposal `id` now instead of with the next scheduler
/// pass (`ctx_tasks propose` runs this off the tool call, as claims and
/// results go out). A proposal that cannot go out now stays queued and the
/// scheduler sends it later. `None`: the scheduler sent it meanwhile (Hive's
/// idempotency key keeps a resend of one proposal one task).
#[must_use]
pub fn send_now(session: &Session, id: &str) -> Option<Delivery> {
    let data = session.store.data_dir().to_path_buf();
    let outbox = Outbox::at(&data);
    let item = outbox
        .list(CHANNEL_TASKS)
        .into_iter()
        .find(|item| item.id == id)?;
    if !session.lease.lease.grants(HiveRight::TasksPropose) {
        return Some(Delivery::Waiting(NO_PROPOSE_RIGHT.into()));
    }
    // Never into a vault a checkout's link file or identity left.
    let map = match crate::resolution::load_current(&data) {
        Ok((map, _)) => map,
        Err(error) => return Some(Delivery::Waiting(error.to_string())),
    };
    Some(deliver(session, &outbox, &map, item, Utc::now()))
}

/// The caller's line for what [`send_now`] did.
#[must_use]
pub fn delivery_line(delivery: Option<&Delivery>) -> String {
    match delivery {
        Some(Delivery::Sent(number, state)) => created_line(*number, *state),
        Some(Delivery::Waiting(reason)) => format!(
            "Task proposal not sent yet ({reason}); it stays queued and the next Hive sync sends it."
        ),
        Some(Delivery::Failed(problem)) => format!("Task proposal not sent: {problem}."),
        None => "Task proposal went out with the last Hive sync.".into(),
    }
}

fn send(
    session: &Session,
    vault_id: &str,
    proposal: &QueuedProposal,
) -> Result<(u64, lean_ctx_protocol::hive::tasks::TaskState), HiveError> {
    let request = proposal.plain_request();
    let key = proposal.idempotency_key();
    let sent = session
        .with_client(|client| super::client::TasksApi { client, vault_id }.propose(&request, &key))
        .map(|ack| (ack.number, ack.state));
    super::reads::outdate(session.store.data_dir(), vault_id);
    sent
}

/// What `lean-ctx hive sync` prints for a task Hive created from a
/// proposal: its real state, so an agent knows whether it can claim it.
#[must_use]
pub fn created_line(number: u64, state: lean_ctx_protocol::hive::tasks::TaskState) -> String {
    use lean_ctx_protocol::hive::tasks::TaskState;
    match state {
        TaskState::Open => format!("Created task #{number}, open now (any agent may claim it)."),
        other => format!(
            "Created task #{number} ({}).",
            format!("{other:?}").to_lowercase()
        ),
    }
}
