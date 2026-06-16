use super::shared::{handle_invalid_json_write, try_text_inject_mcp_entry};
use super::*;
use serde_json::Value;
use std::path::PathBuf;

fn target(name: &'static str, path: PathBuf, ty: ConfigType) -> EditorTarget {
    EditorTarget {
        name,
        agent_key: "test".to_string(),
        config_path: path,
        detect_path: PathBuf::from("/nonexistent"),
        config_type: ty,
    }
}

#[test]
fn mcp_json_upserts_and_preserves_other_servers_without_auto_approve() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcp.json");
    std::fs::write(
            &path,
            r#"{ "mcpServers": { "other": { "command": "other-bin" }, "lean-ctx": { "command": "/old/path/lean-ctx", "autoApprove": [] } } }"#,
        )
        .unwrap();

    let t = target("test", path.clone(), ConfigType::McpJson);
    let res = write_mcp_json(&t, "/new/path/lean-ctx", WriteOptions::default()).unwrap();
    assert_eq!(res.action, WriteAction::Updated);

    let json: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(json["mcpServers"]["other"]["command"], "other-bin");
    assert_eq!(
        json["mcpServers"]["lean-ctx"]["command"],
        "/new/path/lean-ctx"
    );
    assert!(json["mcpServers"]["lean-ctx"].get("autoApprove").is_none());
}

#[test]
fn mcp_json_upserts_and_preserves_other_servers_with_auto_approve_for_cursor() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcp.json");
    std::fs::write(
            &path,
            r#"{ "mcpServers": { "other": { "command": "other-bin" }, "lean-ctx": { "command": "/old/path/lean-ctx", "autoApprove": [] } } }"#,
        )
        .unwrap();

    let t = target("Cursor", path.clone(), ConfigType::McpJson);
    let res = write_mcp_json(&t, "/new/path/lean-ctx", WriteOptions::default()).unwrap();
    assert_eq!(res.action, WriteAction::Updated);

    let json: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(json["mcpServers"]["other"]["command"], "other-bin");
    assert_eq!(
        json["mcpServers"]["lean-ctx"]["command"],
        "/new/path/lean-ctx"
    );
    assert!(json["mcpServers"]["lean-ctx"]["autoApprove"].is_array());
    assert!(
        json["mcpServers"]["lean-ctx"]["autoApprove"]
            .as_array()
            .unwrap()
            .len()
            > 5
    );
}

#[test]
fn crush_config_writes_mcp_root() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("crush.json");
    std::fs::write(
        &path,
        r#"{ "mcp": { "lean-ctx": { "type": "stdio", "command": "old" } } }"#,
    )
    .unwrap();

    let t = target("test", path.clone(), ConfigType::Crush);
    let res = write_crush_config(&t, "new", WriteOptions::default()).unwrap();
    assert_eq!(res.action, WriteAction::Updated);

    let json: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(json["mcp"]["lean-ctx"]["type"], "stdio");
    assert_eq!(json["mcp"]["lean-ctx"]["command"], "new");
}

#[test]
fn codex_toml_upserts_existing_section() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        r#"[mcp_servers.lean-ctx]
command = "old"
args = ["x"]
"#,
    )
    .unwrap();

    let t = target("test", path.clone(), ConfigType::Codex);
    let res = write_codex_config(&t, "new").unwrap();
    assert_eq!(res.action, WriteAction::Updated);

    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains(r#"command = "new""#));
    assert!(content.contains("args = []"));
}

#[test]
fn upsert_codex_toml_inserts_new_section_when_missing() {
    let updated = upsert_codex_toml("[other]\nx=1\n", "lean-ctx");
    assert!(updated.contains("[mcp_servers.lean-ctx]"));
    assert!(updated.contains("command = \"lean-ctx\""));
    assert!(updated.contains("args = []"));
}

#[test]
fn codex_toml_uses_single_quotes_for_backslash_paths() {
    let win_path = r"C:\Users\Foo\AppData\Roaming\npm\lean-ctx.cmd";
    let updated = upsert_codex_toml("", win_path);
    assert!(
        updated.contains(&format!("command = '{win_path}'")),
        "Windows paths must use TOML single quotes to avoid backslash escapes: {updated}"
    );
}

