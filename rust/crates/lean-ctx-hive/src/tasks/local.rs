// SPDX-License-Identifier: Apache-2.0
//! Local tasks (lean-ctx-tasks) for a project that is not linked to a Hive
//! vault: where its store lives, which projects use it, the bus notices of
//! every transition and how an agent reads them.
//!
//! - A project linked to a vault keeps using Hive exactly as before
//!   ([`linked`]); every other project keeps its tasks on this machine in
//!   `<data>/tasks/<project hash>/` (keyed like knowledge).
//! - Every transition posts on the local bus (category `status`, sender
//!   [`SENDER`], key `task:<n>:<event>:<seq>:<i>`), so the Queen and Hive
//!   Desktop react at once. A result to check goes to every agent of the
//!   project except the one that did the work. A notice names the number
//!   only, never task text, so nothing in it reads as an instruction.

use std::path::{Path, PathBuf};

use chrono::Utc;

use lean_ctx::core::a2a::message::{MessagePriority, PrivacyLevel};
use lean_ctx::core::agents::AgentRegistry;
use lean_ctx_protocol::hive::tasks::{EvidenceV1, TaskState, TaskSummaryV1, TaskV1};
use lean_ctx_tasks::{Actor, Board, Change, LocalTask, Store, TaskError, TaskEvent};

use super::EvidenceInput;
use super::render::{origin, state_with};
use crate::state::HiveStore;
use crate::untrusted::{UntrustedKind, wrap};
use crate::vault::VaultMap;

/// Who posts the notices of local tasks on the bus.
pub const SENDER: &str = "local-tasks";

/// Whether `root` is linked to a Hive vault on this machine (its tasks are
/// Hive's then). A Hive state that cannot be read is an error, never
/// "unlinked": a linked project's writes must not go local silently. Only
/// a vault map that does not exist yet means no project is linked.
pub fn linked(root: &Path) -> Result<bool, String> {
    let unavailable =
        |e: &dyn std::fmt::Display| format!("the Hive link of this project cannot be read: {e}");
    let store = HiveStore::open().map_err(|e| unavailable(&e))?;
    let path = store
        .data_dir()
        .join(lean_ctx::core::vault_map::VAULTS_FILE);
    let map: VaultMap = match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| unavailable(&e))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(unavailable(&e)),
    };
    Ok(map.for_root(&root.to_string_lossy()).is_some())
}

/// The directory of `root`'s local tasks.
pub fn store_dir(root: &Path) -> Result<PathBuf, TaskError> {
    let hash = lean_ctx::core::project_hash::hash_project_root(&root.to_string_lossy());
    lean_ctx::core::data_dir::lean_ctx_data_dir()
        .map(|dir| dir.join("tasks").join(hash))
        .map_err(TaskError::Store)
}

fn store(root: &Path) -> Result<Store, TaskError> {
    store_dir(root).map(Store::new)
}

/// The board of `root` as of now; claims that ran out are given back first,
/// and every transition the bus has not taken yet is announced.
pub fn board(root: &Path) -> Result<Board, TaskError> {
    let (board, _) = store(root)?.read_announced(Utc::now(), |changes| announce(root, changes))?;
    Ok(board)
}

/// Change `root`'s tasks and announce every transition of the write (and
/// any an earlier failed bus write left behind).
pub fn write<T>(
    root: &Path,
    change: impl FnOnce(&mut Board, chrono::DateTime<Utc>) -> Result<T, TaskError>,
) -> Result<T, TaskError> {
    let now = Utc::now();
    let (out, _) = store(root)?.write_announced(
        now,
        |board| change(board, now),
        |changes| announce(root, changes),
    )?;
    Ok(out)
}

/// The project's local tasks, by number (LeanCTX · Share brings them into
/// a vault on link).
pub fn list(root: &Path) -> Result<Vec<LocalTask>, String> {
    board(root)
        .map(|board| board.tasks.into_values().collect())
        .map_err(|e| e.to_string())
}

/// Record that local task `number` was brought into `vault` (`ws/vault`)
/// as its task `vault_number`; a second mark is refused (`already_shared`).
pub fn mark_shared(root: &Path, number: u64, vault: &str, vault_number: u64) -> Result<(), String> {
    write(root, |board, now| {
        board.mark_shared(number, &Actor::person("person"), vault, vault_number, now)
    })
    .map(|_| ())
    .map_err(|e| e.to_string())
}

/// Evidence as a caller names it, checked as Hive checks it (a
/// `file_anchor` is digested in `root` as it is now).
pub fn evidence(root: &Path, inputs: Vec<EvidenceInput>) -> Result<Vec<EvidenceV1>, TaskError> {
    inputs
        .into_iter()
        .map(|input| {
            input
                .into_evidence(root)
                .map_err(|e| TaskError::Invalid(e.to_string()))
        })
        .collect()
}

