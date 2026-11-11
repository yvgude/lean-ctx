// SPDX-License-Identifier: Apache-2.0

//! Governor setup for Codex CLI — enforcement only.
//!
//! Codex's `PreToolUse` hook denies with `permissionDecision: "deny"` and
//! applies `updatedInput` only together with `"allow"`
//! (developers.openai.com/codex/hooks), so a rewrite is always an approval.
//! Its payload also looks like Claude Code's (`hook_event_name: PreToolUse`,
//! `transcript_path`), so the shared `hook rewrite-enforce` would answer in a
//! dialect Codex ignores. The governor therefore uses `hook codex-enforce`:
//! secret-path deny for Read/Grep, allowlist deny for Bash, otherwise Codex's
//! empty pass-through — never a grant, never a rewrite. Codex's `PostToolUse`
//! cannot replace a tool result, so no post-tool hook.
//!
//! Writes: `$CODEX_HOME/hooks.json` (`PreToolUse Bash|Read|Grep|Glob` →
//! `hook codex-enforce`), `config.toml` without `[mcp_servers.lean-ctx]` and
//! with `[features] hooks = true`, and the rules block in `AGENTS.md` (the
//! standard `LEAN-CTX.md` and `instructions.md` texts are retired so one
//! channel carries LeanCTX text).

use std::path::{Path, PathBuf};

use super::governor_hosts::{
    GovernorClass, GovernorHostSpec, ensure_group_hook, has_group_hook, load_json,
    load_json_strict, mcp_file_has_lean_ctx, remove_mcp_entry, save_if_changed, strip_group_hooks,
};

pub(crate) const SPEC: GovernorHostSpec = GovernorHostSpec {
    key: "codex",
    name: "Codex CLI",
    class: GovernorClass::EnforceOnly,
    is_installed,
    has_standard_setup,
    touched_files,
    steps: &[
        ("hooks", apply_hooks),
        ("config", apply_config),
        ("rules", write_rules),
    ],
    revert,
    refresh: apply_hooks,
    mcp_config_path: config_path,
    switch_back: "lean-ctx init --agent codex --mode hybrid",
};

const STANDARD_ACTIONS: &[&str] = &[
    "hook codex-pretooluse",
    "hook codex-session-start",
    "hook observe",
    "hook deny",
    "hook rewrite",
];

const AGENTS_START: &str = lean_ctx::core::rules_canonical::AGENTS_BLOCK_START;
const AGENTS_END: &str = lean_ctx::core::rules_canonical::AGENTS_BLOCK_END;

// Detection is shared with the engine's MCP writer and rules injection.
use lean_ctx::core::governor_hosts::codex::{codex_dir, config_path, hooks_path, is_installed};

fn agents_md(home: &Path) -> PathBuf {
    codex_dir(home).join("AGENTS.md")
}

fn touched_files(home: &Path) -> Vec<PathBuf> {
    let dir = codex_dir(home);
    vec![
        hooks_path(home),
        config_path(home),
        agents_md(home),
        dir.join("LEAN-CTX.md"),
        dir.join("instructions.md"),
    ]
}

fn has_standard_setup(home: &Path) -> bool {
    has_group_hook(
        &load_json(&hooks_path(home)),
        &["codex-pretooluse", "codex-session-start", "observe"],
    ) || mcp_file_has_lean_ctx(&config_path(home))
        || codex_dir(home).join("LEAN-CTX.md").exists()
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
        "codex-enforce",
    );
    let Some(hooks) = super::governor_hosts::hooks_object(&mut root) else {
        return;
    };
    strip_group_hooks(hooks, STANDARD_ACTIONS);
    // Same budget as `hook codex-pretooluse`: the hook fails open inside 15 s.
    ensure_group_hook(
        super::governor_hosts::event_array(hooks, "PreToolUse"),
        super::super::support::CODEX_PRE_TOOL_USE_MATCHER,
        &enforce,
        Some(15),
    );
    super::governor_hosts::prune_empty_events(hooks);
    save_if_changed(&path, &before, &root);
}

