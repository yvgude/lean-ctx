// SPDX-License-Identifier: Apache-2.0
//! `task.event` frames (C0 2.2 §10.1): Hive tells every engine of a vault
//! when one of its tasks changed. The engine outdates its task cache (the
//! next `ctx_tasks` read fetches the task again) and hands the work to its
//! own agents on the local bus, so agents orchestrate tasks themselves:
//!
//! - `opened`, `released`, `rejected`: the task is open (again): every agent
//!   of the project may claim it;
//! - `submitted`: a result waits for a second agent's check: every agent of
//!   the project except the one that did the work (Hive refuses a
//!   self-verify);
//! - everything else changes only the cache.
//!
//! The frame carries no task text, only its number, so nothing from it is
//! shown as an instruction. The frame is not stored by Hive: no replay. A
//! frame missed while the relay was away or still subscribing is caught up
//! after each hello (`catch_up`): the open and submitted tasks, read with
//! `GET …/tasks?state=`, go through the same `apply`. The relay state
//! keeps the last change applied per task ([`TasksSeen`]), so neither path
//! announces a change twice or announces an older one after a newer.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::address::Address;
use crate::tasks::ListFilter;
use crate::vault::VaultMap;
use lean_ctx::core::a2a::message::{MessagePriority, PrivacyLevel};
use lean_ctx::core::agents::AgentRegistry;
use lean_ctx_protocol::hive::tasks::TaskState;

/// What happened to the task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskEventKind {
    Opened,
    Claimed,
    Released,
    Submitted,
    Verified,
    Rejected,
    Cancelled,
    Failed,
    #[serde(other)]
    Unknown,
}

/// Who acted: a worker and its engine, or Hive itself (`system`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct TaskEventBy {
    #[serde(default)]
    pub worker_id: Option<String>,
    #[serde(default)]
    pub instance_id: Option<String>,
    #[serde(default)]
    pub system: bool,
}

/// One `task.event` frame.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TaskEvent {
    pub vault: String,
    pub number: u64,
    pub event: TaskEventKind,
    pub state: TaskState,
    pub by: TaskEventBy,
    pub at: u64,
}

impl TaskEvent {
    pub fn parse(frame: &Value) -> Option<Self> {
        serde_json::from_value(frame.clone())
            .ok()
            .filter(|e: &Self| e.number > 0)
    }
}

/// The local posts an event asks for: `(to, text)`, `None` = every agent
/// of the project. `local` are the project's active agents.
pub fn notices(event: &TaskEvent, local: &[String]) -> Vec<(Option<String>, String)> {
    let n = event.number;
    match event.event {
        TaskEventKind::Opened | TaskEventKind::Released | TaskEventKind::Rejected => {
            let why = match event.event {
                TaskEventKind::Opened => "is open",
                TaskEventKind::Released => "is open again (its claim ended)",
                _ => "is open again (a check rejected its result)",
            };
            vec![(
                None,
                format!(
                    "Hive task #{n} {why}: read it with ctx_tasks action=show number={n}, claim it with action=claim."
                ),
            )]
        }
        TaskEventKind::Submitted => {
            let text = format!(
                "Hive task #{n} has a result to check: read it with ctx_tasks action=show number={n}, then action=verify verdict=accept|reject (another agent than the one that did the work)."
            );
            // `by.worker_id` is the agent id the claim and result named
            // (Hive compares the same string for self_verify).
            let worker = event.by.worker_id.as_deref().unwrap_or_default();
            if !local.iter().any(|agent| agent == worker) {
                // The result came from another agent: any agent here may check it.
                return vec![(None, text)];
            }
            local
                .iter()
                .filter(|agent| agent.as_str() != worker)
                .map(|agent| (Some(agent.clone()), text.clone()))
                .collect()
        }
        _ => Vec::new(),
    }
}

