// SPDX-License-Identifier: Apache-2.0

//! Governor setup for Cursor — enforcement only.
//!
//! Cursor's `preToolUse` hook can deny a call (`permission: "deny"`) and takes
//! a rewrite only together with `"allow"`, which would skip the user's prompt.
//! `hook rewrite-enforce` therefore never rewrites here: an allowlist
//! violation is denied, everything else passes through unchanged (`{}`), so
//! no grant is ever added. Cursor's `postToolUse` cannot replace a native
//! Shell/Read result (and wrote hook stdout into edited files, #1200), so the
//! governor installs no post-tool hook.
//!
//! Writes: `~/.cursor/hooks.json` (`preToolUse` Shell and Read|Grep|Glob →
//! `hook rewrite-enforce`), the rules block in `~/.cursor/rules/lean-ctx.mdc`
//! (Cursor's one LeanCTX rules channel) and no MCP server in
//! `~/.cursor/mcp.json`.

use std::path::{Path, PathBuf};

use super::governor_hosts::{
    GovernorClass, GovernorHostSpec, load_json, load_json_strict, mcp_file_has_lean_ctx,
    remove_mcp_entry, save_if_changed,
};

pub(crate) const SPEC: GovernorHostSpec = GovernorHostSpec {
    key: "cursor",
    name: "Cursor",
    class: GovernorClass::EnforceOnly,
    is_installed,
    has_standard_setup,
    touched_files,
    steps: &[
        ("hooks", apply_hooks),
        ("mcp", remove_mcp),
        ("rules", write_rules),
    ],
    revert,
    refresh: apply_hooks,
    mcp_config_path,
    switch_back: "lean-ctx init --agent cursor --mode hybrid",
};

const SHELL_MATCHER: &str = "Shell";
const FILE_MATCHER: &str = "Read|Grep|Glob";
/// Every action a standard or governor install may have written.
const LEAN_CTX_ACTIONS: &[&str] = &["rewrite", "redirect", "deny", "observe", "rewrite-enforce"];

// Detection is shared with the engine's MCP writer and rules injection.
use lean_ctx::core::governor_hosts::cursor::{
    has_entry, hooks_path, is_installed, mcp_config_path,
};

fn rules_path(home: &Path) -> PathBuf {
    home.join(".cursor/rules/lean-ctx.mdc")
}

fn touched_files(home: &Path) -> Vec<PathBuf> {
    vec![hooks_path(home), mcp_config_path(home), rules_path(home)]
}

/// True when a flat Cursor entry (`{matcher, command}`) is a LeanCTX hook
/// for one of `actions`.
fn is_lean_ctx_entry(entry: &serde_json::Value, actions: &[&str]) -> bool {
    lean_ctx::core::governor_hosts::is_lean_ctx_flat_entry(
        entry,
        lean_ctx::core::governor_hosts::cursor::COMMAND_KEYS,
        actions,
    )
}

fn has_standard_setup(home: &Path) -> bool {
    has_entry(home, &["rewrite", "redirect", "deny", "observe"])
        || mcp_file_has_lean_ctx(&mcp_config_path(home))
        || rules_path(home).exists()
}

fn strip_entries(hooks: &mut serde_json::Map<String, serde_json::Value>, actions: &[&str]) {
    for arr in hooks.values_mut().filter_map(|v| v.as_array_mut()) {
        arr.retain(|e| !is_lean_ctx_entry(e, actions));
    }
    super::governor_hosts::prune_empty_events(hooks);
}

fn apply_hooks(home: &Path) {
    let path = hooks_path(home);
    let Some(mut root) = load_json_strict(&path) else {
        tracing::warn!(
            "governor: {} is not valid JSON, left untouched",
            path.display()
        );
        return;
    };
    let before = root.clone();
    let enforce = crate::hooks::hook_shell_command(
        &super::super::resolve_hook_command_binary(),
        "rewrite-enforce",
    );
    if let Some(obj) = root.as_object_mut() {
        obj.insert("version".to_string(), serde_json::json!(1));
    }
    let Some(hooks) = super::governor_hosts::hooks_object(&mut root) else {
        return;
    };
    strip_entries(hooks, LEAN_CTX_ACTIONS);
    let pre = super::governor_hosts::event_array(hooks, "preToolUse");
    for matcher in [SHELL_MATCHER, FILE_MATCHER] {
        pre.push(serde_json::json!({ "matcher": matcher, "command": enforce }));
    }
    save_if_changed(&path, &before, &root);
}

