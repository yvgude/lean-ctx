// SPDX-License-Identifier: Apache-2.0
//! The Queen network's message kinds on agent-bus/1 (QN0) and the Queen
//! relay rule (HN0).
//!
//! - A bus message may carry a `kind`: one of the fourteen [`QueenKind`]s,
//!   or none for an ordinary message. It is stored on the local row
//!   (metadata [`KIND_KEY`]) and travels on the relay frame as `kind`, next
//!   to `task_id` and `thread` ([`frame_fields`]); readers ignore fields
//!   they do not know, so this is additive within agent-bus/1.
//! - Only a Queen session the app registered ([`crate::queens`]) posts a
//!   Queen kind; any other sender is refused with [`NOT_QUEEN`].
//! - Only the registered Queen's posts leave this Instance for a vault's
//!   bus; other workers' posts stay on the local bus (HN0). A person's own
//!   post (`hive-person`) is not a worker's and keeps its rules.
//! - Every sender has a post budget per minute ([`RATE_PER_MINUTE`]), at
//!   posting and again at the relay ([`RelayBudget`]).

use std::collections::{BTreeMap, HashMap, VecDeque};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Metadata key of a local bus row's kind.
pub const KIND_KEY: &str = "hive.kind";
/// Metadata key of a local bus row's thread.
pub const THREAD_KEY: &str = "hive.thread";
/// The reason code of a Queen kind from a sender that is no registered Queen.
pub const NOT_QUEEN: &str = "not_queen";
/// The reason code of a post over the sender's budget.
pub const RATE_LIMITED: &str = "rate_limited";
/// Posts one sender may make per minute.
pub const RATE_PER_MINUTE: usize = 20;
/// Longest `task_id` a post may carry.
pub const TASK_ID_MAX: usize = 128;
/// Longest `thread` the relay carries.
pub const THREAD_MAX: usize = 128;
const MINUTE_MS: i64 = 60_000;

/// The fourteen Queen message kinds (docs/queen-network.md, "Message kinds").
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueenKind {
    Hello,
    Status,
    Offer,
    Request,
    Handoff,
    Coordinate,
    Conflict,
    Escalate,
    Ask,
    Answer,
    Decide,
    Consent,
    Execute,
    Stop,
}

impl QueenKind {
    pub const ALL: [Self; 14] = [
        Self::Hello,
        Self::Status,
        Self::Offer,
        Self::Request,
        Self::Handoff,
        Self::Coordinate,
        Self::Conflict,
        Self::Escalate,
        Self::Ask,
        Self::Answer,
        Self::Decide,
        Self::Consent,
        Self::Execute,
        Self::Stop,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hello => "hello",
            Self::Status => "status",
            Self::Offer => "offer",
            Self::Request => "request",
            Self::Handoff => "handoff",
            Self::Coordinate => "coordinate",
            Self::Conflict => "conflict",
            Self::Escalate => "escalate",
            Self::Ask => "ask",
            Self::Answer => "answer",
            Self::Decide => "decide",
            Self::Consent => "consent",
            Self::Execute => "execute",
            Self::Stop => "stop",
        }
    }

    /// The kind `raw` names exactly; `None` for anything else.
    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == raw)
    }

    /// The kind as a caller sends it: absent, empty or `null` is an
    /// ordinary message; an unknown name is refused with its reason.
    pub fn from_input(raw: Option<&str>) -> Result<Option<Self>, String> {
        match raw.map(str::trim) {
            None | Some("" | "null") => Ok(None),
            Some(name) => Self::parse(name).map(Some).ok_or_else(|| {
                format!(
                    "unknown kind {name:?}: one of {}, or null for an ordinary message",
                    Self::ALL.map(Self::as_str).join(", ")
                )
            }),
        }
    }

    /// Whether this kind may be posted with priority `high` or above: only
    /// `conflict`, `escalate` and `stop`; the others go `normal` or `low`.
    pub fn allows_high(self) -> bool {
        matches!(self, Self::Conflict | Self::Escalate | Self::Stop)
    }
}

/// A `task_id` as a caller sends it: 1-128 characters without blanks or
/// control characters.
pub fn check_task_id(task_id: &str) -> Result<(), String> {
    let n = task_id.chars().count();
    if (1..=TASK_ID_MAX).contains(&n)
        && !task_id.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        Ok(())
    } else {
        Err(format!(
            "task_id is 1-{TASK_ID_MAX} characters without blanks or control characters"
        ))
    }
}

