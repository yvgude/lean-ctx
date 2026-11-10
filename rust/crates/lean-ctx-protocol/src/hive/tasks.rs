// SPDX-License-Identifier: Apache-2.0

//! Tasks, claims and results (C0 1.9, section 1).
//!
//! A task is a question or a piece of work in one Hive vault with an
//! acceptance criterion. People create tasks on the web. An engine may only
//! *propose* one, for its agent or for a contradiction it found, and a person
//! approves it before anyone can claim it.
//!
//! The rules:
//! - A task has at most one claim at a time. The claim is a bus lease
//!   (`kind task`, section 2), so Hold, Veto and Stop end it like any lease.
//! - The holder reports short summaries (never prompts) and submits one
//!   result with evidence. Every write carries the claim's fencing token, so
//!   a stale holder is refused.
//! - A result is a proposal (`origin task`), with its knowledge change if it
//!   brings one.
//! - The claiming instance never decides on it, not through its instance
//!   token either. A person decides (the instance's owner included), or an
//!   auto-merge threshold. A threshold counts independent confirmations: in
//!   Team people and agents of other owners (one by
//!   default, configurable per vault), and in Personal sessions on the owner's
//!   other instances (one by default): never the claiming instance, in any
//!   of its sessions (D1 per instance).
//! - A signal (message, offer) never changes a task's state.
//!
//! ```text
//! proposed --approve--> open --claim--> claimed --result--> submitted --accept--> done
//!    |                   ^  ^              |                     |
//!    +--reject--> rejected  +--release / expiry / veto / stop ---+   +--decline--> open
//! proposed/open/claimed --cancel (people)--> cancelled
//! ```
//!
//! Hive v5 (additive): a result ends the task at once, `done`, or `failed`
//! when the request says `outcome: failed`; no person decides on it. Task
//! views carry a [`TaskLifecycle`] next to `state` (`queued → claimed →
//! running → completed | failed | cancelled`), and a result references the
//! log `seq` of its knowledge instead of a proposal (`proposal` 0). A 1.x
//! Hive answers `submitted` and sends no `lifecycle`; an engine sends
//! `outcome` only to a Hive whose task views carry `lifecycle`.
//!
//! Engine routes, all under `/v1/vaults/{vault_id}/tasks`:
//!
//! | Route | Body | Answer | Lease right |
//! |---|---|---|---|
//! | `GET` (`?state=…&role=…&mine=true&priority=…`) | | [`TaskListV1`] | `tasks.read` |
//! | `GET /{number}` | | [`TaskV1`] | `tasks.read` |
//! | `POST` | [`TaskProposeRequestV1`] | [`TaskAckV1`] (`proposed`) | `tasks.propose` |
//! | `POST /{number}/claim` | [`TaskClaimRequestV1`] | [`TaskClaimAckV1`] | `tasks.work` |
//! | `POST /{number}/claim/renew` | [`TaskClaimRenewRequestV1`] | [`TaskClaimAckV1`] | `tasks.work` |
//! | `POST /{number}/release` | [`TaskReleaseRequestV1`] | [`TaskAckV1`] (`open`) | `tasks.work` |
//! | `POST /{number}/updates` | [`TaskUpdateRequestV1`] | [`TaskAckV1`] | `tasks.work` |
//! | `POST /{number}/result` | [`TaskResultRequestV1`] | [`TaskAckV1`] (`submitted`; v5 `done` or `failed`) | `tasks.work` |
//! | `POST /{number}/verify` | [`TaskVerifyRequestV1`] | [`TaskAckV1`] (`done` or `open`; C0 2.2) | `tasks.work` |
//! | `PATCH /{number}` | [`TaskFieldsRequestV1`] | [`TaskAckV1`] (C0 2.2) | `tasks.propose` |
//!
//! C0 2.2: a result leaves the task `submitted` (`failed` with `outcome:
//! failed`) until another agent verifies it (E48): `accept` → `done`,
//! `reject` → `open`. The verifier is never the result's worker (`403
//! forbidden_right`, `reason` `self_verify`), and only a submitted task
//! takes a verdict (`409 invalid_state`, `reason` `not_submitted`).
//!
//! Errors carry `number` (`HiveErrorDetailV1::number`):
//! - `409 invalid_state`, `reason`:
//!   - `task_claimed`: another claim holds the task;
//!   - `task_not_open`: claim on a task that is not `open`;
//!   - `stale_claim`: the lease id or fencing token is not the current claim's;
//!   - `task_closed`: any write to a `done`, `rejected` or `cancelled` task;
//!   - `reviewer_not_independent`: a confirmation that a threshold may not count.
//! - `403 forbidden_right`, `reason`:
//!   - `not_permitted`: the task's role or the owner's worker consent does
//!     not allow this agent;
//!   - `vetoed`: a veto or stop holds this agent;
//!   - `optin_scope`: the engine's opt-in does not cover this vault
//!     (section 4).
//! - `409 invalid_state`, `reason` `optin_changed` or `optin_inactive`
//!   (section 4).
//! - `429 rate_limited`, `reason` `daily_budget`: the opt-in's daily
//!   budget is used up (`retry_after_s` until it resets).
//!
//! A proposed task, an update and a result carry their text in the clear.

use serde::{Deserialize, Serialize};

use super::anchor::CodeAnchorV1;
use super::knowledge::DraftProposalV1;
use super::{require_schema, validate_hex64, validate_id, validate_text};
use crate::{UtcTimestamp, ValidationError};