/// The last change applied to one task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSeen {
    pub state: TaskState,
    /// Hive's time of the change (unix ms); a catch-up has it to the second.
    pub at: u64,
    /// Recorded by a catch-up, not by a frame.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub caught_up: bool,
}

/// Vault address (`#ws/vault`) → task number → the last change applied.
pub type TasksSeen = BTreeMap<String, BTreeMap<u64, TaskSeen>>;

/// Tasks remembered per vault (the oldest changes drop first).
const MAX_SEEN: usize = 1000;

/// Where an event came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// A `task.event` frame.
    Frame,
    /// A task list read after a hello (`catch_up`).
    CatchUp,
}

/// Record `event` in `seen`; whether its notices are due. An event older
/// than the change already applied is dropped. A catch-up announces only a
/// task whose state changed since; a frame repeats nothing a catch-up of
/// the same change (same state, same second) already announced.
fn admit(seen: &mut TasksSeen, event: &TaskEvent, origin: Origin) -> bool {
    let tasks = seen.entry(event.vault.clone()).or_default();
    let notify = match tasks.get(&event.number) {
        None => true,
        Some(last) if event.at < last.at => return false,
        Some(last) => match origin {
            Origin::Frame => {
                !(last.caught_up && last.state == event.state && event.at < last.at + 1000)
            }
            Origin::CatchUp if event.at > last.at && last.state != event.state => true,
            Origin::CatchUp => return false,
        },
    };
    tasks.insert(
        event.number,
        TaskSeen {
            state: event.state,
            at: event.at,
            caught_up: origin == Origin::CatchUp,
        },
    );
    while tasks.len() > MAX_SEEN {
        let Some(oldest) = tasks.iter().min_by_key(|(_, s)| s.at).map(|(n, _)| *n) else {
            break;
        };
        tasks.remove(&oldest);
    }
    notify
}

/// Apply one event: outdate the vault's task cache and post its notices
/// into the project's local bus, unless `seen` already holds this change
/// or a newer one. An unknown vault changes nothing.
pub(crate) fn apply(
    registry: &mut AgentRegistry,
    map: &VaultMap,
    data_dir: &Path,
    seen: &mut TasksSeen,
    event: &TaskEvent,
    origin: Origin,
) -> Result<(), String> {
    let Ok(Address::Vault { workspace, vault }) = Address::parse(Some(&event.vault)) else {
        return Ok(());
    };
    let Some(record) = map
        .vaults
        .values()
        .find(|r| r.ns == workspace && r.name == vault)
    else {
        return Ok(());
    };
    crate::tasks::cache::outdate(data_dir, &record.vault_id);
    if !admit(seen, event, origin) {
        return Ok(());
    }
    let Some(root) = record.roots.iter().find(|root| Path::new(root).is_dir()) else {
        return Ok(());
    };
    let local: Vec<String> = registry
        .list_active(Some(root))
        .into_iter()
        .map(|a| a.agent_id.clone())
        .collect();
    let source = match origin {
        Origin::Frame => "",
        Origin::CatchUp => "caught_up:",
    };
    let key = format!(
        "{}task:{}:{}:{:?}:{source}{}",
        super::relay_v2::INBOUND_KEY_PREFIX,
        event.vault,
        event.number,
        event.event,
        event.at
    );
    for (i, (to, text)) in notices(event, &local).into_iter().enumerate() {
        registry.post_message_keyed(
            Some(root),
            "hive",
            to.as_deref(),
            "status",
            &text,
            PrivacyLevel::Team,
            MessagePriority::Normal,
            None,
            Some(&format!("{key}:{i}")),
        )?;
    }
    Ok(())
}

