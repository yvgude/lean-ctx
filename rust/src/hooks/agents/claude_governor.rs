// SPDX-License-Identifier: Apache-2.0

//! Zero-overhead governor setup for Claude Code (governor design v1, slice 1).
//!
//! Opt-in only: `lean-ctx init --agent claude --mode governor` (no MCP
//! server; strong tools through `lean-ctx call` in Bash) or `--mode
//! governor-mcp` (MCP server registered with no instructions and deferred
//! schemas, owner decision O-2). Today's default setup is untouched until the
//! user picks one of these, and `--mode hybrid|mcp|replace` reverts it.
//!
//! What the profile writes, and nothing else:
//! - PostToolUse `Read|Bash|Edit|…` → `hook govern` (explicit 5 s timeout);
//!   PreCompact, PostCompact, SessionStart → `hook govern` (purge only, no
//!   stdout); SessionEnd → `hook observe`.
//! - PreToolUse Bash|Read|Grep|Glob → `hook rewrite-enforce`: only security —
//!   the allowlist wrap (#1408) and the secret-path deny of `hook redirect`;
//!   no compression rewrites, no `cat` → `ctx_read` (owner decision O-1).
//! - A ≤ 3-line rule block in `CLAUDE.md` (same managed markers as the
//!   standard block, so either setup replaces the other cleanly).
//!
//! It removes what costs prefix tokens or competes with the governor: the
//! Read redirect, `hook read-dedup` (now rule R1.read), UserPromptSubmit/Stop
//! observe, the lean-ctx skill listing, the Grep/Glob deny and — in the no-MCP
//! arm — the MCP entry and its tool allow-list.

use super::claude::{ensure_command_hook, strip_lean_ctx_hooks};
use super::shared::remove_managed_md_blocks;

/// The governor setup variant (owner decision O-2: both are measured).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GovernorArm {
    /// No MCP server: 0 schema tokens, strong tools through Bash.
    NoMcp,
    /// MCP server without instructions, schemas deferred behind Tool Search.
    McpDeferred,
}

impl GovernorArm {
    /// `--mode` value → arm. `None` for every non-governor mode.
    pub(crate) fn from_mode(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "governor" | "governor-nomcp" => Some(Self::NoMcp),
            "governor-mcp" => Some(Self::McpDeferred),
            _ => None,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::NoMcp => "governor",
            Self::McpDeferred => "governor-mcp",
        }
    }
}

/// PostToolUse matcher: the governed results plus the edit tools that bump
/// the edit epoch (they never get output).
pub(crate) const GOVERN_POST_MATCHER: &str =
    "Read|Bash|Edit|Write|MultiEdit|NotebookEdit|mcp__lean-ctx__ctx_read|mcp__lean_ctx__ctx_read";

/// Marks an MCP entry this profile wrote, so a revert removes only its own keys.
const ARM_MARKER_ENV: &str = "LEAN_CTX_GOVERNOR_ARM";

const RULES_NO_MCP: &str = "\
<!-- lean-ctx -->
<!-- lean-ctx-governor-v1 -->
lean-ctx keeps native Read/Bash results lossless; an identical repeat may show `[unchanged …]` (the full result is earlier in this conversation).
Ranked files with source for a task: `lean-ctx call ctx_compose --project-root \"$PWD\" --json '{\"task\":\"…\"}'`.
<!-- /lean-ctx -->";

const RULES_MCP: &str = "\
<!-- lean-ctx -->
<!-- lean-ctx-governor-v1 -->
lean-ctx keeps native Read/Bash results lossless; an identical repeat may show `[unchanged …]` (the full result is earlier in this conversation).
Optional `ctx_*` tools (e.g. ctx_compose for ranked files with source) load on demand via tool search.
<!-- /lean-ctx -->";

/// The rule block the arm writes — the only per-turn text it adds.
pub(crate) fn rules_block(arm: GovernorArm) -> &'static str {
    match arm {
        GovernorArm::NoMcp => RULES_NO_MCP,
        GovernorArm::McpDeferred => RULES_MCP,
    }
}

const BLOCK_START: &str = crate::core::rules_canonical::AGENTS_BLOCK_START;
const BLOCK_END: &str = crate::core::rules_canonical::AGENTS_BLOCK_END;

fn settings_path(home: &std::path::Path) -> std::path::PathBuf {
    crate::core::editor_registry::claude_state_dir(home).join("settings.json")
}