pub const TASK_SCHEMA: &str = "leanctx.hive.task/v1";
pub const TASK_LIST_SCHEMA: &str = "leanctx.hive.task-list/v1";
pub const TASK_ACK_SCHEMA: &str = "leanctx.hive.task-ack/v1";
pub const TASK_PROPOSE_SCHEMA: &str = "leanctx.hive.task-propose/v1";
pub const TASK_CLAIM_REQUEST_SCHEMA: &str = "leanctx.hive.task-claim-request/v1";
pub const TASK_CLAIM_RENEW_SCHEMA: &str = "leanctx.hive.task-claim-renew/v1";
pub const TASK_CLAIM_ACK_SCHEMA: &str = "leanctx.hive.task-claim/v1";
pub const TASK_RELEASE_SCHEMA: &str = "leanctx.hive.task-release/v1";
pub const TASK_UPDATE_SCHEMA: &str = "leanctx.hive.task-update/v1";
pub const TASK_RESULT_SCHEMA: &str = "leanctx.hive.task-result/v1";
/// C0 2.2 E48: another agent's verdict on a submitted result.
pub const TASK_VERIFY_SCHEMA: &str = "leanctx.hive.task-verify/v1";
/// 3.12.3: the submitter corrects its submitted result (summary, evidence).
pub const TASK_AMEND_SCHEMA: &str = "leanctx.hive.task-amend/v1";
/// C0 2.2 E47 `PATCH`: a task's priority and labels.
pub const TASK_FIELDS_SCHEMA: &str = "leanctx.hive.task-fields/v1";

pub const MIN_TITLE_CHARS: usize = 5;
pub const MAX_TITLE_CHARS: usize = 180;
pub const MAX_GOAL_CHARS: usize = 2000;
pub const MAX_ACCEPTANCE_CHARS: usize = 2000;
/// Updates and result summaries are summaries, not prompts: one line.
pub const MAX_SUMMARY_CHARS: usize = 500;
pub const MAX_REASON_CHARS: usize = 280;
/// A verifier's reason (C0 2.2 E48), required on `reject`.
pub const MAX_VERIFY_REASON_CHARS: usize = 2000;
pub const MAX_TASK_LABELS: usize = 16;
pub const MAX_EVIDENCE: usize = 20;
pub const MAX_EVIDENCE_REF_CHARS: usize = 512;
/// Updates a task view carries, newest last.
pub const MAX_VIEW_UPDATES: usize = 20;
pub const MAX_TASKS_PAGE: usize = 100;
/// A claim is a bus lease: 5 to 15 minutes ([`super::bus::MAX_BUS_LEASE_MS`]),
/// renewed while work goes on, so a silent agent frees the task quickly.
pub const MIN_CLAIM_MINUTES: u32 = 5;
pub const MAX_CLAIM_MINUTES: u32 = 15;

/// Lifecycle of a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    /// Proposed by an engine or the system; a person must approve it.
    Proposed,
    Open,
    Claimed,
    /// A result waits for its decision.
    Submitted,
    Done,
    /// A proposed task a person declined.
    Rejected,
    Cancelled,
    /// Hive v5: the holder ended its claim with a failure (terminal).
    Failed,
    #[serde(other)]
    Unknown,
}

/// Hive v5 lifecycle of a task, sent next to `state` (additive):
/// `queued → claimed → running → completed | failed | cancelled`.
/// `running` = claimed, and the holder reported progress under this claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskLifecycle {
    Queued,
    Claimed,
    Running,
    /// C0 2.2: a result waits for a second agent's check.
    Submitted,
    Completed,
    Failed,
    Cancelled,
    #[serde(other)]
    Unknown,
}

/// How the holder ends its claim with a result (Hive v5); absent on the
/// wire means `completed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskOutcome {
    #[default]
    Completed,
    Failed,
}

/// Who created a task, and why.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskOriginKind {
    /// A person, on the web.
    Person,
    /// An engine, for its agent.
    Agent,
    /// An engine that found two facts contradicting each other.
    Contradiction,
    /// A question no agent answered (section 2).
    Question,
    /// Any other origin.
    #[serde(other)]
    Unknown,
}

/// Origin of a task. `facts` names exactly two distinct fact ids for a
/// contradiction; `message` names the question's message id for a
/// question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskOriginV1 {
    pub kind: TaskOriginKind,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub facts: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl TaskOriginV1 {
    /// What an engine may send: `agent`, or `contradiction` with two facts.
    pub fn validate_from_engine(&self) -> Result<(), ValidationError> {
        if self.message.is_some() {
            return Err(ValidationError::new("engines do not create question tasks"));
        }
        match self.kind {
            TaskOriginKind::Agent if self.facts.is_empty() => Ok(()),
            TaskOriginKind::Agent => Err(ValidationError::new("an agent task names no facts")),
            TaskOriginKind::Contradiction => {
                if self.facts.len() != 2 || self.facts[0] == self.facts[1] {
                    return Err(ValidationError::new(
                        "a contradiction names exactly two distinct facts",
                    ));
                }
                self.facts
                    .iter()
                    .try_for_each(|id| validate_id(id, "facts"))
            }
            _ => Err(ValidationError::new(
                "engines propose agent or contradiction tasks only",
            )),
        }
    }
}

/// An actor as an engine sees it: never a user id or an email.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskActorV1 {
    pub kind: TaskActorKind,
    /// A person's display name, or an instance label.
    pub label: String,
    /// The agent on that instance (bus agent id), for instances.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// The person who owns that instance (display name), for instances.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// The model the agent reported for itself; unverified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Whether this is the instance asking.
    pub mine: bool,
    /// 1.9 (section 5): the agent's profile, for instances.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskActorKind {
    Person,
    Instance,
    #[serde(other)]
    Unknown,
}