#[test]
fn codex_toml_uses_double_quotes_for_unix_paths() {
    let unix_path = "/usr/local/bin/lean-ctx";
    let updated = upsert_codex_toml("", unix_path);
    assert!(
        updated.contains(&format!("command = \"{unix_path}\"")),
        "Unix paths should use double quotes: {updated}"
    );
}

#[test]
fn upsert_codex_toml_inserts_parent_before_orphaned_tool_subtables() {
    let input = "\
[mcp_servers.lean-ctx.tools.ctx_multi_read]
approval_mode = \"approve\"

[mcp_servers.lean-ctx.tools.ctx_read]
approval_mode = \"approve\"
";
    let updated = upsert_codex_toml(input, "lean-ctx");
    let parent_pos = updated
        .find("[mcp_servers.lean-ctx]\n")
        .expect("parent section must be inserted");
    let tools_pos = updated
        .find("[mcp_servers.lean-ctx.tools.")
        .expect("tool sub-tables must be preserved");
    assert!(
        parent_pos < tools_pos,
        "parent must come before tool sub-tables:\n{updated}"
    );
    assert!(updated.contains("command = \"lean-ctx\""));
    assert!(updated.contains("args = []"));
    assert!(updated.contains("approval_mode = \"approve\""));
}

#[test]
fn upsert_codex_toml_handles_issue_191_windows_scenario() {
    let input = "\
[mcp_servers.lean-ctx.tools.ctx_multi_read]
approval_mode = \"approve\"

[mcp_servers.lean-ctx.tools.ctx_read]
approval_mode = \"approve\"

[mcp_servers.lean-ctx.tools.ctx_search]
approval_mode = \"approve\"

[mcp_servers.lean-ctx.tools.ctx_tree]
approval_mode = \"approve\"
";
    let win_path = r"C:\Users\wudon\AppData\Roaming\npm\lean-ctx.cmd";
    let updated = upsert_codex_toml(input, win_path);
    assert!(
        updated.contains(&format!("command = '{win_path}'")),
        "Windows path must use single quotes: {updated}"
    );
    let parent_pos = updated.find("[mcp_servers.lean-ctx]\n").unwrap();
    let first_tool = updated.find("[mcp_servers.lean-ctx.tools.").unwrap();
    assert!(parent_pos < first_tool);
    assert_eq!(
        updated.matches("[mcp_servers.lean-ctx]\n").count(),
        1,
        "parent section must appear exactly once"
    );
}

#[test]
fn upsert_codex_toml_does_not_duplicate_parent_when_present() {
    let input = "\
[mcp_servers.lean-ctx]
command = \"old\"
args = [\"x\"]

[mcp_servers.lean-ctx.tools.ctx_read]
approval_mode = \"approve\"
";
    let updated = upsert_codex_toml(input, "new");
    assert_eq!(
        updated.matches("[mcp_servers.lean-ctx]").count(),
        1,
        "must not duplicate parent section"
    );
    assert!(updated.contains("command = \"new\""));
    assert!(updated.contains("args = []"));
    assert!(updated.contains("approval_mode = \"approve\""));
}

#[test]
fn auto_approve_contains_core_tools() {
    let tools = auto_approve_tools();
    assert!(tools.contains(&"ctx_read"));
    assert!(tools.contains(&"ctx_shell"));
    assert!(tools.contains(&"ctx_search"));
    assert!(tools.contains(&"ctx_workflow"));
    assert!(tools.contains(&"ctx_cost"));
}

#[test]
fn qoder_mcp_config_preserves_probe_and_upserts_lean_ctx() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcp.json");
    std::fs::write(
            &path,
            r#"{ "mcpServers": { "lean-ctx-probe": { "command": "cmd", "args": ["/C", "echo", "lean-ctx-probe"] } } }"#,
        )
        .unwrap();

    let t = target("Qoder", path.clone(), ConfigType::QoderSettings);
    let res = write_qoder_settings(&t, "lean-ctx", WriteOptions::default()).unwrap();
    assert_eq!(res.action, WriteAction::Updated);

    let json: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(json["mcpServers"]["lean-ctx-probe"]["command"], "cmd");
    assert_eq!(json["mcpServers"]["lean-ctx"]["command"], "lean-ctx");
    assert_eq!(
        json["mcpServers"]["lean-ctx"]["args"],
        serde_json::json!([])
    );
    assert!(
        json["mcpServers"]["lean-ctx"]["env"]["LEAN_CTX_DATA_DIR"]
            .as_str()
            .is_some_and(|s| !s.trim().is_empty())
    );
    assert!(json["mcpServers"]["lean-ctx"]["identifier"].is_null());
    assert!(json["mcpServers"]["lean-ctx"]["source"].is_null());
    assert!(json["mcpServers"]["lean-ctx"]["version"].is_null());
}

