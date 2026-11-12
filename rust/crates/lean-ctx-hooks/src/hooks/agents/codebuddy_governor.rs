// SPDX-License-Identifier: Apache-2.0

//! Governor setup for CodeBuddy — the full governor.
//!
//! CodeBuddy shares Claude Code's hook contract: PreToolUse
//! `hookSpecificOutput.updatedInput` applies without a decision and flows
//! through CodeBuddy's own permission rules, and PostToolUse
//! `hookSpecificOutput.updatedToolOutput` replaces the newest tool result
//! (the contract the standard `hook read-dedup` already relies on there). So
//! CodeBuddy gets the Claude Code governor unchanged: `hook rewrite-enforce`
//! (allowlist wrap or deny; [`crate::hook_handlers`] never mirrors Claude Code
//! permissions onto CodeBuddy, so the wrap carries no approval) and
//! `hook govern` (R1–R4).
//!
//! Writes: `~/.codebuddy/settings.json` (hooks; native Grep/Glob back in the
//! tool list; no `mcp__lean-ctx__*` allow rules), no LeanCTX server in
//! `~/.codebuddy/mcp.json`, the rules block in `~/.codebuddy/CODEBUDDY.md`,
//! and no lean-ctx skill (its listing rides every request).

use std::path::{Path, PathBuf};

use super::governor_hosts::{
    GovernorClass, GovernorHostSpec, ensure_group_hook, has_group_hook, load_json,
    load_json_strict, mcp_file_has_lean_ctx, remove_mcp_entry, save_if_changed, strip_group_hooks,
};

pub(crate) const SPEC: GovernorHostSpec = GovernorHostSpec {
    key: "codebuddy",
    name: "CodeBuddy",
    class: GovernorClass::Full,
    is_installed,
    has_standard_setup,
    touched_files,
    steps: &[
        ("hooks", apply_settings),
        ("mcp", remove_mcp),
        ("rules", write_rules),
    ],
    revert,
    refresh: refresh_hooks,
    mcp_config_path,
    switch_back: "lean-ctx init --agent codebuddy --mode hybrid",
};

const STANDARD_ACTIONS: &[&str] = &[
    "hook rewrite",
    "hook redirect",
    "hook read-dedup",
    "hook observe",
    "hook deny",
];

const BLOCK_START: &str = lean_ctx::core::rules_canonical::AGENTS_BLOCK_START;
const BLOCK_END: &str = lean_ctx::core::rules_canonical::AGENTS_BLOCK_END;

// Detection is shared with the engine's MCP writer and rules injection.
use lean_ctx::core::governor_hosts::codebuddy::{
    is_installed, mcp_config_path, settings_path, state_dir,
};

fn codebuddy_md(home: &Path) -> PathBuf {
    state_dir(home).join("CODEBUDDY.md")
}

fn touched_files(home: &Path) -> Vec<PathBuf> {
    vec![
        settings_path(home),
        mcp_config_path(home),
        codebuddy_md(home),
    ]
}

fn has_standard_setup(home: &Path) -> bool {
    has_group_hook(
        &load_json(&settings_path(home)),
        &["rewrite", "redirect", "read-dedup", "observe"],
    ) || mcp_file_has_lean_ctx(&mcp_config_path(home))
}