/// One piece of evidence. `reference` is what the kind names:
/// - `receipt`: a receipt digest (64 lowercase hex), checked by Hive;
/// - `commit`: a commit id (7-64 hex);
/// - `ci_run`: a CI run reference (an `https` URL or a provider id);
/// - `file_anchor`: absent, the anchor is in `anchor`;
/// - `url`: an `https` URL, which Hive never fetches;
/// - `fact`: a fact id of the vault.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceV1 {
    pub kind: EvidenceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<CodeAnchorV1>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    Receipt,
    Commit,
    CiRun,
    FileAnchor,
    Url,
    Fact,
    #[serde(other)]
    Unknown,
}

impl EvidenceV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        let refuse = || ValidationError::new(format!("evidence does not fit kind {:?}", self.kind));
        if self.kind == EvidenceKind::FileAnchor {
            return match (&self.reference, &self.anchor) {
                (None, Some(anchor)) => anchor.validate(),
                _ => Err(refuse()),
            };
        }
        if self.anchor.is_some() {
            return Err(refuse());
        }
        let reference = self.reference.as_deref().ok_or_else(refuse)?;
        validate_text(
            reference,
            MAX_EVIDENCE_REF_CHARS,
            "evidence.reference",
            false,
        )?;
        let https = |r: &str| r.starts_with("https://") && !r.contains(char::is_whitespace);
        let ok = match self.kind {
            EvidenceKind::Receipt => validate_hex64(reference, "evidence.reference").is_ok(),
            EvidenceKind::Commit => {
                (7..=64).contains(&reference.len())
                    && reference
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            }
            EvidenceKind::CiRun => https(reference) || validate_id(reference, "ci_run").is_ok(),
            EvidenceKind::Url => https(reference),
            EvidenceKind::Fact => validate_id(reference, "fact").is_ok(),
            EvidenceKind::FileAnchor | EvidenceKind::Unknown => false,
        };
        if ok { Ok(()) } else { Err(refuse()) }
    }
}

fn validate_evidence(evidence: &[EvidenceV1]) -> Result<(), ValidationError> {
    if evidence.is_empty() || evidence.len() > MAX_EVIDENCE {
        return Err(ValidationError::new(format!(
            "a result carries 1-{MAX_EVIDENCE} pieces of evidence"
        )));
    }
    for (i, item) in evidence.iter().enumerate() {
        item.validate()?;
        if evidence[..i].contains(item) {
            return Err(ValidationError::new("evidence repeats"));
        }
    }
    Ok(())
}

fn validate_title(title: &str) -> Result<(), ValidationError> {
    validate_text(title, MAX_TITLE_CHARS, "title", false)?;
    if title.trim().chars().count() < MIN_TITLE_CHARS {
        return Err(ValidationError::new(format!(
            "title must have at least {MIN_TITLE_CHARS} characters"
        )));
    }
    Ok(())
}

/// An agent role slug (section 5): 1-64 of `[a-z0-9-]`, starting with a letter.
pub fn validate_role(role: &str) -> Result<(), ValidationError> {
    let bytes = role.as_bytes();
    if !bytes.is_empty()
        && bytes.len() <= 64
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
    {
        Ok(())
    } else {
        Err(ValidationError::new(
            "role must be 1-64 characters of [a-z0-9-] starting with a letter",
        ))
    }
}

/// A task write carries its text (`plain`).
fn text_required(plain: bool) -> Result<(), ValidationError> {
    if plain {
        Ok(())
    } else {
        Err(ValidationError::new("text is required"))
    }
}

/// The claim a write acts under: the lease id and its fencing token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimRefV1 {
    pub lease_id: String,
    pub fencing_token: u64,
}

impl ClaimRefV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_id(&self.lease_id, "lease_id")?;
        if self.fencing_token == 0 {
            return Err(ValidationError::new("fencing tokens start at 1"));
        }
        Ok(())
    }
}

fn validate_minutes(minutes: u32) -> Result<(), ValidationError> {
    if (MIN_CLAIM_MINUTES..=MAX_CLAIM_MINUTES).contains(&minutes) {
        Ok(())
    } else {
        Err(ValidationError::new(format!(
            "minutes must be {MIN_CLAIM_MINUTES}-{MAX_CLAIM_MINUTES}"
        )))
    }
}

/// `POST /v1/vaults/{vault_id}/tasks`: an engine proposes a task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskProposeRequestV1 {
    pub schema: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
    /// When the task is done; required in the clear.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acceptance: Option<String>,
    /// The agent role it is for (section 5); any role when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub origin: TaskOriginV1,
    /// Opaque, per-session id (no host or user data).
    pub session_ref: String,
}

impl TaskProposeRequestV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, TASK_PROPOSE_SCHEMA)?;
        self.origin.validate_from_engine()?;
        validate_id(&self.session_ref, "session_ref")?;
        if let Some(role) = &self.role {
            validate_role(role)?;
        }
        let plain = self.title.is_some() || self.goal.is_some() || self.acceptance.is_some();
        text_required(plain)?;
        if plain {
            validate_title(self.title.as_deref().unwrap_or_default())?;
            if let Some(goal) = &self.goal {
                validate_text(goal, MAX_GOAL_CHARS, "goal", true)?;
            }
            validate_text(
                self.acceptance.as_deref().unwrap_or_default(),
                MAX_ACCEPTANCE_CHARS,
                "acceptance",
                true,
            )?;
        }
        Ok(())
    }
}

