// SPDX-License-Identifier: Apache-2.0

//! The governor setup on hosts other than Claude Code (owner decisions
//! 2026-11-01): every host with tool hooks gets native tools, LeanCTX's hooks,
//! a short rules block in its one rules channel and **no** LeanCTX MCP server.
//!
//! Two classes, by what the host's hooks can do (evidence in
//! `docs/reference/governor-hosts.md`):
//!
//! - [`GovernorClass::Full`] — a PreToolUse hook that can rewrite or deny a
//!   shell command **and** a PostToolUse hook that can replace the newest tool
//!   result: `hook rewrite-enforce` + `hook govern`, as on Claude Code.
//! - [`GovernorClass::EnforceOnly`] — a pre-tool hook only: the allowlist
//!   enforcement and the secret-path deny, never a grant ("mirror, don't
//!   grant"); tool results stay native.
//!
//! Hosts without hooks keep today's MCP setup ([`governor_available`] is
//! `None`). Each host lives in its own `<host>_governor` module and exposes a
//! [`GovernorHostSpec`] with the shape the update migration needs: detection,
//! the files a switch touches (backups), idempotent install steps, a revert
//! and the one command that switches back.

use std::path::{Path, PathBuf};

/// What a host's hooks let the governor do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GovernorClass {
    /// Pre-tool enforcement and post-tool result replacement.
    Full,
    /// Pre-tool enforcement only; results stay native.
    EnforceOnly,
}

impl GovernorClass {
    pub fn label(self) -> &'static str {
        match self {
            Self::Full => "governor",
            Self::EnforceOnly => "governor (enforcement only)",
        }
    }
}

/// One host's governor setup.
pub struct GovernorHostSpec {
    /// Agent key as `init --agent` spells it.
    pub key: &'static str,
    pub name: &'static str,
    pub class: GovernorClass,
    pub is_installed: fn(&Path) -> bool,
    /// Any LeanCTX artifact of the standard setup (hooks or MCP entry).
    pub has_standard_setup: fn(&Path) -> bool,
    /// Host files the install steps may rewrite (to back up first).
    pub touched_files: fn(&Path) -> Vec<PathBuf>,
    /// Idempotent install steps, in order.
    pub steps: &'static [(&'static str, fn(&Path))],
    /// Remove the governor's own artifacts; `true` when one was installed.
    pub revert: fn(&Path) -> bool,
    /// Re-render the hooks (binary path moves), keeping the setup.
    pub refresh: fn(&Path),
    /// The MCP config file the governor keeps free of the LeanCTX server.
    pub mcp_config_path: fn(&Path) -> PathBuf,
    /// The one command that switches back to the standard setup.
    pub switch_back: &'static str,
}

/// The per-host capability switch: `Some` where a governor setup exists.
/// Hosts whose hook dialect is unverified stay on today's setup (`None`).
pub fn governor_available(agent: &str) -> Option<&'static GovernorHostSpec> {
    match agent.trim().to_ascii_lowercase().as_str() {
        "codebuddy" => Some(&super::codebuddy_governor::SPEC),
        "cursor" => Some(&super::cursor_governor::SPEC),
        "gemini" => Some(&super::gemini_governor::SPEC),
        "codex" => Some(&super::codex_governor::SPEC),
        "copilot" => Some(&super::copilot_governor::SPEC),
        _ => None,
    }
}

/// Detection lives in core (`core::governor_hosts`) so the MCP writer and the
/// rules injection need no installer code; every spec's `is_installed` and
/// `mcp_config_path` point there.
pub(crate) use lean_ctx::core::governor_hosts::is_installed;

/// Run every install step for `spec`. Idempotent.
pub fn install(spec: &GovernorHostSpec, home: &Path) {
    for (_, step) in spec.steps {
        step(home);
    }
}

/// Re-render the governor hooks when `agent` has the governor setup.
/// Returns whether it did (the caller then skips the standard installer).
pub(crate) fn refresh_if_installed(agent: &str, home: &Path) -> bool {
    match governor_available(agent) {
        Some(spec) if (spec.is_installed)(home) => {
            (spec.refresh)(home);
            true
        }
        _ => false,
    }
}

