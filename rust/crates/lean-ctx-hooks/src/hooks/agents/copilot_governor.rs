// SPDX-License-Identifier: Apache-2.0

//! Governor setup for GitHub Copilot CLI — enforcement only.
//!
//! Copilot CLI's `preToolUse` hook denies through a top-level
//! `permissionDecision: "deny"` and takes `modifiedArgs` only next to an
//! `"allow"` that also skips the prompt. `hook rewrite-enforce` therefore never
//! rewrites here: an allowlist violation is denied, everything else passes
//! through as `{}` ("empty output uses default behavior"). Copilot's
//! `postToolUse` output cannot replace a tool result, so no post-tool hook.
//!
//! Writes only Copilot CLI files: `$COPILOT_HOME/hooks/hooks.json`
//! (`preToolUse` → `hook rewrite-enforce`), no LeanCTX server in
//! `~/.copilot/mcp-config.json`, and the rules block in
//! `~/.copilot/instructions.md`. VS Code's own MCP entry (agent `vscode`, no
//! tool hooks) is left as it is.

use std::path::{Path, PathBuf};

use super::governor_hosts::{
    GovernorClass, GovernorHostSpec, load_json, load_json_strict, mcp_file_has_lean_ctx,
    remove_mcp_entry, save_if_changed,
};

pub(crate) const SPEC: GovernorHostSpec = GovernorHostSpec {
    key: "copilot",
    name: "Copilot CLI",
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
    switch_back: "lean-ctx init --agent copilot --mode hybrid",
};

const LEAN_CTX_ACTIONS: &[&str] = &[
    "rewrite",
    "redirect",
    "observe",
    "copilot",
    "rewrite-enforce",
];

// Detection is shared with the engine's MCP writer and rules injection.
use lean_ctx::core::governor_hosts::copilot::{
    has_entry, hooks_path, is_installed, mcp_config_path,
};

fn rules_path(home: &Path) -> PathBuf {
    home.join(".copilot/instructions.md")
}

fn touched_files(home: &Path) -> Vec<PathBuf> {
    vec![hooks_path(home), mcp_config_path(home), rules_path(home)]
}

/// True when a Copilot entry (`{type, bash, powershell, timeoutSec}`) is a
/// LeanCTX hook for one of `actions`.
fn is_lean_ctx_entry(entry: &serde_json::Value, actions: &[&str]) -> bool {
    lean_ctx::core::governor_hosts::is_lean_ctx_flat_entry(
        entry,
        lean_ctx::core::governor_hosts::copilot::COMMAND_KEYS,
        actions,
    )
}

fn has_standard_setup(home: &Path) -> bool {
    has_entry(home, &["rewrite", "redirect", "observe", "copilot"])
        || mcp_file_has_lean_ctx(&mcp_config_path(home))
        || std::fs::read_to_string(rules_path(home)).is_ok_and(|c| {
            lean_ctx::marked_block::contains_marker_line(
                &c,
                lean_ctx::core::rules_canonical::START_MARK,
            )
        })
}

fn strip_entries(hooks: &mut serde_json::Map<String, serde_json::Value>, actions: &[&str]) {
    for arr in hooks.values_mut().filter_map(|v| v.as_array_mut()) {
        arr.retain(|e| !is_lean_ctx_entry(e, actions));
    }
    super::governor_hosts::prune_empty_events(hooks);
}

/// Copilot runs `bash` on Unix and `powershell` on Windows (#381); both quote
/// the binary for install paths with spaces.
fn enforce_entry(binary: &str) -> serde_json::Value {
    let bash_binary = super::super::to_bash_compatible_path(binary);
    serde_json::json!({
        "type": "command",
        "bash": format!("\"{bash_binary}\" hook rewrite-enforce"),
        "powershell": format!("& \"{binary}\" hook rewrite-enforce"),
        // Same budget as `hook rewrite`: the hook fails open inside 15 s.
        "timeoutSec": 15
    })
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
    if let Some(obj) = root.as_object_mut() {
        obj.insert("version".to_string(), serde_json::json!(1));
    }
    let Some(hooks) = super::governor_hosts::hooks_object(&mut root) else {
        return;
    };
    strip_entries(hooks, LEAN_CTX_ACTIONS);
    super::governor_hosts::event_array(hooks, "preToolUse")
        .push(enforce_entry(&super::super::resolve_hook_command_binary()));
    save_if_changed(&path, &before, &root);
}

fn remove_mcp(home: &Path) {
    remove_mcp_entry(
        "Copilot CLI",
        "copilot",
        mcp_config_path(home),
        lean_ctx::core::editor_registry::ConfigType::CopilotCli,
    );
}

fn write_rules(home: &Path) {
    super::governor_hosts::upsert_rules_block(
        &rules_path(home),
        lean_ctx::core::rules_canonical::START_MARK,
        lean_ctx::core::rules_canonical::END_MARK,
        SPEC.class,
    );
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
    super::governor_hosts::remove_rules_block(
        &rules_path(home),
        lean_ctx::core::rules_canonical::START_MARK,
        lean_ctx::core::rules_canonical::END_MARK,
    );
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
    fn governor_setup_writes_pretooluse_enforcement_rules_and_no_mcp() {
        let env = TestEnv::new();
        let home = env.path();
        std::fs::create_dir_all(home.join(".copilot/hooks")).unwrap();
        std::fs::write(
            hooks_path(home),
            r#"{"version":1,"hooks":{
                "preToolUse":[{"type":"command","bash":"\"lean-ctx\" hook rewrite","powershell":"& \"lean-ctx\" hook rewrite","timeoutSec":15},
                              {"type":"command","bash":"my-check"}],
                "postToolUse":[{"type":"command","bash":"\"lean-ctx\" hook observe"}]}}"#,
        )
        .unwrap();
        std::fs::write(
            mcp_config_path(home),
            r#"{"mcpServers":{"lean-ctx":{"command":"lean-ctx"}}}"#,
        )
        .unwrap();

        install(&SPEC, home);
        install(&SPEC, home);

        let hooks = read_json(&hooks_path(home));
        let pre = hooks["hooks"]["preToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 2, "idempotent, foreign hook kept: {hooks}");
        assert_eq!(pre[0]["bash"], "my-check");
        assert_eq!(pre[1]["bash"], "\"lean-ctx\" hook rewrite-enforce");
        assert!(
            pre[1]["powershell"]
                .as_str()
                .unwrap()
                .ends_with("hook rewrite-enforce")
        );
        assert!(hooks["hooks"].get("postToolUse").is_none());
        assert!(
            read_json(&mcp_config_path(home))["mcpServers"]
                .get("lean-ctx")
                .is_none()
        );
        assert!(
            std::fs::read_to_string(rules_path(home))
                .unwrap()
                .contains(super::super::governor_hosts::GOVERNOR_MARKER)
        );

        assert!(revert(home));
        assert!(!is_installed(home));
        assert!(!rules_path(home).exists());
    }
}