#[test]
fn qoder_mcp_config_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcp.json");
    let t = target("Qoder", path.clone(), ConfigType::QoderSettings);

    let first = write_qoder_settings(&t, "lean-ctx", WriteOptions::default()).unwrap();
    let second = write_qoder_settings(&t, "lean-ctx", WriteOptions::default()).unwrap();

    assert_eq!(first.action, WriteAction::Created);
    assert_eq!(second.action, WriteAction::Already);
}

#[test]
fn qoder_mcp_config_creates_missing_parent_directories() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir
        .path()
        .join("Library/Application Support/Qoder/SharedClientCache/mcp.json");
    let t = target("Qoder", path.clone(), ConfigType::QoderSettings);

    let res = write_config_with_options(&t, "lean-ctx", WriteOptions::default()).unwrap();

    assert_eq!(res.action, WriteAction::Created);
    let json: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(json["mcpServers"]["lean-ctx"]["command"], "lean-ctx");
}

#[test]
fn antigravity_config_omits_auto_approve() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcp_config.json");

    let t = EditorTarget {
        name: "Antigravity",
        agent_key: "antigravity".to_string(),
        config_path: path.clone(),
        detect_path: PathBuf::from("/nonexistent"),
        config_type: ConfigType::McpJson,
    };
    let res = write_mcp_json(&t, "/usr/local/bin/lean-ctx", WriteOptions::default()).unwrap();
    assert_eq!(res.action, WriteAction::Created);

    let json: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(json["mcpServers"]["lean-ctx"]["autoApprove"].is_null());
    assert_eq!(
        json["mcpServers"]["lean-ctx"]["command"],
        "/usr/local/bin/lean-ctx"
    );
}

#[test]
fn hermes_yaml_inserts_into_existing_mcp_servers() {
    let existing = "model: anthropic/claude-sonnet-4\n\nmcp_servers:\n  github:\n    command: \"npx\"\n    args: [\"-y\", \"@modelcontextprotocol/server-github\"]\n\ntool_allowlist:\n  - terminal\n";
    let block = "  lean-ctx:\n    command: \"lean-ctx\"\n    env:\n      LEAN_CTX_DATA_DIR: \"/home/user/.lean-ctx\"";
    let result = upsert_hermes_yaml_mcp(existing, block);
    assert!(result.contains("lean-ctx"));
    assert!(result.contains("model: anthropic/claude-sonnet-4"));
    assert!(result.contains("tool_allowlist:"));
    assert!(result.contains("github:"));
}

#[test]
fn hermes_yaml_creates_mcp_servers_section() {
    let existing = "model: openai/gpt-4o\n";
    let block = "  lean-ctx:\n    command: \"lean-ctx\"";
    let result = upsert_hermes_yaml_mcp(existing, block);
    assert!(result.contains("mcp_servers:"));
    assert!(result.contains("lean-ctx"));
    assert!(result.contains("model: openai/gpt-4o"));
}

