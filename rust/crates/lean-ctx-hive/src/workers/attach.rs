// SPDX-License-Identifier: Apache-2.0
//! Which worker a session belongs to (SPEC-ENGINE §3.6): host app
//! detection, role normalisation, the 32-worker cap and the local book of
//! durable workers. Sessions (`mcp-<pid>-<uuid>`) are only a detail of the
//! worker they attach to.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::super::HiveError;
use super::super::state::{HiveStore, load_or_quarantine, lock_state, write_private};
use super::{GENERAL_ROLE, normalize_role, workspace_worker_id};

/// Durable workers one instance may have (workspace ids).
pub const WORKER_CAP: usize = 32;
const BOOK_FILE: &str = "workers.json";
const BOOK_SCHEMA: u32 = 1;

/// Host app of a session, from the hints in order of trust: the MCP
/// handshake's identified host, the `ctx_agent` agent type, the process
/// environment, then `mcp-client:<clientInfo.name>`.
pub fn detect_host_app<I, K, V>(
    transport_host: Option<&str>,
    agent_type: Option<&str>,
    env: I,
    client_name: Option<&str>,
) -> String
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<str>,
    V: AsRef<str>,
{
    transport_host
        .and_then(known_host)
        .or_else(|| agent_type.and_then(known_host))
        .or_else(|| host_from_env(env))
        .or_else(|| client_name.and_then(known_host))
        .map_or_else(|| mcp_client(client_name), str::to_string)
}

/// SPEC-ENGINE §3.6 host table on a host or client name.
fn known_host(name: &str) -> Option<&'static str> {
    let name = name.trim().to_ascii_lowercase();
    Some(match name.as_str() {
        "claude" | "claude-code" | "claude code" => "claude-code",
        "codex" | "codex-cli" | "codex-mcp-client" => "codex",
        "cursor" | "cursor-agent" | "cursor-vscode" => "cursor",
        "gemini" | "gemini-cli" => "gemini-cli",
        "copilot" | "gh copilot" | "github-copilot" => "copilot",
        "opencode" => "opencode",
        _ => return None,
    })
}

/// SPEC-ENGINE §3.6 environment markers.
fn host_from_env<I, K, V>(env: I) -> Option<&'static str>
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<str>,
    V: AsRef<str>,
{
    let vars: BTreeMap<String, String> = env
        .into_iter()
        .filter(|(_, v)| !v.as_ref().trim().is_empty())
        .map(|(k, v)| (k.as_ref().to_string(), v.as_ref().to_string()))
        .collect();
    let any_prefix = |prefix: &str| vars.keys().any(|k| k.starts_with(prefix));
    if vars.get("CLAUDECODE").is_some_and(|v| v.trim() == "1") {
        Some("claude-code")
    } else if any_prefix("CODEX_") {
        Some("codex")
    } else if vars.contains_key("CURSOR_TRACE_ID") || vars.contains_key("CURSOR_TASK_ID") {
        Some("cursor")
    } else if vars.contains_key("GEMINI_CLI") {
        Some("gemini-cli")
    } else if any_prefix("COPILOT_") {
        Some("copilot")
    } else if any_prefix("OPENCODE") {
        Some("opencode")
    } else {
        None
    }
}

/// `mcp-client:<name>` with the name reduced to `[A-Za-z0-9._ -]{1,64}`.
fn mcp_client(client_name: Option<&str>) -> String {
    let name: String = client_name
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ' ' | '-'))
        .take(64)
        .collect();
    let name = name.trim();
    format!(
        "mcp-client:{}",
        if name.is_empty() { "unknown" } else { name }
    )
}

/// Display name of a host app in default worker names.
pub fn host_display(host_app: &str) -> String {
    match host_app {
        "claude-code" => "Claude Code".to_string(),
        "codex" => "Codex".to_string(),
        "cursor" => "Cursor".to_string(),
        "gemini-cli" => "Gemini CLI".to_string(),
        "copilot" => "Copilot".to_string(),
        "opencode" => "OpenCode".to_string(),
        other => other
            .strip_prefix("mcp-client:")
            .unwrap_or(other)
            .to_string(),
    }
}