/// The posts `change` asks for: `(to, text)`, `None` = every agent of the
/// project. `local` are the project's active agents.
pub fn notices(change: &Change, local: &[String]) -> Vec<(Option<String>, String)> {
    let n = change.number;
    let show = format!("ctx_tasks action=show number={n}");
    let text = match change.event {
        TaskEvent::Proposed => format!(
            "Local task #{n} was proposed: read it with {show}, then action=accept or action=decline."
        ),
        TaskEvent::Accepted => {
            format!("Local task #{n} is open: read it with {show}, claim it with action=claim.")
        }
        TaskEvent::Released => format!(
            "Local task #{n} is open again (its claim ended): read it with {show}, claim it with action=claim."
        ),
        TaskEvent::Rejected => format!(
            "Local task #{n} is open again (a check rejected its result): read it with {show}, claim it with action=claim."
        ),
        TaskEvent::Submitted => {
            let text = format!(
                "Local task #{n} has a result to check: read it with {show}, then action=verify verdict=accept|reject (another agent than the one that did the work)."
            );
            let worker = change.worker.as_deref().unwrap_or_default();
            if !local.iter().any(|agent| agent == worker) {
                return vec![(None, text)];
            }
            return local
                .iter()
                .filter(|agent| agent.as_str() != worker)
                .map(|agent| (Some(agent.clone()), text.clone()))
                .collect();
        }
        TaskEvent::Declined => format!("Local task #{n} was declined."),
        TaskEvent::Claimed => format!("Local task #{n} was claimed."),
        TaskEvent::Updated => format!("Local task #{n} has a progress update."),
        TaskEvent::Failed => format!("Local task #{n} ended as failed."),
        TaskEvent::Verified => format!("Local task #{n} is done: its result was accepted."),
        TaskEvent::Stopped => format!("Local task #{n} was stopped."),
        TaskEvent::Amended => format!(
            "Local task #{n}: its result was amended or its merge recorded (the history keeps old and new): read it with {show}."
        ),
        TaskEvent::Shared | TaskEvent::Fields => return Vec::new(),
    };
    vec![(None, text)]
}

/// Post the notices of `changes` on the project's bus; whether the bus
/// write went through. A notice that cannot be posted never undoes the
/// transition (it is journaled already): when the bus write fails (a
/// contended `registry.lock` past its wait, an unreadable registry) the
/// store keeps it due and it is posted on the next access (Hive task #39;
/// before, it was dropped here with only a warning).
pub fn announce(root: &Path, changes: &[Change]) -> bool {
    if changes
        .iter()
        .all(|c| matches!(c.event, TaskEvent::Shared | TaskEvent::Fields))
    {
        return true;
    }
    let root = root.to_string_lossy().into_owned();
    let posted = AgentRegistry::mutate_locked(|registry| post_notices(registry, &root, changes));
    if let Err(error) = &posted {
        tracing::warn!("local tasks: bus notice not posted yet (next access retries): {error}");
    }
    posted.is_ok()
}

/// [`announce`] into `registry` (no lock of its own).
pub fn post_notices(registry: &mut AgentRegistry, root: &str, changes: &[Change]) {
    let local: Vec<String> = registry
        .list_active(Some(root))
        .into_iter()
        .map(|a| a.agent_id.clone())
        .collect();
    for change in changes {
        let key = format!(
            "task:{}:{}:{}",
            change.number,
            change.event.as_str(),
            change.seq
        );
        for (i, (to, text)) in notices(change, &local).into_iter().enumerate() {
            if let Err(error) = registry.post_message_keyed(
                Some(root),
                SENDER,
                to.as_deref(),
                "status",
                &text,
                PrivacyLevel::Team,
                MessagePriority::Normal,
                None,
                Some(&format!("{key}:{i}")),
            ) {
                tracing::warn!("local tasks: notice {key} not posted: {error}");
            }
        }
    }
}

/// `task` with `mine` set for the reader `agent`.
pub fn for_reader(task: &TaskV1, agent: Option<&str>) -> TaskV1 {
    let mut task = task.clone();
    let mine = |actor: &mut lean_ctx_protocol::hive::tasks::TaskActorV1| {
        actor.mine = agent.is_some() && actor.agent.as_deref() == agent;
    };
    if let Some(claim) = task.claim.as_mut() {
        mine(&mut claim.holder);
    }
    mine(&mut task.created_by);
    if let Some(result) = task.result.as_mut() {
        mine(&mut result.by);
    }
    task
}

