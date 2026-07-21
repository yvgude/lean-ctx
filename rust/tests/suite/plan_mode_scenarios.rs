use lean_ctx::core::editor_registry::WriteAction;
use lean_ctx::core::editor_registry::plan_mode::{
    check_plan_mode_status_for_paths, plan_mode_tools, write_claude_code_plan_permissions_to,
    write_vscode_plan_settings_to,
};
use lean_ctx::server::dynamic_tools::is_readonly_tool;
use serde_json::Value;

fn read_json(path: &std::path::Path) -> Value {
    let content = std::fs::read_to_string(path).expect("read file");
    serde_json::from_str(&content).expect("parse JSON")
}

// ── VS Code scenarios ──────────────────────────────────────

#[test]
fn vscode_fresh_write_on_empty_dir() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");

    let res = write_vscode_plan_settings_to(&path).unwrap();
    assert!(matches!(res.action, WriteAction::Created));

    let json = read_json(&path);
    assert_eq!(json["chat.mcp.enabled"], true);
    let tools = json["github.copilot.chat.planAgent.additionalTools"]
        .as_array()
        .unwrap();
    assert_eq!(tools.len(), plan_mode_tools().len());
    for tool in plan_mode_tools() {
        let expected = format!("lean-ctx_{tool}");
        assert!(
            tools.contains(&Value::String(expected.clone())),
            "missing tool: {expected}"
        );
    }
}

#[test]
fn vscode_merge_preserves_foreign_tools_and_settings() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");

    let initial = serde_json::json!({
        "editor.fontSize": 16,
        "editor.tabSize": 2,
        "github.copilot.chat.planAgent.additionalTools": [
            "other-mcp_analyze",
            "copilot_fetch",
        ],
    });
    std::fs::write(&path, serde_json::to_string_pretty(&initial).unwrap()).unwrap();

    let res = write_vscode_plan_settings_to(&path).unwrap();
    assert!(matches!(res.action, WriteAction::Updated));

    let json = read_json(&path);
    assert_eq!(json["editor.fontSize"], 16);
    assert_eq!(json["editor.tabSize"], 2);
    assert_eq!(json["chat.mcp.enabled"], true);

    let tools = json["github.copilot.chat.planAgent.additionalTools"]
        .as_array()
        .unwrap();
    assert!(tools.contains(&Value::String("other-mcp_analyze".to_string())));
    assert!(tools.contains(&Value::String("copilot_fetch".to_string())));
    assert!(tools.contains(&Value::String("lean-ctx_ctx_read".to_string())));
    assert_eq!(tools.len(), 2 + plan_mode_tools().len());
}

#[test]
fn vscode_idempotent_second_call_returns_already() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");

    let r1 = write_vscode_plan_settings_to(&path).unwrap();
    assert!(matches!(r1.action, WriteAction::Created));

    let content_after_first = std::fs::read_to_string(&path).unwrap();

    let r2 = write_vscode_plan_settings_to(&path).unwrap();
    assert!(
        matches!(r2.action, WriteAction::Already),
        "second call should be Already, got: {:?}",
        r2.action
    );

    let content_after_second = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        content_after_first, content_after_second,
        "file must not change on idempotent call"
    );
}

#[test]
fn vscode_jsonc_with_comments() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");

    let jsonc_content = r#"{
    // This is a VS Code settings file
    "editor.fontSize": 14,
    /* Block comment */
    "workbench.colorTheme": "One Dark Pro"
}"#;
    std::fs::write(&path, jsonc_content).unwrap();

    let res = write_vscode_plan_settings_to(&path).unwrap();
    assert!(matches!(res.action, WriteAction::Updated));

    let json = read_json(&path);
    assert_eq!(json["editor.fontSize"], 14);
    assert_eq!(json["chat.mcp.enabled"], true);
    assert!(
        json["github.copilot.chat.planAgent.additionalTools"]
            .as_array()
            .unwrap()
            .len()
            > 5
    );
}

// ── Claude Code scenarios ──────────────────────────────────

#[test]
fn claude_fresh_write_creates_full_structure() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");

    let res = write_claude_code_plan_permissions_to(&path).unwrap();
    assert!(matches!(res.action, WriteAction::Created));

    let json = read_json(&path);
    let allow = json["permissions"]["allow"].as_array().unwrap();
    assert_eq!(allow.len(), plan_mode_tools().len());
    for tool in plan_mode_tools() {
        let expected = format!("mcp__lean-ctx__{tool}");
        assert!(
            allow.contains(&Value::String(expected.clone())),
            "missing permission: {expected}"
        );
    }
}

#[test]
fn claude_merge_preserves_existing_permissions() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");

    let initial = serde_json::json!({
        "permissions": {
            "allow": [
                "Bash(git *)",
                "Read(~/projects/*)",
                "mcp__other-server__tool_x",
            ],
            "deny": ["Bash(rm -rf /)"],
        },
        "model": "opus",
    });
    std::fs::write(&path, serde_json::to_string_pretty(&initial).unwrap()).unwrap();

    let res = write_claude_code_plan_permissions_to(&path).unwrap();
    assert!(matches!(res.action, WriteAction::Updated));

    let json = read_json(&path);
    assert_eq!(json["model"], "opus");

    let deny = json["permissions"]["deny"].as_array().unwrap();
    assert!(deny.contains(&Value::String("Bash(rm -rf /)".to_string())));

    let allow = json["permissions"]["allow"].as_array().unwrap();
    assert!(allow.contains(&Value::String("Bash(git *)".to_string())));
    assert!(allow.contains(&Value::String("Read(~/projects/*)".to_string())));
    assert!(allow.contains(&Value::String("mcp__other-server__tool_x".to_string())));
    assert!(allow.contains(&Value::String("mcp__lean-ctx__ctx_read".to_string())));
    assert_eq!(allow.len(), 3 + plan_mode_tools().len());
}