/// A durable worker of this instance as the engine remembers it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalWorker {
    pub worker_id: String,
    pub host_app: String,
    pub role: String,
    /// Name last accepted by Hive or set locally; `None` → default name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Ms timestamp of the E10 registration; `None` until registered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registered_at: Option<u64>,
    /// Place-scoped aliases registered through E10 (`place_ref` → alias id).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub aliases: BTreeMap<String, String>,
    /// Ms timestamp of the last session that attached to it; `None` in a
    /// book written before it was kept (oldest first when one must go).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_attached: Option<u64>,
    /// The worker of one app session (`<role>-s<tag>`, [`super::session_role`]),
    /// recorded when a live session with an app session id attached to it:
    /// it ends with that session. A role that merely looks like a tagged
    /// one (`dev-storybook`) is never marked.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub per_session: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WorkerBook {
    schema: u32,
    instance_id: String,
    workers: Vec<LocalWorker>,
    /// Registered workers evicted from the book that Hive still counts:
    /// the relay retires them (`DELETE /v1/workers/{id}`) before the next
    /// registration.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    retired: Vec<String>,
}

/// The worker a session attaches to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub worker: LocalWorker,
    /// The requested `(host_app, role)` did not fit under [`WORKER_CAP`]
    /// and every worker is in use: the session shares another worker
    /// (`hive status` warns `worker_cap_reached`).
    pub cap_reached: bool,
}

/// Pure attachment rule over the known workers: an existing `(host, role)`
/// is reused; a new one is added while fewer than [`WORKER_CAP`] exist;
/// past the cap the session maps to role `general` (of its host when that
/// worker exists, else the first `general` worker, else the first worker).
/// [`attach`] makes room first, so this fallback only happens when every
/// worker is in use.
pub fn attach_in(
    workers: &mut Vec<LocalWorker>,
    instance_id: &str,
    host_app: &str,
    role: &str,
) -> Attachment {
    let role = normalize_role(role);
    let find = |workers: &[LocalWorker], host: &str, role: &str| {
        workers
            .iter()
            .position(|w| w.host_app == host && w.role == role)
    };
    if let Some(i) = find(workers, host_app, &role) {
        return Attachment {
            worker: workers[i].clone(),
            cap_reached: false,
        };
    }
    if workers.len() < WORKER_CAP {
        let worker = new_worker(instance_id, host_app, &role);
        workers.push(worker.clone());
        return Attachment {
            worker,
            cap_reached: false,
        };
    }
    let fallback = find(workers, host_app, GENERAL_ROLE)
        .or_else(|| workers.iter().position(|w| w.role == GENERAL_ROLE))
        .unwrap_or(0);
    Attachment {
        worker: workers[fallback].clone(),
        cap_reached: true,
    }
}

fn new_worker(instance_id: &str, host_app: &str, role: &str) -> LocalWorker {
    LocalWorker {
        worker_id: workspace_worker_id(instance_id, host_app, role),
        host_app: host_app.to_string(),
        role: role.to_string(),
        name: None,
        registered_at: None,
        aliases: BTreeMap::new(),
        last_attached: None,
        per_session: false,
    }
}

/// Make room for a new `(host_app, role)` in a full book: the worker that
/// no session in `in_use` holds and that was attached longest ago leaves
/// the book (a registered one is queued in `retired` for Hive). The
/// connector's own workers (host [`CAPTURE_HOST`]: capture, friction,
/// CLI) never leave. `false` when every worker is in use.
///
/// [`CAPTURE_HOST`]: crate::sync::place::CAPTURE_HOST
pub(super) fn evict_one(
    workers: &mut Vec<LocalWorker>,
    retired: &mut Vec<String>,
    in_use: &BTreeSet<(String, String)>,
) -> bool {
    let Some(i) = workers
        .iter()
        .enumerate()
        .filter(|(_, w)| w.host_app != crate::sync::place::CAPTURE_HOST)
        .filter(|(_, w)| !in_use.contains(&(w.host_app.clone(), w.role.clone())))
        .min_by_key(|(_, w)| (w.last_attached, w.registered_at))
        .map(|(i, _)| i)
    else {
        return false;
    };
    let gone = workers.remove(i);
    if gone.registered_at.is_some() && !retired.contains(&gone.worker_id) {
        retired.push(gone.worker_id);
    }
    true
}