/// A listing row of `task` for the reader `agent` (the newest transition
/// is its update time).
pub fn summary(local: &LocalTask, agent: Option<&str>) -> TaskSummaryV1 {
    let task = &local.task;
    TaskSummaryV1 {
        number: task.number,
        state: task.state,
        lifecycle: task.lifecycle,
        title: task.title.clone(),
        role: task.role.clone(),
        origin: task.origin.kind,
        updated_at: local
            .transitions
            .last()
            .map_or_else(|| task.created_at.clone(), |t| t.at.clone()),
        mine: agent.is_some()
            && task
                .claim
                .as_ref()
                .is_some_and(|c| c.holder.agent.as_deref() == agent),
        priority: task.priority,
        labels: task.labels.clone(),
        fields_version: task.fields_version,
    }
}

/// Which local tasks a listing shows.
#[derive(Debug, Clone, Default)]
pub struct LocalFilter {
    pub state: Option<TaskState>,
    pub role: Option<String>,
    /// Only tasks the reader holds.
    pub mine: bool,
}

/// The tasks `filter` selects, high priority first, then by number, for the
/// reader `agent`.
pub fn select<'a>(
    board: &'a Board,
    filter: &LocalFilter,
    agent: Option<&str>,
) -> Vec<&'a LocalTask> {
    let mut tasks: Vec<&LocalTask> = board
        .tasks
        .values()
        .filter(|local| filter.state.is_none_or(|s| local.task.state == s))
        .filter(|local| {
            filter
                .role
                .as_deref()
                .is_none_or(|r| local.task.role.as_deref() == Some(r))
        })
        .filter(|local| !filter.mine || summary(local, agent).mine)
        .collect();
    // The order of work, as a Hive listing: high first, then by number.
    tasks.sort_by_key(|local| (super::fields::rank(local.task.priority), local.task.number));
    tasks
}

/// The listing as an agent reads it; task text fenced as foreign data.
pub fn render_list(tasks: &[&LocalTask], agent: Option<&str>, limit: usize) -> String {
    if tasks.is_empty() {
        return "No local tasks match (this project is not linked to a Hive vault; its tasks stay on this machine).".into();
    }
    let mut body = String::new();
    for local in tasks.iter().take(limit) {
        let row = summary(local, agent);
        body.push_str(&format!(
            "#{} {}{} ({}){}{}{}: {}\n",
            row.number,
            super::render::high(row.priority),
            state_with(row.state, row.lifecycle),
            origin(row.origin),
            row.role
                .as_deref()
                .map(|role| format!(" role={role}"))
                .unwrap_or_default(),
            if row.mine { " mine" } else { "" },
            super::render::labels(&row.labels),
            row.title.as_deref().unwrap_or("(untitled)"),
        ));
    }
    let mut out = format!("Local tasks ({}):\n", tasks.len());
    out.push_str(&wrap(
        UntrustedKind::TaskBody,
        "local tasks",
        body.trim_end(),
    ));
    let more = tasks.len().saturating_sub(limit);
    if more > 0 {
        out.push_str(&format!("\n… {more} more (ctx_tasks action=list state=…)"));
    }
    out
}

