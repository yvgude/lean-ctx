// SPDX-License-Identifier: Apache-2.0

//! Zero-overhead governor setup for Claude Code (governor design v1, slice 1).
//!
//! The default Claude Code setup since owner decision 2026-11-01
//! ([`crate::hooks::governor_default`]): new installs get the no-MCP arm, and
//! an update migrates an unpinned standard setup to it (with backups).
//! Explicitly: `lean-ctx init --agent claude --mode governor` (no MCP
//! server; strong tools through `lean-ctx call` in Bash) or `--mode
//! governor-mcp` (MCP server registered with no instructions and deferred
//! schemas, owner decision O-2). `--mode hybrid|mcp|replace` reverts it and
//! pins that choice, so no later update migrates it again.
//!
//! What the profile writes, and nothing else:
//! - PostToolUse `Read|Bash|Edit|…` → `hook govern` (explicit 5 s timeout);
//!   PreCompact, PostCompact, SessionStart → `hook govern` (purge, plus the
//!   compaction marker on a compaction or clear; no stdout); SessionEnd →
//!   `hook observe`; PostToolUse `SendMessage` → `hook observe` (mirrors the
//!   host's direct messages to the local bus).
//! - PreToolUse Bash|Read|Grep|Glob → `hook rewrite-enforce`: only security —
//!   the allowlist wrap (#1408) and the secret-path deny of `hook redirect`;
//!   no compression rewrites, no `cat` → `ctx_read` (owner decision O-1).
//! - No rules block in the no-MCP arm: Claude Code wraps any `CLAUDE.md` in
//!   ~220 tokens of its own frame, and the `[unchanged …]` stubs explain
//!   themselves (measured: governor without the block is level with native on
//!   short tasks and cheaper on long ones, Messbuch findings 19/20). An old
//!   lean-ctx block is removed; the user's own text stays. The MCP arm keeps a
//!   ≤ 3-line block for its deferred tools.
//!
//! It removes what costs prefix tokens or competes with the governor: the
//! Read redirect, `hook read-dedup` (now rule R1.read), UserPromptSubmit/Stop
//! observe, the lean-ctx skill listing, the Grep/Glob deny and — in the no-MCP
//! arm — the MCP entry and its tool allow-list.

use super::claude::{ensure_command_hook, strip_lean_ctx_hooks};
use super::shared::remove_managed_md_blocks;
pub use lean_ctx::core::claude_host::GovernorArm;
pub use lean_ctx::core::claude_host::is_governor_installed as is_installed;
use lean_ctx::core::claude_host::rules_block;

/// PostToolUse matcher: the governed results plus the edit tools that bump
/// the edit epoch (they never get output).
pub(crate) const GOVERN_POST_MATCHER: &str =
    "Read|Bash|Edit|Write|MultiEdit|NotebookEdit|mcp__lean-ctx__ctx_read|mcp__lean_ctx__ctx_read";

/// Marks an MCP entry this profile wrote, so a revert removes only its own keys.
const ARM_MARKER_ENV: &str = "LEAN_CTX_GOVERNOR_ARM";

const BLOCK_START: &str = lean_ctx::core::rules_canonical::AGENTS_BLOCK_START;
const BLOCK_END: &str = lean_ctx::core::rules_canonical::AGENTS_BLOCK_END;

fn settings_path(home: &std::path::Path) -> std::path::PathBuf {
    lean_ctx::core::editor_registry::claude_state_dir(home).join("settings.json")
}

fn load_json(path: &std::path::Path) -> serde_json::Value {
    std::fs::read_to_string(path)
        .ok()
        .filter(|c| !c.trim().is_empty())
        .and_then(|c| lean_ctx::core::jsonc::parse_jsonc(&c).ok())
        .unwrap_or_else(|| serde_json::json!({}))
}

fn save_if_changed(path: &std::path::Path, before: &str, value: &serde_json::Value) {
    let after = serde_json::to_string_pretty(value).unwrap_or_default();
    if after != before {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        super::super::write_file(path, &after);
    }
}