// --- Rules block -----------------------------------------------------------

/// Sub-marker inside the managed block: tells the governor block apart from
/// the standard rules so a revert removes only its own text.
pub(crate) const GOVERNOR_MARKER: &str = "<!-- lean-ctx-governor-v1 -->";

const LINE_FULL: &str = "lean-ctx keeps native Read/Bash results lossless; an identical repeat may show `[unchanged …]` (the full result is earlier in this conversation).";
const LINE_ENFORCE: &str = "lean-ctx checks native shell commands against its allowlist; tool results stay native and unchanged.";
const LINE_COMPOSE: &str = "Ranked files with source for a task: `lean-ctx call ctx_compose --project-root \"$PWD\" --json '{\"task\":\"…\"}'`.";

/// The rules text between the host's managed markers (≤ 3 lines).
pub(crate) fn rules_body(class: GovernorClass) -> String {
    let line = match class {
        GovernorClass::Full => LINE_FULL,
        GovernorClass::EnforceOnly => LINE_ENFORCE,
    };
    format!("{GOVERNOR_MARKER}\n{line}\n{LINE_COMPOSE}")
}

/// The complete managed block with the host channel's markers.
pub(crate) fn rules_block(class: GovernorClass, start: &str, end: &str) -> String {
    format!("{start}\n{}\n{end}", rules_body(class))
}

/// Replace every managed block in `path` with the governor block (the
/// single-channel invariant: one LeanCTX block per rules file).
pub(super) fn upsert_rules_block(path: &Path, start: &str, end: &str, class: GovernorClass) {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let cleaned = super::shared::remove_managed_md_blocks(&existing, start, end);
    let cleaned = cleaned.trim();
    let block = rules_block(class, start, end);
    let updated = if cleaned.is_empty() {
        format!("{block}\n")
    } else {
        format!("{cleaned}\n\n{block}\n")
    };
    if updated != existing {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        super::super::write_file(path, &updated);
    }
}

/// Remove the governor block (only when it is ours) from `path`.
pub(super) fn remove_rules_block(path: &Path, start: &str, end: &str) {
    let Ok(existing) = std::fs::read_to_string(path) else {
        return;
    };
    if !existing.contains(GOVERNOR_MARKER) {
        return;
    }
    let cleaned = super::shared::remove_managed_md_blocks(&existing, start, end);
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        let _ = std::fs::remove_file(path);
    } else {
        super::super::write_file(path, &format!("{cleaned}\n"));
    }
}

// --- JSON helpers ----------------------------------------------------------

pub(super) use lean_ctx::core::governor_hosts::{has_group_hook, load_json};

/// Parse a config the install must not clobber: `None` when it exists but
/// is not a JSON object (the step then leaves it untouched).
pub(super) fn load_json_strict(path: &Path) -> Option<serde_json::Value> {
    match std::fs::read_to_string(path) {
        Ok(c) if !c.trim().is_empty() => lean_ctx::core::jsonc::parse_jsonc(&c)
            .ok()
            .filter(serde_json::Value::is_object),
        _ => Some(serde_json::json!({})),
    }
}

pub(super) fn save_if_changed(path: &Path, before: &serde_json::Value, value: &serde_json::Value) {
    if before == value {
        return;
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    super::super::write_file(
        path,
        &serde_json::to_string_pretty(value).unwrap_or_default(),
    );
}

/// `root.hooks` as an object (created when missing or malformed).
pub(super) fn hooks_object(
    root: &mut serde_json::Value,
) -> Option<&mut serde_json::Map<String, serde_json::Value>> {
    let obj = root.as_object_mut()?;
    let hooks = obj
        .entry("hooks".to_string())
        .or_insert_with(|| serde_json::json!({}));
    if !hooks.is_object() {
        *hooks = serde_json::json!({});
    }
    hooks.as_object_mut()
}

/// `hooks[event]` as an array (created when missing or malformed).
pub(super) fn event_array<'a>(
    hooks: &'a mut serde_json::Map<String, serde_json::Value>,
    event: &str,
) -> &'a mut Vec<serde_json::Value> {
    let value = hooks
        .entry(event.to_string())
        .or_insert_with(|| serde_json::json!([]));
    if !value.is_array() {
        *value = serde_json::json!([]);
    }
    match value {
        serde_json::Value::Array(arr) => arr,
        _ => unreachable!("normalized to an array above"),
    }
}

