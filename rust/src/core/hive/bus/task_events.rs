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
//! shown as an instruction. The frame is not stored by Hive: no replay, a
//! missed one is caught up by the next task read.

use std::path::Path;

use serde::Deserialize;
use serde_json::Value;

use super::address::Address;
use crate::core::a2a::message::{MessagePriority, PrivacyLevel};
use crate::core::agents::AgentRegistry;
use crate::core::hive::vault::VaultMap;
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

/// Apply one event: outdate the vault's task cache and post its notices
/// into the project's local bus. An unknown vault changes nothing.
pub(crate) fn apply(
    registry: &mut AgentRegistry,
    map: &VaultMap,
    data_dir: &Path,
    event: &TaskEvent,
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
    crate::core::hive::tasks::cache::outdate(data_dir, &record.vault_id);
    let Some(root) = record.roots.iter().find(|root| Path::new(root).is_dir()) else {
        return Ok(());
    };
    let local: Vec<String> = registry
        .list_active(Some(root))
        .into_iter()
        .map(|a| a.agent_id.clone())
        .collect();
    let key = format!(
        "{}task:{}:{}:{:?}:{}",
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

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURES: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
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
}