/// `POST …/{number}/claim`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskClaimRequestV1 {
    pub schema: String,
    /// The bus agent id the claim is for.
    pub agent: String,
    pub session_ref: String,
    pub minutes: u32,
    /// The engine opt-in the claim works under (section 4), whose budget
    /// it draws on: the `optin_id` the engine last read. Hive requires it
    /// and refuses a stale one (`optin_changed`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub optin_id: Option<String>,
}

impl TaskClaimRequestV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, TASK_CLAIM_REQUEST_SCHEMA)?;
        validate_id(&self.agent, "agent")?;
        validate_id(&self.session_ref, "session_ref")?;
        if let Some(optin) = &self.optin_id {
            validate_id(optin, "optin_id")?;
        }
        validate_minutes(self.minutes)
    }
}

/// `POST …/{number}/claim/renew`. 1.9 erratum: the renewed claim keeps its
/// lease id and fencing token (the bus lease rule).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskClaimRenewRequestV1 {
    pub schema: String,
    pub claim: ClaimRefV1,
    pub minutes: u32,
}

impl TaskClaimRenewRequestV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, TASK_CLAIM_RENEW_SCHEMA)?;
        self.claim.validate()?;
        validate_minutes(self.minutes)
    }
}

/// The one claim on a task, as a task view shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskClaimV1 {
    pub holder: TaskActorV1,
    pub since: UtcTimestamp,
    pub until: UtcTimestamp,
}

/// Answer of claim and renew: the lease the holder writes under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskClaimAckV1 {
    pub schema: String,
    pub number: u64,
    pub claim: ClaimRefV1,
    pub until: UtcTimestamp,
}

/// `POST …/{number}/release`: give the task back (blocked, out of scope).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskReleaseRequestV1 {
    pub schema: String,
    pub claim: ClaimRefV1,
    /// Why, in one line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl TaskReleaseRequestV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, TASK_RELEASE_SCHEMA)?;
        self.claim.validate()?;
        if let Some(reason) = &self.reason {
            validate_text(reason, MAX_REASON_CHARS, "reason", false)?;
        }
        Ok(())
    }
}

/// What the work cost the agent's owner so far (self-reported): model
/// tokens and, if known, US dollars in millionths (integers only, as
/// everything under a signature).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageV1 {
    pub tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usd_micros: Option<u64>,
}

/// `POST …/{number}/updates`: one short summary from the holder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskUpdateRequestV1 {
    pub schema: String,
    pub claim: ClaimRefV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<UsageV1>,
}

impl TaskUpdateRequestV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, TASK_UPDATE_SCHEMA)?;
        self.claim.validate()?;
        text_required(self.summary.is_some())?;
        if let Some(summary) = &self.summary {
            validate_text(summary, MAX_SUMMARY_CHARS, "summary", false)?;
        }
        Ok(())
    }
}

/// `POST …/{number}/result`: the holder's result. It ends the claim; a 1.x
/// Hive turns it into a proposal (`origin task`), Hive v5 ends the task
/// (`done`, or `failed` with `outcome: failed`). `draft` is its knowledge
/// change, if it brings one, exactly as a regular draft; a failed result
/// brings none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskResultRequestV1 {
    pub schema: String,
    pub claim: ClaimRefV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<EvidenceV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft: Option<DraftProposalV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<UsageV1>,
    /// Hive v5; absent = `completed`. A 1.x Hive refuses the field
    /// (unknown fields are denied), so it is sent only to a v5 Hive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<TaskOutcome>,
    /// Process v1 G6: the Instance's part of the record for the judged
    /// commit (clean locked paths, a green rerun; blocked on the CI pipeline
    /// Hive reads). Sent only for a process task (a task view with
    /// `process`), so a Hive without process tasks never sees the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate_record: Option<super::gate::G6RecordV1>,
}

impl TaskResultRequestV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, TASK_RESULT_SCHEMA)?;
        self.claim.validate()?;
        let plain = self.summary.is_some() || !self.evidence.is_empty();
        text_required(plain)?;
        if plain {
            validate_text(
                self.summary.as_deref().unwrap_or_default(),
                MAX_SUMMARY_CHARS,
                "summary",
                false,
            )?;
            validate_evidence(&self.evidence)?;
        }
        if let Some(draft) = &self.draft {
            if self.outcome == Some(TaskOutcome::Failed) {
                return Err(ValidationError::new("a failed result brings no draft"));
            }
            draft.validate()?;
        }
        if let Some(record) = &self.gate_record {
            record.validate()?;
            if !record.ready_for_hive() || self.outcome == Some(TaskOutcome::Failed) {
                return Err(ValidationError::new(
                    "a result carries only a G6 record whose Instance checks hold, on a completed result",
                ));
            }
        }
        Ok(())
    }
}

/// Answer of every task write: the number and the state afterwards.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskAckV1 {
    pub schema: String,
    pub number: u64,
    pub state: TaskState,
}

/// A verifier's verdict on a submitted result (C0 2.2): `accept` makes the
/// task `done`, `reject` opens it again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskVerdict {
    Accept,
    Reject,
    #[serde(other)]
    Unknown,
}

/// `POST …/{number}/verify` (C0 2.2 E48, `tasks.work`): another agent's
/// verdict. `worker_id` is the verifying agent, never the result's
/// (`403 self_verify`); `reason` is required on `reject`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskVerifyRequestV1 {
    pub schema: String,
    pub worker_id: String,
    pub verdict: TaskVerdict,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