/// [`attach_in`] on a book that makes room first (see [`evict_one`]) and
/// stamps the worker it attaches with `now_ms`. Without `live` (the agent
/// registry could not be read) no worker is evicted: every one may be in
/// use, so a full book shares as when every worker is.
fn attach_making_room(
    book: &mut WorkerBook,
    instance_id: &str,
    host_app: &str,
    role: &str,
    live: Option<&LiveSessions>,
    now_ms: u64,
) -> Attachment {
    let role = normalize_role(role);
    let known = book
        .workers
        .iter()
        .any(|w| w.host_app == host_app && w.role == role);
    let full = !known && book.workers.len() >= WORKER_CAP;
    if let Some(live) = live.filter(|_| full) {
        let mut in_use = live.in_use.clone();
        in_use.insert((host_app.to_string(), role.clone()));
        evict_one(&mut book.workers, &mut book.retired, &in_use);
    }
    let per_session = live.is_some_and(|live| {
        live.per_session
            .contains(&(host_app.to_string(), role.clone()))
    });
    let attached = attach_in(&mut book.workers, instance_id, host_app, &role);
    // A worker that comes back is no longer to be retired.
    book.retired.retain(|id| *id != attached.worker.worker_id);
    let worker = book
        .workers
        .iter_mut()
        .find(|w| w.worker_id == attached.worker.worker_id);
    match worker {
        Some(worker) => {
            worker.last_attached = Some(now_ms);
            // Only the worker attached under its own `(host, role)` (not a
            // shared fallback past the cap) is the session's.
            if per_session && worker.host_app == host_app && worker.role == role {
                worker.per_session = true;
            }
            Attachment {
                worker: worker.clone(),
                cap_reached: attached.cap_reached,
            }
        }
        None => attached,
    }
}

/// The workers the active local sessions hold.
#[derive(Debug, Default)]
struct LiveSessions {
    /// `(host_app, role)` of every active session: these stay in the book.
    in_use: BTreeSet<(String, String)>,
    /// Those of sessions with an app session id (`<role>-s<tag>`).
    per_session: BTreeSet<(String, String)>,
}

/// The workers of every active local session, from the agent registry read
/// strictly: a registry that exists but cannot be read or parsed is an
/// error, never "no session" (review 5!246: an empty fallback made every
/// live session's worker look idle to the sweeps). Logged once per process.
fn live_sessions() -> Result<LiveSessions, HiveError> {
    static WARNED: std::sync::Once = std::sync::Once::new();
    let registry = lean_ctx::core::agents::AgentRegistry::load_checked().map_err(|error| {
        WARNED.call_once(|| {
            tracing::warn!("hive: agent registry unreadable, no worker is retired: {error}");
        });
        HiveError::Local(format!(
            "agent registry unreadable ({error}): which workers are in use is unknown"
        ))
    })?;
    let mut live = LiveSessions::default();
    for agent in registry.list_active(None) {
        let key = agent_worker_key(agent);
        if agent
            .app_session_id
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty())
        {
            live.per_session.insert(key.clone());
        }
        live.in_use.insert(key);
    }
    Ok(live)
}

/// The `(host_app, role)` of the worker a local presence speaks as: the
/// host app from its transport or agent type, and its `ctx_agent` role
/// made per session when the launching app named one
/// ([`super::session_role`]). Two sessions of one role on one instance are
/// two agents on Hive; a subagent in the same session with the same role
/// is the same agent.
pub fn agent_worker_key(agent: &lean_ctx::core::agents::AgentEntry) -> (String, String) {
    let transport = agent.transport.as_ref();
    let host = detect_host_app(
        transport.map(|t| t.host.as_str()),
        Some(agent.agent_type.as_str()),
        std::iter::empty::<(&str, &str)>(),
        transport.and_then(|t| t.name.as_deref()),
    );
    let role = super::session_role(
        agent.role.as_deref().unwrap_or_default(),
        agent.app_session_id.as_deref(),
    );
    (host, role)
}

/// Attach a session of `host_app` with `role` to a durable worker of the
/// enrolled instance, adding it to the book when new. A full book (32,
/// as Hive allows per instance) gives up the longest-unused worker no
/// active session holds, so a new role gets its own worker instead of
/// sharing another one's.
pub fn attach(
    store: &HiveStore,
    instance_id: &str,
    host_app: &str,
    role: &str,
) -> Result<Attachment, HiveError> {
    let live = live_sessions().ok();
    let now_ms = u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap_or_default();
    update_whole_book(store, instance_id, |book| {
        attach_making_room(book, instance_id, host_app, role, live.as_ref(), now_ms)
    })
}