/// The open and submitted tasks of the `subscribed` vaults as events, for
/// `apply` with [`Origin::CatchUp`], and what could not be read. Blocking
/// (HTTP); the relay runs it off its session loop after each hello.
pub(crate) fn catch_up(map: &VaultMap, subscribed: &[String]) -> (Vec<TaskEvent>, Vec<String>) {
    let (mut events, mut errors) = (Vec::new(), Vec::new());
    for record in map.vaults.values() {
        let vault = super::relay_v2::address_of(record);
        if !subscribed.contains(&vault) {
            continue;
        }
        let Some(root) = record.roots.iter().find(|root| Path::new(root).is_dir()) else {
            continue;
        };
        let root = Path::new(root);
        for (state, kind) in [
            (TaskState::Open, TaskEventKind::Opened),
            (TaskState::Submitted, TaskEventKind::Submitted),
        ] {
            let filter = ListFilter {
                state: Some(state),
                ..ListFilter::default()
            };
            let list = match crate::tasks::list(root, &filter) {
                Ok(list) => list,
                Err(error) => {
                    errors.push(format!("{vault}: {error}"));
                    continue;
                }
            };
            for task in list.tasks.into_iter().filter(|task| task.state == state) {
                // A result's worker gets no request to check its own work.
                let worker_id = (kind == TaskEventKind::Submitted)
                    .then(|| crate::tasks::show(root, task.number).ok())
                    .flatten()
                    .and_then(|view| view.task.result)
                    .and_then(|result| result.by.agent);
                events.push(TaskEvent {
                    vault: vault.clone(),
                    number: task.number,
                    event: kind,
                    state,
                    by: TaskEventBy {
                        worker_id,
                        ..TaskEventBy::default()
                    },
                    at: unix_ms(task.updated_at.as_str()),
                });
            }
        }
    }
    (events, errors)
}