fn validate_worker_id(worker_id: &str) -> Result<(), ValidationError> {
    let worker_ok = (1..=128).contains(&worker_id.len())
        && worker_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'));
    if worker_ok {
        Ok(())
    } else {
        Err(ValidationError::new(
            "worker_id is 1-128 of [A-Za-z0-9._:-]",
        ))
    }
}

impl TaskVerifyRequestV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, TASK_VERIFY_SCHEMA)?;
        validate_worker_id(&self.worker_id)?;
        match (self.verdict, &self.reason) {
            (TaskVerdict::Unknown, _) => Err(ValidationError::new("verdict is accept or reject")),
            (TaskVerdict::Reject, None) => Err(ValidationError::new("a reject names its reason")),
            (_, Some(reason)) => validate_text(reason, MAX_VERIFY_REASON_CHARS, "reason", false),
            (TaskVerdict::Accept, None) => Ok(()),
        }
    }
}

/// The last verdict on a task's result, as a task view shows it (C0 2.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskVerificationV1 {
    pub verdict: TaskVerdict,
    pub by: TaskActorV1,
    pub at: UtcTimestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// `POST …/{number}/amend` (3.12.3, `tasks.work`): a result is corrected
/// without a claim (a submit ends the claim). Two shapes, by state
/// ([`amend_rule`]):
/// - `submitted`: the agent that submitted it (`worker_id`) replaces
///   `evidence` (the whole new list) and optionally `summary`;
/// - `done`: the submitter, a person or a registered Queen session records
///   the merge once (`merge_commit`, the merge commit's SHA); nothing else
///   changes. Never the reviewer (`verified_by`): `403 not_merger`.
///
/// Anyone else is `403 not_submitter` ([`may_amend`]). Hive appends a `kind: amendment`
/// update with `amended_from` (the evidence before) and `amended_to` (the
/// evidence after, or the merge commit), so the history keeps old → new.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskAmendRequestV1 {
    pub schema: String,
    pub worker_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<EvidenceV1>,
    /// The merge commit's SHA (7-64 lowercase hex), on a `done` task only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl TaskAmendRequestV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, TASK_AMEND_SCHEMA)?;
        validate_worker_id(&self.worker_id)?;
        if let Some(reason) = &self.reason {
            validate_text(reason, MAX_REASON_CHARS, "reason", false)?;
        }
        match &self.merge_commit {
            Some(commit) => {
                if !self.evidence.is_empty() || self.summary.is_some() {
                    return Err(ValidationError::new(
                        "a merge record carries only merge_commit (no evidence, no summary)",
                    ));
                }
                merge_evidence(commit).validate()
            }
            None => {
                if let Some(summary) = &self.summary {
                    validate_text(summary, MAX_SUMMARY_CHARS, "summary", false)?;
                }
                validate_evidence(&self.evidence)
            }
        }
    }
}

/// The merge commit as a piece of evidence (how history shows it).
pub fn merge_evidence(commit: &str) -> EvidenceV1 {
    EvidenceV1 {
        kind: EvidenceKind::Commit,
        reference: Some(commit.to_owned()),
        anchor: None,
    }
}

/// Why a result cannot be amended (the Hive reason code in brackets).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AmendRefusal {
    /// Only a `submitted` or `done` task carries a result to amend
    /// (`not_amendable`).
    NotAmendable,
    /// A `done` task takes only a merge record (`record_merge_only`).
    RecordMergeOnly,
    /// A merge record is for a `done` task; a `submitted` one is not merged
    /// yet (`not_merged_yet`).
    NotMergedYet,
    /// The task's merge is already recorded; it is never replaced
    /// (`merge_recorded`).
    MergeRecorded,
}

impl AmendRefusal {
    pub fn code(self) -> &'static str {
        match self {
            Self::NotAmendable => "not_amendable",
            Self::RecordMergeOnly => "record_merge_only",
            Self::NotMergedYet => "not_merged_yet",
            Self::MergeRecorded => "merge_recorded",
        }
    }
}

/// What a task's state allows an amendment of its result: `submitted`
/// replaces summary and evidence; `done` records the merge once and
/// changes nothing else; any other state refuses. The one rule for Hive and
/// the client (the client checks first, Hive enforces); who may amend is
/// [`may_amend`].
pub fn amend_rule(task: &TaskV1, request: &TaskAmendRequestV1) -> Result<(), AmendRefusal> {
    let result = task.result.as_ref().ok_or(AmendRefusal::NotAmendable)?;
    match (task.state, &request.merge_commit) {
        (TaskState::Submitted, None) => Ok(()),
        (TaskState::Submitted, Some(_)) => Err(AmendRefusal::NotMergedYet),
        (TaskState::Done, Some(_)) if result.merge.is_some() => Err(AmendRefusal::MergeRecorded),
        (TaskState::Done, Some(_)) => Ok(()),
        (TaskState::Done, None) => Err(AmendRefusal::RecordMergeOnly),
        _ => Err(AmendRefusal::NotAmendable),
    }
}

/// Who is amending, as the caller established it: the result's submitter,
/// the reviewer (the actor whose verdict made the task `done`), a person,
/// or a Queen session the app registered.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AmendCaller {
    pub submitter: bool,
    pub reviewer: bool,
    pub person: bool,
    pub queen: bool,
}

/// Why this caller may not amend (`403` with the code).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AmendForbidden {
    /// Only the submitter corrects a result (`not_submitter`).
    NotSubmitter,
    /// The reviewer never records the merge: checker and merger stay
    /// separate (`not_merger`).
    NotMerger,
}