#[test]
fn hermes_yaml_skips_if_already_present() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.yaml");
    let data_dir = crate::core::data_dir::lean_ctx_data_dir()
        .map(|d| d.to_string_lossy().to_string())
        .unwrap_or_default();
    std::fs::write(
            &path,
            format!("mcp_servers:\n  lean-ctx:\n    command: \"lean-ctx\"\n    env:\n      LEAN_CTX_DATA_DIR: \"{data_dir}\"\n"),
        )
        .unwrap();
    let t = target("test", path.clone(), ConfigType::HermesYaml);
    let res = write_hermes_yaml(&t, "lean-ctx", WriteOptions::default()).unwrap();
    assert_eq!(res.action, WriteAction::Already);
}

#[test]
fn remove_codex_section_also_removes_env_subtable() {
    let input = "\
[other]
x = 1

[mcp_servers.lean-ctx]
args = []
command = \"/usr/local/bin/lean-ctx\"

[mcp_servers.lean-ctx.env]
LEAN_CTX_DATA_DIR = \"/home/user/.lean-ctx\"

[features]
codex_hooks = true
";
    let result = remove_codex_toml_section(input, "[mcp_servers.lean-ctx]");
    assert!(
        !result.contains("[mcp_servers.lean-ctx]"),
        "parent section must be removed"
    );
    assert!(
        !result.contains("LEAN_CTX_DATA_DIR"),
        "env sub-table must be removed too"
    );
    assert!(result.contains("[other]"), "unrelated sections preserved");
    assert!(
        result.contains("[features]"),
        "sections after must be preserved"
    );
}

#[test]
fn remove_codex_section_preserves_other_mcp_servers() {
    let input = "\
[mcp_servers.lean-ctx]
command = \"lean-ctx\"

[mcp_servers.lean-ctx.env]
X = \"1\"

[mcp_servers.other]
command = \"other\"
";
    let result = remove_codex_toml_section(input, "[mcp_servers.lean-ctx]");
    assert!(!result.contains("[mcp_servers.lean-ctx]"));
    assert!(
        result.contains("[mcp_servers.other]"),
        "other MCP servers must be preserved"
    );
    assert!(result.contains("command = \"other\""));
}

#[test]
fn remove_codex_section_does_not_remove_similarly_named_server() {
    let input = "\
[mcp_servers.lean-ctx]
command = \"lean-ctx\"

[mcp_servers.lean-ctx-probe]
command = \"probe\"
";
    let result = remove_codex_toml_section(input, "[mcp_servers.lean-ctx]");
    assert!(
        !result.contains("[mcp_servers.lean-ctx]\n"),
        "target section must be removed"
    );
    assert!(
        result.contains("[mcp_servers.lean-ctx-probe]"),
        "similarly-named server must NOT be removed"
    );
    assert!(result.contains("command = \"probe\""));
}

#[test]
fn remove_codex_section_handles_no_match() {
    let input = "[other]\nx = 1\n";
    let result = remove_codex_toml_section(input, "[mcp_servers.lean-ctx]");
    assert_eq!(result, "[other]\nx = 1\n");
}

#[test]
fn text_inject_into_existing_mcp_object() {
    let content = r#"{
  "mcp": {}
}"#;
    let value = serde_json::json!({"type": "local", "command": ["lean-ctx"]});
    let result = try_text_inject_mcp_entry(content, "mcp", "lean-ctx", &value);
    assert!(result.is_some());
    let patched = result.unwrap();
    assert!(patched.contains("\"lean-ctx\""));
    assert!(patched.contains("\"type\": \"local\""));
}

#[test]
fn text_inject_creates_container_when_missing() {
    let content = r#"{
  "some_other_key": "value"
}"#;
    let value = serde_json::json!({"command": "lean-ctx"});
    // For mcpServers container
    let result = try_text_inject_mcp_entry(content, "mcpServers", "lean-ctx", &value);
    assert!(result.is_some());
    let patched = result.unwrap();
    assert!(patched.contains("\"mcpServers\""));
    assert!(patched.contains("\"lean-ctx\""));

    // For mcp container (OpenCode)
    let result2 = try_text_inject_mcp_entry(content, "mcp", "lean-ctx", &value);
    assert!(result2.is_some());
    let patched2 = result2.unwrap();
    assert!(patched2.contains("\"mcp\""));
    assert!(patched2.contains("\"lean-ctx\""));

    // For context_servers container (Zed)
    let result3 = try_text_inject_mcp_entry(content, "context_servers", "lean-ctx", &value);
    assert!(result3.is_some());
    let patched3 = result3.unwrap();
    assert!(patched3.contains("\"context_servers\""));
    assert!(patched3.contains("\"lean-ctx\""));
}