/// No MCP server; Codex runs hooks only with `[features] hooks = true`.
fn apply_config(home: &Path) {
    let path = config_path(home);
    remove_mcp_entry(
        "Codex CLI",
        "codex",
        path.clone(),
        lean_ctx::core::editor_registry::ConfigType::Codex,
    );
    let content = std::fs::read_to_string(&path).unwrap_or_default();
    if let Some(updated) = super::super::ensure_codex_hooks_enabled(&content) {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        super::super::write_file(&path, &updated);
    }
}

fn write_rules(home: &Path) {
    let dir = codex_dir(home);
    let lean_ctx_md = dir.join("LEAN-CTX.md");
    if lean_ctx_md.exists() {
        let _ = std::fs::remove_file(lean_ctx_md);
    }
    lean_ctx::marked_block::remove_from_file(
        &dir.join("instructions.md"),
        lean_ctx::core::rules_canonical::START_MARK,
        lean_ctx::core::rules_canonical::END_MARK,
        true,
        "Codex rules",
    );
    super::governor_hosts::upsert_rules_block(
        &agents_md(home),
        AGENTS_START,
        AGENTS_END,
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
        strip_group_hooks(hooks, &["hook codex-enforce"]);
        super::governor_hosts::prune_empty_events(hooks);
    }
    save_if_changed(&path, &before, &root);
    super::governor_hosts::remove_rules_block(&agents_md(home), AGENTS_START, AGENTS_END);
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
    fn governor_setup_writes_codex_enforce_hooks_rules_and_no_mcp() {
        let env = TestEnv::new();
        let home = env.path();
        let dir = home.join(".codex");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            hooks_path(home),
            r#"{"hooks":{
                "PreToolUse":[{"matcher":"Bash|Read|Grep|Glob","hooks":[{"type":"command","command":"lean-ctx hook codex-pretooluse","timeout":15}]}],
                "SessionStart":[{"matcher":"startup|resume|clear","hooks":[{"type":"command","command":"lean-ctx hook codex-session-start"}]}],
                "Stop":[{"hooks":[{"type":"command","command":"my-stop"}]}]}}"#,
        )
        .unwrap();
        std::fs::write(
            config_path(home),
            "model = \"o4\"\n\n[mcp_servers.lean-ctx]\ncommand = \"lean-ctx\"\n",
        )
        .unwrap();
        std::fs::write(dir.join("LEAN-CTX.md"), "full rules").unwrap();
        std::fs::write(
            agents_md(home),
            format!("# mine\n\n{AGENTS_START}\nsee LEAN-CTX.md\n{AGENTS_END}\n"),
        )
        .unwrap();

        install(&SPEC, home);
        install(&SPEC, home);

        let hooks = read_json(&hooks_path(home));
        let pre = hooks["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 1, "{hooks}");
        assert_eq!(pre[0]["hooks"][0]["command"], "lean-ctx hook codex-enforce");
        assert_eq!(pre[0]["hooks"][0]["timeout"], 15);
        assert!(hooks["hooks"].get("SessionStart").is_none());
        assert_eq!(hooks["hooks"]["Stop"][0]["hooks"][0]["command"], "my-stop");

        let config = std::fs::read_to_string(config_path(home)).unwrap();
        assert!(!config.contains("mcp_servers.lean-ctx"), "{config}");
        assert!(config.contains("model = \"o4\""));
        assert!(config.contains("hooks = true"), "{config}");

        assert!(!dir.join("LEAN-CTX.md").exists());
        let agents = std::fs::read_to_string(agents_md(home)).unwrap();
        assert!(agents.starts_with("# mine"));
        assert_eq!(agents.matches(AGENTS_START).count(), 1);
        assert!(!agents.contains("see LEAN-CTX.md"));

        assert!(revert(home));
        assert!(!is_installed(home));
        assert_eq!(
            std::fs::read_to_string(agents_md(home)).unwrap(),
            "# mine\n"
        );
    }
}
