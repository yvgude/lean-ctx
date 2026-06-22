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
pub const FORWARD_PREFIXES: &[&str] = &[
    "CODEX_",
    "CLAUDE_",
    "CODEBUDDY_",
    "OPENCODE_",
    "HERMES_",
    "GEMINI_",
];

/// Case-insensitive name substrings that mark an env var as credential-shaped.
///
/// A variable whose name contains any of these is NEVER forwarded to `ctx_shell`
/// children — and never captured to disk — even when it matches a forwardable
/// prefix. Forwarding API keys / tokens / passwords into every child shell is an
/// exfiltration risk that output redaction cannot stop: a `curl … -d "$KEY"`
/// child never prints the secret to stdout, so the redactor never sees it (GH
/// security audit, finding 2). Only non-secret session/thread identifiers
/// (`*_THREAD_ID`, `*_SESSION`, …) should cross the bridge.
const CREDENTIAL_MARKERS: &[&str] = &[
    "_KEY",       // *_API_KEY, *_ACCESS_KEY, *_PRIVATE_KEY, *_SECRET_KEY
    "APIKEY",     // unseparated spelling
    "SECRET",     // *_SECRET, *_CLIENT_SECRET
    "TOKEN",      // *_TOKEN, *_ACCESS_TOKEN, *_REFRESH_TOKEN
    "PASSWORD",   // *_PASSWORD, *_SERVER_PASSWORD
    "PASSWD",     // unseparated spelling
    "CREDENTIAL", // *_CREDENTIAL(S)
    "AUTH",       // *_AUTH, *_OAUTH, *_AUTHORIZATION
];

/// Whether `key` looks like a secret/credential that must never be forwarded or
/// persisted, regardless of any matching forwardable prefix.
#[must_use]
pub fn is_credential_shaped(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    CREDENTIAL_MARKERS
        .iter()
        .any(|marker| upper.contains(marker))
}

const FILE_NAME: &str = "agent_runtime_env.json";

/// Captured variables older than this are ignored: a stale session/thread id is
/// worse than forwarding none, and a fresh session re-captures on its first hook.
const TTL_SECS: u64 = 7_200;

/// Whether `key` is an agent runtime variable lean-ctx forwards to child shells.
///
/// A variable qualifies only when it (1) matches a forwardable agent prefix AND
/// (2) is not credential-shaped — session/thread identifiers cross the bridge,
/// secrets never do (GH security audit, finding 2).
#[must_use]
pub fn is_forwardable(key: &str) -> bool {
    FORWARD_PREFIXES
        .iter()
        .any(|prefix| key.starts_with(prefix))
        && !is_credential_shaped(key)
}

/// Canonical capture-file location: the STATE dir (GH #408 / GL #605).
///
/// This file holds captured agent **session/thread identifiers** (e.g.
/// `CODEX_THREAD_ID`); credential-shaped vars are filtered out by
/// [`is_forwardable`] and never written here (GH security audit, finding 2). It
/// still lives in the RW state category — never the RO/shareable config dir —
/// and is always written `0o600` as defence-in-depth.
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
    if let Some((existing, _)) = read_store(&path)
        && existing == vars
    {
        return;
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
    // Defense-in-depth: a capture written by a build predating finding 2 may
    // still hold credential-shaped vars. Drop them from the returned set, and if
    // any were present rewrite (or delete) the file so the plaintext secret does
    // not linger at rest — not just out of the forwarded env.
    let cleaned: BTreeMap<String, String> = vars
        .iter()
        .filter(|(key, _)| is_forwardable(key))
        .map(|(key, val)| (key.clone(), val.clone()))
        .collect();
    if cleaned.len() != vars.len() {
        scrub_store(&path, &cleaned, captured_at);
    }
    cleaned
}