#[test]
fn text_inject_into_populated_mcp_object() {
    let content = r#"{
  "mcp": {
    "other-server": {"type": "local"}
  }
}"#;
    let value = serde_json::json!({"type": "local", "command": ["lean-ctx"]});
    let result = try_text_inject_mcp_entry(content, "mcp", "lean-ctx", &value);
    assert!(result.is_some());
    let patched = result.unwrap();
    assert!(patched.contains("\"lean-ctx\""));
    assert!(patched.contains("\"other-server\""));
}

#[test]
fn handle_invalid_json_skips_when_entry_already_present() {
    let content = r#"{ invalid json "lean-ctx": stuff }"#;
    let value = serde_json::json!({"type": "local"});
    let result = handle_invalid_json_write(
        std::path::Path::new("/tmp/test.json"),
        content,
        "mcp",
        "lean-ctx",
        &value,
        true,
    );
    assert!(result.is_ok());
    let r = result.unwrap();
    assert_eq!(r.action, WriteAction::Already);
}

#[test]
fn handle_invalid_json_returns_error_when_inject_disabled() {
    let content = r"{ invalid json without key }";
    let value = serde_json::json!({"type": "local"});
    let result = handle_invalid_json_write(
        std::path::Path::new("/tmp/test.json"),
        content,
        "mcp",
        "lean-ctx",
        &value,
        false,
    );
    assert!(result.is_err());
}

#[test]
fn handle_invalid_json_does_not_overwrite_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("opencode.json");
    let invalid_content = r#"{ "mcp": { BROKEN "other": true } }"#;
    std::fs::write(&path, invalid_content).unwrap();

    let value = serde_json::json!({"type": "local", "command": ["lean-ctx"]});
    let result = handle_invalid_json_write(&path, invalid_content, "mcp", "lean-ctx", &value, true);
    assert!(result.is_ok());
    let r = result.unwrap();
    assert_eq!(r.action, WriteAction::Updated);

    // Original file should still exist (not deleted/renamed)
    let final_content = std::fs::read_to_string(&path).unwrap();
    assert!(
        final_content.contains("lean-ctx"),
        "lean-ctx should be injected"
    );
    assert!(
        final_content.contains("BROKEN"),
        "original content preserved"
    );
}

// -----------------------------------------------------------------------
// Augment VS Code extension (top-level JSON array) writer/remover tests
// -----------------------------------------------------------------------

#[test]
fn augment_vscode_creates_array_with_lean_ctx_entry() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcpServers.json");
    let t = target("Augment (VS Code)", path.clone(), ConfigType::AugmentVsCode);

    let res = write_config_with_options(&t, "lean-ctx", WriteOptions::default()).unwrap();
    assert_eq!(res.action, WriteAction::Created);

    let arr: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let entries = arr.as_array().expect("top-level must be array");
    assert_eq!(entries.len(), 1);
    let e = &entries[0];
    assert_eq!(e["name"], "lean-ctx");
    assert_eq!(e["type"], "stdio");
    assert_eq!(e["command"], "lean-ctx");
    assert_eq!(e["disabled"], false);
    assert_eq!(e["useShellInterpolation"], false);
    assert!(e["id"].as_str().is_some());
    assert!(e["env"]["LEAN_CTX_DATA_DIR"].as_str().is_some());
}