impl AmendForbidden {
    pub fn code(self) -> &'static str {
        match self {
            Self::NotSubmitter => "not_submitter",
            Self::NotMerger => "not_merger",
        }
    }
}

/// Who may amend: the submitter always (a correction, or its merge
/// record); a merge record also a person or a registered Queen session;
/// never the reviewer (`not_merger`), anyone else `not_submitter`.
pub fn may_amend(request: &TaskAmendRequestV1, caller: AmendCaller) -> Result<(), AmendForbidden> {
    if caller.submitter {
        return Ok(());
    }
    if request.merge_commit.is_none() {
        return Err(AmendForbidden::NotSubmitter);
    }
    if caller.reviewer {
        return Err(AmendForbidden::NotMerger);
    }
    if caller.person || caller.queen {
        Ok(())
    } else {
        Err(AmendForbidden::NotSubmitter)
    }
}

/// 3.12.3: a `done` task's merge record (`amend` with `merge_commit`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskMergeV1 {
    pub commit: String,
    pub by: TaskActorV1,
    pub at: UtcTimestamp,
}

/// What a task update is (C0 2.2): the holder's progress, or a verifier's
/// verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskUpdateKind {
    Progress,
    Verification,
    /// 3.12.3: the submitter amended its result (`amended_from` holds the
    /// evidence before).
    Amendment,
    #[serde(other)]
    Unknown,
}

/// A task's priority (C0 2.2); absent means `normal`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskPriority {
    High,
    #[default]
    Normal,
}

/// `PATCH …/{number}` (C0 2.2 E47, `tasks.propose`): an agent sets a
/// task's priority and labels. `base_version` is the task's
/// `fields_version`; a mismatch answers `409 stale_base`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskFieldsRequestV1 {
    pub schema: String,
    pub base_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<TaskPriority>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub labels: Option<Vec<String>>,
}

impl TaskFieldsRequestV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, TASK_FIELDS_SCHEMA)?;
        if self.priority.is_none() && self.labels.is_none() {
            return Err(ValidationError::new("set priority, labels or both"));
        }
        if let Some(labels) = &self.labels {
            validate_labels(labels)?;
        }
        Ok(())
    }
}

/// At most [`MAX_TASK_LABELS`] unique labels of `^[a-z0-9-]{1,32}$`.
pub fn validate_labels(labels: &[String]) -> Result<(), ValidationError> {
    if labels.len() > MAX_TASK_LABELS {
        return Err(ValidationError::new("at most 16 labels"));
    }
    let mut seen = std::collections::BTreeSet::new();
    for label in labels {
        let ok = (1..=32).contains(&label.len())
            && label
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        if !ok {
            return Err(ValidationError::new("a label is 1-32 of [a-z0-9-]"));
        }
        if !seen.insert(label) {
            return Err(ValidationError::new("labels are unique"));
        }
    }
    Ok(())
}

/// One update as a task view shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskUpdateV1 {
    pub by: TaskActorV1,
    pub at: UtcTimestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<UsageV1>,
    /// C0 2.2; absent from older Hives (progress).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<TaskUpdateKind>,
    /// C0 2.2: a verifier's verdict, on `kind: verification`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<TaskVerdict>,
    /// 3.12.3, on `kind: amendment`: the result's evidence before the
    /// amendment, so the history keeps every earlier reference.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub amended_from: Vec<EvidenceViewV1>,
    /// 3.12.3, on `kind: amendment`: the evidence after it (old → new), or
    /// the merge commit of a merge record.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub amended_to: Vec<EvidenceViewV1>,
}

/// Evidence as Hive shows it: `verified` is set by Hive (a `receipt`
/// whose digest and signature it checked), never by a client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceViewV1 {
    pub kind: EvidenceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<CodeAnchorV1>,
    pub verified: bool,
}

/// The submitted result, as a task view shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskResultV1 {
    pub by: TaskActorV1,
    pub at: UtcTimestamp,
    /// The proposal that carries it; 0 when there is none (Hive v5: the
    /// result completed the task directly).
    pub proposal: u64,
    /// Hive v5: the log seq of the result's appended knowledge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<EvidenceViewV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<UsageV1>,
    /// 3.12.3: the merge, once recorded on a `done` task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge: Option<TaskMergeV1>,
}

/// Who decided on a result: a person, or the auto-merge threshold (Personal).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DecidedByV1 {
    Person {
        label: String,
    },
    Threshold,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskDecisionOutcome {
    Accepted,
    Declined,
    #[serde(other)]
    Unknown,
}

/// The decision on a result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskDecisionV1 {
    pub outcome: TaskDecisionOutcome,
    pub decided_by: DecidedByV1,
    pub at: UtcTimestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// `GET /v1/vaults/{vault_id}/tasks/{number}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskV1 {
    pub schema: String,
    pub vault_id: String,
    pub number: u64,
    pub state: TaskState,
    /// Hive v5 lifecycle; absent when Hive sends none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<TaskLifecycle>,
    /// The task's title; absent when Hive sends none (shown as untitled).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acceptance: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub origin: TaskOriginV1,
    pub created_by: TaskActorV1,
    pub created_at: UtcTimestamp,
    /// The person who approved a proposed task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_by: Option<TaskActorV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim: Option<TaskClaimV1>,
    /// Newest last, at most [`MAX_VIEW_UPDATES`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub updates: Vec<TaskUpdateV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<TaskResultV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<TaskDecisionV1>,
    /// C0 2.2: the agent whose `accept` made the task `done`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_by: Option<TaskActorV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_at: Option<UtcTimestamp>,
    /// C0 2.2: the last verdict (accept or reject).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification: Option<TaskVerificationV1>,
    /// C0 2.2; absent means `normal`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<TaskPriority>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    /// C0 2.2: +1 whenever priority or labels change (the `base_version`
    /// of [`TaskFieldsRequestV1`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fields_version: Option<u32>,
    /// Process v1: the task's process part (risk, what G6 needs, Hive's G6
    /// record). Absent on a task that is not a process task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process: Option<super::gate::TaskProcessV1>,
}