/// The refusal of a Queen kind from `sender`, which is no registered Queen.
pub fn not_queen(kind: QueenKind, sender: &str) -> String {
    format!(
        "{NOT_QUEEN}: kind {} is posted only by the Queen session the app registered; {sender} is not one",
        kind.as_str()
    )
}

/// A `thread` as a caller sends it: 1-128 characters without control
/// characters.
pub fn check_thread(thread: &str) -> Result<(), String> {
    if line_within(thread, THREAD_MAX) {
        Ok(())
    } else {
        Err(format!(
            "thread is 1-{THREAD_MAX} characters without control characters"
        ))
    }
}

/// The local address of the registered Queen: a post `to` it reaches the
/// Queen session the app registered, never an agent that only took the
/// role name `queen` (a role is a label).
pub const QUEEN_ADDRESS: &str = "queen";
/// The reason code of a post to [`QUEEN_ADDRESS`] while no registered
/// Queen is active on this Instance.
pub const NO_QUEEN: &str = "no_queen";

/// The agent id of this Instance's registered Queen: an active agent whose
/// app session the app registered as a Queen ([`crate::queens`]); the most
/// recently active one when there are several.
pub fn registered_queen(registry: &lean_ctx::core::agents::AgentRegistry) -> Option<String> {
    registry
        .list_active(None)
        .into_iter()
        .filter(|a| {
            super::person_rules::Speaker::of_registry(registry, Some(&a.agent_id)).registered_queen
        })
        .max_by_key(|a| a.last_active)
        .map(|a| a.agent_id.clone())
}

/// `to` as the bus routes it: [`QUEEN_ADDRESS`] names the registered Queen
/// ([`NO_QUEEN`] when none is active); any other address is itself.
pub fn resolve_to(
    registry: &lean_ctx::core::agents::AgentRegistry,
    to: &str,
) -> Result<String, String> {
    if to != QUEEN_ADDRESS {
        return Ok(to.to_owned());
    }
    registered_queen(registry).ok_or_else(|| {
        format!(
            "{NO_QUEEN}: no registered Queen is active on this Instance; `{QUEEN_ADDRESS}` names the Queen session the app registered, not an agent's role"
        )
    })
}

/// Why a sender may not post now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SenderRefusal {
    /// A Queen kind from a sender that is no registered Queen ([`NOT_QUEEN`]).
    NotQueen(String),
    /// The sender's budget for the minute is spent ([`RATE_LIMITED`]).
    RateLimited(String),
}

/// Whether `sender` may post a message of `kind` at `now_ms`: a Queen kind
/// only from the registered Queen, and at most [`RATE_PER_MINUTE`] posts of
/// the sender in the last minute (inbound Hive deliveries not counted).
pub fn check_sender(
    registry: &lean_ctx::core::agents::AgentRegistry,
    sender: &str,
    kind: Option<QueenKind>,
    now_ms: i64,
) -> Result<(), SenderRefusal> {
    if let Some(kind) = kind
        && !super::person_rules::Speaker::of_registry(registry, Some(sender)).registered_queen
    {
        return Err(SenderRefusal::NotQueen(not_queen(kind, sender)));
    }
    let recent = in_last_minute(
        registry
            .scratchpad
            .iter()
            .filter(|e| e.from_agent == sender && !e.is_hive_inbound())
            .map(|e| e.timestamp.timestamp_millis()),
        now_ms,
    );
    if recent >= RATE_PER_MINUTE {
        return Err(SenderRefusal::RateLimited(rate_limited(sender)));
    }
    Ok(())
}

/// Of `stamps` (unix ms of one sender's posts), those in the minute
/// before `now_ms`.
pub fn in_last_minute(stamps: impl IntoIterator<Item = i64>, now_ms: i64) -> usize {
    stamps
        .into_iter()
        .filter(|at| (0..MINUTE_MS).contains(&now_ms.saturating_sub(*at)))
        .count()
}

/// The refusal of a post over its sender's budget.
pub fn rate_limited(sender: &str) -> String {
    format!(
        "{RATE_LIMITED}: {sender} posted {RATE_PER_MINUTE} messages in the last minute; try again shortly"
    )
}