fn load_json(path: &std::path::Path) -> serde_json::Value {
    std::fs::read_to_string(path)
        .ok()
        .filter(|c| !c.trim().is_empty())
        .and_then(|c| crate::core::jsonc::parse_jsonc(&c).ok())
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

/// True when the settings carry the governor's PostToolUse hook. Standard
/// installers and refreshes check this so they never fight the profile.
pub(crate) fn is_installed(home: &std::path::Path) -> bool {
    std::fs::read_to_string(settings_path(home)).is_ok_and(|c| c.contains(" hook govern\""))
}

/// Which governor arm is installed under `home`, if any (for `doctor overhead`).
pub(crate) fn installed_arm(home: &std::path::Path) -> Option<GovernorArm> {
    if !is_installed(home) {
        return None;
    }
    let mcp = load_json(&crate::core::editor_registry::claude_mcp_json_path(home));
    let marked = mcp
        .pointer(&format!("/mcpServers/lean-ctx/env/{ARM_MARKER_ENV}"))
        .is_some();
    Some(if marked {
        GovernorArm::McpDeferred
    } else {
        GovernorArm::NoMcp
    })
}

/// Install (or re-render) the governor profile. Idempotent.
pub(crate) fn install(home: &std::path::Path, arm: GovernorArm) {
    let binary = super::super::resolve_hook_command_binary();
    let path = settings_path(home);
    let mut root = load_json(&path);
    let before = serde_json::to_string_pretty(&root).unwrap_or_default();
    apply_hooks(&mut root, &binary);
    apply_permissions(&mut root, arm);
    save_if_changed(&path, &before, &root);
    apply_mcp_entry(home, arm);
    write_rules_block(home, arm);
    // The skill listing rides every request's system prompt.
    let skill = crate::core::editor_registry::claude_state_dir(home).join("skills/lean-ctx");
    let _ = std::fs::remove_dir_all(skill);
    if !super::super::mcp_server_quiet_mode() {
        eprintln!(
            "Installed the lean-ctx {} setup for Claude Code at {}",
            arm.label(),
            path.display()
        );
    }
}

/// Re-render only the hooks (binary path moves), keeping the chosen arm.
pub(crate) fn refresh(home: &std::path::Path) {
    let path = settings_path(home);
    let mut root = load_json(&path);
    let before = serde_json::to_string_pretty(&root).unwrap_or_default();
    apply_hooks(&mut root, &super::super::resolve_hook_command_binary());
    save_if_changed(&path, &before, &root);
}

/// Undo the profile's own artifacts so a standard install starts clean.
/// Returns whether a governor setup was found.
pub(crate) fn revert(home: &std::path::Path) -> bool {
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

    let mcp_path = crate::core::editor_registry::claude_mcp_json_path(home);
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
    let govern = format!("{binary} hook govern");
    let enforce = format!("{binary} hook rewrite-enforce");
    let observe = format!("{binary} hook observe");
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
    }
    if let Some(arr) = event_array(hooks, "PostToolUse") {
        timed_hook(arr, GOVERN_POST_MATCHER, &govern, timeout);
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
    let path = crate::core::editor_registry::claude_mcp_json_path(home);
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
                        "autoApprove": crate::core::editor_registry::writers::auto_approve_tools(),
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
    let dir = crate::core::editor_registry::claude_state_dir(home);
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("CLAUDE.md");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let cleaned = remove_managed_md_blocks(&existing, BLOCK_START, BLOCK_END);
    let cleaned = cleaned.trim();
    let block = rules_block(arm);
    let updated = if cleaned.is_empty() {
        format!("{block}\n")
    } else {
        format!("{cleaned}\n\n{block}\n")
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
    struct TestEnv(#[allow(dead_code)] crate::core::data_dir::IsolatedDataDir);
    impl TestEnv {
        fn new() -> Self {
            let iso = crate::core::data_dir::isolated_data_dir();
            crate::test_env::set_var("LEAN_CTX_HOOK_BINARY", "lean-ctx");
            crate::test_env::remove_var("CLAUDE_CONFIG_DIR");
            Self(iso)
        }
    }
    impl Drop for TestEnv {
        fn drop(&mut self) {
            crate::test_env::remove_var("LEAN_CTX_HOOK_BINARY");
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
            ("PreCompact", "hook govern", t),
            ("PreToolUse", "hook rewrite-enforce", None),
            ("SessionEnd", "hook observe", t),
            ("SessionStart", "hook govern", t),
        ]
        .into_iter()
        .map(|(e, a, t)| (e.to_string(), a.to_string(), t))
        .collect();
        assert_eq!(lean_ctx_actions(home.path()), want);
        assert!(is_installed(home.path()));

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
        let mcp_path = crate::core::editor_registry::claude_mcp_json_path(home.path());
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
        let md = crate::core::editor_registry::claude_state_dir(home.path()).join("CLAUDE.md");
        std::fs::create_dir_all(md.parent().unwrap()).unwrap();
        std::fs::write(
            &md,
            "# Mine\n\n<!-- lean-ctx -->\nold standard block\n<!-- /lean-ctx -->\n",
        )
        .unwrap();
        install(home.path(), GovernorArm::NoMcp);
        let text = std::fs::read_to_string(&md).unwrap();
        assert!(text.starts_with("# Mine\n"), "user content kept");
        assert!(!text.contains("old standard block"));
        assert_eq!(text.matches(BLOCK_START).count(), 1);
        for arm in [GovernorArm::NoMcp, GovernorArm::McpDeferred] {
            let body_lines = rules_block(arm)
                .lines()
                .filter(|l| !l.starts_with("<!--"))
                .count();
            assert!(body_lines <= 3, "{arm:?}: ≤ 3 rule lines");
        }
    }
}
