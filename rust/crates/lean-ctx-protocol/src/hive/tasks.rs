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
//!   Team and swarms people and agents of other owners (Team: one by
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
//! | `GET` (`?state=…&role=…&mine=true`) | | [`TaskListV1`] | `tasks.read` |
//! | `GET /{number}` | | [`TaskV1`] | `tasks.read` |
//! | `POST` | [`TaskProposeRequestV1`] | [`TaskAckV1`] (`proposed`) | `tasks.propose` |
//! | `POST /{number}/claim` | [`TaskClaimRequestV1`] | [`TaskClaimAckV1`] | `tasks.work` |
//! | `POST /{number}/claim/renew` | [`TaskClaimRenewRequestV1`] | [`TaskClaimAckV1`] | `tasks.work` |
//! | `POST /{number}/release` | [`TaskReleaseRequestV1`] | [`TaskAckV1`] (`open`) | `tasks.work` |
//! | `POST /{number}/updates` | [`TaskUpdateRequestV1`] | [`TaskAckV1`] | `tasks.work` |
//! | `POST /{number}/result` | [`TaskResultRequestV1`] | [`TaskAckV1`] (`submitted`; v5 `done` or `failed`) | `tasks.work` |
//!
//! Errors carry `number` (`HiveErrorDetailV1::number`):
//! - `409 invalid_state`, `reason`:
//!   - `task_claimed`: another claim holds the task;
//!   - `task_not_open`: claim on a task that is not `open`;
//!   - `stale_claim`: the lease id or fencing token is not the current claim's;
//!   - `task_closed`: any write to a `done`, `rejected` or `cancelled` task;
//!   - `reviewer_not_independent`: a confirmation that a threshold may not count;
//!   - `cross_network`: an object of another network (org bus vs a swarm);
//!     for message replies Hive answers `404 not_found` instead (section 2).
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
//! Sealed vaults: an engine sends no readable text. A proposed task, an
//! update or a result carries `sealed` instead of the plaintext fields,
//! encrypted under the vault key with the AAD
//! `vault_id|<task number>|epoch|content_schema`. A proposed task, which
//! has no number yet, uses `0`. Evidence travels inside the sealed result.
//! Text that people type on the web is Hive-visible by nature, as for gaps.

use serde::{Deserialize, Serialize};

use super::anchor::CodeAnchorV1;
use super::knowledge::DraftProposalV1;
use super::sealed::SealedBlobV1;
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

/// `content_schema` of a sealed proposed task: `{title, goal?, acceptance}`.
pub const SEALED_TASK_CONTENT_SCHEMA: &str = "leanctx.hive.sealed-task/v1";
/// `content_schema` of a sealed update: `{summary}`.
pub const SEALED_TASK_UPDATE_SCHEMA: &str = "leanctx.hive.sealed-task-update/v1";
/// `content_schema` of a sealed result: `{summary, evidence}`.
pub const SEALED_TASK_RESULT_SCHEMA: &str = "leanctx.hive.sealed-task-result/v1";

pub const MIN_TITLE_CHARS: usize = 5;
pub const MAX_TITLE_CHARS: usize = 180;
pub const MAX_GOAL_CHARS: usize = 2000;
pub const MAX_ACCEPTANCE_CHARS: usize = 2000;
/// Updates and result summaries are summaries, not prompts: one line.
pub const MAX_SUMMARY_CHARS: usize = 500;
pub const MAX_REASON_CHARS: usize = 280;
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
    /// New knowledge that stayed in quarantine past its review window
    /// (section 3); `facts` names the quarantined facts.
    QuarantineTimeout,
    #[serde(other)]
    Unknown,
}

/// Origin of a task. `facts` names exactly two distinct fact ids for a
/// contradiction and the quarantined facts for a quarantine timeout;
/// `message` names the question's message id for a question.
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

/// Exactly one of plaintext (`plain`) or `sealed` with the given schema.
fn one_of(plain: bool, sealed: Option<&SealedBlobV1>, schema: &str) -> Result<(), ValidationError> {
    match (plain, sealed) {
        (true, None) => Ok(()),
        (false, Some(blob)) => {
            blob.validate()?;
            if blob.content_schema == schema {
                Ok(())
            } else {
                Err(ValidationError::new(format!(
                    "sealed content must be {schema}"
                )))
            }
        }
        (true, Some(_)) => Err(ValidationError::new("sealed writes carry no plaintext")),
        (false, None) => Err(ValidationError::new("text or sealed content is required")),
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sealed: Option<SealedBlobV1>,
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
        one_of(plain, self.sealed.as_ref(), SEALED_TASK_CONTENT_SCHEMA)?;
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
    /// Why, in one line; absent for sealed vaults.
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
    pub sealed: Option<SealedBlobV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<UsageV1>,
}

impl TaskUpdateRequestV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, TASK_UPDATE_SCHEMA)?;
        self.claim.validate()?;
        one_of(
            self.summary.is_some(),
            self.sealed.as_ref(),
            SEALED_TASK_UPDATE_SCHEMA,
        )?;
        if let Some(summary) = &self.summary {
            validate_text(summary, MAX_SUMMARY_CHARS, "summary", false)?;
        }
        Ok(())
    }
}

/// `POST …/{number}/result`: the holder's result. It ends the claim; a 1.x
/// Hive turns it into a proposal (`origin task`), Hive v5 ends the task
/// (`done`, or `failed` with `outcome: failed`). `draft` is its knowledge
/// change, if it brings one, exactly as a regular draft (sealed rules
/// included); a failed result brings none.
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
    pub sealed: Option<SealedBlobV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<UsageV1>,
    /// Hive v5; absent = `completed`. A 1.x Hive refuses the field
    /// (unknown fields are denied), so it is sent only to a v5 Hive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<TaskOutcome>,
}

impl TaskResultRequestV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, TASK_RESULT_SCHEMA)?;
        self.claim.validate()?;
        let plain = self.summary.is_some() || !self.evidence.is_empty();
        one_of(plain, self.sealed.as_ref(), SEALED_TASK_RESULT_SCHEMA)?;
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
            if draft.context_patch.sealed.is_some() != self.sealed.is_some() {
                return Err(ValidationError::new(
                    "a result and its draft are both sealed or both in the clear",
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

/// One update as a task view shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskUpdateV1 {
    pub by: TaskActorV1,
    pub at: UtcTimestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sealed: Option<SealedBlobV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<UsageV1>,
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
    pub sealed: Option<SealedBlobV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<UsageV1>,
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
    /// Hive v5 lifecycle (additive; absent from a 1.x Hive).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<TaskLifecycle>,
    /// Absent when an engine proposed it in a sealed vault.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acceptance: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sealed: Option<SealedBlobV1>,
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
            sealed: None,
        }
    }

    fn claim() -> ClaimRefV1 {
        ClaimRefV1 {
            lease_id: "lease-1".into(),
            fencing_token: 3,
        }
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
            sealed: None,
            usage: None,
            outcome: None,
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
            sealed: None,
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
}
