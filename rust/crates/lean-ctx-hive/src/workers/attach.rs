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
/// stamps the worker it attaches with `now_ms`.
fn attach_making_room(
    book: &mut WorkerBook,
    instance_id: &str,
    host_app: &str,
    role: &str,
    in_use: &BTreeSet<(String, String)>,
    now_ms: u64,
) -> Attachment {
    let role = normalize_role(role);
    let known = book
        .workers
        .iter()
        .any(|w| w.host_app == host_app && w.role == role);
    if !known && book.workers.len() >= WORKER_CAP {
        let mut in_use = in_use.clone();
        in_use.insert((host_app.to_string(), role.clone()));
        evict_one(&mut book.workers, &mut book.retired, &in_use);
    }
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
            Attachment {
                worker: worker.clone(),
                cap_reached: attached.cap_reached,
            }
        }
        None => attached,
    }
}

/// The `(host_app, role)` of every active local session: the workers that
/// must stay in the book.
fn workers_in_use() -> BTreeSet<(String, String)> {
    let registry = lean_ctx::core::agents::AgentRegistry::load_or_create();
    registry
        .list_active(None)
        .into_iter()
        .map(|agent| {
            let transport = agent.transport.as_ref();
            let host = detect_host_app(
                transport.map(|t| t.host.as_str()),
                Some(agent.agent_type.as_str()),
                std::iter::empty::<(&str, &str)>(),
                transport.and_then(|t| t.name.as_deref()),
            );
            (
                host,
                normalize_role(agent.role.as_deref().unwrap_or_default()),
            )
        })
        .collect()
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
    let in_use = workers_in_use();
    let now_ms = u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap_or_default();
    update_whole_book(store, instance_id, |book| {
        attach_making_room(book, instance_id, host_app, role, &in_use, now_ms)
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
