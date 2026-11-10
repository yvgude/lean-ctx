// SPDX-License-Identifier: Apache-2.0
//! Local project tasks: the Hive task model
//! ([`TaskV1`](lean_ctx_protocol::hive::tasks::TaskV1), [`TaskState`]) kept
//! per project on this machine, so tasks work without Hive (owner decision
//! 2026-11-04: everything a project has works locally in the Instance; Hive
//! only adds sharing).
//!
//! ```text
//! proposed --accept--> open --claim--> claimed --submit--> submitted --verify accept--> done
//!    |                  ^ ^               |  |                  |
//!    +--decline--> rejected +--release----+  +--submit failed--> failed
//!                           +--------------------- verify reject ---+
//! proposed/open/claimed --stop--> cancelled
//! ```
//!
//! The rules ([`board`]):
//! - anyone (an agent or a person) may accept or decline a proposal, the
//!   proposer included;
//! - only an agent claims, the proposer included; a claim lasts its minutes
//!   and is renewed by the holder's claim again or update; an expired claim
//!   opens the task again;
//! - the holder updates, submits or releases; a person may release too;
//! - a result is verified by a person or by another agent, never by the
//!   agent that submitted it; `reject` opens the task again;
//! - a person (never an agent) stops a proposed, open or claimed task, with a reason;
//! - every transition is recorded with who and when, on the task and in the
//!   store's journal ([`store`]).
//!
//! Text rules (lengths, one-line summaries, evidence kinds) are Hive's: each
//! write is checked with the matching Hive request type's `validate`.
//!
//! This crate never posts on the bus or decides whether a project is linked
//! to a vault: every write returns its [`Change`]s, and the caller announces
//! them (lean-ctx-hive `tasks::local`).

pub mod board;
pub mod store;

#[cfg(test)]
mod tests;

use serde::{Deserialize, Serialize};

use lean_ctx_protocol::UtcTimestamp;
use lean_ctx_protocol::hive::tasks::{TaskActorKind, TaskActorV1, TaskState};

pub use board::{Amendment, Board, LocalTask, Proposal, SharedRef, Submission, Transition};
pub use store::Store;

/// The `vault_id` every local task carries (it lives in no vault).
pub const LOCAL_VAULT: &str = "local";
/// Claim length when the caller names none.
pub const DEFAULT_CLAIM_MINUTES: u32 = 60;
pub const MIN_CLAIM_MINUTES: u32 = 5;
/// No keeper renews a local claim: the holder renews it by working on it.
pub const MAX_CLAIM_MINUTES: u32 = 240;

/// Who acts on a task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Actor {
    /// A bus agent (its id) and how it is shown.
    Agent { id: String, label: Option<String> },
    /// A person on this machine (Hive Desktop, the CLI).
    Person { label: String },
}

impl Actor {
    pub fn agent(id: impl Into<String>) -> Self {
        Self::Agent {
            id: id.into(),
            label: None,
        }
    }

    pub fn person(label: impl Into<String>) -> Self {
        Self::Person {
            label: label.into(),
        }
    }

    /// The bus agent id, for an agent.
    pub fn agent_id(&self) -> Option<&str> {
        match self {
            Self::Agent { id, .. } => Some(id),
            Self::Person { .. } => None,
        }
    }

    /// The actor as a task view names it (`mine` is the reader's, set when
    /// a view is rendered).
    pub fn view(&self) -> TaskActorV1 {
        match self {
            Self::Agent { id, label } => TaskActorV1 {
                kind: TaskActorKind::Instance,
                label: label.clone().unwrap_or_else(|| id.clone()),
                agent: Some(id.clone()),
                owner: None,
                model: None,
                mine: false,
                profile_id: None,
            },
            Self::Person { label } => TaskActorV1 {
                kind: TaskActorKind::Person,
                label: label.clone(),
                agent: None,
                owner: None,
                model: None,
                mine: false,
                profile_id: None,
            },
        }
    }

    /// Whether `actor` (a view) is this actor.
    fn is(&self, actor: &TaskActorV1) -> bool {
        match self {
            Self::Agent { id, .. } => actor.agent.as_deref() == Some(id.as_str()),
            Self::Person { .. } => actor.kind == TaskActorKind::Person,
        }
    }

    /// Whether `actor` is this actor by name too: a person by its label
    /// (two people on one machine stay apart).
    fn is_exactly(&self, actor: &TaskActorV1) -> bool {
        match self {
            Self::Agent { .. } => self.is(actor),
            Self::Person { label } => actor.kind == TaskActorKind::Person && actor.label == *label,
        }
    }
}

