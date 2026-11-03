// SPDX-License-Identifier: Apache-2.0

//! Governor setup for Gemini CLI — enforcement only.
//!
//! Gemini's `BeforeTool` hook takes new arguments from
//! `hookSpecificOutput.tool_input` and blocks on `decision: "deny"`; no hook
//! decision approves a call, and Gemini runs its own policy check and
//! confirmation on the modified arguments (`hook_handlers::gemini_output`).
//! `hook rewrite-enforce` therefore delivers the allowlist wrap only when the
//! original would have prompted anyway, and denies otherwise. Gemini's
//! `AfterTool` hook has no documented field that replaces a tool result
//! losslessly, so the governor installs no post-tool hook.
//!
//! Writes: `~/.gemini/settings.json` (`BeforeTool` shell and file tools →
//! `hook rewrite-enforce`; no `mcpServers.lean-ctx`) and the rules block in
//! `~/.gemini/GEMINI.md` (the dedicated `LEANCTX.md` channel is retired so
//! exactly one channel carries LeanCTX text). The Antigravity CLI (`agy`)
//! keeps its standard setup (MCP target + lean-ctx plugin, #284), which
//! `gemini` setup has always written alongside.

use std::path::{Path, PathBuf};

use super::governor_hosts::{
    GovernorClass, GovernorHostSpec, ensure_group_hook, has_group_hook, load_json,
    load_json_strict, mcp_file_has_lean_ctx, remove_mcp_entry, save_if_changed, strip_group_hooks,
};

pub(crate) const SPEC: GovernorHostSpec = GovernorHostSpec {
    key: "gemini",
    name: "Gemini CLI",
    class: GovernorClass::EnforceOnly,
    is_installed,
    has_standard_setup,
    touched_files,
    steps: &[
        ("hooks", apply_hooks),
        ("mcp", remove_mcp),
        ("rules", write_rules),
        // `gemini` setup has always configured the Antigravity CLI too (#284);
        // its hook dialect is unverified, so it keeps the standard setup.
        (
            "antigravity-cli",
            super::antigravity::install_antigravity_cli_for,
        ),
    ],
    revert,
    refresh: apply_hooks,
    mcp_config_path: settings_path,
    switch_back: "lean-ctx init --agent gemini --mode hybrid",
};

const SHELL_MATCHER: &str = "run_shell_command|shell|execute_command";
const FILE_MATCHER: &str =
    "read_file|read_many_files|search_file_content|grep|search|glob|list_files|list_dir";
const STANDARD_ACTIONS: &[&str] = &["hook rewrite", "hook redirect", "hook deny", "hook observe"];

// Detection is shared with the engine's MCP writer and rules injection.
use lean_ctx::core::governor_hosts::gemini::{is_installed, settings_path};

fn gemini_md(home: &Path) -> PathBuf {
    home.join(".gemini/GEMINI.md")
}

fn touched_files(home: &Path) -> Vec<PathBuf> {
    let plugin = super::antigravity::antigravity_cli_plugin_dir(home);
    vec![
        settings_path(home),
        gemini_md(home),
        lean_ctx::rules_inject::gemini_dedicated_rules_path(home),
        super::antigravity::antigravity_cli_config_dir(home).join("import_manifest.json"),
        home.join(".gemini/antigravity-cli/mcp_config.json"),
        plugin.join("plugin.json"),
        plugin.join("hooks/hooks.json"),
        plugin.join("mcp_config.json"),
    ]
}

fn has_standard_setup(home: &Path) -> bool {
    has_group_hook(
        &load_json(&settings_path(home)),
        &["rewrite", "redirect", "deny", "observe"],
    ) || mcp_file_has_lean_ctx(&settings_path(home))
        || lean_ctx::rules_inject::gemini_dedicated_rules_path(home).exists()
        || std::fs::read_to_string(gemini_md(home)).is_ok_and(|c| {
            lean_ctx::marked_block::contains_marker_line(
                &c,
                lean_ctx::core::rules_canonical::START_MARK,
            )
        })
}

