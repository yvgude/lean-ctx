use serde_json::Value;

/// Core read-only tools exposed to IDE plan modes.
/// Kept as a curated subset (not all readonly tools) to avoid
/// overwhelming plan agents with architecture/debug tools.
#[must_use]
pub fn plan_mode_tools() -> &'static [&'static str] {
    &[
        "ctx_read",
        "ctx_search",
        "ctx_tree",
        "ctx_overview",
        "ctx_plan",
        "ctx_metrics",
        "ctx_compress",
        "ctx_session",
        "ctx_knowledge",
        "ctx_graph",
        "ctx_retrieve",
        "ctx_provider",
    ]
}

fn vscode_plan_tool_ids() -> Vec<String> {
    plan_mode_tools()
        .iter()
        .map(|t| format!("lean-ctx_{t}"))
        .collect()
}

pub fn vscode_settings_path() -> Option<std::path::PathBuf> {
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = dirs::home_dir() {
            let p = home.join("Library/Application Support/Code/User/settings.json");
            if p.parent().is_some_and(std::path::Path::exists) {
                return Some(p);
            }
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Some(home) = dirs::home_dir() {
            let paths = [
                home.join(".config/Code/User/settings.json"),
                home.join(".config/Code - Insiders/User/settings.json"),
                home.join(".vscode-server/data/User/settings.json"),
            ];
            for p in paths {
                if p.parent().is_some_and(std::path::Path::exists) {
                    return Some(p);
                }
            }
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            let p = std::path::PathBuf::from(appdata).join("Code/User/settings.json");
            if p.parent().is_some_and(std::path::Path::exists) {
                return Some(p);
            }
        }
    }
    None
}

pub fn write_vscode_plan_settings() -> Result<super::WriteResult, String> {
    let path = vscode_settings_path().ok_or("VS Code settings.json directory not found")?;
    write_vscode_plan_settings_to(&path)
}

