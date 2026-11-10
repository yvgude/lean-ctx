// SPDX-License-Identifier: Apache-2.0
//! `ctx_tasks` without waiting for Hive (connector rule "Hive never blocks
//! a tool call").
//!
//! Reads (`list`, `show`, the session briefing) answer at once from the
//! task lists and tasks Hive last sent, kept in `<data>/hive/task-cache.json`
//! (0600). An entry that is missing, older than [`FRESH`] or outdated by a
//! change of this machine is refreshed off the tool call
//! ([`crate::background`]); the answer says when its data is
//! stale or not loaded yet. Changes (claim, update, submit, release, read,
//! confirm, object) are queued the same way ([`queue`]): the tool call
//! answers that they are queued, and what Hive answered comes in front of
//! the caller's next `ctx_tasks` answer. Nothing is shown as done before
//! Hive confirmed it.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{
    Connected, ListFilter, TaskListing, connected, list_cached, render_listing, render_task,
    show_cached, view_of,
};
use crate::HiveError;
use crate::background;
use crate::state::{load_or_quarantine, lock_state, write_private};
use lean_ctx_protocol::hive::lease::HiveRight;
use lean_ctx_protocol::hive::tasks::{TaskState, TaskV1};

const CACHE_FILE: &str = "task-cache.json";
/// An entry younger than this is shown without a refresh.
pub const FRESH: Duration = Duration::from_secs(60);
/// The answer while nothing is cached yet.
const NOT_LOADED: &str = "Hive tasks are not loaded on this machine yet; they are being fetched in the background. Ask again in a moment.";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry<T> {
    at: DateTime<Utc>,
    /// A change of this machine came after it (a claim, an update…).
    #[serde(default)]
    outdated: bool,
    value: T,
}

impl<T> Entry<T> {
    fn new(value: T) -> Self {
        Self {
            at: Utc::now(),
            outdated: false,
            value,
        }
    }

