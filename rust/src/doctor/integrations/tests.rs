#[allow(clippy::wildcard_imports)]
use super::*;

#[test]
fn codex_chatgpt_proxy_artifact_detects_only_top_level_entries() {
    use CodexProxyState::*;
    // Incomplete ChatGPT subscription config is stale/broken.
    assert_eq!(
        classify_codex_proxy_entries("model_provider = \"leanctx-chatgpt\"\nmodel = \"gpt-5.5\"\n"),
        Artifact
    );
    assert_eq!(
        classify_codex_proxy_entries("model_provider = \"leanctx-chatgpt\"\n"),
        Artifact,
        "a provider pin without the backend rail is incomplete"
    );
    // backend-api override on the API-key rail breaks codex cloud/remote → artifact.
    assert_eq!(
        classify_codex_proxy_entries(
            "openai_base_url = \"http://127.0.0.1:8765/backend-api/codex\"\n"
        ),
        Artifact
    );
    // Bare chatgpt_base_url is incomplete; the generated provider block is
    // required with the top-level provider + rail.
    assert_eq!(
        classify_codex_proxy_entries("chatgpt_base_url = \"http://127.0.0.1:8765/backend-api/\"\n"),
        Artifact
    );
    assert_eq!(
        classify_codex_proxy_entries(
            "model_provider = \"leanctx-chatgpt\"\nchatgpt_base_url = \"http://127.0.0.1:8765/backend-api/\"\n"
        ),
        Artifact,
        "top-level pair without provider block is incomplete"
    );
    assert_eq!(
        classify_codex_proxy_entries(
            "model_provider = \"leanctx-chatgpt\"\nchatgpt_base_url = \"http://127.0.0.1:8765/backend-api/\"\n\n[model_providers.leanctx-chatgpt]\nbase_url = \"https://example.com/backend-api/codex\"\n"
        ),
        Artifact,
        "provider block must target the local proxy backend"
    );
    assert_eq!(
        classify_codex_proxy_entries(
            "model_provider = \"leanctx-chatgpt\"\nchatgpt_base_url = \"http://127.0.0.1:8765/backend-api/\"\n\n[model_providers.leanctx-chatgpt]\nbase_url = \"http://127.0.0.1:8765/backend-api/codex\"\n"
        ),
        OptInRouted
    );
    // Default provider → native.
    assert_eq!(
        classify_codex_proxy_entries("model_provider = \"openai\"\n"),
        Native
    );
    // API-key rail (/v1) is legitimate → native.
    assert_eq!(
        classify_codex_proxy_entries("openai_base_url = \"http://127.0.0.1:4444/v1\"\n"),
        Native
    );
    // A per-profile choice is the user's own → native.
    assert_eq!(
        classify_codex_proxy_entries(
            "model = \"gpt-5.5\"\n\n[profiles.proxy]\nmodel_provider = \"leanctx-chatgpt\"\n"
        ),
        Native
    );
}

#[test]
fn codex_desktop_note_is_informational_and_never_fails() {
    let note = codex_desktop_note();
    assert!(
        note.ok,
        "the Codex Desktop note is informational, never a failure"
    );
    assert!(
        note.detail.contains("ctx_shell") && note.detail.contains("every surface"),
        "note must steer users to the MCP tools as the reliable cross-surface path: {}",
        note.detail
    );
}

#[test]
fn antigravity_cli_hooks_note_is_informational_and_explains_gating() {
    let note = antigravity_cli_hooks_note();
    assert!(
        note.ok,
        "the Antigravity CLI gating note is informational, never a failure"
    );
    assert!(
        note.detail.contains("enable_json_hooks"),
        "note must name the server-side flag that gates hook execution: {}",
        note.detail
    );
    assert!(
        note.detail.contains("ctx_") && note.detail.contains("regardless"),
        "note must reassure that the MCP tools compress regardless: {}",
        note.detail
    );
}