fn apply_hooks(home: &Path) {
    let path = settings_path(home);
    let Some(mut root) = load_json_strict(&path) else {
        tracing::warn!(
            "governor: {} is not valid JSON, left untouched",
            path.display()
        );
        return;
    };
    let before = root.clone();
    let enforce = format!(
        "{} hook rewrite-enforce",
        super::super::resolve_hook_command_binary()
    );
    let Some(hooks) = super::governor_hosts::hooks_object(&mut root) else {
        return;
    };
    strip_group_hooks(hooks, STANDARD_ACTIONS);
    let before_tool = super::governor_hosts::event_array(hooks, "BeforeTool");
    // One action per group in `ensure_command_hook`; the two matchers need
    // two groups, so the file group is added by hand after the shell one.
    ensure_group_hook(before_tool, SHELL_MATCHER, &enforce, None);
    let hook = serde_json::json!({ "type": "command", "command": enforce });
    match before_tool
        .iter_mut()
        .find(|g| g.get("matcher").and_then(|m| m.as_str()) == Some(FILE_MATCHER))
        .and_then(|g| g.get_mut("hooks").and_then(|h| h.as_array_mut()))
    {
        Some(hooks) => hooks.insert(0, hook),
        None => before_tool.push(serde_json::json!({ "matcher": FILE_MATCHER, "hooks": [hook] })),
    }
    super::governor_hosts::prune_empty_events(hooks);
    save_if_changed(&path, &before, &root);
}

fn remove_mcp(home: &Path) {
    remove_mcp_entry(
        "Gemini CLI",
        "gemini",
        settings_path(home),
        lean_ctx::core::editor_registry::ConfigType::GeminiSettings,
    );
}

fn write_rules(home: &Path) {
    let dedicated = lean_ctx::rules_inject::gemini_dedicated_rules_path(home);
    if dedicated.exists() {
        let _ = std::fs::remove_file(&dedicated);
    }
    super::unregister_gemini_context_filename(home);
    super::governor_hosts::upsert_rules_block(
        &gemini_md(home),
        lean_ctx::core::rules_canonical::START_MARK,
        lean_ctx::core::rules_canonical::END_MARK,
        SPEC.class,
    );
}

fn revert(home: &Path) -> bool {
    if !is_installed(home) {
        return false;
    }
    let path = settings_path(home);
    let mut root = load_json(&path);
    let before = root.clone();
    if let Some(hooks) = root.get_mut("hooks").and_then(|h| h.as_object_mut()) {
        strip_group_hooks(hooks, &["hook rewrite-enforce"]);
        super::governor_hosts::prune_empty_events(hooks);
    }
    save_if_changed(&path, &before, &root);
    super::governor_hosts::remove_rules_block(
        &gemini_md(home),
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
    fn governor_setup_writes_before_tool_enforcement_rules_and_no_mcp() {
        let env = TestEnv::new();
        let home = env.path();
        std::fs::create_dir_all(home.join(".gemini")).unwrap();
        std::fs::write(
            settings_path(home),
            r#"{"theme":"dark",
                "mcpServers":{"lean-ctx":{"command":"lean-ctx"}},
                "hooks":{"BeforeTool":[
                    {"matcher":"run_shell_command","hooks":[{"type":"command","command":"lean-ctx hook rewrite"}]},
                    {"matcher":"x","hooks":[{"type":"command","command":"my-hook"}]}],
                  "AfterTool":[{"matcher":".*","hooks":[{"type":"command","command":"lean-ctx hook observe"}]}]}}"#,
        )
        .unwrap();
        std::fs::write(gemini_md(home), "# mine\n").unwrap();

        install(&SPEC, home);
        install(&SPEC, home);

        let s = read_json(&settings_path(home));
        assert_eq!(s["theme"], "dark");
        assert!(
            s.get("mcpServers")
                .is_none_or(|m| m.get("lean-ctx").is_none())
        );
        assert!(s["hooks"].get("AfterTool").is_none(), "no post hook: {s}");
        let groups = s["hooks"]["BeforeTool"].as_array().unwrap();
        let enforce: Vec<&str> = groups
            .iter()
            .filter(|g| g["hooks"][0]["command"] == "lean-ctx hook rewrite-enforce")
            .map(|g| g["matcher"].as_str().unwrap())
            .collect();
        assert_eq!(enforce, [SHELL_MATCHER, FILE_MATCHER], "idempotent: {s}");
        assert!(groups.iter().any(|g| g["hooks"][0]["command"] == "my-hook"));
        assert!(!s.to_string().contains("hook rewrite\""), "{s}");

        let md = std::fs::read_to_string(gemini_md(home)).unwrap();
        assert!(md.starts_with("# mine"));
        assert!(md.contains(super::super::governor_hosts::GOVERNOR_MARKER));

        assert!(revert(home));
        assert!(!is_installed(home));
        assert_eq!(
            std::fs::read_to_string(gemini_md(home)).unwrap(),
            "# mine\n"
        );
    }
}
