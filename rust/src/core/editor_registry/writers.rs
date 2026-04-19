use serde_json::Value;

use super::types::{ConfigType, EditorTarget};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteAction {
    Created,
    Updated,
    Already,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct WriteOptions {
    pub overwrite_invalid: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteResult {
    pub action: WriteAction,
    pub note: Option<String>,
}

pub fn write_config(target: &EditorTarget, binary: &str) -> Result<WriteResult, String> {
    write_config_with_options(target, binary, WriteOptions::default())
}

pub fn write_config_with_options(
    target: &EditorTarget,
    binary: &str,
    opts: WriteOptions,
) -> Result<WriteResult, String> {
    if let Some(parent) = target.config_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    match target.config_type {
        ConfigType::McpJson => write_mcp_json(target, binary, opts),
        ConfigType::Zed => write_zed_config(target, binary, opts),
        ConfigType::Codex => write_codex_config(target, binary),
        ConfigType::VsCodeMcp => write_vscode_mcp(target, binary, opts),
        ConfigType::OpenCode => write_opencode_config(target, binary, opts),
        ConfigType::Crush => write_crush_config(target, binary, opts),
    }
}

fn auto_approve_tools() -> Vec<&'static str> {
    vec![
        "ctx_read",
        "ctx_shell",
        "ctx_search",
        "ctx_tree",
        "ctx_overview",
        "ctx_preload",
        "ctx_compress",
        "ctx_metrics",
        "ctx_session",
        "ctx_knowledge",
        "ctx_agent",
        "ctx_share",
        "ctx_analyze",
        "ctx_benchmark",
        "ctx_cache",
        "ctx_discover",
        "ctx_smart_read",
        "ctx_delta",
        "ctx_edit",
        "ctx_dedup",
        "ctx_fill",
        "ctx_intent",
        "ctx_response",
        "ctx_context",
        "ctx_graph",
        "ctx_wrapped",
        "ctx_multi_read",
        "ctx_semantic_search",
        "ctx_symbol",
        "ctx_outline",
        "ctx_callers",
        "ctx_callees",
        "ctx_routes",
        "ctx_graph_diagram",
        "ctx_cost",
        "ctx_heatmap",
        "ctx_task",
        "ctx_impact",
        "ctx_architecture",
        "ctx_workflow",
        "ctx",
    ]
}

fn lean_ctx_server_entry(binary: &str, data_dir: &str) -> Value {
    serde_json::json!({
        "command": binary,
        "env": {
            "LEAN_CTX_DATA_DIR": data_dir
        },
        "autoApprove": auto_approve_tools()
    })
}

fn default_data_dir() -> Result<String, String> {
    Ok(crate::core::data_dir::lean_ctx_data_dir()?
        .to_string_lossy()
        .to_string())
}

fn write_mcp_json(
    target: &EditorTarget,
    binary: &str,
    opts: WriteOptions,
) -> Result<WriteResult, String> {
    let data_dir = default_data_dir()?;
    let desired = lean_ctx_server_entry(binary, &data_dir);

    // Claude Code manages ~/.claude.json and may overwrite it on first start.
    // Prefer the official CLI integration when available.
    if target.agent_key == "claude" || target.name == "Claude Code" {
        if let Ok(result) = try_claude_mcp_add(&desired) {
            return Ok(result);
        }
    }

    if target.config_path.exists() {
        let content = std::fs::read_to_string(&target.config_path).map_err(|e| e.to_string())?;
        let mut json = match serde_json::from_str::<Value>(&content) {
            Ok(v) => v,
            Err(e) => {
                if !opts.overwrite_invalid {
                    return Err(e.to_string());
                }
                backup_invalid_file(&target.config_path)?;
                return write_mcp_json_fresh(
                    &target.config_path,
                    desired,
                    Some("overwrote invalid JSON".to_string()),
                );
            }
        };
        let obj = json
            .as_object_mut()
            .ok_or_else(|| "root JSON must be an object".to_string())?;

        let servers = obj
            .entry("mcpServers")
            .or_insert_with(|| serde_json::json!({}));
        let servers_obj = servers
            .as_object_mut()
            .ok_or_else(|| "\"mcpServers\" must be an object".to_string())?;

        let existing = servers_obj.get("lean-ctx").cloned();
        if existing.as_ref() == Some(&desired) {
            return Ok(WriteResult {
                action: WriteAction::Already,
                note: None,
            });
        }
        servers_obj.insert("lean-ctx".to_string(), desired);

        let formatted = serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?;
        crate::config_io::write_atomic_with_backup(&target.config_path, &formatted)?;
        return Ok(WriteResult {
            action: WriteAction::Updated,
            note: None,
        });
    }

    write_mcp_json_fresh(&target.config_path, desired, None)
}

fn try_claude_mcp_add(desired: &Value) -> Result<WriteResult, String> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let server_json = serde_json::to_string(desired).map_err(|e| e.to_string())?;

    let mut child = Command::new("claude")
        .args(["mcp", "add-json", "--scope", "user", "lean-ctx"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;

    if let Some(stdin) = child.stdin.as_mut() {
        stdin
            .write_all(server_json.as_bytes())
            .map_err(|e| e.to_string())?;
    }
    let status = child.wait().map_err(|e| e.to_string())?;

    if status.success() {
        Ok(WriteResult {
            action: WriteAction::Updated,
            note: Some("via claude mcp add-json".to_string()),
        })
    } else {
        Err("claude mcp add-json failed".to_string())
    }
}

fn write_mcp_json_fresh(
    path: &std::path::Path,
    desired: Value,
    note: Option<String>,
) -> Result<WriteResult, String> {
    let content = serde_json::to_string_pretty(&serde_json::json!({
        "mcpServers": { "lean-ctx": desired }
    }))
    .map_err(|e| e.to_string())?;
    crate::config_io::write_atomic_with_backup(path, &content)?;
    Ok(WriteResult {
        action: if note.is_some() {
            WriteAction::Updated
        } else {
            WriteAction::Created
        },
        note,
    })
}

fn write_zed_config(
    target: &EditorTarget,
    binary: &str,
    opts: WriteOptions,
) -> Result<WriteResult, String> {
    let desired = serde_json::json!({
        "source": "custom",
        "command": binary,
        "args": [],
        "env": {}
    });

    if target.config_path.exists() {
        let content = std::fs::read_to_string(&target.config_path).map_err(|e| e.to_string())?;
        let mut json = match serde_json::from_str::<Value>(&content) {
            Ok(v) => v,
            Err(e) => {
                if !opts.overwrite_invalid {
                    return Err(e.to_string());
                }
                backup_invalid_file(&target.config_path)?;
                return write_zed_config_fresh(
                    &target.config_path,
                    desired,
                    Some("overwrote invalid JSON".to_string()),
                );
            }
        };
        let obj = json
            .as_object_mut()
            .ok_or_else(|| "root JSON must be an object".to_string())?;

        let servers = obj
            .entry("context_servers")
            .or_insert_with(|| serde_json::json!({}));
        let servers_obj = servers
            .as_object_mut()
            .ok_or_else(|| "\"context_servers\" must be an object".to_string())?;

        let existing = servers_obj.get("lean-ctx").cloned();
        if existing.as_ref() == Some(&desired) {
            return Ok(WriteResult {
                action: WriteAction::Already,
                note: None,
            });
        }
        servers_obj.insert("lean-ctx".to_string(), desired);

        let formatted = serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?;
        crate::config_io::write_atomic_with_backup(&target.config_path, &formatted)?;
        return Ok(WriteResult {
            action: WriteAction::Updated,
            note: None,
        });
    }

    write_zed_config_fresh(&target.config_path, desired, None)
}

fn write_codex_config(target: &EditorTarget, binary: &str) -> Result<WriteResult, String> {
    if target.config_path.exists() {
        let content = std::fs::read_to_string(&target.config_path).map_err(|e| e.to_string())?;
        let updated = upsert_codex_toml(&content, binary);
        if updated == content {
            return Ok(WriteResult {
                action: WriteAction::Already,
                note: None,
            });
        }
        crate::config_io::write_atomic_with_backup(&target.config_path, &updated)?;
        return Ok(WriteResult {
            action: WriteAction::Updated,
            note: None,
        });
    }

    let content = format!(
        "[mcp_servers.lean-ctx]\ncommand = \"{}\"\nargs = []\n",
        binary
    );
    crate::config_io::write_atomic_with_backup(&target.config_path, &content)?;
    Ok(WriteResult {
        action: WriteAction::Created,
        note: None,
    })
}

fn write_zed_config_fresh(
    path: &std::path::Path,
    desired: Value,
    note: Option<String>,
) -> Result<WriteResult, String> {
    let content = serde_json::to_string_pretty(&serde_json::json!({
        "context_servers": { "lean-ctx": desired }
    }))
    .map_err(|e| e.to_string())?;
    crate::config_io::write_atomic_with_backup(path, &content)?;
    Ok(WriteResult {
        action: if note.is_some() {
            WriteAction::Updated
        } else {
            WriteAction::Created
        },
        note,
    })
}

fn write_vscode_mcp(
    target: &EditorTarget,
    binary: &str,
    opts: WriteOptions,
) -> Result<WriteResult, String> {
    let desired = serde_json::json!({ "command": binary, "args": [] });

    if target.config_path.exists() {
        let content = std::fs::read_to_string(&target.config_path).map_err(|e| e.to_string())?;
        let mut json = match serde_json::from_str::<Value>(&content) {
            Ok(v) => v,
            Err(e) => {
                if !opts.overwrite_invalid {
                    return Err(e.to_string());
                }
                backup_invalid_file(&target.config_path)?;
                return write_vscode_mcp_fresh(
                    &target.config_path,
                    binary,
                    Some("overwrote invalid JSON".to_string()),
                );
            }
        };
        let obj = json
            .as_object_mut()
            .ok_or_else(|| "root JSON must be an object".to_string())?;

        let servers = obj
            .entry("servers")
            .or_insert_with(|| serde_json::json!({}));
        let servers_obj = servers
            .as_object_mut()
            .ok_or_else(|| "\"servers\" must be an object".to_string())?;

        let existing = servers_obj.get("lean-ctx").cloned();
        if existing.as_ref() == Some(&desired) {
            return Ok(WriteResult {
                action: WriteAction::Already,
                note: None,
            });
        }
        servers_obj.insert("lean-ctx".to_string(), desired);

        let formatted = serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?;
        crate::config_io::write_atomic_with_backup(&target.config_path, &formatted)?;
        return Ok(WriteResult {
            action: WriteAction::Updated,
            note: None,
        });
    }

    write_vscode_mcp_fresh(&target.config_path, binary, None)
}

fn write_vscode_mcp_fresh(
    path: &std::path::Path,
    binary: &str,
    note: Option<String>,
) -> Result<WriteResult, String> {
    let content = serde_json::to_string_pretty(&serde_json::json!({
        "servers": { "lean-ctx": { "command": binary, "args": [] } }
    }))
    .map_err(|e| e.to_string())?;
    crate::config_io::write_atomic_with_backup(path, &content)?;
    Ok(WriteResult {
        action: if note.is_some() {
            WriteAction::Updated
        } else {
            WriteAction::Created
        },
        note,
    })
}

fn write_opencode_config(
    target: &EditorTarget,
    binary: &str,
    opts: WriteOptions,
) -> Result<WriteResult, String> {
    let desired = serde_json::json!({
        "type": "local",
        "command": [binary],
        "enabled": true
    });

    if target.config_path.exists() {
        let content = std::fs::read_to_string(&target.config_path).map_err(|e| e.to_string())?;
        let mut json = match serde_json::from_str::<Value>(&content) {
            Ok(v) => v,
            Err(e) => {
                if !opts.overwrite_invalid {
                    return Err(e.to_string());
                }
                backup_invalid_file(&target.config_path)?;
                return write_opencode_fresh(
                    &target.config_path,
                    binary,
                    Some("overwrote invalid JSON".to_string()),
                );
            }
        };
        let obj = json
            .as_object_mut()
            .ok_or_else(|| "root JSON must be an object".to_string())?;
        let mcp = obj.entry("mcp").or_insert_with(|| serde_json::json!({}));
        let mcp_obj = mcp
            .as_object_mut()
            .ok_or_else(|| "\"mcp\" must be an object".to_string())?;

        let existing = mcp_obj.get("lean-ctx").cloned();
        if existing.as_ref() == Some(&desired) {
            return Ok(WriteResult {
                action: WriteAction::Already,
                note: None,
            });
        }
        mcp_obj.insert("lean-ctx".to_string(), desired);

        let formatted = serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?;
        crate::config_io::write_atomic_with_backup(&target.config_path, &formatted)?;
        return Ok(WriteResult {
            action: WriteAction::Updated,
            note: None,
        });
    }

    write_opencode_fresh(&target.config_path, binary, None)
}

fn write_opencode_fresh(
    path: &std::path::Path,
    binary: &str,
    note: Option<String>,
) -> Result<WriteResult, String> {
    let content = serde_json::to_string_pretty(&serde_json::json!({
        "$schema": "https://opencode.ai/config.json",
        "mcp": { "lean-ctx": { "type": "local", "command": [binary], "enabled": true } }
    }))
    .map_err(|e| e.to_string())?;
    crate::config_io::write_atomic_with_backup(path, &content)?;
    Ok(WriteResult {
        action: if note.is_some() {
            WriteAction::Updated
        } else {
            WriteAction::Created
        },
        note,
    })
}

fn write_crush_config(
    target: &EditorTarget,
    binary: &str,
    opts: WriteOptions,
) -> Result<WriteResult, String> {
    let desired = serde_json::json!({ "type": "stdio", "command": binary });

    if target.config_path.exists() {
        let content = std::fs::read_to_string(&target.config_path).map_err(|e| e.to_string())?;
        let mut json = match serde_json::from_str::<Value>(&content) {
            Ok(v) => v,
            Err(e) => {
                if !opts.overwrite_invalid {
                    return Err(e.to_string());
                }
                backup_invalid_file(&target.config_path)?;
                return write_crush_fresh(
                    &target.config_path,
                    desired,
                    Some("overwrote invalid JSON".to_string()),
                );
            }
        };
        let obj = json
            .as_object_mut()
            .ok_or_else(|| "root JSON must be an object".to_string())?;
        let mcp = obj.entry("mcp").or_insert_with(|| serde_json::json!({}));
        let mcp_obj = mcp
            .as_object_mut()
            .ok_or_else(|| "\"mcp\" must be an object".to_string())?;

        let existing = mcp_obj.get("lean-ctx").cloned();
        if existing.as_ref() == Some(&desired) {
            return Ok(WriteResult {
                action: WriteAction::Already,
                note: None,
            });
        }
        mcp_obj.insert("lean-ctx".to_string(), desired);

        let formatted = serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?;
        crate::config_io::write_atomic_with_backup(&target.config_path, &formatted)?;
        return Ok(WriteResult {
            action: WriteAction::Updated,
            note: None,
        });
    }

    write_crush_fresh(&target.config_path, desired, None)
}

fn write_crush_fresh(
    path: &std::path::Path,
    desired: Value,
    note: Option<String>,
) -> Result<WriteResult, String> {
    let content = serde_json::to_string_pretty(&serde_json::json!({
        "mcp": { "lean-ctx": desired }
    }))
    .map_err(|e| e.to_string())?;
    crate::config_io::write_atomic_with_backup(path, &content)?;
    Ok(WriteResult {
        action: if note.is_some() {
            WriteAction::Updated
        } else {
            WriteAction::Created
        },
        note,
    })
}

fn upsert_codex_toml(existing: &str, binary: &str) -> String {
    let mut out = String::with_capacity(existing.len() + 128);
    let mut in_section = false;
    let mut saw_section = false;
    let mut wrote_command = false;
    let mut wrote_args = false;

    for line in existing.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            if in_section && !wrote_command {
                out.push_str(&format!("command = \"{}\"\n", binary));
                wrote_command = true;
            }
            if in_section && !wrote_args {
                out.push_str("args = []\n");
                wrote_args = true;
            }
            in_section = trimmed == "[mcp_servers.lean-ctx]";
            if in_section {
                saw_section = true;
            }
            out.push_str(line);
            out.push('\n');
            continue;
        }

        if in_section {
            if trimmed.starts_with("command") && trimmed.contains('=') {
                out.push_str(&format!("command = \"{}\"\n", binary));
                wrote_command = true;
                continue;
            }
            if trimmed.starts_with("args") && trimmed.contains('=') {
                out.push_str("args = []\n");
                wrote_args = true;
                continue;
            }
        }

        out.push_str(line);
        out.push('\n');
    }

    if saw_section {
        if in_section && !wrote_command {
            out.push_str(&format!("command = \"{}\"\n", binary));
        }
        if in_section && !wrote_args {
            out.push_str("args = []\n");
        }
        return out;
    }

    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("\n[mcp_servers.lean-ctx]\n");
    out.push_str(&format!("command = \"{}\"\n", binary));
    out.push_str("args = []\n");
    out
}