/// Which governor arm is installed under `home`, if any (for `doctor overhead`).
pub fn installed_arm(home: &std::path::Path) -> Option<GovernorArm> {
    if !is_installed(home) {
        return None;
    }
    let mcp = load_json(&lean_ctx::core::editor_registry::claude_mcp_json_path(home));
    let marked = mcp
        .pointer(&format!("/mcpServers/lean-ctx/env/{ARM_MARKER_ENV}"))
        .is_some();
    Some(if marked {
        GovernorArm::McpDeferred
    } else {
        GovernorArm::NoMcp
    })
}

/// True when `home` carries any lean-ctx artifact of the standard Claude Code
/// setup (hooks, MCP entry, rules block, legacy rules file or skill). A home
/// without one is a new install.
pub fn has_standard_setup(home: &std::path::Path) -> bool {
    let dir = lean_ctx::core::editor_registry::claude_state_dir(home);
    let contains = |p: std::path::PathBuf, needle: &str| {
        std::fs::read_to_string(p).is_ok_and(|c| c.contains(needle))
    };
    contains(settings_path(home), "lean-ctx")
        || dir.join("hooks/lean-ctx-rewrite.sh").exists()
        || load_json(&lean_ctx::core::editor_registry::claude_mcp_json_path(home))
            .pointer("/mcpServers/lean-ctx")
            .is_some()
        || contains(dir.join("CLAUDE.md"), "<!-- lean-ctx")
        || lean_ctx::core::editor_registry::claude_rules_dir(home)
            .join("lean-ctx.md")
            .exists()
        || dir.join("skills/lean-ctx").exists()
}

/// The files the governor profile rewrites (backed up before a migration).
pub(crate) fn touched_files(home: &std::path::Path) -> Vec<std::path::PathBuf> {
    vec![
        settings_path(home),
        lean_ctx::core::editor_registry::claude_mcp_json_path(home),
        lean_ctx::core::editor_registry::claude_state_dir(home).join("CLAUDE.md"),
    ]
}

/// The profile's steps in order, each idempotent and re-runnable, so an
/// interrupted migration completes on its next run (`governor_default`).
pub(crate) const NO_MCP_STEPS: &[(&str, fn(&std::path::Path))] = &[
    ("settings", step_settings),
    ("mcp", step_mcp),
    ("rules", step_rules),
    ("skill", remove_skill),
    ("rule-modes", step_rule_modes),
];

fn step_settings(home: &std::path::Path) {
    install_settings(home, GovernorArm::NoMcp);
}
fn step_mcp(home: &std::path::Path) {
    apply_mcp_entry(home, GovernorArm::NoMcp);
}
fn step_rules(home: &std::path::Path) {
    write_rules_block(home, GovernorArm::NoMcp);
}
fn step_rule_modes(_home: &std::path::Path) {
    persist_rule_modes();
}

/// Some measured rule mode is unset in the global `[governor]` section.
pub(crate) fn rule_modes_missing() -> bool {
    lean_ctx::core::config::Config::load_global()
        .governor
        .fill_install_defaults()
}

/// Fill the unset rule modes (any arm; hooks and MCP entry stay).
pub(crate) fn enable_rule_modes(_home: &std::path::Path) {
    persist_rule_modes();
}

/// Install (or re-render) the governor profile. Idempotent.
pub fn install(home: &std::path::Path, arm: GovernorArm) {
    install_settings(home, arm);
    apply_mcp_entry(home, arm);
    write_rules_block(home, arm);
    remove_skill(home);
    persist_rule_modes();
    if !super::super::mcp_server_quiet_mode() {
        eprintln!(
            "Installed the lean-ctx {} setup for Claude Code at {}",
            arm.label(),
            settings_path(home).display()
        );
    }
}

fn install_settings(home: &std::path::Path, arm: GovernorArm) {
    let binary = super::super::resolve_hook_command_binary();
    let path = settings_path(home);
    let mut root = load_json(&path);
    let before = serde_json::to_string_pretty(&root).unwrap_or_default();
    apply_hooks(&mut root, &binary);
    apply_permissions(&mut root, arm);
    save_if_changed(&path, &before, &root);
}

/// The skill listing rides every request's system prompt.
fn remove_skill(home: &std::path::Path) {
    let skill = lean_ctx::core::editor_registry::claude_state_dir(home).join("skills/lean-ctx");
    let _ = std::fs::remove_dir_all(skill);
}