fn remove_mcp(home: &Path) {
    remove_mcp_entry(
        "Cursor",
        "cursor",
        mcp_config_path(home),
        lean_ctx::core::editor_registry::ConfigType::McpJson,
    );
}

fn write_rules(home: &Path) {
    let body = format!(
        "{}\n{}\n{}",
        lean_ctx::core::rules_canonical::START_MARK,
        super::governor_hosts::rules_body(SPEC.class),
        lean_ctx::core::rules_canonical::END_MARK
    );
    let content = lean_ctx::rules_inject::cursor_mdc_document(&body);
    let path = rules_path(home);
    if std::fs::read_to_string(&path).ok().as_deref() != Some(content.as_str()) {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        super::super::write_file(&path, &content);
    }
}

fn revert(home: &Path) -> bool {
    if !is_installed(home) {
        return false;
    }
    let path = hooks_path(home);
    let mut root = load_json(&path);
    let before = root.clone();
    if let Some(hooks) = root.get_mut("hooks").and_then(|h| h.as_object_mut()) {
        strip_entries(hooks, &["rewrite-enforce"]);
    }
    save_if_changed(&path, &before, &root);
    let rules = rules_path(home);
    if std::fs::read_to_string(&rules)
        .is_ok_and(|c| c.contains(super::governor_hosts::GOVERNOR_MARKER))
    {
        let _ = std::fs::remove_file(rules);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::super::governor_hosts::{
        install,
        test_support::{TestEnv, read_json},
    };
    use super::*;

    #[test]
    fn governor_setup_writes_enforcement_rules_and_no_mcp() {
        let env = TestEnv::new();
        let home = env.path();
        std::fs::create_dir_all(home.join(".cursor")).unwrap();
        std::fs::write(
            hooks_path(home),
            r#"{"version":1,"hooks":{
                "preToolUse":[{"matcher":"Shell","command":"lean-ctx hook rewrite"},
                              {"matcher":"Read|Grep|Glob","command":"lean-ctx hook redirect"},
                              {"matcher":"Shell","command":"my-audit"}],
                "sessionStart":[{"command":"lean-ctx hook observe"}]}}"#,
        )
        .unwrap();
        std::fs::write(
            mcp_config_path(home),
            r#"{"mcpServers":{"lean-ctx":{"command":"lean-ctx"},"other":{"command":"x"}}}"#,
        )
        .unwrap();

        install(&SPEC, home);
        install(&SPEC, home);

        let hooks = read_json(&hooks_path(home));
        let pre = hooks["hooks"]["preToolUse"].as_array().unwrap();
        let cmds: Vec<(&str, &str)> = pre
            .iter()
            .map(|e| {
                (
                    e["matcher"].as_str().unwrap(),
                    e["command"].as_str().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            cmds,
            [
                ("Shell", "my-audit"),
                ("Shell", "lean-ctx hook rewrite-enforce"),
                ("Read|Grep|Glob", "lean-ctx hook rewrite-enforce"),
            ],
            "idempotent, foreign hook kept"
        );
        assert!(hooks["hooks"].get("sessionStart").is_none());
        assert!(hooks["hooks"].get("postToolUse").is_none(), "no post hook");

        let mcp = read_json(&mcp_config_path(home));
        assert!(mcp["mcpServers"].get("lean-ctx").is_none());
        assert!(mcp["mcpServers"].get("other").is_some());

        let rules = std::fs::read_to_string(rules_path(home)).unwrap();
        assert!(rules.contains("alwaysApply: true"));
        assert!(rules.contains(super::super::governor_hosts::GOVERNOR_MARKER));
        assert!(is_installed(home));

        assert!(revert(home));
        assert!(!is_installed(home));
        assert!(!rules_path(home).exists());
        assert!(!revert(home), "nothing left to revert");
    }
}