/// Drop empty event arrays.
pub(super) fn prune_empty_events(hooks: &mut serde_json::Map<String, serde_json::Value>) {
    hooks.retain(|_, v| v.as_array().is_none_or(|a| !a.is_empty()));
}

/// Claude-dialect groups (`{matcher, hooks:[{type, command}]}`): strip every
/// LeanCTX hook for `actions` (`"hook rewrite"`, …) in every event.
pub(super) fn strip_group_hooks(
    hooks: &mut serde_json::Map<String, serde_json::Value>,
    actions: &[&str],
) {
    for arr in hooks.values_mut().filter_map(|v| v.as_array_mut()) {
        for action in actions {
            super::claude::strip_lean_ctx_hooks(arr, action);
        }
    }
}

/// Claude-dialect: exactly one LeanCTX hook for `command` under `matcher`,
/// with an explicit host timeout when `timeout` is set.
pub(super) fn ensure_group_hook(
    arr: &mut Vec<serde_json::Value>,
    matcher: &str,
    command: &str,
    timeout: Option<u64>,
) {
    super::claude::ensure_command_hook(arr, matcher, command);
    let Some(timeout) = timeout else {
        return;
    };
    let hook = arr
        .iter_mut()
        .filter(|g| g.get("matcher").and_then(|m| m.as_str()) == Some(matcher))
        .filter_map(|g| g.get_mut("hooks").and_then(|h| h.as_array_mut()))
        .flatten()
        .find(|h| h.get("command").and_then(|c| c.as_str()) == Some(command));
    if let Some(obj) = hook.and_then(|h| h.as_object_mut()) {
        obj.insert("timeout".to_string(), serde_json::json!(timeout));
    }
}

/// True when `path` (an `mcpServers`-style or TOML config) names the LeanCTX
/// server.
pub(super) fn mcp_file_has_lean_ctx(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .is_ok_and(|c| c.contains("\"lean-ctx\"") || c.contains("[mcp_servers.lean-ctx]"))
}

/// Remove the LeanCTX MCP server from one host config file.
pub(super) fn remove_mcp_entry(
    name: &'static str,
    agent_key: &str,
    config_path: PathBuf,
    config_type: lean_ctx::core::editor_registry::ConfigType,
) {
    if !config_path.exists() {
        return;
    }
    let target = lean_ctx::core::editor_registry::EditorTarget {
        name,
        agent_key: agent_key.to_string(),
        detect_path: config_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default(),
        config_path,
        config_type,
    };
    if let Err(e) = lean_ctx::core::editor_registry::remove_lean_ctx_server(
        &target,
        lean_ctx::core::editor_registry::WriteOptions::default(),
    ) {
        tracing::warn!("governor: could not remove the lean-ctx MCP entry for {name}: {e}");
    }
}

#[cfg(test)]
pub(super) mod test_support {
    /// Isolated data dir (holds the env lock), a stable hook binary and an
    /// isolated `HOME`, so rendered commands read `lean-ctx hook …` and no
    /// real host config is ever touched.
    pub(crate) struct TestEnv {
        _iso: lean_ctx::core::data_dir::IsolatedDataDir,
        pub home: tempfile::TempDir,
        prev_home: Option<std::ffi::OsString>,
    }