#[test]
fn augment_vscode_preserves_existing_entries_and_upserts() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcpServers.json");
    std::fs::write(
            &path,
            r#"[{"type":"stdio","id":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","name":"github","disabled":false,"command":"gh-mcp","args":[],"env":{}}]"#,
        )
        .unwrap();

    let t = target("Augment (VS Code)", path.clone(), ConfigType::AugmentVsCode);
    let res = write_config_with_options(&t, "lean-ctx", WriteOptions::default()).unwrap();
    assert_eq!(res.action, WriteAction::Updated);

    let arr: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let entries = arr.as_array().unwrap();
    assert_eq!(entries.len(), 2, "github entry must be preserved");
    assert!(entries.iter().any(|e| e["name"] == "github"));
    assert!(entries.iter().any(|e| e["name"] == "lean-ctx"));
}

#[test]
fn augment_vscode_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcpServers.json");
    let t = target("Augment (VS Code)", path.clone(), ConfigType::AugmentVsCode);

    let first = write_config_with_options(&t, "lean-ctx", WriteOptions::default()).unwrap();
    let second = write_config_with_options(&t, "lean-ctx", WriteOptions::default()).unwrap();
    assert_eq!(first.action, WriteAction::Created);
    assert_eq!(second.action, WriteAction::Already);

    let arr: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let entries = arr.as_array().unwrap();
    assert_eq!(
        entries.iter().filter(|e| e["name"] == "lean-ctx").count(),
        1,
        "lean-ctx must not duplicate"
    );
}

#[test]
fn augment_vscode_remove_only_drops_lean_ctx_entry() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcpServers.json");
    let t = target("Augment (VS Code)", path.clone(), ConfigType::AugmentVsCode);

    // Seed: github + lean-ctx (via the writer so the id matches).
    std::fs::write(
            &path,
            r#"[{"type":"stdio","id":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","name":"github","disabled":false,"command":"gh-mcp","args":[],"env":{}}]"#,
        )
        .unwrap();
    write_config_with_options(&t, "lean-ctx", WriteOptions::default()).unwrap();

    let res = remove_lean_ctx_server(&t, WriteOptions::default()).unwrap();
    assert_eq!(res.action, WriteAction::Updated);

    let arr: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let entries = arr.as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["name"], "github");
}

#[test]
fn augment_vscode_remove_is_noop_when_missing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcpServers.json");
    std::fs::write(&path, "[]").unwrap();
    let t = target("Augment (VS Code)", path.clone(), ConfigType::AugmentVsCode);

    let res = remove_lean_ctx_server(&t, WriteOptions::default()).unwrap();
    assert_eq!(res.action, WriteAction::Already);
}

// ---------------------------------------------------------------------------
// OpenClaw (GitHub #390): nested mcp.servers schema since 2026.6.1, legacy
// mcpServers migration, version detection, idempotency, dual-schema removal.
// ---------------------------------------------------------------------------

#[test]
fn openclaw_fresh_write_uses_nested_schema() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("openclaw.json");
    let t = target("OpenClaw", path.clone(), ConfigType::OpenClaw);

    let res = write_config_with_options(&t, "lean-ctx", WriteOptions::default()).unwrap();
    assert_eq!(res.action, WriteAction::Created);

    let json: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(json["mcp"]["servers"]["lean-ctx"]["command"], "lean-ctx");
    assert!(
        json.get("mcpServers").is_none(),
        "fresh write must never produce the legacy camelCase key"
    );
}

#[test]
fn openclaw_migrates_legacy_camelcase_entry() {
    // The reporter's exact scenario: a manually migrated mcp.servers block
    // exists, but our old writer re-injected mcpServers. After the fix a
    // re-run must converge to a single nested block.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("openclaw.json");
    std::fs::write(
        &path,
        r#"{
            "meta": { "lastTouchedVersion": "2026.6.1" },
            "mcpServers": { "lean-ctx": { "command": "/old/lean-ctx" } },
            "mcp": { "servers": { "lean-ctx": { "command": "/old/lean-ctx" } } }
        }"#,
    )
    .unwrap();
    let t = target("OpenClaw", path.clone(), ConfigType::OpenClaw);

    let res = write_config_with_options(&t, "lean-ctx", WriteOptions::default()).unwrap();
    assert_eq!(res.action, WriteAction::Updated);

    let json: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(
        json.get("mcpServers").is_none(),
        "legacy duplicate block must be removed — it breaks the 2026.6.1 validator"
    );
    assert_eq!(json["mcp"]["servers"]["lean-ctx"]["command"], "lean-ctx");
    assert_eq!(json["meta"]["lastTouchedVersion"], "2026.6.1");
}