/// Persist the measured rule modes in `[governor]` (keys the user set stay),
/// so they apply without environment variables.
fn persist_rule_modes() {
    let mut governor = lean_ctx::core::config::Config::load_global().governor;
    if !governor.fill_install_defaults() {
        return;
    }
    if let Err(e) = lean_ctx::core::config::Config::update_global(|c| {
        c.governor.fill_install_defaults();
    }) {
        tracing::warn!("could not persist the governor rule modes: {e}");
    }
}

/// Re-render only the hooks (binary path moves), keeping the chosen arm.
pub fn refresh(home: &std::path::Path) {
    let path = settings_path(home);
    let mut root = load_json(&path);
    let before = serde_json::to_string_pretty(&root).unwrap_or_default();
    apply_hooks(&mut root, &super::super::resolve_hook_command_binary());
    save_if_changed(&path, &before, &root);
    // Installs from before the zero-prefix governor drop their rules block.
    if installed_arm(home) == Some(GovernorArm::NoMcp) {
        write_rules_block(home, GovernorArm::NoMcp);
    }
}

/// Undo the profile's own artifacts so a standard install starts clean.
/// Returns whether a governor setup was found.
pub fn revert(home: &std::path::Path) -> bool {
    if !is_installed(home) {
        return false;
    }
    let path = settings_path(home);
    let mut root = load_json(&path);
    let before = serde_json::to_string_pretty(&root).unwrap_or_default();
    if let Some(hooks) = root.get_mut("hooks").and_then(|h| h.as_object_mut()) {
        for arr in hooks.values_mut().filter_map(|v| v.as_array_mut()) {
            // Observe too: the standard installer re-adds it on every event
            // in its own shape (no governor timeout).
            for action in ["hook govern", "hook rewrite-enforce", "hook observe"] {
                strip_lean_ctx_hooks(arr, action);
            }
        }
        hooks.retain(|_, v| v.as_array().is_none_or(|a| !a.is_empty()));
    }
    save_if_changed(&path, &before, &root);

    let mcp_path = lean_ctx::core::editor_registry::claude_mcp_json_path(home);
    let mut mcp = load_json(&mcp_path);
    let mcp_before = serde_json::to_string_pretty(&mcp).unwrap_or_default();
    if let Some(entry) = mcp
        .pointer_mut("/mcpServers/lean-ctx")
        .and_then(|e| e.as_object_mut())
        && let Some(env) = entry.get_mut("env").and_then(|e| e.as_object_mut())
        && env.remove(ARM_MARKER_ENV).is_some()
    {
        env.remove("LEAN_CTX_RULES_INJECTION");
        if env.is_empty() {
            entry.remove("env");
        }
    }
    save_if_changed(&mcp_path, &mcp_before, &mcp);
    true
}

fn timed_hook(arr: &mut Vec<serde_json::Value>, matcher: &str, command: &str, timeout: u64) {
    ensure_command_hook(arr, matcher, command);
    // Exec form (Windows, binary path with a space) keeps the action in `args`.
    let desired = crate::hooks::claude_command_hook(command);
    let hook = arr
        .iter_mut()
        .filter(|g| g.get("matcher").and_then(|m| m.as_str()) == Some(matcher))
        .filter_map(|g| g.get_mut("hooks").and_then(|h| h.as_array_mut()))
        .flatten()
        .find(|h| {
            h.get("command") == desired.get("command") && h.get("args") == desired.get("args")
        });
    if let Some(obj) = hook.and_then(|h| h.as_object_mut()) {
        obj.insert("timeout".to_string(), serde_json::json!(timeout));
    }
}

fn event_array<'a>(
    hooks: &'a mut serde_json::Map<String, serde_json::Value>,
    event: &str,
) -> Option<&'a mut Vec<serde_json::Value>> {
    hooks
        .entry(event.to_string())
        .or_insert_with(|| serde_json::json!([]))
        .as_array_mut()
}