    impl TestEnv {
        pub(crate) fn new() -> Self {
            let iso = lean_ctx::core::data_dir::isolated_data_dir();
            let home = tempfile::tempdir().unwrap();
            let prev_home = std::env::var_os("HOME");
            lean_ctx::test_env::set_var("LEAN_CTX_HOOK_BINARY", "lean-ctx");
            lean_ctx::test_env::set_var("HOME", home.path());
            for var in [
                "CODEX_HOME",
                "COPILOT_HOME",
                "CODEBUDDY_CONFIG_DIR",
                "LEAN_CTX_HOOK_MODE",
                "CLAUDE_CONFIG_DIR",
            ] {
                lean_ctx::test_env::remove_var(var);
            }
            Self {
                _iso: iso,
                home,
                prev_home,
            }
        }

        pub(crate) fn path(&self) -> &std::path::Path {
            self.home.path()
        }
    }

    impl Drop for TestEnv {
        fn drop(&mut self) {
            lean_ctx::test_env::remove_var("LEAN_CTX_HOOK_BINARY");
            match self.prev_home.take() {
                Some(home) => lean_ctx::test_env::set_var("HOME", home),
                None => lean_ctx::test_env::remove_var("HOME"),
            }
        }
    }

    pub(crate) fn read_json(path: &std::path::Path) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_switch_lists_only_verified_hook_hosts() {
        for key in [
            "codebuddy",
            "cursor",
            "gemini",
            "codex",
            "copilot",
            "Cursor",
        ] {
            assert!(governor_available(key).is_some(), "{key}");
        }
        for key in [
            "claude",
            "vscode",
            "windsurf",
            "qoder",
            "vibe",
            "opencode",
            "pi",
            "zed",
            "kiro",
            "antigravity",
            "hermes",
            "amp",
            "cline",
        ] {
            assert!(
                governor_available(key).is_none(),
                "{key} stays on today's setup"
            );
        }
        assert_eq!(
            governor_available("codebuddy").unwrap().class,
            GovernorClass::Full
        );
        for key in ["cursor", "gemini", "codex", "copilot"] {
            assert_eq!(
                governor_available(key).unwrap().class,
                GovernorClass::EnforceOnly,
                "{key}"
            );
        }
    }

    /// Core detection (used by the MCP writer and rules injection) and the
    /// installer specs name the same hosts.
    #[test]
    fn core_detection_covers_exactly_the_installer_hosts() {
        for key in [
            "codebuddy",
            "cursor",
            "gemini",
            "codex",
            "copilot",
            "claude",
            "vscode",
            "windsurf",
            "qoder",
            "antigravity",
        ] {
            let spec = governor_available(key);
            let detect = lean_ctx::core::governor_hosts::detect(key);
            assert_eq!(spec.is_some(), detect.is_some(), "{key}");
            if let (Some(spec), Some(detect)) = (spec, detect) {
                assert_eq!(spec.key, detect.key);
                let env = test_support::TestEnv::new();
                let home = env.path();
                assert_eq!((spec.mcp_config_path)(home), (detect.mcp_config_path)(home));
                assert!(!(detect.is_installed)(home), "{key}");
                install(spec, home);
                assert!((detect.is_installed)(home), "{key}");
                assert!(is_installed(key, home), "{key}");
            }
        }
    }

    /// The migration backs up `touched_files` before running `steps`: every
    /// file a step writes must be in that list, or a failed update could not
    /// be rolled back.
    #[test]
    fn install_steps_write_only_the_files_listed_for_backup() {
        for key in ["codebuddy", "cursor", "gemini", "codex", "copilot"] {
            let env = test_support::TestEnv::new();
            let home = env.path();
            let spec = governor_available(key).unwrap();
            assert_eq!(spec.key, key);
            install(spec, home);
            let listed = (spec.touched_files)(home);
            for entry in walk(home) {
                assert!(
                    listed.contains(&entry),
                    "{key}: {} not listed for backup",
                    entry.display()
                );
            }
            assert!((spec.is_installed)(home), "{key}");
        }
    }