/// The worker of `(host_app, role)` if the book has one, without adding
/// or stamping anything (for sessions that have ended).
pub fn known(
    store: &HiveStore,
    instance_id: &str,
    host_app: &str,
    role: &str,
) -> Option<LocalWorker> {
    let role = normalize_role(role);
    read_book(store, instance_id)
        .ok()?
        .workers
        .into_iter()
        .find(|w| w.host_app == host_app && w.role == role)
}

/// Every durable worker of the enrolled instance.
pub fn list(store: &HiveStore, instance_id: &str) -> Result<Vec<LocalWorker>, HiveError> {
    Ok(read_book(store, instance_id)?.workers)
}

/// Workers evicted from the book that Hive still counts.
pub fn retired(store: &HiveStore, instance_id: &str) -> Result<Vec<String>, HiveError> {
    Ok(read_book(store, instance_id)?.retired)
}

/// Forget `worker_id` as pending retirement (Hive retired it, or never knew it).
pub fn retirement_done(
    store: &HiveStore,
    instance_id: &str,
    worker_id: &str,
) -> Result<(), HiveError> {
    update_whole_book(store, instance_id, |book| {
        book.retired.retain(|id| id != worker_id);
    })
}

/// When a session last attached to `worker` (ms), else when it was
/// registered; `None` when neither is known (a book from before either).
#[must_use]
pub fn last_seen_ms(worker: &LocalWorker) -> Option<u64> {
    worker.last_attached.or(worker.registered_at)
}

/// The workers that may retire (owner 09.11.: workers that are not active
/// do not count and are switched off): not the connector's own (host
/// [`CAPTURE_HOST`]), held by no active session in `in_use`, and last seen
/// before `cutoff_ms` (never seen: idle). With `session_only`, only the
/// workers recorded as one app session's ([`LocalWorker::per_session`]),
/// which end with it.
///
/// [`CAPTURE_HOST`]: crate::sync::place::CAPTURE_HOST
#[must_use]
pub fn idle_workers(
    workers: &[LocalWorker],
    in_use: &BTreeSet<(String, String)>,
    cutoff_ms: u64,
    session_only: bool,
) -> Vec<String> {
    workers
        .iter()
        .filter(|w| w.host_app != crate::sync::place::CAPTURE_HOST)
        .filter(|w| !in_use.contains(&(w.host_app.clone(), w.role.clone())))
        .filter(|w| !session_only || w.per_session)
        .filter(|w| last_seen_ms(w).is_none_or(|seen| seen < cutoff_ms))
        .map(|w| w.worker_id.clone())
        .collect()
}

/// Take `ids` out of the book; each one Hive may still count (registered,
/// or not in the book at all) is queued for retirement on Hive
/// ([`super::register::retire_evicted`]). The ids taken, in order.
fn give_up_in(book: &mut WorkerBook, ids: &[String]) -> Vec<String> {
    let mut taken = Vec::new();
    for id in ids {
        let registered = match book.workers.iter().position(|w| &w.worker_id == id) {
            Some(i) => book.workers.remove(i).registered_at.is_some(),
            None => crate::bus::address::is_worker_id(id),
        };
        if registered && !book.retired.contains(id) {
            book.retired.push(id.clone());
        }
        taken.push(id.clone());
    }
    taken
}

/// The ids in `ids` whose worker an active session in `in_use` holds: a
/// worker of the book under a `(host, role)` in use, or (for an id the
/// book does not have) the deterministic workspace id of a `(host, role)`
/// in use on this instance.
fn busy_ids(
    book: &WorkerBook,
    instance_id: &str,
    ids: &[String],
    in_use: &BTreeSet<(String, String)>,
) -> Vec<String> {
    let mut busy: BTreeSet<String> = in_use
        .iter()
        .map(|(host, role)| workspace_worker_id(instance_id, host, role))
        .collect();
    busy.extend(
        book.workers
            .iter()
            .filter(|w| in_use.contains(&(w.host_app.clone(), w.role.clone())))
            .map(|w| w.worker_id.clone()),
    );
    ids.iter()
        .filter(|id| busy.contains(*id))
        .cloned()
        .collect()
}