#[test]
fn openclaw_migration_preserves_foreign_legacy_servers() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("openclaw.json");
    std::fs::write(
        &path,
        r#"{
            "meta": { "lastTouchedVersion": "2026.6.2" },
            "mcpServers": {
                "lean-ctx": { "command": "/old/lean-ctx" },
                "github": { "command": "gh-mcp" }
            }
        }"#,
    )
    .unwrap();
    let t = target("OpenClaw", path.clone(), ConfigType::OpenClaw);

    write_config_with_options(&t, "lean-ctx", WriteOptions::default()).unwrap();

    let json: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        json["mcpServers"]["github"]["command"], "gh-mcp",
        "foreign legacy servers are not ours to migrate"
    );
    assert!(json["mcpServers"].get("lean-ctx").is_none());
    assert_eq!(json["mcp"]["servers"]["lean-ctx"]["command"], "lean-ctx");
}

#[test]
fn openclaw_is_idempotent_after_migration() {
    // The watchdog re-runs setup every 30 min — the second run must be a
    // byte-identical no-op (Already), otherwise every tick causes a reload.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("openclaw.json");
    std::fs::write(
        &path,
        r#"{ "mcpServers": { "lean-ctx": { "command": "/old/lean-ctx" } } }"#,
    )
    .unwrap();
    let t = target("OpenClaw", path.clone(), ConfigType::OpenClaw);

    let first = write_config_with_options(&t, "lean-ctx", WriteOptions::default()).unwrap();
    assert_eq!(first.action, WriteAction::Updated);
    let after_first = std::fs::read_to_string(&path).unwrap();

    let second = write_config_with_options(&t, "lean-ctx", WriteOptions::default()).unwrap();
    assert_eq!(
        second.action,
        WriteAction::Already,
        "re-run must be a no-op"
    );
    let after_second = std::fs::read_to_string(&path).unwrap();
    assert_eq!(after_first, after_second, "no churn on repeated runs");
}

#[test]
fn openclaw_legacy_version_keeps_camelcase_schema() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("openclaw.json");
    std::fs::write(
        &path,
        r#"{ "meta": { "lastTouchedVersion": "2026.5.9" }, "mcpServers": {} }"#,
    )
    .unwrap();
    let t = target("OpenClaw", path.clone(), ConfigType::OpenClaw);

    let res = write_config_with_options(&t, "lean-ctx", WriteOptions::default()).unwrap();
    assert_eq!(res.action, WriteAction::Updated);

    let json: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        json["mcpServers"]["lean-ctx"]["command"], "lean-ctx",
        "pre-2026.6.1 OpenClaw still reads the camelCase schema"
    );
    assert!(
        json.get("mcp").is_none(),
        "nested schema must not be forced onto old versions"
    );
}

#[test]
fn openclaw_existing_nested_block_wins_over_old_version_stamp() {
    // If mcp.servers already exists, the schema migration has happened —
    // regardless of what meta.lastTouchedVersion claims.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("openclaw.json");
    std::fs::write(
        &path,
        r#"{
            "meta": { "lastTouchedVersion": "2025.12.1" },
            "mcp": { "servers": {} }
        }"#,
    )
    .unwrap();
    let t = target("OpenClaw", path.clone(), ConfigType::OpenClaw);

    write_config_with_options(&t, "lean-ctx", WriteOptions::default()).unwrap();

    let json: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(json["mcp"]["servers"]["lean-ctx"]["command"], "lean-ctx");
    assert!(json.get("mcpServers").is_none());
}