/// Rewrite the capture file with `vars` (preserving `captured_at`), or remove it
/// entirely when nothing forwardable remains. Retroactively strips
/// credential-shaped vars from captures written by older builds (finding 2).
fn scrub_store(path: &Path, vars: &BTreeMap<String, String>, captured_at: u64) {
    if vars.is_empty() {
        let _ = std::fs::remove_file(path);
        return;
    }
    let payload = serde_json::json!({ "vars": vars, "captured_at": captured_at });
    if let Ok(json) = serde_json::to_string_pretty(&payload) {
        let _ = crate::config_io::write_atomic(path, &json);
    }
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
        crate::test_env::set_var("CODEX_THREAD_ID", "thread-roundtrip");

        capture();
        let loaded = load();

        crate::test_env::remove_var("CODEX_THREAD_ID");

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
            crate::test_env::remove_var(key);
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
            crate::test_env::remove_var(key);
        }
        // A forwardable (non-credential) var so a file is actually written; it
        // can still hold session ids, so owner-only perms remain a requirement.
        crate::test_env::set_var("CODEX_THREAD_ID", "session-id");

        capture();
        let path = store_path().unwrap();
        crate::test_env::remove_var("CODEX_THREAD_ID");

        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "captured runtime-env file must be owner-only"
        );
    }

    // Finding 2 (GH security audit): credential-shaped vars must never be
    // forwarded, even when they match a forwardable agent prefix.
    #[test]
    fn is_forwardable_rejects_credential_shaped_vars() {
        // Session/thread identifiers cross the bridge.
        assert!(is_forwardable("CODEX_THREAD_ID"));
        assert!(is_forwardable("CLAUDE_SESSION_ID"));
        assert!(is_forwardable("OPENCODE_SESSION"));
        // Secrets matching a forwardable prefix do NOT.
        assert!(!is_forwardable("GEMINI_API_KEY"));
        assert!(!is_forwardable("OPENCODE_API_KEY"));
        assert!(!is_forwardable("OPENCODE_SERVER_PASSWORD"));
        assert!(!is_forwardable("CLAUDE_CODE_OAUTH_TOKEN"));
        assert!(!is_forwardable("CODEX_ACCESS_TOKEN"));
        assert!(!is_forwardable("CODEX_CLIENT_SECRET"));
        assert!(!is_forwardable("CODEX_PRIVATE_KEY"));
        assert!(!is_forwardable("GEMINI_CREDENTIALS"));
    }

    #[test]
    fn capture_excludes_credentials() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        for (key, _) in collect_from_process() {
            crate::test_env::remove_var(key);
        }
        crate::test_env::set_var("CODEX_THREAD_ID", "thread-keep");
        crate::test_env::set_var("GEMINI_API_KEY", "secret-drop");

        capture();
        let loaded = load();

        crate::test_env::remove_var("CODEX_THREAD_ID");
        crate::test_env::remove_var("GEMINI_API_KEY");

        assert_eq!(
            loaded.get("CODEX_THREAD_ID").map(String::as_str),
            Some("thread-keep")
        );
        assert!(
            !loaded.contains_key("GEMINI_API_KEY"),
            "API key must never be captured or forwarded"
        );
    }

    #[test]
    fn load_scrubs_legacy_credentials_from_disk() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let path = store_path().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        // Simulate a capture written by an older build: holds a real secret.
        let payload = serde_json::json!({
            "vars": { "CODEX_THREAD_ID": "t", "OPENCODE_SERVER_PASSWORD": "p4ssw0rd" },
            "captured_at": now_secs()
        });
        std::fs::write(&path, serde_json::to_string_pretty(&payload).unwrap()).unwrap();

        let loaded = load();

        assert!(
            !loaded.contains_key("OPENCODE_SERVER_PASSWORD"),
            "legacy credential must not be loaded"
        );
        assert_eq!(loaded.get("CODEX_THREAD_ID").map(String::as_str), Some("t"));

        // The plaintext secret must be scrubbed from disk, not just the env.
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(
            !on_disk.contains("OPENCODE_SERVER_PASSWORD") && !on_disk.contains("p4ssw0rd"),
            "secret must be removed from the capture file at rest: {on_disk}"
        );
    }

    #[test]
    fn store_path_is_under_state_dir_not_config_dir_when_split() {
        let _lock = crate::core::data_dir::test_env_lock();
        let state = tempfile::tempdir().unwrap();
        let config = tempfile::tempdir().unwrap();
        crate::test_env::set_var("LEAN_CTX_STATE_DIR", state.path());
        crate::test_env::set_var("LEAN_CTX_CONFIG_DIR", config.path());

        let path = store_path().unwrap();

        crate::test_env::remove_var("LEAN_CTX_STATE_DIR");
        crate::test_env::remove_var("LEAN_CTX_CONFIG_DIR");

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
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", data.path());
        crate::test_env::set_var("LEAN_CTX_STATE_DIR", state.path());

        let legacy = data.path().join(FILE_NAME);
        let payload =
            serde_json::json!({ "vars": { "GEMINI_API_KEY": "k" }, "captured_at": now_secs() });
        std::fs::write(&legacy, serde_json::to_string_pretty(&payload).unwrap()).unwrap();

        let state_path = store_path().unwrap();
        migrate_legacy_key_file(&state_path);

        let legacy_exists = legacy.exists();
        let migrated = state_path.exists();
        let parent_ok = state_path.parent() == Some(state.path());

        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
        crate::test_env::remove_var("LEAN_CTX_STATE_DIR");

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
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", data.path());
        crate::test_env::set_var("LEAN_CTX_STATE_DIR", state.path());

        let legacy = data.path().join(FILE_NAME);
        std::fs::write(&legacy, "{}").unwrap();
        let state_path = store_path().unwrap();
        std::fs::write(&state_path, "{}").unwrap();

        migrate_legacy_key_file(&state_path);

        let legacy_exists = legacy.exists();
        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
        crate::test_env::remove_var("LEAN_CTX_STATE_DIR");

        assert!(
            !legacy_exists,
            "stale legacy key file must be removed when a state copy exists"
        );
    }
}
