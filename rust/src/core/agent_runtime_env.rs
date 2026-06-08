//! Bridge agent runtime/session environment variables across lean-ctx processes.
//!
//! The lean-ctx MCP server is a long-lived child of the agent host. Some agents
//! (notably Codex) expose runtime/session variables such as `CODEX_THREAD_ID`
//! only in the *native agent shell* environment, not in the MCP server process
//! (#370). `ctx_shell` runs inside the MCP server, so it cannot forward those
//! variables by reading its own `std::env`.
//!
//! Short-lived lean-ctx processes that *do* run inside the agent environment —
//! the hook handlers (`lean-ctx hook …`) and the `lean-ctx -c` shell wrapper —
//! [`capture`] the relevant variables into a small file in the data dir. The MCP
//! server then [`load`]s them when constructing the child environment for
//! `ctx_shell` (see `crate::server::execute`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Env var name prefixes identifying agent runtime/session state worth forwarding
/// to `ctx_shell` child processes.
pub const FORWARD_PREFIXES: &[&str] = &["CODEX_", "CLAUDE_", "OPENCODE_", "HERMES_", "GEMINI_"];

const FILE_NAME: &str = "agent_runtime_env.json";

/// Captured variables older than this are ignored: a stale session/thread id is
/// worse than forwarding none, and a fresh session re-captures on its first hook.
const TTL_SECS: u64 = 7_200;

/// Whether `key` is an agent runtime variable lean-ctx forwards to child shells.
#[must_use]
pub fn is_forwardable(key: &str) -> bool {
    FORWARD_PREFIXES
        .iter()
        .any(|prefix| key.starts_with(prefix))
}

fn store_path() -> Option<PathBuf> {
    crate::core::data_dir::lean_ctx_data_dir()
        .ok()
        .map(|dir| dir.join(FILE_NAME))
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Forwardable variables present in the current process environment.
#[must_use]
pub fn collect_from_process() -> BTreeMap<String, String> {
    std::env::vars()
        .filter(|(key, _)| is_forwardable(key))
        .collect()
}

fn read_store(path: &Path) -> Option<(BTreeMap<String, String>, u64)> {
    let content = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&content).ok()?;
    let captured_at = value
        .get("captured_at")
        .and_then(serde_json::Value::as_u64)?;
    let vars = value
        .get("vars")
        .and_then(serde_json::Value::as_object)?
        .iter()
        .filter_map(|(key, val)| val.as_str().map(|s| (key.clone(), s.to_string())))
        .collect();
    Some((vars, captured_at))
}

/// Capture forwardable variables from the current (agent) environment into the
/// data dir so the MCP server can forward them to `ctx_shell` children.
///
/// No-op when the current environment carries no forwardable variables — this
/// prevents a process with a stripped environment (e.g. the MCP server itself)
/// from clobbering a good capture. The file is only rewritten when the variable
/// set actually changes, keeping the cost of capturing on every shell command low.
pub fn capture() {
    let vars = collect_from_process();
    if vars.is_empty() {
        return;
    }
    let Some(path) = store_path() else {
        return;
    };
    if let Some((existing, _)) = read_store(&path) {
        if existing == vars {
            return;
        }
    }
    let payload = serde_json::json!({ "vars": vars, "captured_at": now_secs() });
    let Ok(json) = serde_json::to_string_pretty(&payload) else {
        return;
    };
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, &json).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// Load captured agent runtime variables, honoring the freshness TTL.
///
/// Returns an empty map when no capture exists or it has expired.
#[must_use]
pub fn load() -> BTreeMap<String, String> {
    let Some(path) = store_path() else {
        return BTreeMap::new();
    };
    let Some((vars, captured_at)) = read_store(&path) else {
        return BTreeMap::new();
    };
    if now_secs().saturating_sub(captured_at) > TTL_SECS {
        return BTreeMap::new();
    }
    vars
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_forwardable_matches_known_prefixes() {
        assert!(is_forwardable("CODEX_THREAD_ID"));
        assert!(is_forwardable("CLAUDE_SESSION"));
        assert!(is_forwardable("OPENCODE_FOO"));
        assert!(!is_forwardable("PATH"));
        assert!(!is_forwardable("HOME"));
        assert!(!is_forwardable("LEAN_CTX_DATA_DIR"));
    }

    #[test]
    fn capture_then_load_roundtrips() {
        let _lock = crate::core::data_dir::test_env_lock();
        let dir = std::env::temp_dir().join("lean_ctx_runtime_env_roundtrip");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("LEAN_CTX_DATA_DIR", &dir);
        std::env::set_var("CODEX_THREAD_ID", "thread-roundtrip");

        capture();
        let loaded = load();

        std::env::remove_var("CODEX_THREAD_ID");
        std::env::remove_var("LEAN_CTX_DATA_DIR");
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(
            loaded.get("CODEX_THREAD_ID").map(String::as_str),
            Some("thread-roundtrip")
        );
    }

    #[test]
    fn capture_is_noop_without_forwardable_vars() {
        let _lock = crate::core::data_dir::test_env_lock();
        let dir = std::env::temp_dir().join("lean_ctx_runtime_env_noop");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("LEAN_CTX_DATA_DIR", &dir);
        // Ensure no forwardable vars leak in from the host test environment.
        for (key, _) in collect_from_process() {
            std::env::remove_var(key);
        }

        capture();
        let exists = dir.join(FILE_NAME).exists();

        std::env::remove_var("LEAN_CTX_DATA_DIR");
        let _ = std::fs::remove_dir_all(&dir);

        assert!(!exists, "capture must not write a store with no vars");
    }

    #[test]
    fn load_ignores_expired_capture() {
        let _lock = crate::core::data_dir::test_env_lock();
        let dir = std::env::temp_dir().join("lean_ctx_runtime_env_expired");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("LEAN_CTX_DATA_DIR", &dir);

        let stale = now_secs().saturating_sub(TTL_SECS + 60);
        let payload =
            serde_json::json!({ "vars": { "CODEX_THREAD_ID": "old" }, "captured_at": stale });
        std::fs::write(
            dir.join(FILE_NAME),
            serde_json::to_string_pretty(&payload).unwrap(),
        )
        .unwrap();

        let loaded = load();

        std::env::remove_var("LEAN_CTX_DATA_DIR");
        let _ = std::fs::remove_dir_all(&dir);

        assert!(loaded.is_empty(), "expired capture must not be loaded");
    }
}