pub fn write_vscode_plan_settings_to(path: &std::path::Path) -> Result<super::WriteResult, String> {
    let desired_tools: Value = serde_json::json!(vscode_plan_tool_ids());

    if path.exists() {
        let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let mut json = crate::core::jsonc::parse_jsonc(&content)
            .map_err(|e| format!("VS Code settings.json parse error: {e}"))?;
        let obj = json
            .as_object_mut()
            .ok_or("VS Code settings.json root must be an object")?;

        let mut changed = false;

        if obj.get("chat.mcp.enabled") != Some(&Value::Bool(true)) {
            obj.insert("chat.mcp.enabled".to_string(), Value::Bool(true));
            changed = true;
        }

        let key = "github.copilot.chat.planAgent.additionalTools";
        let existing = obj.get(key);
        if existing != Some(&desired_tools) {
            if let Some(existing_arr) = existing.and_then(|v| v.as_array()) {
                let merged = merge_tool_arrays(existing_arr, &desired_tools);
                if obj.get(key) != Some(&merged) {
                    obj.insert(key.to_string(), merged);
                    changed = true;
                }
            } else {
                obj.insert(key.to_string(), desired_tools);
                changed = true;
            }
        }

        if !changed {
            return Ok(super::WriteResult {
                action: super::WriteAction::Already,
                note: Some("plan mode tools already configured".to_string()),
            });
        }

        let formatted = serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?;
        crate::config_io::write_atomic_with_backup(path, &formatted)?;
        return Ok(super::WriteResult {
            action: super::WriteAction::Updated,
            note: Some("plan mode tools + chat.mcp.enabled".to_string()),
        });
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let content = serde_json::to_string_pretty(&serde_json::json!({
        "chat.mcp.enabled": true,
        "github.copilot.chat.planAgent.additionalTools": vscode_plan_tool_ids(),
    }))
    .map_err(|e| e.to_string())?;
    crate::config_io::write_atomic_with_backup(path, &content)?;
    Ok(super::WriteResult {
        action: super::WriteAction::Created,
        note: Some("plan mode tools + chat.mcp.enabled".to_string()),
    })
}

fn merge_tool_arrays(existing: &[Value], desired: &Value) -> Value {
    let mut merged: Vec<Value> = existing.to_vec();
    if let Some(desired_arr) = desired.as_array() {
        for tool in desired_arr {
            if !merged.iter().any(|v| v == tool) {
                merged.push(tool.clone());
            }
        }
    }
    Value::Array(merged)
}

fn claude_settings_path() -> Option<std::path::PathBuf> {
    let home = dirs::home_dir()?;
    let global = home.join(".claude/settings.json");
    if global.parent().is_some_and(std::path::Path::exists) {
        return Some(global);
    }
    None
}

pub fn write_claude_code_plan_permissions() -> Result<super::WriteResult, String> {
    let path = claude_settings_path().ok_or("~/.claude/ directory not found")?;
    write_claude_code_plan_permissions_to(&path)
}

pub fn write_claude_code_plan_permissions_to(
    path: &std::path::Path,
) -> Result<super::WriteResult, String> {
    let plan_perms: Vec<String> = plan_mode_tools()
        .iter()
        .map(|t| format!("mcp__lean-ctx__{t}"))
        .collect();

    if path.exists() {
        let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let mut json = crate::core::jsonc::parse_jsonc(&content)
            .map_err(|e| format!("~/.claude/settings.json parse error: {e}"))?;
        let obj = json
            .as_object_mut()
            .ok_or("~/.claude/settings.json root must be an object")?;

        let perms = obj
            .entry("permissions")
            .or_insert_with(|| serde_json::json!({}));
        let perms_obj = perms
            .as_object_mut()
            .ok_or("\"permissions\" must be an object")?;
        let allow = perms_obj
            .entry("allow")
            .or_insert_with(|| serde_json::json!([]));
        let allow_arr = allow
            .as_array_mut()
            .ok_or("\"permissions.allow\" must be an array")?;

        let mut changed = false;
        for perm in &plan_perms {
            let val = Value::String(perm.clone());
            if !allow_arr.iter().any(|v| v == &val) {
                allow_arr.push(val);
                changed = true;
            }
        }

        if !changed {
            return Ok(super::WriteResult {
                action: super::WriteAction::Already,
                note: Some("plan mode permissions already present".to_string()),
            });
        }

        let formatted = serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?;
        crate::config_io::write_atomic_with_backup(path, &formatted)?;
        return Ok(super::WriteResult {
            action: super::WriteAction::Updated,
            note: Some("plan mode permissions added".to_string()),
        });
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let content = serde_json::to_string_pretty(&serde_json::json!({
        "permissions": {
            "allow": plan_perms,
        }
    }))
    .map_err(|e| e.to_string())?;
    crate::config_io::write_atomic_with_backup(path, &content)?;
    Ok(super::WriteResult {
        action: super::WriteAction::Created,
        note: Some("plan mode permissions created".to_string()),
    })
}

#[derive(Debug)]
pub struct PlanModeStatus {
    pub vscode_configured: Option<bool>,
    pub claude_configured: Option<bool>,
}

#[must_use]
pub fn check_plan_mode_status() -> PlanModeStatus {
    PlanModeStatus {
        vscode_configured: check_vscode_plan_mode(),
        claude_configured: check_claude_plan_mode(),
    }
}

pub fn check_plan_mode_status_for_paths(
    vscode_path: Option<&std::path::Path>,
    claude_path: Option<&std::path::Path>,
) -> PlanModeStatus {
    PlanModeStatus {
        vscode_configured: vscode_path.map(check_settings_file_vscode),
        claude_configured: claude_path.map(check_settings_file_claude),
    }
}

fn check_vscode_plan_mode() -> Option<bool> {
    let path = vscode_settings_path()?;
    Some(check_settings_file_vscode(&path))
}

fn check_settings_file_vscode(path: &std::path::Path) -> bool {
    if !path.exists() {
        return false;
    }
    let Ok(content) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(json) = crate::core::jsonc::parse_jsonc(&content) else {
        return false;
    };
    let Some(obj) = json.as_object() else {
        return false;
    };

    let mcp_enabled = obj
        .get("chat.mcp.enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let has_tools = obj
        .get("github.copilot.chat.planAgent.additionalTools")
        .and_then(|v| v.as_array())
        .is_some_and(|arr| {
            arr.iter()
                .any(|v| v.as_str().is_some_and(|s| s.starts_with("lean-ctx_")))
        });

    mcp_enabled && has_tools
}

fn check_claude_plan_mode() -> Option<bool> {
    let path = claude_settings_path()?;
    Some(check_settings_file_claude(&path))
}

fn check_settings_file_claude(path: &std::path::Path) -> bool {
    if !path.exists() {
        return false;
    }
    let Ok(content) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(json) = crate::core::jsonc::parse_jsonc(&content) else {
        return false;
    };
    let Some(obj) = json.as_object() else {
        return false;
    };

    obj.get("permissions")
        .and_then(|v| v.as_object())
        .and_then(|p| p.get("allow"))
        .and_then(|v| v.as_array())
        .is_some_and(|arr| {
            arr.iter()
                .any(|v| v.as_str().is_some_and(|s| s.starts_with("mcp__lean-ctx__")))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::editor_registry::WriteAction;

    #[test]
    fn plan_mode_tools_are_readonly() {
        let tools = plan_mode_tools();
        assert!(tools.contains(&"ctx_read"));
        assert!(tools.contains(&"ctx_search"));
        assert!(tools.contains(&"ctx_tree"));
        assert!(tools.contains(&"ctx_overview"));
        assert!(tools.contains(&"ctx_plan"));

        assert!(!tools.contains(&"ctx_edit"));
        assert!(!tools.contains(&"ctx_shell"));
        assert!(!tools.contains(&"ctx_compile"));
    }

    #[test]
    fn vscode_plan_tool_ids_have_prefix() {
        let ids = vscode_plan_tool_ids();
        assert!(ids.iter().all(|id| id.starts_with("lean-ctx_")));
        assert!(ids.contains(&"lean-ctx_ctx_read".to_string()));
    }

    #[test]
    fn merge_preserves_existing_and_adds_new() {
        let existing = vec![
            Value::String("other-server_tool".to_string()),
            Value::String("lean-ctx_ctx_read".to_string()),
        ];
        let desired = serde_json::json!(["lean-ctx_ctx_read", "lean-ctx_ctx_search"]);
        let merged = merge_tool_arrays(&existing, &desired);
        let arr = merged.as_array().unwrap();
        assert_eq!(arr.len(), 3);
        assert!(arr.contains(&Value::String("other-server_tool".to_string())));
        assert!(arr.contains(&Value::String("lean-ctx_ctx_read".to_string())));
        assert!(arr.contains(&Value::String("lean-ctx_ctx_search".to_string())));
    }

    #[test]
    fn vscode_fresh_write_creates_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");

        let res = write_vscode_plan_settings_to(&path).unwrap();
        assert!(matches!(res.action, WriteAction::Created));

        let json: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(json["chat.mcp.enabled"], true);
        let tools = json["github.copilot.chat.planAgent.additionalTools"]
            .as_array()
            .unwrap();
        assert!(tools.len() >= plan_mode_tools().len());
        assert!(tools.contains(&Value::String("lean-ctx_ctx_read".to_string())));
    }

    #[test]
    fn vscode_merge_preserves_existing_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");

        let initial = serde_json::json!({
            "editor.fontSize": 14,
            "workbench.colorTheme": "Monokai",
        });
        std::fs::write(&path, serde_json::to_string_pretty(&initial).unwrap()).unwrap();

        let res = write_vscode_plan_settings_to(&path).unwrap();
        assert!(matches!(res.action, WriteAction::Updated));

        let json: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(json["editor.fontSize"], 14);
        assert_eq!(json["workbench.colorTheme"], "Monokai");
        assert_eq!(json["chat.mcp.enabled"], true);
        assert!(
            json["github.copilot.chat.planAgent.additionalTools"]
                .as_array()
                .unwrap()
                .len()
                > 5
        );
    }

    #[test]
    fn vscode_merge_preserves_foreign_tools() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");

        let initial = serde_json::json!({
            "chat.mcp.enabled": true,
            "github.copilot.chat.planAgent.additionalTools": [
                "other-mcp_tool_a",
                "other-mcp_tool_b",
            ],
        });
        std::fs::write(&path, serde_json::to_string_pretty(&initial).unwrap()).unwrap();

        let res = write_vscode_plan_settings_to(&path).unwrap();
        assert!(matches!(res.action, WriteAction::Updated));

        let json: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let tools = json["github.copilot.chat.planAgent.additionalTools"]
            .as_array()
            .unwrap();
        assert!(tools.contains(&Value::String("other-mcp_tool_a".to_string())));
        assert!(tools.contains(&Value::String("other-mcp_tool_b".to_string())));
        assert!(tools.contains(&Value::String("lean-ctx_ctx_read".to_string())));
    }

    #[test]
    fn vscode_idempotent_returns_already() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");

        let r1 = write_vscode_plan_settings_to(&path).unwrap();
        assert!(matches!(r1.action, WriteAction::Created));

        let r2 = write_vscode_plan_settings_to(&path).unwrap();
        assert!(matches!(r2.action, WriteAction::Already));
    }

    #[test]
    fn claude_fresh_write_creates_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");

        let res = write_claude_code_plan_permissions_to(&path).unwrap();
        assert!(matches!(res.action, WriteAction::Created));

        let json: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let allow = json["permissions"]["allow"].as_array().unwrap();
        assert!(allow.contains(&Value::String("mcp__lean-ctx__ctx_read".to_string())));
        assert!(allow.len() >= plan_mode_tools().len());
    }

    #[test]
    fn claude_merge_preserves_existing_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");

        let initial = serde_json::json!({
            "permissions": {
                "allow": ["Bash(git *)", "Read(~/projects/*)"],
            }
        });
        std::fs::write(&path, serde_json::to_string_pretty(&initial).unwrap()).unwrap();

        let res = write_claude_code_plan_permissions_to(&path).unwrap();
        assert!(matches!(res.action, WriteAction::Updated));

        let json: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let allow = json["permissions"]["allow"].as_array().unwrap();
        assert!(allow.contains(&Value::String("Bash(git *)".to_string())));
        assert!(allow.contains(&Value::String("Read(~/projects/*)".to_string())));
        assert!(allow.contains(&Value::String("mcp__lean-ctx__ctx_read".to_string())));
    }

    #[test]
    fn claude_idempotent_returns_already() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");

        let r1 = write_claude_code_plan_permissions_to(&path).unwrap();
        assert!(matches!(r1.action, WriteAction::Created));

        let r2 = write_claude_code_plan_permissions_to(&path).unwrap();
        assert!(matches!(r2.action, WriteAction::Already));
    }

    #[test]
    fn check_status_detects_configured_vscode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");

        let status = check_plan_mode_status_for_paths(Some(&path), None);
        assert_eq!(status.vscode_configured, Some(false));

        write_vscode_plan_settings_to(&path).unwrap();
        let status = check_plan_mode_status_for_paths(Some(&path), None);
        assert_eq!(status.vscode_configured, Some(true));
    }

    #[test]
    fn check_status_detects_configured_claude() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");

        let status = check_plan_mode_status_for_paths(None, Some(&path));
        assert_eq!(status.claude_configured, Some(false));

        write_claude_code_plan_permissions_to(&path).unwrap();
        let status = check_plan_mode_status_for_paths(None, Some(&path));
        assert_eq!(status.claude_configured, Some(true));
    }
}