/// What happened to a task; the bus key names it (`task:<n>:<event>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskEvent {
    Proposed,
    Accepted,
    Declined,
    Claimed,
    Updated,
    Released,
    Submitted,
    Failed,
    Verified,
    Rejected,
    Stopped,
    /// The submitter amended its result (no state change).
    Amended,
    /// Brought into a Hive vault (no state change, no bus notice).
    Shared,
    /// Priority and/or labels set (no state change, no bus notice).
    Fields,
}

impl TaskEvent {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Proposed => "proposed",
            Self::Accepted => "accepted",
            Self::Declined => "declined",
            Self::Claimed => "claimed",
            Self::Updated => "updated",
            Self::Released => "released",
            Self::Submitted => "submitted",
            Self::Failed => "failed",
            Self::Verified => "verified",
            Self::Rejected => "rejected",
            Self::Stopped => "stopped",
            Self::Amended => "amended",
            Self::Shared => "shared",
            Self::Fields => "fields",
        }
    }
}

/// One recorded transition, as a write returns it for its notices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// The store's journal sequence of this transition (unique per store).
    pub seq: u64,
    pub number: u64,
    pub event: TaskEvent,
    pub state: TaskState,
    /// The agent the event is about when it must not be told: the worker
    /// whose result waits for a check.
    pub worker: Option<String>,
}

/// Why a write was refused. `reason` codes follow Hive's (`task_claimed`,
/// `task_not_open`, `task_closed`, `not_submitted`, `self_verify`, …).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskError {
    NotFound(u64),
    /// The task's state does not allow the write.
    InvalidState {
        number: u64,
        reason: &'static str,
    },
    /// The actor may not make the write.
    Forbidden {
        number: u64,
        reason: &'static str,
    },
    /// The request is malformed (text rules, missing fields).
    Invalid(String),
    /// The store could not be read, locked or written.
    Store(String),
}

impl TaskError {
    /// A stable code: `not_found`, `invalid_state`, `forbidden`,
    /// `invalid_argument`, `unavailable`.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotFound(_) => "not_found",
            Self::InvalidState { .. } => "invalid_state",
            Self::Forbidden { .. } => "forbidden",
            Self::Invalid(_) => "invalid_argument",
            Self::Store(_) => "unavailable",
        }
    }

    /// The Hive-style reason, for state and right refusals.
    pub fn reason(&self) -> Option<&'static str> {
        match self {
            Self::InvalidState { reason, .. } | Self::Forbidden { reason, .. } => Some(reason),
            _ => None,
        }
    }
}

impl std::fmt::Display for TaskError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(n) => write!(f, "task #{n} does not exist"),
            Self::InvalidState { number, reason } | Self::Forbidden { number, reason } => {
                write!(f, "task #{number}: {}", describe(reason))
            }
            Self::Invalid(message) | Self::Store(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for TaskError {}

fn describe(reason: &str) -> &'static str {
    match reason {
        "task_claimed" => "another agent holds its claim (task_claimed)",
        "task_not_open" => "it is not open (task_not_open)",
        "not_proposed" => "it is not a proposal (not_proposed)",
        "not_claimed" => "it is not claimed (not_claimed)",
        "task_closed" => "it is closed (task_closed)",
        "not_submitted" => "no result waits for a check (not_submitted)",
        "self_verify" => {
            "the agent that submitted the result never checks it; a person or another agent does (self_verify)"
        }
        "not_holder" => "only the agent holding its claim does that (not_holder)",
        "agents_claim" => "only an agent claims a task (agents_claim)",
        "task_submitted" => "its result waits for a check; verify it instead (task_submitted)",
        "agents_stop" => "only a person stops a task (agents_stop)",
        "already_shared" => "it was already brought into a vault (already_shared)",
        "too_many_tasks" => "the project holds too many unfinished tasks (too_many_tasks)",
        "not_submitter" => {
            "only the agent that submitted its result corrects it; a merge record also a person or a Queen session (not_submitter)"
        }
        "not_merger" => {
            "the reviewer never records the merge; the submitter, a person or a Queen session does (not_merger)"
        }
        "not_amendable" => "it has no submitted or done result to amend (not_amendable)",
        "record_merge_only" => {
            "it is done: it only takes its merge record, --merge <sha> (record_merge_only)"
        }
        "not_merged_yet" => {
            "its result waits for a check; a merge is recorded once it is done (not_merged_yet)"
        }
        "merge_recorded" => "its merge is already recorded (merge_recorded)",
        _ => "refused",
    }
}

/// Whole-second UTC, as Hive writes task times.
pub fn timestamp(at: chrono::DateTime<chrono::Utc>) -> UtcTimestamp {
    UtcTimestamp::new(at.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_else(|_| unreachable_timestamp())
}

#[cold]
fn unreachable_timestamp() -> UtcTimestamp {
    // `%Y-%m-%dT%H:%M:%SZ` of a chrono time is always canonical.
    UtcTimestamp::new("1970-01-01T00:00:00Z").unwrap_or_else(|_| unreachable!())
}