/// [`give_up_in`] on the book of `instance_id`, for an explicit retirement
/// (`lean-ctx hive worker retire <wkr_…>`). Refused when an active local
/// session holds one of `ids` (Hive would delete a worker that keeps
/// posting), or when the agent registry cannot be read to tell; `force`
/// skips that check.
pub fn give_up(
    store: &HiveStore,
    instance_id: &str,
    ids: &[String],
    force: bool,
) -> Result<Vec<String>, HiveError> {
    let in_use = if force {
        BTreeSet::new()
    } else {
        live_sessions()?.in_use
    };
    update_whole_book(store, instance_id, |book| {
        let busy = busy_ids(book, instance_id, ids, &in_use);
        if busy.is_empty() {
            Ok(give_up_in(book, ids))
        } else {
            Err(HiveError::Config(format!(
                "{} in use by an active session; end that session first, or retire with --force",
                busy.join(", ")
            )))
        }
    })?
}

/// Give up every worker idle for `idle` or longer ([`idle_workers`]),
/// under the book's lock. No request: Hive hears of them with
/// [`super::register::retire_evicted`] (the sync, the next registration).
/// Fails closed: when the agent registry cannot be read, nothing is given
/// up (which workers live sessions hold is unknown).
pub fn give_up_idle(
    store: &HiveStore,
    instance_id: &str,
    idle: std::time::Duration,
    session_only: bool,
) -> Result<Vec<String>, HiveError> {
    let in_use = live_sessions()?.in_use;
    let now_ms = u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap_or_default();
    let cutoff_ms = now_ms.saturating_sub(u64::try_from(idle.as_millis()).unwrap_or(u64::MAX));
    update_whole_book(store, instance_id, |book| {
        let ids = idle_workers(&book.workers, &in_use, cutoff_ms, session_only);
        give_up_in(book, &ids)
    })
}

/// Read-modify-write the book under its lock.
pub fn update_book<T>(
    store: &HiveStore,
    instance_id: &str,
    change: impl FnOnce(&mut Vec<LocalWorker>) -> T,
) -> Result<T, HiveError> {
    update_whole_book(store, instance_id, |book| change(&mut book.workers))
}

fn update_whole_book<T>(
    store: &HiveStore,
    instance_id: &str,
    change: impl FnOnce(&mut WorkerBook) -> T,
) -> Result<T, HiveError> {
    let dir = store.data_dir();
    let _lock = lock_state(dir, BOOK_FILE)?;
    let mut book = read_book(store, instance_id)?;
    let out = change(&mut book);
    let bytes = serde_json::to_vec(&book).map_err(|e| HiveError::Local(e.to_string()))?;
    write_private(dir, BOOK_FILE, &bytes)?;
    Ok(out)
}

/// The book of `instance_id`; a book of another (earlier) instance is
/// replaced, since its ids are bound to that instance. Books are keyed by
/// the canonical id, so a book from before the canonical form (whose ids
/// were derived from `inst_<hex>`) is replaced as well.
fn read_book(store: &HiveStore, instance_id: &str) -> Result<WorkerBook, HiveError> {
    let path = store.data_dir().join(BOOK_FILE);
    let book = load_or_quarantine::<WorkerBook>(&path, |b| b.schema == BOOK_SCHEMA)?;
    let canonical = super::canonical_instance_id(instance_id);
    Ok(book
        .filter(|b| b.instance_id == canonical)
        .unwrap_or_else(|| WorkerBook {
            schema: BOOK_SCHEMA,
            instance_id: canonical,
            workers: Vec::new(),
            retired: Vec::new(),
        }))
}

#[cfg(test)]
mod retire_tests {
    use super::*;

    fn worker(
        id: &str,
        host: &str,
        role: &str,
        seen: Option<u64>,
        registered: bool,
    ) -> LocalWorker {
        LocalWorker {
            worker_id: id.into(),
            host_app: host.into(),
            role: role.into(),
            name: None,
            registered_at: registered.then_some(1),
            aliases: BTreeMap::new(),
            last_attached: seen,
            per_session: false,
        }
    }

    #[test]
    fn idle_workers_spare_active_recent_and_connector_workers() {
        let workers = vec![
            worker("wkr_old", "claude", "reviewer", Some(100), true),
            worker("wkr_new", "claude", "builder", Some(5_000), true),
            worker("wkr_busy", "codex", "queen", Some(100), true),
            worker(
                "wkr_cap",
                crate::sync::place::CAPTURE_HOST,
                "capture",
                Some(100),
                true,
            ),
            worker("wkr_never", "cursor", "general", None, false),
        ];
        let in_use: BTreeSet<_> = [("codex".to_string(), "queen".to_string())].into();
        assert_eq!(
            idle_workers(&workers, &in_use, 1_000, false),
            vec!["wkr_old".to_string(), "wkr_never".to_string()]
        );
    }

