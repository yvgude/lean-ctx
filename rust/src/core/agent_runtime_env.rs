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
pub const FORWARD_PREFIXES: &[&str] = &["CODEX_", "CLAUDE_", "CODEBUDDY_", "OPENCODE_", "HERMES_", "GEMINI_"];

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

/// Canonical key-file location: the STATE dir (GH #408 / GL #605).
///
/// This file holds captured API keys (`GEMINI_API_KEY`, `OPENCODE_API_KEY`, …),
/// so it must live in the RW state category — never in the RO/shareable config
/// dir — and is always written `0o600`.
fn store_path() -> Option<PathBuf> {
    crate::core::paths::state_dir()
        .ok()
        .map(|dir| dir.join(FILE_NAME))
}

/// Legacy location: the file historically lived in the (config-shaped) data dir.
/// Used only to migrate existing captures into [`store_path`].
fn legacy_store_path() -> Option<PathBuf> {
    crate::core::data_dir::lean_ctx_data_dir()
        .ok()
        .map(|dir| dir.join(FILE_NAME))
}

/// Relocate a pre-#408 key file from the data dir into the state dir, and never
/// leave captured keys behind in the config-shaped legacy location.
///
/// Idempotent and a no-op in single-dir mode (where state == legacy). Safe to
/// call on every access: the existence checks make the steady state one `stat`.
fn migrate_legacy_key_file(state_path: &Path) {
    let Some(legacy) = legacy_store_path() else {
        return;
    };
    if legacy == *state_path || !legacy.exists() {
        return;
    }
    if state_path.exists() {
        // A state-dir copy already exists; drop the stale legacy file so keys
        // never linger in the config-shaped data dir.
        let _ = std::fs::remove_file(&legacy);
        return;
    }
    if let Some(parent) = state_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if std::fs::rename(&legacy, state_path).is_err() {
        // Cross-filesystem move: copy then remove the original.
        if std::fs::copy(&legacy, state_path).is_ok() {
            let _ = std::fs::remove_file(&legacy);
        } else {
            return;
        }
    }
    restrict_key_file_permissions(state_path);
}

#[cfg(unix)]
fn restrict_key_file_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict_key_file_permissions(_path: &Path) {}

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
    migrate_legacy_key_file(&path);
    if let Some((existing, _)) = read_store(&path) {
        if existing == vars {
            return;
        }
    }
    let payload = serde_json::json!({ "vars": vars, "captured_at": now_secs() });
    let Ok(json) = serde_json::to_string_pretty(&payload) else {
        return;
    };
    // Atomic write + `0o600` (owner-only): captured keys must never be
    // group/world-readable. `write_atomic` also rejects symlinks and creates
    // the state dir if needed.
    let _ = crate::config_io::write_atomic(&path, &json);
}

/// Load captured agent runtime variables, honoring the freshness TTL.
///
/// Returns an empty map when no capture exists or it has expired.
#[must_use]
pub fn load() -> BTreeMap<String, String> {
    let Some(path) = store_path() else {
        return BTreeMap::new();
    };
    migrate_legacy_key_file(&path);
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
        let _iso = crate::core::data_dir::isolated_data_dir();
        std::env::set_var("CODEX_THREAD_ID", "thread-roundtrip");

        capture();
        let loaded = load();

        std::env::remove_var("CODEX_THREAD_ID");

        assert_eq!(
            loaded.get("CODEX_THREAD_ID").map(String::as_str),
            Some("thread-roundtrip")
        );
    }

    #[test]
    fn capture_is_noop_without_forwardable_vars() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        // Ensure no forwardable vars leak in from the host test environment.
        for (key, _) in collect_from_process() {
            std::env::remove_var(key);
        }

        capture();
        let exists = store_path().is_some_and(|p| p.exists());

        assert!(!exists, "capture must not write a store with no vars");
    }

    #[test]
    fn load_ignores_expired_capture() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let path = store_path().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        let stale = now_secs().saturating_sub(TTL_SECS + 60);
        let payload =
            serde_json::json!({ "vars": { "CODEX_THREAD_ID": "old" }, "captured_at": stale });
        std::fs::write(&path, serde_json::to_string_pretty(&payload).unwrap()).unwrap();

        let loaded = load();

        assert!(loaded.is_empty(), "expired capture must not be loaded");
    }

    #[cfg(unix)]
    #[test]
    fn capture_sets_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let _iso = crate::core::data_dir::isolated_data_dir();
        for (key, _) in collect_from_process() {
            std::env::remove_var(key);
        }
        std::env::set_var("GEMINI_API_KEY", "secret-token");

        capture();
        let path = store_path().unwrap();
        std::env::remove_var("GEMINI_API_KEY");

        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "captured key file must be owner-only");
    }

    #[test]
    fn store_path_is_under_state_dir_not_config_dir_when_split() {
        let _lock = crate::core::data_dir::test_env_lock();
        let state = tempfile::tempdir().unwrap();
        let config = tempfile::tempdir().unwrap();
        std::env::set_var("LEAN_CTX_STATE_DIR", state.path());
        std::env::set_var("LEAN_CTX_CONFIG_DIR", config.path());

        let path = store_path().unwrap();

        std::env::remove_var("LEAN_CTX_STATE_DIR");
        std::env::remove_var("LEAN_CTX_CONFIG_DIR");

        assert!(
            path.starts_with(state.path()),
            "key file must live under the state dir: {}",
            path.display()
        );
        assert!(
            !path.starts_with(config.path()),
            "key file must never resolve under the config dir"
        );
    }

    #[test]
    fn migrates_legacy_key_file_to_state_dir() {
        let _lock = crate::core::data_dir::test_env_lock();
        let data = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        std::env::set_var("LEAN_CTX_DATA_DIR", data.path());
        std::env::set_var("LEAN_CTX_STATE_DIR", state.path());

        let legacy = data.path().join(FILE_NAME);
        let payload =
            serde_json::json!({ "vars": { "GEMINI_API_KEY": "k" }, "captured_at": now_secs() });
        std::fs::write(&legacy, serde_json::to_string_pretty(&payload).unwrap()).unwrap();

        let state_path = store_path().unwrap();
        migrate_legacy_key_file(&state_path);

        let legacy_exists = legacy.exists();
        let migrated = state_path.exists();
        let parent_ok = state_path.parent() == Some(state.path());

        std::env::remove_var("LEAN_CTX_DATA_DIR");
        std::env::remove_var("LEAN_CTX_STATE_DIR");

        assert!(migrated, "key file must be moved into the state dir");
        assert!(!legacy_exists, "legacy key file must be removed after move");
        assert!(
            parent_ok,
            "migrated file must sit directly in the state dir"
        );
    }

    #[test]
    fn removes_stale_legacy_when_state_copy_exists() {
        let _lock = crate::core::data_dir::test_env_lock();
        let data = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        std::env::set_var("LEAN_CTX_DATA_DIR", data.path());
        std::env::set_var("LEAN_CTX_STATE_DIR", state.path());

        let legacy = data.path().join(FILE_NAME);
        std::fs::write(&legacy, "{}").unwrap();
        let state_path = store_path().unwrap();
        std::fs::write(&state_path, "{}").unwrap();

        migrate_legacy_key_file(&state_path);

        let legacy_exists = legacy.exists();
        std::env::remove_var("LEAN_CTX_DATA_DIR");
        std::env::remove_var("LEAN_CTX_STATE_DIR");

        assert!(
            !legacy_exists,
            "stale legacy key file must be removed when a state copy exists"
        );
    }
}