fn apply_hooks(root: &mut serde_json::Value, binary: &str) {
    let Some(obj) = root.as_object_mut() else {
        return;
    };
    let Some(hooks) = obj
        .entry("hooks".to_string())
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
    else {
        return;
    };
    let govern = crate::hooks::hook_shell_command(&binary, "govern");
    let enforce = crate::hooks::hook_shell_command(&binary, "rewrite-enforce");
    let observe = crate::hooks::hook_shell_command(&binary, "observe");
    let timeout = crate::hook_handlers::GOVERN_HOST_TIMEOUT_SECS;
    // One group: `ensure_command_hook` keeps a single hook per action.
    let enforce_matcher = if cfg!(windows) {
        "Bash|PowerShell|Read|Grep|Glob"
    } else {
        "Bash|Read|Grep|Glob"
    };

    for arr in hooks.values_mut().filter_map(|v| v.as_array_mut()) {
        for action in [
            "hook rewrite",
            "hook redirect",
            "hook read-dedup",
            "hook observe",
        ] {
            strip_lean_ctx_hooks(arr, action);
        }
    }
    if let Some(arr) = event_array(hooks, "PreToolUse") {
        // Security only: Bash allowlist wrap + secret-path deny for native
        // Read/Grep/Glob. Same contract as today's `hook rewrite` (no host
        // timeout, 15 s internal fail-open): a short host timeout would let
        // a violation through unwrapped under load.
        ensure_command_hook(arr, enforce_matcher, &enforce);
        // A native write must not reconfigure LeanCTX or the host's hooks.
        ensure_command_hook(
            arr,
            super::governor_hosts::CLAUDE_WRITE_MATCHER,
            &format!("{binary} hook write-guard"),
        );
    }
    if let Some(arr) = event_array(hooks, "PostToolUse") {
        timed_hook(arr, GOVERN_POST_MATCHER, &govern, timeout);
        // SendMessage → local bus mirror (one group, no other tool).
        timed_hook(
            arr,
            crate::hook_handlers::send_mirror::SEND_MESSAGE_TOOL,
            &observe,
            timeout,
        );
    }
    for event in ["PreCompact", "PostCompact", "SessionStart"] {
        if let Some(arr) = event_array(hooks, event) {
            timed_hook(arr, ".*", &govern, timeout);
        }
    }
    if let Some(arr) = event_array(hooks, "SessionEnd") {
        timed_hook(arr, ".*", &observe, timeout);
    }
    hooks.retain(|_, v| v.as_array().is_none_or(|a| !a.is_empty()));
}

fn apply_permissions(root: &mut serde_json::Value, arm: GovernorArm) {
    let Some(perms) = root.get_mut("permissions").and_then(|p| p.as_object_mut()) else {
        return;
    };
    if let Some(deny) = perms.get_mut("deny").and_then(|d| d.as_array_mut()) {
        // The standard default hides native Grep/Glob; the governor keeps the
        // host's own tool list.
        deny.retain(|v| !matches!(v.as_str(), Some("Grep" | "Glob")));
    }
    if arm == GovernorArm::NoMcp
        && let Some(allow) = perms.get_mut("allow").and_then(|a| a.as_array_mut())
    {
        allow.retain(|v| !v.as_str().is_some_and(|s| s.starts_with("mcp__lean-ctx__")));
    }
}

fn apply_mcp_entry(home: &std::path::Path, arm: GovernorArm) {
    let path = lean_ctx::core::editor_registry::claude_mcp_json_path(home);
    let mut root = load_json(&path);
    let before = serde_json::to_string_pretty(&root).unwrap_or_default();
    let Some(obj) = root.as_object_mut() else {
        return;
    };
    match arm {
        GovernorArm::NoMcp => {
            if let Some(servers) = obj.get_mut("mcpServers").and_then(|s| s.as_object_mut()) {
                servers.remove("lean-ctx");
            }
        }
        GovernorArm::McpDeferred => {
            let servers = obj
                .entry("mcpServers".to_string())
                .or_insert_with(|| serde_json::json!({}));
            if let Some(servers) = servers.as_object_mut() {
                // No initialize instructions (`declines_rule_steering`).
                let mut env = serde_json::Map::new();
                env.insert("LEAN_CTX_RULES_INJECTION".into(), "off".into());
                env.insert(ARM_MARKER_ENV.into(), "mcp".into());
                servers.insert(
                    "lean-ctx".to_string(),
                    serde_json::json!({
                        "command": super::super::resolve_binary_path(),
                        "args": [],
                        "env": env,
                        "autoApprove": lean_ctx::core::editor_registry::writers::auto_approve_tools(),
                        // Deferred: names only until Tool Search loads a schema.
                        "alwaysLoad": false,
                    }),
                );
            }
        }
    }
    save_if_changed(&path, &before, &root);
}