impl TaskV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, TASK_SCHEMA)?;
        super::validate_vault_id(&self.vault_id)?;
        if self.number == 0 {
            return Err(ValidationError::new("task numbers start at 1"));
        }
        if (self.state == TaskState::Claimed) != self.claim.is_some() {
            return Err(ValidationError::new(
                "a task carries a claim exactly while it is claimed",
            ));
        }
        if self.updates.len() > MAX_VIEW_UPDATES {
            return Err(ValidationError::new("too many updates in a task view"));
        }
        if let Some(approver) = &self.approved_by
            && approver.kind != TaskActorKind::Person
        {
            return Err(ValidationError::new("only a person approves a task"));
        }
        if matches!(self.state, TaskState::Submitted | TaskState::Done) && self.result.is_none() {
            return Err(ValidationError::new(
                "a submitted or done task carries its result",
            ));
        }
        Ok(())
    }
}

/// One task in a listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSummaryV1 {
    pub number: u64,
    pub state: TaskState,
    /// Hive v5 lifecycle (additive; absent from a 1.x Hive).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<TaskLifecycle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub origin: TaskOriginKind,
    pub updated_at: UtcTimestamp,
    /// Whether this instance holds its claim.
    pub mine: bool,
    /// C0 2.2; absent means `normal`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<TaskPriority>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fields_version: Option<u32>,
}

/// `GET /v1/vaults/{vault_id}/tasks`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskListV1 {
    pub schema: String,
    pub vault_id: String,
    pub tasks: Vec<TaskSummaryV1>,
}