#[test]
fn openclaw_preserves_unrelated_config_keys() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("openclaw.json");
    std::fs::write(
        &path,
        r#"{
            "gateway": { "port": 8443 },
            "meta": { "lastTouchedVersion": "2026.6.1" },
            "mcp": { "servers": { "github": { "command": "gh-mcp" } } }
        }"#,
    )
    .unwrap();
    let t = target("OpenClaw", path.clone(), ConfigType::OpenClaw);

    write_config_with_options(&t, "lean-ctx", WriteOptions::default()).unwrap();

    let json: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(json["gateway"]["port"], 8443);
    assert_eq!(json["mcp"]["servers"]["github"]["command"], "gh-mcp");
    assert_eq!(json["mcp"]["servers"]["lean-ctx"]["command"], "lean-ctx");
}

#[test]
fn openclaw_invalid_json_is_never_text_injected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("openclaw.json");
    let broken = r#"{ "mcp": { "servers": {, } }"#;
    std::fs::write(&path, broken).unwrap();
    let t = target("OpenClaw", path.clone(), ConfigType::OpenClaw);

    // Even with overwrite_invalid the writer must refuse to patch openclaw.json
    // textually — a malformed result would take the gateway down on restart.
    let res = write_config_with_options(
        &t,
        "lean-ctx",
        WriteOptions {
            overwrite_invalid: true,
        },
    );
    assert!(
        res.is_err(),
        "invalid openclaw.json must surface as an error"
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        broken,
        "file must remain untouched"
    );
}

#[test]
fn openclaw_remove_drops_both_schemas_and_empty_containers() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("openclaw.json");
    std::fs::write(
        &path,
        r#"{
            "gateway": { "port": 8443 },
            "mcpServers": { "lean-ctx": { "command": "/old/lean-ctx" } },
            "mcp": { "servers": { "lean-ctx": { "command": "lean-ctx" } } }
        }"#,
    )
    .unwrap();
    let t = target("OpenClaw", path.clone(), ConfigType::OpenClaw);

    let res = remove_lean_ctx_server(&t, WriteOptions::default()).unwrap();
    assert_eq!(res.action, WriteAction::Updated);

    let json: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(json.get("mcpServers").is_none(), "legacy block removed");
    assert!(json.get("mcp").is_none(), "emptied mcp container removed");
    assert_eq!(json["gateway"]["port"], 8443, "unrelated config preserved");
}

#[test]
fn openclaw_remove_preserves_foreign_servers() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("openclaw.json");
    std::fs::write(
        &path,
        r#"{ "mcp": { "servers": {
            "lean-ctx": { "command": "lean-ctx" },
            "github": { "command": "gh-mcp" }
        } } }"#,
    )
    .unwrap();
    let t = target("OpenClaw", path.clone(), ConfigType::OpenClaw);

    remove_lean_ctx_server(&t, WriteOptions::default()).unwrap();

    let json: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(json["mcp"]["servers"]["github"]["command"], "gh-mcp");
    assert!(json["mcp"]["servers"].get("lean-ctx").is_none());
}

#[test]
fn openclaw_remove_is_noop_without_entry() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("openclaw.json");
    std::fs::write(&path, r#"{ "mcp": { "servers": {} } }"#).unwrap();
    let t = target("OpenClaw", path.clone(), ConfigType::OpenClaw);

    let res = remove_lean_ctx_server(&t, WriteOptions::default()).unwrap();
    assert_eq!(res.action, WriteAction::Already);
}

#[test]
fn openclaw_version_parsing_handles_real_world_formats() {
    use super::install::parse_openclaw_version as v;
    assert_eq!(v("2026.6.1"), Some((2026, 6, 1)));
    assert_eq!(v("2026.6"), Some((2026, 6, 0)));
    assert_eq!(v("2026"), Some((2026, 0, 0)));
    assert_eq!(v(" 2026.6.1 "), Some((2026, 6, 1)));
    assert_eq!(v("2026.6.1-beta.2"), Some((2026, 6, 1)));
    assert_eq!(v("2026.6.1+build5"), Some((2026, 6, 1)));
    assert_eq!(v("not-a-version"), None);
    assert_eq!(v(""), None);
}