fn apply_hooks(root: &mut serde_json::Value) {
    let binary = super::super::resolve_hook_command_binary();
    let govern = crate::hooks::hook_shell_command(&binary, "govern");
    let enforce = crate::hooks::hook_shell_command(&binary, "rewrite-enforce");
    let observe = crate::hooks::hook_shell_command(&binary, "observe");
    let timeout = crate::hook_handlers::GOVERN_HOST_TIMEOUT_SECS;
    let enforce_matcher = if cfg!(windows) {
        "Bash|bash|PowerShell|powershell|Read|Grep|Glob"
    } else {
        "Bash|bash|Read|Grep|Glob"
    };
    let Some(hooks) = super::governor_hosts::hooks_object(root) else {
        return;
    };
    strip_group_hooks(hooks, STANDARD_ACTIONS);
    let event = super::governor_hosts::event_array;
    // No host timeout on the enforcement hook: it fails open inside 15 s,
    // and a short host timeout would let a violation through unwrapped.
    ensure_group_hook(event(hooks, "PreToolUse"), enforce_matcher, &enforce, None);
    super::governor_hosts::ensure_write_guard(
        hooks,
        "PreToolUse",
        super::governor_hosts::CLAUDE_WRITE_MATCHER,
        None,
    );
    ensure_group_hook(
        event(hooks, "PostToolUse"),
        super::claude_governor::GOVERN_POST_MATCHER,
        &govern,
        Some(timeout),
    );
    // CodeBuddy's lifecycle events (no PostCompact): purge, plus the compaction marker on
    // PreCompact / SessionStart compact|clear; no stdout.
    for name in ["PreCompact", "SessionStart"] {
        ensure_group_hook(event(hooks, name), ".*", &govern, Some(timeout));
    }
    ensure_group_hook(event(hooks, "SessionEnd"), ".*", &observe, Some(timeout));
    super::governor_hosts::prune_empty_events(hooks);
}

fn apply_settings(home: &Path) {
    let path = settings_path(home);
    let Some(mut root) = load_json_strict(&path) else {
        tracing::warn!(
            "governor: {} is not valid JSON, left untouched",
            path.display()
        );
        return;
    };
    let before = root.clone();
    apply_hooks(&mut root);
    if let Some(perms) = root.get_mut("permissions").and_then(|p| p.as_object_mut()) {
        if let Some(deny) = perms.get_mut("deny").and_then(|d| d.as_array_mut()) {
            // The standard default hides native Grep/Glob; the governor keeps
            // the host's own tool list.
            deny.retain(|v| !matches!(v.as_str(), Some("Grep" | "Glob")));
        }
        if let Some(allow) = perms.get_mut("allow").and_then(|a| a.as_array_mut()) {
            allow.retain(|v| !v.as_str().is_some_and(|s| s.starts_with("mcp__lean-ctx__")));
        }
    }
    save_if_changed(&path, &before, &root);
}

fn refresh_hooks(home: &Path) {
    let path = settings_path(home);
    let Some(mut root) = load_json_strict(&path) else {
        return;
    };
    let before = root.clone();
    apply_hooks(&mut root);
    save_if_changed(&path, &before, &root);
}

fn remove_mcp(home: &Path) {
    remove_mcp_entry(
        "CodeBuddy",
        "codebuddy",
        mcp_config_path(home),
        lean_ctx::core::editor_registry::ConfigType::McpJson,
    );
}

fn write_rules(home: &Path) {
    super::governor_hosts::upsert_rules_block(
        &codebuddy_md(home),
        BLOCK_START,
        BLOCK_END,
        SPEC.class,
    );
    let _ = std::fs::remove_dir_all(home.join(".codebuddy/skills/lean-ctx"));
}

fn revert(home: &Path) -> bool {
    if !is_installed(home) {
        return false;
    }
    let path = settings_path(home);
    let mut root = load_json(&path);
    let before = root.clone();
    if let Some(hooks) = root.get_mut("hooks").and_then(|h| h.as_object_mut()) {
        // Observe too: the standard installer re-adds it in its own shape.
        strip_group_hooks(
            hooks,
            &["hook govern", "hook rewrite-enforce", "hook observe"],
        );
        super::governor_hosts::prune_empty_events(hooks);
    }
    save_if_changed(&path, &before, &root);
    super::governor_hosts::remove_rules_block(&codebuddy_md(home), BLOCK_START, BLOCK_END);
    true
}

#[cfg(test)]
mod tests {
    use super::super::governor_hosts::{
        install,
        test_support::{TestEnv, read_json},
    };
    use super::*;

