use serde_json::Value;

use super::types::{ConfigType, EditorTarget};

fn toml_quote(value: &str) -> String {
    if value.contains('\\') {
        format!("'{value}'")
    } else {
        format!("\"{value}\"")
    }
}

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
        ConfigType::JetBrains => write_jetbrains_config(target, binary, opts),
        ConfigType::Amp => write_amp_config(target, binary, opts),
        ConfigType::HermesYaml => write_hermes_yaml(target, binary, opts),
        ConfigType::GeminiSettings => write_gemini_settings(target, binary, opts),
    }
}

pub fn auto_approve_tools() -> Vec<&'static str> {
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
        "ctx_callgraph",
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

fn lean_ctx_server_entry(binary: &str, data_dir: &str, include_auto_approve: bool) -> Value {
    let mut entry = serde_json::json!({
        "command": binary,
        "env": {
            "LEAN_CTX_DATA_DIR": data_dir
        }
    });
    if include_auto_approve {
        entry["autoApprove"] = serde_json::json!(auto_approve_tools());
    }
    entry
}

const NO_AUTO_APPROVE_EDITORS: &[&str] = &["Antigravity"];

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
    let include_aa = !NO_AUTO_APPROVE_EDITORS.contains(&target.name);
    let desired = lean_ctx_server_entry(binary, &data_dir, include_aa);

    // Claude Code manages ~/.claude.json and may overwrite it on first start.
    // Prefer the official CLI integration when available.
    if target.agent_key == "claude" || target.name == "Claude Code" {
        if let Ok(result) = try_claude_mcp_add(&desired) {
            return Ok(result);
        }
    }

    if target.config_path.exists() {
        let content = std::fs::read_to_string(&target.config_path).map_err(|e| e.to_string())?;
        let mut json = match crate::core::jsonc::parse_jsonc(&content) {
            Ok(v) => v,
            Err(e) => {
                if !opts.overwrite_invalid {
                    return Err(e.to_string());
                }
                backup_invalid_file(&target.config_path)?;
                return write_mcp_json_fresh(
                    &target.config_path,
                    &desired,
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

    write_mcp_json_fresh(&target.config_path, &desired, None)
}

fn try_claude_mcp_add(desired: &Value) -> Result<WriteResult, String> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let server_json = serde_json::to_string(desired).map_err(|e| e.to_string())?;

    // On Windows, `claude` may be a `.cmd` shim and cannot be executed directly
    // via CreateProcess; route through `cmd /C` for reliable invocation.
    let mut cmd = if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.args([
            "/C", "claude", "mcp", "add-json", "--scope", "user", "lean-ctx",
        ]);
        c
    } else {
        let mut c = Command::new("claude");
        c.args(["mcp", "add-json", "--scope", "user", "lean-ctx"]);
        c
    };

    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(server_json.as_bytes());
    }

    let deadline = Duration::from_secs(3);
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return if status.success() {
                    Ok(WriteResult {
                        action: WriteAction::Updated,
                        note: Some("via claude mcp add-json".to_string()),
                    })
                } else {
                    Err("claude mcp add-json failed".to_string())
                };
            }
            Ok(None) => {
                if start.elapsed() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("claude mcp add-json timed out".to_string());
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

fn write_mcp_json_fresh(
    path: &std::path::Path,
    desired: &Value,
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
        let mut json = match crate::core::jsonc::parse_jsonc(&content) {
            Ok(v) => v,
            Err(e) => {
                if !opts.overwrite_invalid {
                    return Err(e.to_string());
                }
                backup_invalid_file(&target.config_path)?;
                return write_zed_config_fresh(
                    &target.config_path,
                    &desired,
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

    write_zed_config_fresh(&target.config_path, &desired, None)
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
        "[mcp_servers.lean-ctx]\ncommand = {}\nargs = []\n",
        toml_quote(binary)
    );
    crate::config_io::write_atomic_with_backup(&target.config_path, &content)?;
    Ok(WriteResult {
        action: WriteAction::Created,
        note: None,
    })
}

fn write_zed_config_fresh(
    path: &std::path::Path,
    desired: &Value,
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
    let data_dir = crate::core::data_dir::lean_ctx_data_dir()
        .map(|d| d.to_string_lossy().to_string())
        .unwrap_or_default();
    let desired = serde_json::json!({ "type": "stdio", "command": binary, "args": [], "env": { "LEAN_CTX_DATA_DIR": data_dir } });

    if target.config_path.exists() {
        let content = std::fs::read_to_string(&target.config_path).map_err(|e| e.to_string())?;
        let mut json = match crate::core::jsonc::parse_jsonc(&content) {
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
    let data_dir = crate::core::data_dir::lean_ctx_data_dir()
        .map(|d| d.to_string_lossy().to_string())
        .unwrap_or_default();
    let content = serde_json::to_string_pretty(&serde_json::json!({
        "servers": { "lean-ctx": { "type": "stdio", "command": binary, "args": [], "env": { "LEAN_CTX_DATA_DIR": data_dir } } }
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
    let data_dir = crate::core::data_dir::lean_ctx_data_dir()
        .map(|d| d.to_string_lossy().to_string())
        .unwrap_or_default();
    let desired = serde_json::json!({
        "type": "local",
        "command": [binary],
        "enabled": true,
        "environment": { "LEAN_CTX_DATA_DIR": data_dir }
    });

    if target.config_path.exists() {
        let content = std::fs::read_to_string(&target.config_path).map_err(|e| e.to_string())?;
        let mut json = match crate::core::jsonc::parse_jsonc(&content) {
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
    let data_dir = crate::core::data_dir::lean_ctx_data_dir()
        .map(|d| d.to_string_lossy().to_string())
        .unwrap_or_default();
    let content = serde_json::to_string_pretty(&serde_json::json!({
        "$schema": "https://opencode.ai/config.json",
        "mcp": { "lean-ctx": { "type": "local", "command": [binary], "enabled": true, "environment": { "LEAN_CTX_DATA_DIR": data_dir } } }
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

fn write_jetbrains_config(
    target: &EditorTarget,
    binary: &str,
    opts: WriteOptions,
) -> Result<WriteResult, String> {
    let data_dir = crate::core::data_dir::lean_ctx_data_dir()
        .map(|d| d.to_string_lossy().to_string())
        .unwrap_or_default();
    // JetBrains AI Assistant expects an "mcpServers" mapping in the JSON snippet
    // you paste into Settings | Tools | AI Assistant | Model Context Protocol (MCP).
    // We write that snippet to a file for easy copy/paste.
    let desired = serde_json::json!({
        "command": binary,
        "args": [],
        "env": { "LEAN_CTX_DATA_DIR": data_dir }
    });

    if target.config_path.exists() {
        let content = std::fs::read_to_string(&target.config_path).map_err(|e| e.to_string())?;
        let mut json = match crate::core::jsonc::parse_jsonc(&content) {
            Ok(v) => v,
            Err(e) => {
                if !opts.overwrite_invalid {
                    return Err(e.to_string());
                }
                backup_invalid_file(&target.config_path)?;
                let fresh = serde_json::json!({ "mcpServers": { "lean-ctx": desired } });
                let formatted = serde_json::to_string_pretty(&fresh).map_err(|e| e.to_string())?;
                crate::config_io::write_atomic_with_backup(&target.config_path, &formatted)?;
                return Ok(WriteResult {
                    action: WriteAction::Updated,
                    note: Some(
                        "overwrote invalid JSON (paste this snippet into JetBrains MCP settings)"
                            .to_string(),
                    ),
                });
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
                note: Some("paste this snippet into JetBrains MCP settings".to_string()),
            });
        }
        servers_obj.insert("lean-ctx".to_string(), desired);

        let formatted = serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?;
        crate::config_io::write_atomic_with_backup(&target.config_path, &formatted)?;
        return Ok(WriteResult {
            action: WriteAction::Updated,
            note: Some("paste this snippet into JetBrains MCP settings".to_string()),
        });
    }

    let config = serde_json::json!({ "mcpServers": { "lean-ctx": desired } });
    let formatted = serde_json::to_string_pretty(&config).map_err(|e| e.to_string())?;
    crate::config_io::write_atomic_with_backup(&target.config_path, &formatted)?;
    Ok(WriteResult {
        action: WriteAction::Created,
        note: Some("paste this snippet into JetBrains MCP settings".to_string()),
    })
}

fn write_amp_config(
    target: &EditorTarget,
    binary: &str,
    opts: WriteOptions,
) -> Result<WriteResult, String> {
    let data_dir = crate::core::data_dir::lean_ctx_data_dir()
        .map(|d| d.to_string_lossy().to_string())
        .unwrap_or_default();
    let entry = serde_json::json!({
        "command": binary,
        "env": { "LEAN_CTX_DATA_DIR": data_dir }
    });

    if target.config_path.exists() {
        let content = std::fs::read_to_string(&target.config_path).map_err(|e| e.to_string())?;
        let mut json = match crate::core::jsonc::parse_jsonc(&content) {
            Ok(v) => v,
            Err(e) => {
                if !opts.overwrite_invalid {
                    return Err(e.to_string());
                }
                backup_invalid_file(&target.config_path)?;
                let fresh = serde_json::json!({ "amp.mcpServers": { "lean-ctx": entry } });
                let formatted = serde_json::to_string_pretty(&fresh).map_err(|e| e.to_string())?;
                crate::config_io::write_atomic_with_backup(&target.config_path, &formatted)?;
                return Ok(WriteResult {
                    action: WriteAction::Updated,
                    note: Some("overwrote invalid JSON".to_string()),
                });
            }
        };
        let obj = json
            .as_object_mut()
            .ok_or_else(|| "root JSON must be an object".to_string())?;
        let servers = obj
            .entry("amp.mcpServers")
            .or_insert_with(|| serde_json::json!({}));
        let servers_obj = servers
            .as_object_mut()
            .ok_or_else(|| "\"amp.mcpServers\" must be an object".to_string())?;

        let existing = servers_obj.get("lean-ctx").cloned();
        if existing.as_ref() == Some(&entry) {
            return Ok(WriteResult {
                action: WriteAction::Already,
                note: None,
            });
        }
        servers_obj.insert("lean-ctx".to_string(), entry);

        let formatted = serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?;
        crate::config_io::write_atomic_with_backup(&target.config_path, &formatted)?;
        return Ok(WriteResult {
            action: WriteAction::Updated,
            note: None,
        });
    }

    let config = serde_json::json!({ "amp.mcpServers": { "lean-ctx": entry } });
    let formatted = serde_json::to_string_pretty(&config).map_err(|e| e.to_string())?;
    crate::config_io::write_atomic_with_backup(&target.config_path, &formatted)?;
    Ok(WriteResult {
        action: WriteAction::Created,
        note: None,
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
        let mut json = match crate::core::jsonc::parse_jsonc(&content) {
            Ok(v) => v,
            Err(e) => {
                if !opts.overwrite_invalid {
                    return Err(e.to_string());
                }
                backup_invalid_file(&target.config_path)?;
                return write_crush_fresh(
                    &target.config_path,
                    &desired,
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

    write_crush_fresh(&target.config_path, &desired, None)
}

fn write_crush_fresh(
    path: &std::path::Path,
    desired: &Value,
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
        if trimmed == "[]" {
            continue;
        }
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            if in_section && !wrote_command {
                out.push_str(&format!("command = {}\n", toml_quote(binary)));
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
                out.push_str(&format!("command = {}\n", toml_quote(binary)));
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
            out.push_str(&format!("command = {}\n", toml_quote(binary)));
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
    out.push_str(&format!("command = {}\n", toml_quote(binary)));
    out.push_str("args = []\n");
    out
}

fn write_gemini_settings(
    target: &EditorTarget,
    binary: &str,
    opts: WriteOptions,
) -> Result<WriteResult, String> {
    let data_dir = crate::core::data_dir::lean_ctx_data_dir()
        .map(|d| d.to_string_lossy().to_string())
        .unwrap_or_default();
    let entry = serde_json::json!({
        "command": binary,
        "env": { "LEAN_CTX_DATA_DIR": data_dir },
        "trust": true,
    });

    if target.config_path.exists() {
        let content = std::fs::read_to_string(&target.config_path).map_err(|e| e.to_string())?;
        let mut json = match crate::core::jsonc::parse_jsonc(&content) {
            Ok(v) => v,
            Err(e) => {
                if !opts.overwrite_invalid {
                    return Err(e.to_string());
                }
                backup_invalid_file(&target.config_path)?;
                let fresh = serde_json::json!({ "mcpServers": { "lean-ctx": entry } });
                let formatted = serde_json::to_string_pretty(&fresh).map_err(|e| e.to_string())?;
                crate::config_io::write_atomic_with_backup(&target.config_path, &formatted)?;
                return Ok(WriteResult {
                    action: WriteAction::Updated,
                    note: Some("overwrote invalid JSON".to_string()),
                });
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
        if existing.as_ref() == Some(&entry) {
            return Ok(WriteResult {
                action: WriteAction::Already,
                note: None,
            });
        }
        servers_obj.insert("lean-ctx".to_string(), entry);

        let formatted = serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?;
        crate::config_io::write_atomic_with_backup(&target.config_path, &formatted)?;
        return Ok(WriteResult {
            action: WriteAction::Updated,
            note: None,
        });
    }

    let config = serde_json::json!({ "mcpServers": { "lean-ctx": entry } });
    let formatted = serde_json::to_string_pretty(&config).map_err(|e| e.to_string())?;
    crate::config_io::write_atomic_with_backup(&target.config_path, &formatted)?;
    Ok(WriteResult {
        action: WriteAction::Created,
        note: None,
    })
}

fn write_hermes_yaml(
    target: &EditorTarget,
    binary: &str,
    _opts: WriteOptions,
) -> Result<WriteResult, String> {
    let data_dir = default_data_dir()?;

    let lean_ctx_block = format!(
        "  lean-ctx:\n    command: \"{binary}\"\n    env:\n      LEAN_CTX_DATA_DIR: \"{data_dir}\""
    );

    if target.config_path.exists() {
        let content = std::fs::read_to_string(&target.config_path).map_err(|e| e.to_string())?;

        if content.contains("lean-ctx") {
            return Ok(WriteResult {
                action: WriteAction::Already,
                note: None,
            });
        }

        let updated = upsert_hermes_yaml_mcp(&content, &lean_ctx_block);
        crate::config_io::write_atomic_with_backup(&target.config_path, &updated)?;
        return Ok(WriteResult {
            action: WriteAction::Updated,
            note: None,
        });
    }

    let content = format!("mcp_servers:\n{lean_ctx_block}\n");
    crate::config_io::write_atomic_with_backup(&target.config_path, &content)?;
    Ok(WriteResult {
        action: WriteAction::Created,
        note: None,
    })
}

fn upsert_hermes_yaml_mcp(existing: &str, lean_ctx_block: &str) -> String {
    let mut out = String::with_capacity(existing.len() + lean_ctx_block.len() + 32);
    let mut in_mcp_section = false;
    let mut saw_mcp_child = false;
    let mut inserted = false;
    let lines: Vec<&str> = existing.lines().collect();

    for line in &lines {
        if !inserted && line.trim_end() == "mcp_servers:" {
            in_mcp_section = true;
            out.push_str(line);
            out.push('\n');
            continue;
        }

        if in_mcp_section && !inserted {
            let is_child = line.starts_with("  ") && !line.trim().is_empty();
            let is_toplevel = !line.starts_with(' ') && !line.trim().is_empty();

            if is_child {
                saw_mcp_child = true;
                out.push_str(line);
                out.push('\n');
                continue;
            }

            if saw_mcp_child && (line.trim().is_empty() || is_toplevel) {
                out.push_str(lean_ctx_block);
                out.push('\n');
                inserted = true;
                in_mcp_section = false;
            }
        }

        out.push_str(line);
        out.push('\n');
    }

    if in_mcp_section && !inserted {
        out.push_str(lean_ctx_block);
        out.push('\n');
        inserted = true;
    }

    if !inserted {
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("\nmcp_servers:\n");
        out.push_str(lean_ctx_block);
        out.push('\n');
    }

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
        .map_or(0, |d| d.as_nanos());
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
    fn auto_approve_contains_core_tools() {
        let tools = auto_approve_tools();
        assert!(tools.contains(&"ctx_read"));
        assert!(tools.contains(&"ctx_shell"));
        assert!(tools.contains(&"ctx_search"));
        assert!(tools.contains(&"ctx_workflow"));
        assert!(tools.contains(&"ctx_cost"));
    }

    #[test]
    fn antigravity_config_omits_auto_approve() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp_config.json");

        let t = EditorTarget {
            name: "Antigravity",
            agent_key: "gemini".to_string(),
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
        std::fs::write(
            &path,
            "mcp_servers:\n  lean-ctx:\n    command: \"lean-ctx\"\n",
        )
        .unwrap();
        let t = target(path.clone(), ConfigType::HermesYaml);
        let res = write_hermes_yaml(&t, "lean-ctx", WriteOptions::default()).unwrap();
        assert_eq!(res.action, WriteAction::Already);
    }
}