impl TaskListV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, TASK_LIST_SCHEMA)?;
        super::validate_vault_id(&self.vault_id)?;
        if self.tasks.len() > MAX_TASKS_PAGE {
            return Err(ValidationError::new("too many tasks in one page"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn propose() -> TaskProposeRequestV1 {
        TaskProposeRequestV1 {
            schema: TASK_PROPOSE_SCHEMA.into(),
            title: Some("Explain the retry budget".into()),
            goal: None,
            acceptance: Some("A fact with an anchor on retry.rs".into()),
            role: None,
            origin: TaskOriginV1 {
                kind: TaskOriginKind::Agent,
                facts: Vec::new(),
                message: None,
            },
            session_ref: "s-1".into(),
        }
    }

    fn claim() -> ClaimRefV1 {
        ClaimRefV1 {
            lease_id: "lease-1".into(),
            fencing_token: 3,
        }
    }

    #[test]
    fn an_unknown_task_origin_parses_as_unknown() {
        let origin: TaskOriginV1 = serde_json::from_value(serde_json::json!({
            "kind": "quarantine_timeout", "facts": ["f-1"],
        }))
        .unwrap();
        assert_eq!(origin.kind, TaskOriginKind::Unknown);
    }

    #[test]
    fn engines_propose_only_agent_or_contradiction_tasks() {
        assert!(propose().validate().is_ok());
        let mut person = propose();
        person.origin.kind = TaskOriginKind::Person;
        assert!(person.validate().is_err());
        let mut contradiction = propose();
        contradiction.origin = TaskOriginV1 {
            kind: TaskOriginKind::Contradiction,
            facts: vec!["f-1".into(), "f-1".into()],
            message: None,
        };
        assert!(contradiction.validate().is_err());
        contradiction.origin.facts[1] = "f-2".into();
        assert!(contradiction.validate().is_ok());
    }

    #[test]
    fn a_result_needs_evidence_that_fits_its_kind() {
        let mut result = TaskResultRequestV1 {
            schema: TASK_RESULT_SCHEMA.into(),
            claim: claim(),
            summary: Some("Done".into()),
            evidence: Vec::new(),
            draft: None,
            usage: None,
            outcome: None,
            gate_record: None,
        };
        assert!(result.validate().is_err(), "no evidence");
        let r = |kind, reference: &str| EvidenceV1 {
            kind,
            reference: Some(reference.into()),
            anchor: None,
        };
        for (evidence, ok) in [
            (r(EvidenceKind::Commit, "9f01cf32a3"), true),
            (r(EvidenceKind::Commit, "main"), false),
            (r(EvidenceKind::Url, "http://example.com"), false),
            (r(EvidenceKind::Receipt, &"a".repeat(64)), true),
            (r(EvidenceKind::FileAnchor, "src/retry.rs"), false),
        ] {
            result.evidence = vec![evidence.clone()];
            assert_eq!(result.validate().is_ok(), ok, "{evidence:?}");
        }
        result.claim.fencing_token = 0;
        result.evidence = vec![r(EvidenceKind::Commit, "9f01cf32a3")];
        assert!(result.validate().is_err(), "fencing tokens start at 1");
    }

    #[test]
    fn summaries_are_one_line() {
        let update = |summary: &str| TaskUpdateRequestV1 {
            schema: TASK_UPDATE_SCHEMA.into(),
            claim: claim(),
            summary: Some(summary.into()),
            usage: None,
        };
        assert!(update("Reproduced the timeout").validate().is_ok());
        assert!(update("line one\nline two").validate().is_err());
        assert!(
            update(&"x".repeat(MAX_SUMMARY_CHARS + 1))
                .validate()
                .is_err()
        );
    }

    #[test]
    fn decided_by_is_a_person_or_the_threshold() {
        let person: DecidedByV1 =
            serde_json::from_value(serde_json::json!({"kind": "person", "label": "Ada"})).unwrap();
        assert_eq!(
            person,
            DecidedByV1::Person {
                label: "Ada".into()
            }
        );
        let threshold: DecidedByV1 =
            serde_json::from_value(serde_json::json!({"kind": "threshold"})).unwrap();
        assert_eq!(threshold, DecidedByV1::Threshold);
    }

    fn fixture(name: &str) -> serde_json::Value {
        let path = format!("{}/tests/fixtures/hive/{name}", env!("CARGO_MANIFEST_DIR"));
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }

    /// Hive v5: `outcome` is sent only when set (a 1.x Hive denies the
    /// field), and a failed result brings no draft.
    #[test]
    fn a_failed_result_names_its_outcome_and_brings_no_draft() {
        let mut result: TaskResultRequestV1 =
            serde_json::from_value(fixture("task_result.request.json")).unwrap();
        assert_eq!(result.outcome, None);
        assert!(
            serde_json::to_value(&result)
                .unwrap()
                .get("outcome")
                .is_none(),
            "a completed result sends no outcome"
        );
        result.outcome = Some(TaskOutcome::Failed);
        assert!(result.validate().is_ok());
        assert_eq!(serde_json::to_value(&result).unwrap()["outcome"], "failed");
        result.draft =
            Some(serde_json::from_value(fixture("proposal.import.request.json")).unwrap());
        assert_eq!(
            result.validate().unwrap_err().to_string(),
            "a failed result brings no draft"
        );
        result.outcome = Some(TaskOutcome::Completed);
        assert!(
            result.validate().is_ok(),
            "a completed result may bring one"
        );
    }

    /// A 1.x view (`submitted`, no `lifecycle`, a proposal) and a v5 view
    /// (`failed`, `lifecycle`, `seq` with proposal 0) both parse; unknown
    /// lifecycle words stay tolerated.
    #[test]
    fn task_views_of_both_lines_parse() {
        let old: TaskV1 = serde_json::from_value(fixture("task.done.response.json")).unwrap();
        assert_eq!(old.lifecycle, None);
        assert_eq!(old.result.as_ref().unwrap().seq, None);
        assert!(
            serde_json::to_value(&old)
                .unwrap()
                .get("lifecycle")
                .is_none()
        );

        let mut v5 = fixture("task.done.response.json");
        v5["state"] = "failed".into();
        v5["lifecycle"] = "failed".into();
        v5["result"]["proposal"] = 0.into();
        v5["result"]["seq"] = 42.into();
        let v5: TaskV1 = serde_json::from_value(v5).unwrap();
        assert_eq!(v5.state, TaskState::Failed);
        assert_eq!(v5.lifecycle, Some(TaskLifecycle::Failed));
        let result = v5.result.as_ref().unwrap();
        assert_eq!((result.proposal, result.seq), (0, Some(42)));
        assert!(v5.validate().is_ok());

        let mut list = fixture("task_list.response.json");
        list["tasks"][0]["lifecycle"] = "paused_someday".into();
        let list: TaskListV1 = serde_json::from_value(list).unwrap();
        assert_eq!(list.tasks[0].lifecycle, Some(TaskLifecycle::Unknown));
        let state: TaskState = serde_json::from_value("someday".into()).unwrap();
        assert_eq!(state, TaskState::Unknown);
    }

    fn v2_route(name: &str) -> serde_json::Value {
        let path = format!(
            "{}/tests/fixtures/hive-v2/routes/{name}.json",
            env!("CARGO_MANIFEST_DIR")
        );
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    /// C0 2.2 (E48, E47 fields): the contract's requests validate, its
    /// answers parse, and a reject without reason or an empty field change
    /// is refused before it is sent.
    #[test]
    fn the_c0_2_2_task_shapes_hold() {
        for name in ["task_verify.request", "task_verify.reject.request"] {
            let request: TaskVerifyRequestV1 = serde_json::from_value(v2_route(name)).unwrap();
            request.validate().unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(serde_json::to_value(&request).unwrap(), v2_route(name));
        }
        let mut reject: TaskVerifyRequestV1 =
            serde_json::from_value(v2_route("task_verify.reject.request")).unwrap();
        reject.reason = None;
        assert_eq!(
            reject.validate().unwrap_err().to_string(),
            "a reject names its reason"
        );
        for (name, state) in [
            ("task_verify.response", TaskState::Done),
            ("task_verify.reject.response", TaskState::Open),
            ("task_result.submitted.response", TaskState::Submitted),
        ] {
            let ack: TaskAckV1 = serde_json::from_value(v2_route(name)).unwrap();
            assert_eq!(ack.state, state, "{name}");
        }
        let fields: TaskFieldsRequestV1 =
            serde_json::from_value(v2_route("task_fields.request")).unwrap();
        fields.validate().unwrap();
        assert_eq!(fields.priority, Some(TaskPriority::High));
        let empty = TaskFieldsRequestV1 {
            labels: None,
            priority: None,
            ..fields.clone()
        };
        assert!(empty.validate().is_err());
        let bad = TaskFieldsRequestV1 {
            labels: Some(vec!["Docs".into()]),
            ..fields
        };
        assert!(bad.validate().is_err());
    }
}