/// The Queen-network fields of a local post that travel on its relay
/// frame: its kind, its task and its conversation, each on its own (a post
/// filed under a task travels with it without a kind). `None` when the post
/// has none of them, whose frame stays as it was.
pub fn frame_fields(
    metadata: &HashMap<String, String>,
    task_id: Option<&str>,
    thread: Option<&str>,
) -> Option<Map<String, Value>> {
    let mut fields = Map::new();
    if let Some(kind) = metadata.get(KIND_KEY).and_then(|k| QueenKind::parse(k)) {
        fields.insert("kind".into(), kind.as_str().into());
    }
    if let Some(task_id) = task_id.filter(|t| check_task_id(t).is_ok()) {
        fields.insert("task_id".into(), task_id.into());
    }
    if let Some(thread) = thread.filter(|t| line_within(t, THREAD_MAX)) {
        fields.insert("thread".into(), thread.into());
    }
    (!fields.is_empty()).then_some(fields)
}

/// The Queen-network fields of a delivered message, each dropped when it
/// is not well formed: `(kind, task_id, thread)`.
pub fn delivered_fields(message: &Value) -> (Option<QueenKind>, Option<String>, Option<String>) {
    let kind = message["kind"].as_str().and_then(QueenKind::parse);
    let task_id = message["task_id"]
        .as_str()
        .filter(|t| check_task_id(t).is_ok())
        .map(str::to_owned);
    let thread = message["thread"]
        .as_str()
        .filter(|t| line_within(t, THREAD_MAX))
        .map(str::to_owned);
    (kind, task_id, thread)
}

fn line_within(value: &str, max: usize) -> bool {
    let n = value.chars().count();
    (1..=max).contains(&n) && !value.chars().any(char::is_control)
}

/// The relay's per-worker budget: publishes per worker in the last minute.
/// In memory only: a restart forgives the last minute.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RelayBudget {
    sent: BTreeMap<String, VecDeque<i64>>,
}

impl RelayBudget {
    /// Take one publish from `worker`'s budget at `now_ms`; `false` when it
    /// is spent (the post waits for a later pass).
    pub fn take(&mut self, worker: &str, now_ms: i64) -> bool {
        let sent = self.sent.entry(worker.to_owned()).or_default();
        while sent
            .front()
            .is_some_and(|at| now_ms.saturating_sub(*at) >= MINUTE_MS)
        {
            sent.pop_front();
        }
        if sent.len() >= RATE_PER_MINUTE {
            return false;
        }
        sent.push_back(now_ms);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_parse_exactly_and_unknown_is_refused_with_its_reason() {
        for kind in QueenKind::ALL {
            assert_eq!(QueenKind::from_input(Some(kind.as_str())), Ok(Some(kind)));
        }
        assert_eq!(QueenKind::from_input(None), Ok(None));
        assert_eq!(QueenKind::from_input(Some("null")), Ok(None));
        let refused = QueenKind::from_input(Some("Hello")).unwrap_err();
        assert!(
            refused.contains("unknown kind") && refused.contains("stop"),
            "{refused}"
        );
    }

    #[test]
    fn relay_budget_refills_after_a_minute_per_worker() {
        let mut budget = RelayBudget::default();
        for i in 0..RATE_PER_MINUTE {
            assert!(budget.take("wkr_a", 1_000 + i64::try_from(i).unwrap()));
        }
        assert!(!budget.take("wkr_a", 2_000), "budget spent");
        assert!(budget.take("wkr_b", 2_000), "another worker has its own");
        assert!(
            budget.take("wkr_a", 61_000),
            "the first send left the window"
        );
    }

    #[test]
    fn frame_fields_carry_task_and_thread_with_or_without_a_kind_and_round_trip() {
        let mut meta = HashMap::new();
        assert_eq!(
            frame_fields(&meta, None, None),
            None,
            "a plain post's frame stays as it was"
        );
        // Filed under a task without a kind: the task and thread still travel.
        let fields = frame_fields(&meta, Some("t1"), Some("task-app-3")).unwrap();
        let frame = Value::Object(fields);
        assert_eq!(
            frame,
            serde_json::json!({"task_id": "t1", "thread": "task-app-3"})
        );
        assert_eq!(
            delivered_fields(&frame),
            (None, Some("t1".into()), Some("task-app-3".into()))
        );
        meta.insert(KIND_KEY.into(), "coordinate".into());
        let frame = Value::Object(frame_fields(&meta, Some("t1"), None).unwrap());
        assert_eq!(
            frame,
            serde_json::json!({"kind": "coordinate", "task_id": "t1"})
        );
        assert_eq!(
            delivered_fields(&frame),
            (Some(QueenKind::Coordinate), Some("t1".into()), None)
        );
    }
}