fn backup_invalid_file(path: &std::path::Path) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    let parent = path
        .parent()
        .ok_or_else(|| "invalid path (no parent directory)".to_string())?;
    let filename = path
        .file_name()
        .ok_or_else(|| "invalid path (no filename)".to_string())?
        .to_string_lossy();
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let bak = parent.join(format!("{filename}.lean-ctx.invalid.{pid}.{nanos}.bak"));
    std::fs::rename(path, bak).map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn target(path: PathBuf, ty: ConfigType) -> EditorTarget {
        EditorTarget {
            name: "test",
            agent_key: "test".to_string(),
            config_path: path,
            detect_path: PathBuf::from("/nonexistent"),
            config_type: ty,
        }
    }

    #[test]
    fn mcp_json_upserts_and_preserves_other_servers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        std::fs::write(
            &path,
            r#"{ "mcpServers": { "other": { "command": "other-bin" }, "lean-ctx": { "command": "/old/path/lean-ctx", "autoApprove": [] } } }"#,
        )
        .unwrap();

        let t = target(path.clone(), ConfigType::McpJson);
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

        let t = target(path.clone(), ConfigType::Crush);
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

        let t = target(path.clone(), ConfigType::Codex);
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
    fn auto_approve_contains_core_tools() {
        let tools = auto_approve_tools();
        assert!(tools.contains(&"ctx_read"));
        assert!(tools.contains(&"ctx_shell"));
        assert!(tools.contains(&"ctx_search"));
        assert!(tools.contains(&"ctx_workflow"));
        assert!(tools.contains(&"ctx_cost"));
    }
}