#[test]
fn claude_idempotent_second_call_returns_already() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");

    let r1 = write_claude_code_plan_permissions_to(&path).unwrap();
    assert!(matches!(r1.action, WriteAction::Created));

    let content_after_first = std::fs::read_to_string(&path).unwrap();

    let r2 = write_claude_code_plan_permissions_to(&path).unwrap();
    assert!(
        matches!(r2.action, WriteAction::Already),
        "second call should be Already, got: {:?}",
        r2.action
    );

    let content_after_second = std::fs::read_to_string(&path).unwrap();
    assert_eq!(content_after_first, content_after_second);
}

#[test]
fn claude_empty_permissions_object_gets_allow_added() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");

    let initial = serde_json::json!({
        "permissions": {},
        "model": "sonnet",
    });
    std::fs::write(&path, serde_json::to_string_pretty(&initial).unwrap()).unwrap();

    let res = write_claude_code_plan_permissions_to(&path).unwrap();
    assert!(matches!(res.action, WriteAction::Updated));

    let json = read_json(&path);
    assert_eq!(json["model"], "sonnet");
    let allow = json["permissions"]["allow"].as_array().unwrap();
    assert!(allow.contains(&Value::String("mcp__lean-ctx__ctx_read".to_string())));
}

// ── Doctor check scenarios ──────────────────────────────────

#[test]
fn doctor_detects_missing_file() {
    let dir = tempfile::tempdir().unwrap();
    let vscode_path = dir.path().join("vscode_settings.json");
    let claude_path = dir.path().join("claude_settings.json");

    let status = check_plan_mode_status_for_paths(Some(&vscode_path), Some(&claude_path));
    assert_eq!(status.vscode_configured, Some(false));
    assert_eq!(status.claude_configured, Some(false));
}

#[test]
fn doctor_detects_unconfigured_existing_file() {
    let dir = tempfile::tempdir().unwrap();
    let vscode_path = dir.path().join("vscode_settings.json");
    let claude_path = dir.path().join("claude_settings.json");

    std::fs::write(
        &vscode_path,
        serde_json::to_string_pretty(&serde_json::json!({"editor.fontSize": 14})).unwrap(),
    )
    .unwrap();
    std::fs::write(
        &claude_path,
        serde_json::to_string_pretty(&serde_json::json!({"model": "opus"})).unwrap(),
    )
    .unwrap();

    let status = check_plan_mode_status_for_paths(Some(&vscode_path), Some(&claude_path));
    assert_eq!(status.vscode_configured, Some(false));
    assert_eq!(status.claude_configured, Some(false));
}

#[test]
fn doctor_detects_configured_after_write() {
    let dir = tempfile::tempdir().unwrap();
    let vscode_path = dir.path().join("vscode_settings.json");
    let claude_path = dir.path().join("claude_settings.json");

    write_vscode_plan_settings_to(&vscode_path).unwrap();
    write_claude_code_plan_permissions_to(&claude_path).unwrap();

    let status = check_plan_mode_status_for_paths(Some(&vscode_path), Some(&claude_path));
    assert_eq!(status.vscode_configured, Some(true));
    assert_eq!(status.claude_configured, Some(true));
}

#[test]
fn doctor_none_when_path_not_provided() {
    let status = check_plan_mode_status_for_paths(None, None);
    assert!(status.vscode_configured.is_none());
    assert!(status.claude_configured.is_none());
}

// ── is_readonly_tool cross-check ──────────────────────────────────

#[test]
fn all_plan_mode_tools_are_readonly() {
    for tool in plan_mode_tools() {
        assert!(
            is_readonly_tool(tool),
            "plan_mode_tools() contains '{tool}' which is_readonly_tool() says is NOT readonly"
        );
    }
}

#[test]
fn write_tools_are_not_readonly() {
    let write_tools = [
        "ctx_edit",
        "ctx_shell",
        "ctx_compile",
        "ctx_cache",
        "ctx_control",
        "ctx_fill",
        "ctx_execute",
        "ctx_expand",
        "ctx_pack",
        "ctx_feedback",
        "ctx_prefetch",
        "ctx_agent",
        "ctx_handoff",
        "ctx_workflow",
    ];
    for tool in &write_tools {
        assert!(
            !is_readonly_tool(tool),
            "'{tool}' is a write tool but is_readonly_tool() returns true"
        );
    }
}

#[test]
fn vscode_doctor_partial_config_mcp_enabled_but_no_tools() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");

    let partial = serde_json::json!({
        "chat.mcp.enabled": true,
    });
    std::fs::write(&path, serde_json::to_string_pretty(&partial).unwrap()).unwrap();

    let status = check_plan_mode_status_for_paths(Some(&path), None);
    assert_eq!(status.vscode_configured, Some(false));
}

#[test]
fn vscode_doctor_partial_config_tools_but_no_mcp() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");

    let partial = serde_json::json!({
        "github.copilot.chat.planAgent.additionalTools": ["lean-ctx_ctx_read"],
    });
    std::fs::write(&path, serde_json::to_string_pretty(&partial).unwrap()).unwrap();

    let status = check_plan_mode_status_for_paths(Some(&path), None);
    assert_eq!(status.vscode_configured, Some(false));
}