#[test]
fn claude_flags_instructions_advertising_ctx_without_mcp_registration() {
    // GH #637 (second half) / GL #1139: a CLAUDE.md block advertising ctx_*
    // tools while no lean-ctx MCP server is registered strands the agent on
    // fallbacks that do not exist in the session. The combination must be
    // surfaced as its own failing check with a repair hint.
    let _lock = crate::core::data_dir::test_env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    // Hermetic config: the check consults rules_scope/rules_injection via
    // Config::load(); the developer's real config must not leak in.
    crate::test_env::set_var(
        "LEAN_CTX_CONFIG_DIR",
        home.join("cfg").to_string_lossy().to_string(),
    );
    let claude_dir = crate::core::editor_registry::claude_state_dir(home);
    std::fs::create_dir_all(&claude_dir).unwrap();
    std::fs::write(
        claude_dir.join("CLAUDE.md"),
        format!(
            "{}\n## lean-ctx\nctx_read guidance…\n{}\n",
            crate::hooks::agents::CLAUDE_MD_BLOCK_START,
            crate::core::rules_canonical::AGENTS_BLOCK_END
        ),
    )
    .unwrap();
    // No ~/.claude.json → MCP registration missing.

    let status = integration_claude(home, "/usr/local/bin/lean-ctx", "/tmp/data");
    let consistency = status
        .checks
        .iter()
        .find(|c| c.name == "Instructions/MCP consistency");
    let consistency =
        consistency.expect("block-without-MCP must produce the Instructions/MCP consistency check");
    assert!(!consistency.ok, "the combination is a failure, not a note");
    assert!(
        consistency.detail.contains("lean-ctx setup"),
        "detail must carry the repair hint: {}",
        consistency.detail
    );

    // With the MCP server registered the consistency check disappears.
    std::fs::write(
        crate::core::editor_registry::claude_mcp_json_path(home),
        r#"{"mcpServers":{"lean-ctx":{"command":"/usr/local/bin/lean-ctx","args":[]}}}"#,
    )
    .unwrap();
    let healthy = integration_claude(home, "/usr/local/bin/lean-ctx", "/tmp/data");
    assert!(
        !healthy
            .checks
            .iter()
            .any(|c| c.name == "Instructions/MCP consistency"),
        "registered MCP must suppress the consistency warning"
    );
    crate::test_env::remove_var("LEAN_CTX_CONFIG_DIR");
}

#[test]
fn hook_binary_refs_extracts_token_before_hook_keyword() {
    let content =
        r#"{"command": "/opt/lean-ctx hook rewrite"} {"command": "/opt/lean-ctx hook redirect"}"#;
    let refs = hook_binary_refs(content);
    assert_eq!(refs, vec!["/opt/lean-ctx", "/opt/lean-ctx"]);
}

#[test]
fn hook_binary_refs_empty_without_hook_invocation() {
    assert!(hook_binary_refs(r#"{"command": "echo nothing here"}"#).is_empty());
}

#[test]
fn hook_binary_refs_handles_minified_json() {
    // `serde_json::to_string` emits no spaces around keys/values; the binary
    // token must still be extracted cleanly. Regression: the whitespace-only
    // split used to capture the entire JSON prefix as the "binary".
    let content =
        r#"[{"hooks":[{"command":"lean-ctx hook rewrite"},{"command":"lean-ctx hook redirect"}]}]"#;
    assert_eq!(hook_binary_refs(content), vec!["lean-ctx", "lean-ctx"]);
}

#[test]
fn stale_hook_binary_accepts_minified_bare_command() {
    let content = r#"[{"hooks":[{"command":"lean-ctx hook rewrite"}]}]"#;
    assert!(stale_hook_binary(content, "/anything/lean-ctx").is_none());
}

#[test]
fn stale_hook_binary_flags_minified_foreign_path() {
    let content = r#"[{"hooks":[{"command":"/old/install/lean-ctx hook rewrite"}]}]"#;
    assert_eq!(
        stale_hook_binary(content, "/current/lean-ctx").as_deref(),
        Some("/old/install/lean-ctx")
    );
}

#[test]
fn stale_hook_binary_flags_foreign_path() {
    let content = r#""/nonexistent/old/lean-ctx hook rewrite""#;
    let stale = stale_hook_binary(content, "/current/install/lean-ctx");
    assert_eq!(stale.as_deref(), Some("/nonexistent/old/lean-ctx"));
}

#[test]
fn stale_hook_binary_accepts_current_binary() {
    let bin = "/current/install/lean-ctx";
    let content = format!(r#""{bin} hook rewrite""#);
    assert!(stale_hook_binary(&content, bin).is_none());
}

#[test]
fn stale_hook_binary_accepts_bare_path_command() {
    // The bare `lean-ctx` PATH form is always considered current.
    let content = r#""lean-ctx hook rewrite""#;
    assert!(stale_hook_binary(content, "/anything/lean-ctx").is_none());
}

/// #708: a configured verbatim hook binary ($HOME/... synced across machines)
/// is the expected command — doctor must not flag it stale, or --fix would
/// rewrite it back to an absolute path and restart the sync ping-pong.
#[test]
fn stale_hook_binary_accepts_configured_portable_override() {
    let _lock = crate::core::data_dir::test_env_lock();
    // SAFETY: serialized by test_env_lock.
    unsafe { std::env::set_var("LEAN_CTX_HOOK_BINARY", "$HOME/.local/bin/lean-ctx") };
    let content = r#""$HOME/.local/bin/lean-ctx hook rewrite""#;
    assert!(stale_hook_binary(content, "/machine/abs/lean-ctx").is_none());
    // SAFETY: serialized by test_env_lock.
    unsafe { std::env::remove_var("LEAN_CTX_HOOK_BINARY") };

    // Without the override the same content IS stale — the acceptance is
    // strictly opt-in.
    assert_eq!(
        stale_hook_binary(content, "/machine/abs/lean-ctx").as_deref(),
        Some("$HOME/.local/bin/lean-ctx")
    );
}

#[test]
fn finalize_hook_check_reports_drift_missing_and_stale() {
    let p = std::path::Path::new("/tmp/hooks.json");

    let missing = finalize_hook_check("Hooks", p, false, None);
    assert!(!missing.ok);
    assert!(missing.detail.contains("drift"));

    let stale = finalize_hook_check("Hooks", p, true, Some("/old/lean-ctx".to_string()));
    assert!(!stale.ok);
    assert!(stale.detail.contains("stale binary"));
    assert!(stale.detail.contains("setup --fix"));

    let healthy = finalize_hook_check("Hooks", p, true, None);
    assert!(healthy.ok);
    assert!(healthy.detail.contains("ok"));
}

#[test]
fn check_antigravity_cli_verifies_self_contained_bundle() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let plugin_dir = crate::hooks::agents::antigravity_cli_plugin_dir(home);
    std::fs::create_dir_all(plugin_dir.join("hooks")).unwrap();

    std::fs::write(
        plugin_dir.join("plugin.json"),
        r#"{"name":"lean-ctx","version":"0.0.1"}"#,
    )
    .unwrap();
    std::fs::write(
        plugin_dir.join("hooks").join("hooks.json"),
        r#"{"hooks":{"PostToolUse":[{"matcher":"*","hooks":[{"type":"command","command":"lean-ctx hook observe"}]}]}}"#,
    )
    .unwrap();
    // The self-contained, spec-compliant piece (#284): plugin-local MCP config.
    std::fs::write(
        plugin_dir.join("mcp_config.json"),
        r#"{"mcpServers":{"lean-ctx":{"command":"lean-ctx"}}}"#,
    )
    .unwrap();
    let cfg_dir = crate::hooks::agents::antigravity_cli_config_dir(home);
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::write(
        cfg_dir.join("import_manifest.json"),
        r#"{"imports":[{"name":"lean-ctx"}]}"#,
    )
    .unwrap();

    let full = check_antigravity_cli_hooks(home, "lean-ctx");
    assert!(
        full.ok,
        "full self-contained bundle must pass: {}",
        full.detail
    );

    // Drop the plugin-local mcp_config.json -> the check must fail and name it
    // (so `doctor --fix`, which re-runs the installer, knows what to repair).
    std::fs::remove_file(plugin_dir.join("mcp_config.json")).unwrap();
    let drift = check_antigravity_cli_hooks(home, "lean-ctx");
    assert!(!drift.ok, "missing plugin-local mcp_config.json must fail");
    assert!(
        drift.detail.contains("mcp_config.json"),
        "detail must point at the missing mcp_config.json: {}",
        drift.detail
    );
}

#[test]
fn check_cursor_hooks_detects_stale_binary() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("hooks.json");
    std::fs::write(
        &path,
        r#"{
  "hooks": {
"preToolUse": [
  { "matcher": "Shell", "command": "/old/bin/lean-ctx hook rewrite" },
  { "matcher": "Read|Grep", "command": "/old/bin/lean-ctx hook redirect" }
]
  }
}"#,
    )
    .unwrap();
    let check = check_cursor_hooks(&path, "/new/bin/lean-ctx");
    assert!(!check.ok, "stale binary path must fail the hook check");
    assert!(check.detail.contains("stale binary"));
}