    fn fresh(&self, now: DateTime<Utc>) -> bool {
        !self.outdated && chrono::Duration::from_std(FRESH).is_ok_and(|fresh| self.at + fresh > now)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct VaultCache {
    /// Keyed by the list query (`?state=open`, `` for all).
    #[serde(default)]
    lists: BTreeMap<String, Entry<TaskListing>>,
    #[serde(default)]
    tasks: BTreeMap<u64, Entry<TaskV1>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Cache {
    #[serde(default)]
    vaults: BTreeMap<String, VaultCache>,
}

fn load(data_dir: &Path) -> Cache {
    load_or_quarantine(&data_dir.join(CACHE_FILE), |_: &Cache| true)
        .unwrap_or_else(|error| {
            tracing::warn!("hive task cache: {error}");
            None
        })
        .unwrap_or_default()
}

/// Change the vault's cache under its lock; never around a request.
fn mutate(data_dir: &Path, vault_id: &str, change: impl FnOnce(&mut VaultCache)) {
    let saved = lock_state(data_dir, CACHE_FILE).and_then(|_lock| {
        let mut cache = load(data_dir);
        change(cache.vaults.entry(vault_id.to_string()).or_default());
        let bytes = serde_json::to_vec(&cache).map_err(|e| HiveError::Local(e.to_string()))?;
        write_private(data_dir, CACHE_FILE, &bytes)
    });
    if let Err(error) = saved {
        tracing::debug!("hive task cache not saved: {error}");
    }
}

/// Mark what is cached for the vault outdated: the next read refreshes it
/// (a change of this machine, or a `task.event` from Hive).
pub(crate) fn outdate(data_dir: &Path, vault_id: &str) {
    super::reads::outdate(data_dir, vault_id);
    mutate(data_dir, vault_id, |vault| {
        vault.lists.values_mut().for_each(|e| e.outdated = true);
        vault.tasks.values_mut().for_each(|e| e.outdated = true);
    });
}

/// The line under data that is not fresh.
fn stale_note(at: DateTime<Utc>) -> String {
    format!(
        "\n(Hive data from {}; a refresh is running, ask again for newer data)",
        at.format("%Y-%m-%dT%H:%M:%SZ")
    )
}

/// The caller of a `ctx_tasks` call: its bus agent, else its session.
pub fn caller(agent_or_session: &str) -> String {
    format!("ctx_tasks:{agent_or_session}")
}

/// Refresh the list `query` off the tool call. A failure reaches `caller`
/// with its next call; without a caller (the briefing) it is only logged.
fn refresh_list(c: Connected, query: String, filter: ListFilter, caller: Option<&str>) {
    let key = format!("tasks:{}:list{query}", c.project.vault_id);
    let report = caller.is_some();
    background::submit_once(
        caller.unwrap_or("ctx_tasks:briefing"),
        &key,
        move || match list_cached(&c.link(), &filter) {
            // A list Hive did not send now (rate limit, not reachable) is
            // not stored as fresh; the next read tries again.
            Ok(read) if read.stale.is_some() => None,
            Ok(read) => {
                mutate(c.store.data_dir(), &c.project.vault_id, |vault| {
                    vault.lists.insert(query, Entry::new(read.value));
                });
                None
            }
            Err(error) if report => Some(format!(
                "Hive tasks: the task list could not be refreshed: {error}"
            )),
            Err(error) => {
                tracing::debug!("hive task briefing refresh: {error}");
                None
            }
        },
    );
}

/// `ctx_tasks list`: the cached list, refreshed off the call when needed.
pub fn list(project_root: &Path, filter: &ListFilter, caller: &str) -> Result<String, HiveError> {
    let c = connected(project_root, HiveRight::TasksRead)?;
    let query = filter.query()?;
    let entry = load(c.store.data_dir())
        .vaults
        .remove(&c.project.vault_id)
        .and_then(|mut vault| vault.lists.remove(&query));
    let answer = match &entry {
        Some(entry) if entry.fresh(Utc::now()) => return Ok(render_listing(&entry.value, 50)),
        Some(entry) => format!(
            "{}{}",
            render_listing(&entry.value, 50),
            stale_note(entry.at)
        ),
        None => NOT_LOADED.to_string(),
    };
    refresh_list(c, query, filter.clone(), Some(caller));
    Ok(answer)
}

/// `ctx_tasks show`: the cached task, refreshed off the call when needed.
pub fn show(project_root: &Path, number: u64, caller: &str) -> Result<String, HiveError> {
    let c = connected(project_root, HiveRight::TasksRead)?;
    let entry = load(c.store.data_dir())
        .vaults
        .remove(&c.project.vault_id)
        .and_then(|mut vault| vault.tasks.remove(&number));
    let rendered = |entry: &Entry<TaskV1>| render_task(&view_of(&c.link(), entry.value.clone()));
    let answer = match &entry {
        Some(entry) if entry.fresh(Utc::now()) => return Ok(rendered(entry)),
        Some(entry) => format!("{}{}", rendered(entry), stale_note(entry.at)),
        None => NOT_LOADED.to_string(),
    };
    let key = format!("tasks:{}:#{number}", c.project.vault_id);
    background::submit_once(caller, &key, move || match show_cached(&c.link(), number) {
        Ok(read) if read.stale.is_some() => None,
        Ok(read) => {
            mutate(c.store.data_dir(), &c.project.vault_id, |vault| {
                vault.tasks.insert(number, Entry::new(read.value.task));
            });
            None
        }
        Err(error) => Some(format!(
            "Hive tasks: task #{number} could not be fetched: {error}"
        )),
    });
    Ok(answer)
}

/// The open tasks for the session briefing, from the cache; `None` when
/// there are none. Never waits for Hive.
pub(super) fn open_tasks(c: Connected) -> Option<String> {
    let filter = ListFilter {
        state: Some(TaskState::Open),
        ..ListFilter::default()
    };
    let query = filter.query().ok()?;
    let entry = load(c.store.data_dir())
        .vaults
        .remove(&c.project.vault_id)
        .and_then(|mut vault| vault.lists.remove(&query));
    let answer = match &entry {
        Some(entry) if entry.fresh(Utc::now()) => {
            return (!entry.value.list.tasks.is_empty()).then(|| render_listing(&entry.value, 5));
        }
        Some(entry) if entry.value.list.tasks.is_empty() => None,
        Some(entry) => Some(format!(
            "{}{}",
            render_listing(&entry.value, 5),
            stale_note(entry.at)
        )),
        None => Some(
            "Open Hive tasks are being fetched in the background (ctx_tasks action=list)."
                .to_string(),
        ),
    };
    refresh_list(c, query, filter, None);
    answer
}

/// Fetch the task lists a session reads first (all, and open) for the
/// checkout at `project_root` into the cache, in the caller's thread: the
/// daemon's background sync keeps them warm, so a fresh session answers at
/// once from them (with their age) instead of "being fetched". A list the
/// Instance fetched less than [`super::reads::READ_TTL`] ago is not asked
/// again. `Ok(false)` when tasks do not apply there (no right, not
/// shared): nothing fetched.
pub fn warm(project_root: &Path) -> Result<bool, HiveError> {
    let Ok(c) = connected(project_root, HiveRight::TasksRead) else {
        return Ok(false);
    };
    let open = ListFilter {
        state: Some(TaskState::Open),
        ..ListFilter::default()
    };
    for filter in [ListFilter::default(), open] {
        let query = filter.query()?;
        let read = list_cached(&c.link(), &filter)?;
        if read.stale.is_some() {
            continue;
        }
        mutate(c.store.data_dir(), &c.project.vault_id, |vault| {
            vault.lists.insert(query, Entry::new(read.value));
        });
    }
    Ok(true)
}

/// Queue the change `job` (`what` names it, e.g. "claim of task #4") for
/// `caller`. Answers at once that it is queued; what Hive answered comes
/// with the caller's next `ctx_tasks` call, and the cached lists and tasks
/// are refreshed on the next read. `Err` at once when tasks do not apply
/// here with `right` (no request made).
pub fn queue(
    project_root: &Path,
    right: HiveRight,
    caller: &str,
    what: &str,
    job: impl FnOnce() -> Result<String, HiveError> + Send + 'static,
) -> Result<String, HiveError> {
    let c = connected(project_root, right)?;
    let (data_dir, vault_id) = (c.store.data_dir().to_path_buf(), c.project.vault_id);
    let named = what.to_string();
    background::submit(caller, move || {
        let answer = job();
        outdate(&data_dir, &vault_id);
        Some(match answer {
            Ok(text) => text,
            Err(error) => format!("Hive tasks ({named}): {error}"),
        })
    });
    Ok(format!(
        "Queued for Hive: {what}. Hive's answer comes with your next ctx_tasks call; nothing is confirmed before then."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lean_ctx_protocol::hive::tasks::TaskListV1;

    #[test]
    fn an_entry_is_fresh_until_it_ages_or_a_change_outdates_it() {
        let mut entry = Entry::new(());
        let now = Utc::now();
        assert!(entry.fresh(now));
        assert!(!entry.fresh(now + chrono::Duration::seconds(61)));
        entry.outdated = true;
        assert!(!entry.fresh(now));
    }

    #[test]
    fn the_cache_keeps_lists_and_tasks_per_vault_and_outdates_them() {
        let dir = tempfile::tempdir().unwrap();
        let list: TaskListV1 = serde_json::from_value(serde_json::json!({
            "schema": lean_ctx_protocol::hive::tasks::TASK_LIST_SCHEMA,
            "vault_id": "vlt_a", "tasks": []
        }))
        .unwrap();
        // A list cached before 3.12.3 (no `partial`) reads as whole.
        let before: TaskListing = serde_json::from_value(serde_json::to_value(&list).unwrap())
            .expect("an older cached list reads");
        assert!(!before.partial);
        mutate(dir.path(), "vlt_a", |vault| {
            vault.lists.insert(
                "?state=open".into(),
                Entry::new(TaskListing {
                    list: list.clone(),
                    partial: false,
                }),
            );
        });
        let cached = load(dir.path());
        let entry = &cached.vaults["vlt_a"].lists["?state=open"];
        assert_eq!(entry.value.list, list);
        assert!(!entry.value.partial);
        assert!(entry.fresh(Utc::now()));
        assert!(!cached.vaults.contains_key("vlt_b"));
        outdate(dir.path(), "vlt_a");
        assert!(!load(dir.path()).vaults["vlt_a"].lists["?state=open"].fresh(Utc::now()));
    }
}