    /// Regular files under `dir`, ignoring write-backup siblings.
    fn walk(dir: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                out.extend(walk(&path));
            } else if !path.to_string_lossy().contains(".bak")
                && !path.to_string_lossy().contains(".lean-ctx")
            {
                out.push(path);
            }
        }
        out
    }

    #[test]
    fn rules_block_stays_within_three_lines_and_names_no_mcp_tool() {
        for class in [GovernorClass::Full, GovernorClass::EnforceOnly] {
            let body = rules_body(class);
            assert_eq!(body.lines().count(), 3, "{body}");
            assert!(!body.contains("ctx_read"), "no MCP tool names: {body}");
            assert!(body.contains("lean-ctx call ctx_compose"));
        }
    }

    #[test]
    fn upsert_replaces_the_managed_block_and_keeps_user_text() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("RULES.md");
        let start = lean_ctx::core::rules_canonical::START_MARK;
        let end = lean_ctx::core::rules_canonical::END_MARK;
        std::fs::write(&path, format!("# mine\n\n{start}\nfull rules\n{end}\n")).unwrap();
        upsert_rules_block(&path, start, end, GovernorClass::EnforceOnly);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# mine"));
        assert!(!text.contains("full rules"));
        assert_eq!(text.matches(start).count(), 1, "single channel: {text}");
        assert!(text.contains(GOVERNOR_MARKER));
        upsert_rules_block(&path, start, end, GovernorClass::EnforceOnly);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text, "idempotent");
        remove_rules_block(&path, start, end);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "# mine\n");
    }

    /// `hook rewrite-enforce` on each enforcement-only host's payload: an
    /// allowlist violation is denied in a field that host reads, a clean
    /// command passes through with no decision, and no answer ever approves
    /// ("mirror, don't grant").
    #[test]
    fn enforcement_hosts_get_a_deny_in_their_dialect_and_never_a_grant() {
        let _env = test_support::TestEnv::new();
        lean_ctx::test_env::set_var("LEAN_CTX_SHELL_SECURITY", "enforce");
        lean_ctx::test_env::set_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE", "git,cargo");
        let bad = "python3 -c 'print(1)'";
        let cases = [
            (
                "cursor",
                serde_json::json!({"hook_event_name": "preToolUse", "tool_name": "Shell",
                    "tool_input": {"command": bad}}),
                "/permission",
            ),
            (
                "copilot",
                serde_json::json!({"toolName": "bash",
                    "toolArgs": serde_json::json!({"command": bad}).to_string()}),
                "/permissionDecision",
            ),
        ];
        let mut outputs = Vec::new();
        for (host, payload, deny_field) in &cases {
            let out = crate::hook_handlers::enforce_decision_for_test(&payload.to_string());
            outputs.push((*host, out, *deny_field));
        }
        let gemini_bad = crate::hook_handlers::enforce_decision_for_test(
            &serde_json::json!({"hook_event_name": "BeforeTool",
                "tool_name": "run_shell_command", "tool_input": {"command": bad}})
            .to_string(),
        );
        let clean = crate::hook_handlers::enforce_decision_for_test(
            &serde_json::json!({"hook_event_name": "preToolUse", "tool_name": "Shell",
                "tool_input": {"command": "git status"}})
            .to_string(),
        );
        lean_ctx::test_env::remove_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE");
        lean_ctx::test_env::remove_var("LEAN_CTX_SHELL_SECURITY");

        for (host, out, deny_field) in outputs {
            let v: serde_json::Value = serde_json::from_str(&out).unwrap();
            assert_eq!(v.pointer(deny_field).unwrap(), "deny", "{host}: {out}");
            assert!(!out.contains("\"allow\""), "{host} never granted: {out}");
            assert!(v.get("updated_input").is_none() && v.get("modifiedArgs").is_none());
        }
        // Gemini cannot be approved by a hook: the wrap (Gemini still asks)
        // or a `decision: deny`, never an approval.
        let v: serde_json::Value = serde_json::from_str(&gemini_bad).unwrap();
        assert!(!gemini_bad.contains("\"allow\""), "{gemini_bad}");
        assert!(
            v["decision"] == "deny"
                || v["hookSpecificOutput"]["tool_input"]["command"]
                    .as_str()
                    .is_some_and(|c| c.starts_with("lean-ctx -c --raw ")),
            "{gemini_bad}"
        );
        assert_eq!(clean, "{}", "pass-through carries no decision");
    }
}