#[test]
fn check_cursor_hooks_ok_for_bare_command() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("hooks.json");
    std::fs::write(
        &path,
        r#"{
  "hooks": {
"preToolUse": [
  { "matcher": "Shell", "command": "lean-ctx hook rewrite" },
  { "matcher": "Read|Grep", "command": "lean-ctx hook redirect" }
]
  }
}"#,
    )
    .unwrap();
    let check = check_cursor_hooks(&path, "/new/bin/lean-ctx");
    assert!(
        check.ok,
        "bare lean-ctx command is PATH-resolved and current"
    );
}

#[test]
fn check_codex_toml_detects_missing_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    let check = check_codex_toml(&path, "lean-ctx");
    assert!(!check.ok);
    assert!(check.detail.contains("missing"));
}

#[test]
fn check_codex_toml_detects_legacy_empty_args_drift() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        "[mcp_servers.lean-ctx]\ncommand = \"lean-ctx\"\nargs = []\n",
    )
    .unwrap();
    let check = check_codex_toml(&path, "lean-ctx");
    assert!(!check.ok);
    assert!(check.detail.contains("drift"));
}

#[test]
fn check_codex_toml_detects_missing_args_drift() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "[mcp_servers.lean-ctx]\ncommand = \"lean-ctx\"\n").unwrap();
    let check = check_codex_toml(&path, "lean-ctx");
    assert!(!check.ok);
    assert!(check.detail.contains("drift"));
}

#[test]
fn check_codex_toml_ok_with_mcp_arg() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        "[mcp_servers.lean-ctx]\ncommand = \"lean-ctx\"\nargs = [\"mcp\"]\n",
    )
    .unwrap();
    let check = check_codex_toml(&path, "lean-ctx");
    assert!(check.ok);
    assert!(check.detail.contains("ok"));
}