    /// Review 5!246 #5: only workers recorded as an app session's are
    /// session workers; `dev-storybook` looks like `<role>-s<tag>` (base
    /// `dev`, tag `torybook`) but is a plain role and keeps the 24 h sweep.
    #[test]
    fn session_only_takes_the_workers_of_ended_app_sessions() {
        let per_session = super::super::session_role("worker", Some("app-session-1"));
        let mut tagged = worker("wkr_a", "claude", &per_session, Some(5_000), true);
        tagged.per_session = true;
        let workers = vec![
            tagged,
            worker("wkr_b", "claude", "worker", Some(100), true),
            worker("wkr_c", "claude", "dev-storybook", Some(100), true),
        ];
        let none = BTreeSet::new();
        assert_eq!(
            idle_workers(&workers, &none, u64::MAX, true),
            vec!["wkr_a".to_string()]
        );
    }

    /// Review 5!246 #5: attaching marks the worker of a live session with
    /// an app session id, and only that one.
    #[test]
    fn attaching_records_which_workers_are_per_session() {
        let tagged = super::super::session_role("worker", Some("app-session-1"));
        let live = LiveSessions {
            in_use: [("claude".to_string(), tagged.clone())].into(),
            per_session: [("claude".to_string(), tagged.clone())].into(),
        };
        let mut book = WorkerBook {
            schema: BOOK_SCHEMA,
            instance_id: "i".into(),
            workers: Vec::new(),
            retired: Vec::new(),
        };
        let a = attach_making_room(&mut book, "i", "claude", &tagged, Some(&live), 1);
        assert!(a.worker.per_session);
        let b = attach_making_room(&mut book, "i", "claude", "dev-storybook", Some(&live), 1);
        assert!(!b.worker.per_session);
        let c = attach_making_room(&mut book, "i", "codex", "x", None, 1);
        assert!(
            !c.worker.per_session,
            "an unreadable registry marks nothing"
        );
    }

    /// Review 5!246 #1: an id a live session holds is busy, whether the
    /// book has it or not (worker ids are deterministic per host and role).
    #[test]
    fn busy_ids_cover_book_workers_and_unknown_ids_in_use() {
        let book = WorkerBook {
            schema: BOOK_SCHEMA,
            instance_id: "i".into(),
            workers: vec![
                worker("wkr_queen", "codex", "queen", Some(1), true),
                worker("wkr_idle", "codex", "idle", Some(1), true),
            ],
            retired: Vec::new(),
        };
        let in_use: BTreeSet<_> = [
            ("codex".to_string(), "queen".to_string()),
            ("claude-code".to_string(), "ops".to_string()),
        ]
        .into();
        let unbooked = workspace_worker_id("i", "claude-code", "ops");
        let stranger = format!("wkr_{}", "a".repeat(20));
        let ids = vec![
            "wkr_queen".to_string(),
            "wkr_idle".to_string(),
            unbooked.clone(),
            stranger,
        ];
        assert_eq!(
            busy_ids(&book, "i", &ids, &in_use),
            vec!["wkr_queen".to_string(), unbooked]
        );
    }

    #[test]
    fn giving_up_queues_what_hive_counts_and_drops_the_rest() {
        let mut book = WorkerBook {
            schema: BOOK_SCHEMA,
            instance_id: "i".into(),
            workers: vec![
                worker("wkr_registered", "claude", "a", Some(1), true),
                worker("wkr_local", "claude", "b", Some(1), false),
                worker("wkr_kept", "claude", "c", Some(1), true),
            ],
            retired: Vec::new(),
        };
        let unknown = format!("wkr_{}", "a".repeat(20));
        let ids = vec![
            "wkr_registered".to_string(),
            "wkr_local".to_string(),
            unknown.clone(),
        ];
        assert_eq!(give_up_in(&mut book, &ids), ids);
        assert_eq!(book.workers.len(), 1);
        assert_eq!(book.workers[0].worker_id, "wkr_kept");
        // A worker Hive may know though the book does not is retired there
        // too; a local, never registered one only leaves the book.
        assert_eq!(book.retired, vec!["wkr_registered".to_string(), unknown]);
    }
}