    /// Every LeanCTX hook as `(event, action, timeout)`.
    fn actions(home: &Path) -> Vec<(String, String, Option<u64>)> {
        let s = read_json(&settings_path(home));
        let mut out = Vec::new();
        for (event, groups) in s["hooks"].as_object().into_iter().flatten() {
            for g in groups.as_array().into_iter().flatten() {
                for h in g["hooks"].as_array().into_iter().flatten() {
                    let cmd = h["command"].as_str().unwrap_or_default();
                    if cmd.starts_with("lean-ctx ") {
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
    fn governor_replaces_the_standard_codebuddy_setup() {
        let env = TestEnv::new();
        let home = env.path();
        super::super::install_codebuddy_hook_config(home);
        std::fs::write(
            mcp_config_path(home),
            r#"{"mcpServers":{"lean-ctx":{"command":"lean-ctx"}}}"#,
        )
        .unwrap();
        assert!(has_standard_setup(home) && !is_installed(home));

        install(&SPEC, home);
        install(&SPEC, home);

        let t = Some(crate::hook_handlers::GOVERN_HOST_TIMEOUT_SECS);
        assert_eq!(
            actions(home),
            [
                ("PostToolUse".into(), "hook govern".into(), t),
                ("PreCompact".into(), "hook govern".into(), t),
                ("PreToolUse".into(), "hook rewrite-enforce".into(), None),
                ("PreToolUse".into(), "hook write-guard".into(), None),
                ("SessionEnd".into(), "hook observe".into(), t),
                ("SessionStart".into(), "hook govern".into(), t),
            ]
        );
        assert!(
            read_json(&mcp_config_path(home))["mcpServers"]
                .get("lean-ctx")
                .is_none()
        );
        let md = std::fs::read_to_string(codebuddy_md(home)).unwrap();
        assert!(md.contains("[unchanged"), "full-governor rules: {md}");
        assert!(is_installed(home));

        assert!(revert(home));
        // The write guard belongs to both setups and stays.
        assert_eq!(
            actions(home),
            [("PreToolUse".into(), "hook write-guard".into(), None)]
        );
        assert!(!codebuddy_md(home).exists());
    }

    /// CodeBuddy sends Claude Code's payloads: the PostToolUse answer is the
    /// same `updatedToolOutput` envelope, and the PreToolUse wrap of an
    /// allowlist violation carries no `allow` (Claude permissions are never
    /// mirrored onto CodeBuddy).
    #[test]
    fn codebuddy_payloads_get_the_claude_dialect_without_a_grant() {
        let _env = TestEnv::new();
        lean_ctx::test_env::set_var("CODEBUDDY_SESSION_ID", "cb-1");
        lean_ctx::test_env::set_var("LEAN_CTX_SHELL_SECURITY", "enforce");
        lean_ctx::test_env::set_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE", "git,cargo");
        let pre = serde_json::json!({
            "session_id": "cb-1", "transcript_path": "/t",
            "hook_event_name": "PreToolUse", "tool_name": "Bash",
            "tool_input": {"command": "python3 -c 'print(1)'"}
        });
        let out = crate::hook_handlers::enforce_decision_for_test(&pre.to_string());
        lean_ctx::test_env::remove_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE");
        lean_ctx::test_env::remove_var("LEAN_CTX_SHELL_SECURITY");
        lean_ctx::test_env::remove_var("CODEBUDDY_SESSION_ID");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let hso = &v["hookSpecificOutput"];
        assert_ne!(hso["permissionDecision"], "allow", "never a grant: {out}");
        let wrapped = hso["updatedInput"]["command"].as_str();
        assert!(
            hso["permissionDecision"] == "deny"
                || wrapped.is_some_and(|c| c.starts_with("lean-ctx -c --raw ")),
            "violation is wrapped or denied: {out}"
        );
    }
}