/// One local task with its updates, result and transitions.
pub fn render_task(local: &LocalTask, agent: Option<&str>) -> String {
    let task = for_reader(&local.task, agent);
    let mut out = format!(
        "Local task #{} — {} ({})",
        task.number,
        state_with(task.state, task.lifecycle),
        origin(task.origin.kind)
    );
    if let Some(role) = task
        .role
        .as_deref()
        .filter(|role| lean_ctx_protocol::hive::tasks::validate_role(role).is_ok())
    {
        out.push_str(&format!(" role={role}"));
    }
    if let Some(claim) = &task.claim {
        out.push_str(&format!(
            "\nClaimed until {}{}.",
            claim.until.as_str(),
            if claim.holder.mine { " (by you)" } else { "" }
        ));
    }
    let source = format!("local task #{}", task.number);
    let mut body = String::new();
    if let Some(title) = &task.title {
        body.push_str(&format!("Title: {title}\n"));
    }
    if let Some(goal) = &task.goal {
        body.push_str(&format!("Goal: {goal}\n"));
    }
    if let Some(acceptance) = &task.acceptance {
        body.push_str(&format!("Done when: {acceptance}\n"));
    }
    body.push_str(&format!(
        "Priority: {}\n",
        super::fields::priority_word(task.priority)
    ));
    if !task.labels.is_empty() {
        body.push_str(&format!("Labels: {}\n", task.labels.join(", ")));
    }
    body.push_str(&format!("Created by: {}\n", task.created_by.label));
    out.push('\n');
    out.push_str(&wrap(UntrustedKind::TaskBody, &source, body.trim_end()));
    if !task.updates.is_empty() {
        let mut updates = String::new();
        for update in &task.updates {
            updates.push_str(&format!(
                "{} {}: {}\n",
                update.at.as_str(),
                update.by.label,
                update.summary.as_deref().unwrap_or("(no summary)")
            ));
        }
        out.push_str("\nUpdates:\n");
        out.push_str(&wrap(
            UntrustedKind::TaskUpdate,
            &source,
            updates.trim_end(),
        ));
    }
    if let Some(result) = &task.result {
        out.push_str(&format!(
            "\nResult by {} ({} evidence){}",
            result.by.label,
            result.evidence.len(),
            if result.by.mine {
                ", yours: another agent or a person checks it"
            } else {
                ""
            }
        ));
    }
    if !local.transitions.is_empty() {
        let mut history = String::new();
        for t in &local.transitions {
            history.push_str(&format!(
                "{} {} by {}{}\n",
                t.at.as_str(),
                t.event.as_str(),
                t.by.label,
                t.reason
                    .as_deref()
                    .map(|r| format!(": {r}"))
                    .unwrap_or_default()
            ));
        }
        out.push_str("\nHistory:\n");
        out.push_str(&wrap(
            UntrustedKind::TaskUpdate,
            &source,
            history.trim_end(),
        ));
    }
    out
}

/// The answer of a write, in the words of the Hive answers.
pub fn render_change(change: &Change) -> String {
    let n = change.number;
    match change.event {
        TaskEvent::Proposed => format!(
            "Local task #{n} proposed; anyone accepts it (action=accept) before an agent claims it."
        ),
        TaskEvent::Accepted => format!("Local task #{n} is open for any agent to claim."),
        TaskEvent::Declined => format!("Local task #{n} declined."),
        TaskEvent::Claimed => format!(
            "Claimed local task #{n}. Report with action=update (renews the claim), finish with action=submit."
        ),
        TaskEvent::Updated => format!("Update recorded on local task #{n}."),
        TaskEvent::Released => format!("Local task #{n} released."),
        TaskEvent::Submitted => format!(
            "Result submitted on local task #{n}; another agent or a person checks it before it is done."
        ),
        TaskEvent::Failed => format!("Local task #{n} ended as failed with this result."),
        TaskEvent::Verified => format!("Local task #{n} is done: the result is accepted."),
        TaskEvent::Rejected => format!("Local task #{n} is open again: the result is rejected."),
        TaskEvent::Stopped => format!("Local task #{n} stopped."),
        TaskEvent::Amended => {
            format!("Result of local task #{n} amended; the history keeps old and new.")
        }
        TaskEvent::Shared => format!("Local task #{n} marked as brought into a vault."),
        TaskEvent::Fields => format!("Local task #{n}: priority and labels set."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(event: TaskEvent, worker: Option<&str>) -> Change {
        Change {
            seq: 3,
            number: 7,
            event,
            state: TaskState::Submitted,
            worker: worker.map(str::to_owned),
        }
    }

    #[test]
    fn an_unreadable_vault_map_is_an_error_not_unlinked() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        let root = tempfile::tempdir().expect("project");
        assert_eq!(linked(root.path()), Ok(false), "no vault map yet");
        let store = HiveStore::open().expect("hive store");
        std::fs::create_dir_all(store.data_dir()).expect("hive dir");
        std::fs::write(
            store
                .data_dir()
                .join(lean_ctx::core::vault_map::VAULTS_FILE),
            b"{not json",
        )
        .expect("corrupt map");
        assert!(linked(root.path()).is_err());
    }

    #[test]
    fn a_result_to_check_goes_to_everyone_but_its_worker() {
        let local = vec!["queen".to_owned(), "w1".to_owned(), "w2".to_owned()];
        let to: Vec<_> = notices(&change(TaskEvent::Submitted, Some("w1")), &local)
            .into_iter()
            .map(|(to, _)| to)
            .collect();
        assert_eq!(to, [Some("queen".to_owned()), Some("w2".to_owned())]);
        // A worker not on this bus: everyone may check it.
        let to: Vec<_> = notices(&change(TaskEvent::Submitted, Some("gone")), &local)
            .into_iter()
            .map(|(to, _)| to)
            .collect();
        assert_eq!(to, [None]);
        assert_eq!(notices(&change(TaskEvent::Proposed, None), &local).len(), 1);
        assert!(notices(&change(TaskEvent::Shared, None), &local).is_empty());
    }
}