fn write_rules_block(home: &std::path::Path, arm: GovernorArm) {
    let dir = lean_ctx::core::editor_registry::claude_state_dir(home);
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("CLAUDE.md");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let cleaned = remove_managed_md_blocks(&existing, BLOCK_START, BLOCK_END);
    let cleaned = cleaned.trim();
    let updated = match (rules_block(arm), cleaned.is_empty()) {
        (Some(block), true) => format!("{block}\n"),
        (Some(block), false) => format!("{cleaned}\n\n{block}\n"),
        (None, true) => {
            // The file held only the lean-ctx block: remove it, so Claude Code
            // adds no CLAUDE.md frame at all.
            if !existing.is_empty() {
                let _ = std::fs::remove_file(&path);
            }
            return;
        }
        (None, false) => format!("{cleaned}\n"),
    };
    if updated != existing {
        super::super::write_file(&path, &updated);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Isolated data dir (holds the env lock) and a stable hook binary, so
    /// rendered commands read `lean-ctx hook …` whatever binary runs the test.
    struct TestEnv(#[allow(dead_code)] lean_ctx::core::data_dir::IsolatedDataDir);
    impl TestEnv {
        fn new() -> Self {
            let iso = lean_ctx::core::data_dir::isolated_data_dir();
            lean_ctx::test_env::set_var("LEAN_CTX_HOOK_BINARY", "lean-ctx");
            lean_ctx::test_env::remove_var("CLAUDE_CONFIG_DIR");
            Self(iso)
        }
    }
    impl Drop for TestEnv {
        fn drop(&mut self) {
            lean_ctx::test_env::remove_var("LEAN_CTX_HOOK_BINARY");
        }
    }

    fn home_with_standard_setup() -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        super::super::install_claude_hook_config(home.path());
        home
    }

    fn settings(home: &std::path::Path) -> serde_json::Value {
        load_json(&settings_path(home))
    }

    /// Every lean-ctx command per hook event, as `event → [action…]`.
    fn lean_ctx_actions(home: &std::path::Path) -> Vec<(String, String, Option<u64>)> {
        let mut out = Vec::new();
        let s = settings(home);
        for (event, groups) in s["hooks"].as_object().into_iter().flatten() {
            for g in groups.as_array().into_iter().flatten() {
                for h in g["hooks"].as_array().into_iter().flatten() {
                    let cmd = h["command"].as_str().unwrap_or_default();
                    if cmd.contains("lean-ctx") {
                        out.push((
                            event.clone(),
                            super::super::claude::lean_ctx_action_token(cmd).to_string(),
                            h["timeout"].as_u64(),
                        ));
                    }
                }
            }
        }
        out.sort();
        out
    }

    #[test]
    fn governor_profile_writes_exactly_the_expected_events() {
        let _env = TestEnv::new();
        let home = home_with_standard_setup();
        install(home.path(), GovernorArm::NoMcp);
        let t = Some(crate::hook_handlers::GOVERN_HOST_TIMEOUT_SECS);
        let want: Vec<(String, String, Option<u64>)> = [
            ("PostCompact", "hook govern", t),
            ("PostToolUse", "hook govern", t),
            ("PostToolUse", "hook observe", t),
            ("PreCompact", "hook govern", t),
            ("PreToolUse", "hook rewrite-enforce", None),
            ("PreToolUse", "hook write-guard", None),
            ("SessionEnd", "hook observe", t),
            ("SessionStart", "hook govern", t),
        ]
        .into_iter()
        .map(|(e, a, t)| (e.to_string(), a.to_string(), t))
        .collect();
        assert_eq!(lean_ctx_actions(home.path()), want);
        assert!(is_installed(home.path()));
        let mirror: Vec<_> = settings(home.path())["hooks"]["PostToolUse"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|g| g["matcher"] == "SendMessage")
            .cloned()
            .collect();
        assert_eq!(mirror.len(), 1, "one SendMessage group");
        assert_eq!(mirror[0]["hooks"][0]["command"], "lean-ctx hook observe");

        // Idempotent: a second install changes nothing.
        let before = std::fs::read_to_string(settings_path(home.path())).unwrap();
        install(home.path(), GovernorArm::NoMcp);
        refresh(home.path());
        assert_eq!(
            std::fs::read_to_string(settings_path(home.path())).unwrap(),
            before
        );
    }

    #[test]
    fn standard_refresh_keeps_the_governor_and_revert_restores_the_default() {
        let _env = TestEnv::new();
        let home = home_with_standard_setup();
        let standard = lean_ctx_actions(home.path());
        install(home.path(), GovernorArm::McpDeferred);
        let governed = lean_ctx_actions(home.path());

        // A refresh path (MCP start, update) must not re-add competing hooks.
        super::super::install_claude_hook_config(home.path());
        assert_eq!(lean_ctx_actions(home.path()), governed);

        assert!(revert(home.path()));
        assert!(!is_installed(home.path()));
        super::super::install_claude_hook_config(home.path());
        let restored: Vec<_> = lean_ctx_actions(home.path());
        for entry in &standard {
            assert!(restored.contains(entry), "standard hook {entry:?} restored");
        }
        assert!(
            !restored
                .iter()
                .any(|(_, a, _)| a == "hook govern" || a == "hook rewrite-enforce"),
            "no governor hook left after revert"
        );
    }

    #[test]
    fn arms_differ_only_in_the_mcp_entry() {
        let _env = TestEnv::new();
        let home = tempfile::tempdir().unwrap();
        let mcp_path = lean_ctx::core::editor_registry::claude_mcp_json_path(home.path());
        std::fs::write(
            &mcp_path,
            r#"{"mcpServers":{"lean-ctx":{"command":"lean-ctx","alwaysLoad":true},"other":{"command":"x"}}}"#,
        )
        .unwrap();

        install(home.path(), GovernorArm::NoMcp);
        let mcp = load_json(&mcp_path);
        assert!(
            mcp.pointer("/mcpServers/lean-ctx").is_none(),
            "no MCP server"
        );
        assert!(
            mcp.pointer("/mcpServers/other").is_some(),
            "foreign entry kept"
        );

        install(home.path(), GovernorArm::McpDeferred);
        let entry = load_json(&mcp_path)["mcpServers"]["lean-ctx"].clone();
        assert_eq!(entry["alwaysLoad"], false, "schemas deferred");
        assert_eq!(
            entry["env"]["LEAN_CTX_RULES_INJECTION"], "off",
            "no instructions"
        );

        assert!(revert(home.path()));
        let entry = load_json(&mcp_path)["mcpServers"]["lean-ctx"].clone();
        assert!(
            entry.get("env").is_none(),
            "revert removes only its own env keys"
        );
    }

    #[test]
    fn rules_block_replaces_the_standard_block_and_stays_tiny() {
        let _env = TestEnv::new();
        let home = tempfile::tempdir().unwrap();
        let md = lean_ctx::core::editor_registry::claude_state_dir(home.path()).join("CLAUDE.md");
        std::fs::create_dir_all(md.parent().unwrap()).unwrap();
        std::fs::write(
            &md,
            "# Mine\n\n<!-- lean-ctx -->\nold standard block\n<!-- /lean-ctx -->\n",
        )
        .unwrap();
        install(home.path(), GovernorArm::NoMcp);
        let text = std::fs::read_to_string(&md).unwrap();
        assert_eq!(text, "# Mine\n", "user content kept, no lean-ctx block");
        install(home.path(), GovernorArm::McpDeferred);
        let text = std::fs::read_to_string(&md).unwrap();
        assert!(text.starts_with("# Mine\n"), "user content kept");
        assert_eq!(text.matches(BLOCK_START).count(), 1);
        let body_lines = rules_block(GovernorArm::McpDeferred)
            .unwrap()
            .lines()
            .filter(|l| !l.starts_with("<!--"))
            .count();
        assert!(body_lines <= 3, "≤ 3 rule lines");

        // A CLAUDE.md that held only the lean-ctx block disappears.
        let only = tempfile::tempdir().unwrap();
        let only_md =
            lean_ctx::core::editor_registry::claude_state_dir(only.path()).join("CLAUDE.md");
        std::fs::create_dir_all(only_md.parent().unwrap()).unwrap();
        std::fs::write(&only_md, "<!-- lean-ctx -->\nold\n<!-- /lean-ctx -->\n").unwrap();
        install(only.path(), GovernorArm::NoMcp);
        assert!(!only_md.exists());
    }
}