/// `YYYY-MM-DDTHH:MM:SSZ` as unix ms; 0 (older than any change) if invalid.
fn unix_ms(timestamp: &str) -> u64 {
    chrono::DateTime::parse_from_rfc3339(timestamp)
        .ok()
        .and_then(|t| u64::try_from(t.timestamp_millis()).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURES: &str = concat!(
        concat!(env!("CARGO_MANIFEST_DIR"), "/../.."),
        "/crates/lean-ctx-protocol/tests/fixtures/hive-v2/bus"
    );

    fn frame(name: &str) -> TaskEvent {
        let text =
            std::fs::read_to_string(format!("{FIXTURES}/frame.task_event.{name}.json")).unwrap();
        let value: Value = serde_json::from_str(&text).unwrap();
        let event = TaskEvent::parse(&value).unwrap_or_else(|| panic!("{name}"));
        assert_eq!(
            super::super::v2::parse_frame(&text).unwrap(),
            super::super::v2::Inbound::TaskEvent(Box::new(event.clone())),
            "{name}"
        );
        event
    }

    #[test]
    fn the_contract_frames_parse() {
        for (name, kind, state) in [
            ("opened", TaskEventKind::Opened, TaskState::Open),
            ("released", TaskEventKind::Released, TaskState::Open),
            ("submitted", TaskEventKind::Submitted, TaskState::Submitted),
            ("verified", TaskEventKind::Verified, TaskState::Done),
        ] {
            let event = frame(name);
            assert_eq!((event.event, event.state), (kind, state), "{name}");
            assert_eq!(event.vault, "#acme/payments");
        }
    }

    #[test]
    fn open_tasks_go_to_every_agent_and_results_to_the_others() {
        let opened = frame("opened");
        let out = notices(&opened, &["a1".to_string()]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, None);
        assert!(
            out[0].1.starts_with("Hive task #7 is open:"),
            "{}",
            out[0].1
        );

        let submitted = frame("submitted");
        let worker = submitted.by.worker_id.clone().unwrap();
        let others = vec!["a1".to_string(), "a3".to_string()];
        // Another agent's result: every agent here may check it.
        assert_eq!(notices(&submitted, &others)[0].0, None);
        // This machine's agent did the work: never that agent (no self-verify).
        let local = vec!["a1".to_string(), worker, "a3".to_string()];
        let to: Vec<_> = notices(&submitted, &local)
            .into_iter()
            .map(|(to, text)| {
                assert!(text.contains("action=verify"), "{text}");
                to.unwrap()
            })
            .collect();
        assert_eq!(to, ["a1", "a3"]);

        assert!(notices(&frame("verified"), &local).is_empty());
    }

    /// Task 7 of the fixture vault in `state` at `at` (unix ms).
    fn change(event: TaskEventKind, state: TaskState, at: u64) -> TaskEvent {
        TaskEvent {
            event,
            state,
            at,
            ..frame("opened")
        }
    }

    #[test]
    fn a_task_opened_while_subscribing_is_announced_once_by_the_catch_up() {
        let mut seen = TasksSeen::new();
        // The frame went by before the hello: only the catch-up sees it.
        let listed = change(TaskEventKind::Opened, TaskState::Open, 1_792_800_020_000);
        assert!(admit(&mut seen, &listed, Origin::CatchUp));
        // The same change as a frame (same second, ms precision): no repeat.
        let late = change(TaskEventKind::Opened, TaskState::Open, 1_792_800_020_431);
        assert!(!admit(&mut seen, &late, Origin::Frame));
        // A second catch-up of the unchanged task: still nothing.
        assert!(!admit(&mut seen, &listed, Origin::CatchUp));
    }

    #[test]
    fn a_catch_up_never_repeats_or_rewinds_what_a_frame_announced() {
        let mut seen = TasksSeen::new();
        let opened = change(TaskEventKind::Opened, TaskState::Open, 1_792_800_020_431);
        assert!(admit(&mut seen, &opened, Origin::Frame));
        // The list read the same change (to the second): no repeat.
        let listed = change(TaskEventKind::Opened, TaskState::Open, 1_792_800_020_000);
        assert!(!admit(&mut seen, &listed, Origin::CatchUp));
        // The task is done; a list read before that is stale and says nothing.
        let done = change(TaskEventKind::Verified, TaskState::Done, 1_792_800_030_000);
        assert!(admit(&mut seen, &done, Origin::Frame));
        assert!(!admit(&mut seen, &listed, Origin::CatchUp));
        assert_eq!(seen["#acme/payments"][&7].state, TaskState::Done);
    }

    #[test]
    fn a_task_open_again_is_announced_even_when_its_claim_was_missed() {
        let mut seen = TasksSeen::new();
        let opened = change(TaskEventKind::Opened, TaskState::Open, 1_000);
        assert!(admit(&mut seen, &opened, Origin::Frame));
        // `claimed` never arrived; the release still says "open again".
        let released = change(TaskEventKind::Released, TaskState::Open, 9_000);
        assert!(admit(&mut seen, &released, Origin::Frame));
        // Reopened while the relay was away: the catch-up announces it.
        let claimed = change(TaskEventKind::Claimed, TaskState::Claimed, 10_000);
        assert!(admit(&mut seen, &claimed, Origin::Frame));
        let reopened = change(TaskEventKind::Opened, TaskState::Open, 20_000);
        assert!(admit(&mut seen, &reopened, Origin::CatchUp));
        // A frame older than what was applied is dropped.
        assert!(!admit(&mut seen, &claimed, Origin::Frame));
        assert_eq!(seen["#acme/payments"][&7].state, TaskState::Open);
    }

    #[test]
    fn the_oldest_changes_drop_first() {
        let mut seen = TasksSeen::new();
        for number in 1..=(MAX_SEEN as u64 + 1) {
            let event = TaskEvent {
                number,
                ..change(TaskEventKind::Opened, TaskState::Open, number)
            };
            assert!(admit(&mut seen, &event, Origin::Frame));
        }
        let tasks = &seen["#acme/payments"];
        assert_eq!(tasks.len(), MAX_SEEN);
        assert!(!tasks.contains_key(&1));
    }

    #[test]
    fn listed_times_are_unix_ms() {
        assert_eq!(unix_ms("2026-10-15T08:00:20Z"), 1_792_051_220_000);
        assert_eq!(unix_ms("not a time"), 0);
    }
}
