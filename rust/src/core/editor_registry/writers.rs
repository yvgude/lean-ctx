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
        ConfigType::CopilotCli => write_copilot_cli(target, binary, opts),
        ConfigType::OpenCode => write_opencode_config(target, binary, opts),
        ConfigType::Crush => write_crush_config(target, binary, opts),
        ConfigType::JetBrains => write_jetbrains_config(target, binary, opts),
        ConfigType::Amp => write_amp_config(target, binary, opts),
        ConfigType::HermesYaml => write_hermes_yaml(target, binary, opts),
        ConfigType::GeminiSettings => write_gemini_settings(target, binary, opts),
        ConfigType::QoderSettings => write_qoder_settings(target, binary, opts),
        ConfigType::AugmentVsCode => write_augment_vscode(target, binary, opts),
    }
}

pub fn remove_lean_ctx_mcp_server(
    path: &std::path::Path,
    opts: WriteOptions,
) -> Result<WriteResult, String> {
    if !path.exists() {
        return Ok(WriteResult {
            action: WriteAction::Already,
            note: Some("mcp.json not found".to_string()),
        });
    }

    let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut json = match crate::core::jsonc::parse_jsonc(&content) {
        Ok(v) => v,
        Err(e) => {
            if !opts.overwrite_invalid {
                return Err(e.to_string());
            }
            eprintln!(
                "\x1b[33m⚠\x1b[0m  {} has JSON syntax errors — skipping removal.",
                path.display()
            );
            return Ok(WriteResult {
                action: WriteAction::Already,
                note: Some("invalid JSON — cannot safely remove lean-ctx entry".to_string()),
            });
        }
    };

    let obj = json
        .as_object_mut()
        .ok_or_else(|| "root JSON must be an object".to_string())?;

    let Some(servers) = obj.get_mut("mcpServers") else {
        return Ok(WriteResult {
            action: WriteAction::Already,
            note: Some("no mcpServers key".to_string()),
        });
    };
    let servers_obj = servers
        .as_object_mut()
        .ok_or_else(|| "\"mcpServers\" must be an object".to_string())?;

    if servers_obj.remove("lean-ctx").is_none() {
        return Ok(WriteResult {
            action: WriteAction::Already,
            note: Some("lean-ctx not configured".to_string()),
        });
    }

    let formatted = serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?;
    crate::config_io::write_atomic_with_backup(path, &formatted)?;
    Ok(WriteResult {
        action: WriteAction::Updated,
        note: Some("removed lean-ctx from mcpServers".to_string()),
    })
}

pub fn remove_lean_ctx_server(
    target: &EditorTarget,
    opts: WriteOptions,
) -> Result<WriteResult, String> {
    match target.config_type {
        ConfigType::McpJson
        | ConfigType::JetBrains
        | ConfigType::GeminiSettings
        | ConfigType::QoderSettings => remove_lean_ctx_mcp_server(&target.config_path, opts),
        ConfigType::VsCodeMcp | ConfigType::CopilotCli => {
            remove_lean_ctx_vscode_server(&target.config_path, opts)
        }
        ConfigType::Codex => remove_lean_ctx_codex_server(&target.config_path),
        ConfigType::OpenCode | ConfigType::Crush => {
            remove_lean_ctx_named_json_server(&target.config_path, "mcp", opts)
        }
        ConfigType::Zed => {
            remove_lean_ctx_named_json_server(&target.config_path, "context_servers", opts)
        }
        ConfigType::Amp => remove_lean_ctx_amp_server(&target.config_path, opts),
        ConfigType::HermesYaml => remove_lean_ctx_hermes_yaml_server(&target.config_path),
        ConfigType::AugmentVsCode => {
            remove_lean_ctx_augment_vscode_server(&target.config_path, opts)
        }
    }
}

fn remove_lean_ctx_vscode_server(
    path: &std::path::Path,
    opts: WriteOptions,
) -> Result<WriteResult, String> {
    if !path.exists() {
        return Ok(WriteResult {
            action: WriteAction::Already,
            note: Some("vscode mcp.json not found".to_string()),
        });
    }

    let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut json = match crate::core::jsonc::parse_jsonc(&content) {
        Ok(v) => v,
        Err(e) => {
            if !opts.overwrite_invalid {
                return Err(e.to_string());
            }
            eprintln!(
                "\x1b[33m⚠\x1b[0m  {} has JSON syntax errors — skipping removal.",
                path.display()
            );
            return Ok(WriteResult {
                action: WriteAction::Already,
                note: Some("invalid JSON — cannot safely remove lean-ctx entry".to_string()),
            });
        }
    };

    let obj = json
        .as_object_mut()
        .ok_or_else(|| "root JSON must be an object".to_string())?;

    let Some(servers) = obj.get_mut("servers") else {
        return Ok(WriteResult {
            action: WriteAction::Already,
            note: Some("no servers key".to_string()),
        });
    };
    let servers_obj = servers
        .as_object_mut()
        .ok_or_else(|| "\"servers\" must be an object".to_string())?;

    if servers_obj.remove("lean-ctx").is_none() {
        return Ok(WriteResult {
            action: WriteAction::Already,
            note: Some("lean-ctx not configured".to_string()),
        });
    }

    let formatted = serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?;
    crate::config_io::write_atomic_with_backup(path, &formatted)?;
    Ok(WriteResult {
        action: WriteAction::Updated,
        note: Some("removed lean-ctx from servers".to_string()),
    })
}

fn remove_lean_ctx_amp_server(
    path: &std::path::Path,
    opts: WriteOptions,
) -> Result<WriteResult, String> {
    if !path.exists() {
        return Ok(WriteResult {
            action: WriteAction::Already,
            note: Some("amp settings not found".to_string()),
        });
    }

    let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut json = match crate::core::jsonc::parse_jsonc(&content) {
        Ok(v) => v,
        Err(e) => {
            if !opts.overwrite_invalid {
                return Err(e.to_string());
            }
            eprintln!(
                "\x1b[33m⚠\x1b[0m  {} has JSON syntax errors — skipping removal.",
                path.display()
            );
            return Ok(WriteResult {
                action: WriteAction::Already,
                note: Some("invalid JSON — cannot safely remove lean-ctx entry".to_string()),
            });
        }
    };

    let obj = json
        .as_object_mut()
        .ok_or_else(|| "root JSON must be an object".to_string())?;
    let Some(servers) = obj.get_mut("amp.mcpServers") else {
        return Ok(WriteResult {
            action: WriteAction::Already,
            note: Some("no amp.mcpServers key".to_string()),
        });
    };
    let servers_obj = servers
        .as_object_mut()
        .ok_or_else(|| "\"amp.mcpServers\" must be an object".to_string())?;

    if servers_obj.remove("lean-ctx").is_none() {
        return Ok(WriteResult {
            action: WriteAction::Already,
            note: Some("lean-ctx not configured".to_string()),
        });
    }

    let formatted = serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?;
    crate::config_io::write_atomic_with_backup(path, &formatted)?;
    Ok(WriteResult {
        action: WriteAction::Updated,
        note: Some("removed lean-ctx from amp.mcpServers".to_string()),
    })
}

fn remove_lean_ctx_named_json_server(
    path: &std::path::Path,
    container_key: &str,
    opts: WriteOptions,
) -> Result<WriteResult, String> {
    if !path.exists() {
        return Ok(WriteResult {
            action: WriteAction::Already,
            note: Some("config not found".to_string()),
        });
    }

    let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut json = match crate::core::jsonc::parse_jsonc(&content) {
        Ok(v) => v,
        Err(e) => {
            if !opts.overwrite_invalid {
                return Err(e.to_string());
            }
            eprintln!(
                "\x1b[33m⚠\x1b[0m  {} has JSON syntax errors — skipping removal.",
                path.display()
            );
            return Ok(WriteResult {
                action: WriteAction::Already,
                note: Some("invalid JSON — cannot safely remove lean-ctx entry".to_string()),
            });
        }
    };

    let obj = json
        .as_object_mut()
        .ok_or_else(|| "root JSON must be an object".to_string())?;
    let Some(container) = obj.get_mut(container_key) else {
        return Ok(WriteResult {
            action: WriteAction::Already,
            note: Some(format!("no {container_key} key")),
        });
    };
    let container_obj = container
        .as_object_mut()
        .ok_or_else(|| format!("\"{container_key}\" must be an object"))?;

    if container_obj.remove("lean-ctx").is_none() {
        return Ok(WriteResult {
            action: WriteAction::Already,
            note: Some("lean-ctx not configured".to_string()),
        });
    }

    let formatted = serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?;
    crate::config_io::write_atomic_with_backup(path, &formatted)?;
    Ok(WriteResult {
        action: WriteAction::Updated,
        note: Some(format!("removed lean-ctx from {container_key}")),
    })
}

fn remove_lean_ctx_codex_server(path: &std::path::Path) -> Result<WriteResult, String> {
    if !path.exists() {
        return Ok(WriteResult {
            action: WriteAction::Already,
            note: Some("codex config not found".to_string()),
        });
    }
    let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let updated = remove_codex_toml_section(&content, "[mcp_servers.lean-ctx]");
    if updated == content {
        return Ok(WriteResult {
            action: WriteAction::Already,
            note: Some("lean-ctx not configured".to_string()),
        });
    }
    crate::config_io::write_atomic_with_backup(path, &updated)?;
    Ok(WriteResult {
        action: WriteAction::Updated,
        note: Some("removed [mcp_servers.lean-ctx]".to_string()),
    })
}

fn remove_codex_toml_section(existing: &str, header: &str) -> String {
    let prefix = header.trim_end_matches(']');
    let mut out = String::with_capacity(existing.len());
    let mut skipping = false;
    for line in existing.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            if trimmed == header || trimmed.starts_with(&format!("{prefix}.")) {
                skipping = true;
                continue;
            }
            skipping = false;
        }
        if skipping {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn remove_lean_ctx_hermes_yaml_server(path: &std::path::Path) -> Result<WriteResult, String> {
    if !path.exists() {
        return Ok(WriteResult {
            action: WriteAction::Already,
            note: Some("hermes config not found".to_string()),
        });
    }
    let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let updated = remove_hermes_yaml_mcp_server_block(&content, "lean-ctx");
    if updated == content {
        return Ok(WriteResult {
            action: WriteAction::Already,
            note: Some("lean-ctx not configured".to_string()),
        });
    }
    crate::config_io::write_atomic_with_backup(path, &updated)?;
    Ok(WriteResult {
        action: WriteAction::Updated,
        note: Some("removed lean-ctx from mcp_servers".to_string()),
    })
}

fn remove_hermes_yaml_mcp_server_block(existing: &str, name: &str) -> String {
    let mut out = String::with_capacity(existing.len());
    let mut in_mcp = false;
    let mut skipping = false;
    for line in existing.lines() {
        let trimmed = line.trim_end();
        if trimmed == "mcp_servers:" {
            in_mcp = true;
            out.push_str(line);
            out.push('\n');
            continue;
        }

        if in_mcp {
            let is_child = line.starts_with("  ") && !line.starts_with("    ");
            let is_toplevel = !line.starts_with(' ') && !line.trim().is_empty();

            if is_toplevel {
                in_mcp = false;
                skipping = false;
            }

            if skipping {
                if is_child || is_toplevel {
                    skipping = false;
                    out.push_str(line);
                    out.push('\n');
                }
                continue;
            }

            if is_child && line.trim() == format!("{name}:") {
                skipping = true;
                continue;
            }
        }

        out.push_str(line);
        out.push('\n');
    }
    out
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
        "ctx_multi_read",
        "ctx_semantic_search",
        "ctx_symbol",
        "ctx_outline",
        "ctx_callgraph",
        "ctx_refactor",
        "ctx_routes",
        "ctx_cost",
        "ctx_heatmap",
        "ctx_gain",
        "ctx_expand",
        "ctx_task",
        "ctx_impact",
        "ctx_architecture",
        "ctx_workflow",
        "ctx_review",
        "ctx_pack",
        "ctx_index",
        "ctx_artifacts",
        "ctx_smells",
        "ctx_proof",
        "ctx_verify",
        "ctx_execute",
        "ctx_handoff",
        "ctx_feedback",
        "ctx_control",
        "ctx_plan",
        "ctx_compile",
        "ctx_discover_tools",
        "ctx_provider",
        "ctx_radar",
        "ctx_retrieve",
        "ctx_compress_memory",
        "ctx_load_tools",
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

fn lean_ctx_server_entry_with_instructions(
    binary: &str,
    data_dir: &str,
    include_auto_approve: bool,
    agent_key: &str,
) -> Value {
    let mut entry = lean_ctx_server_entry(binary, data_dir, include_auto_approve);
    let mode = crate::core::rules_canonical::Mode::from_hook_mode(
        &crate::hooks::recommend_hook_mode(agent_key),
    );
    let instructions = crate::core::rules_canonical::mcp_instructions(mode);

    let constraints = crate::core::client_constraints::by_client_id(agent_key);
    if let Some(max_chars) = constraints.and_then(|c| c.mcp_instructions_max_chars) {
        let truncated = if instructions.len() > max_chars {
            &instructions[..max_chars]
        } else {
            instructions
        };
        entry["instructions"] = serde_json::json!(truncated);
    }
    entry
}

fn supports_auto_approve(target: &EditorTarget) -> bool {
    crate::core::client_constraints::by_editor_name(target.name)
        .is_some_and(|c| c.supports_auto_approve)
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
    let include_aa = supports_auto_approve(target);
    let desired = if target.agent_key.is_empty() {
        lean_ctx_server_entry(binary, &data_dir, include_aa)
    } else {
        lean_ctx_server_entry_with_instructions(binary, &data_dir, include_aa, &target.agent_key)
    };

    // Claude Code manages ~/.claude.json and may overwrite it on first start.
    // Prefer the official CLI integration when available.
    // Skip when LEAN_CTX_QUIET=1 (bootstrap --json / setup --json) to avoid
    // spawning `claude mcp add-json` which can stall in non-interactive CI.
    if (target.agent_key == "claude" || target.name == "Claude Code")
        && !matches!(std::env::var("LEAN_CTX_QUIET"), Ok(v) if v.trim() == "1")
    {
        if let Ok(result) = try_claude_mcp_add(&desired) {
            return Ok(result);
        }
    }

    if target.config_path.exists() {
        let content = std::fs::read_to_string(&target.config_path).map_err(|e| e.to_string())?;
        let mut json = match crate::core::jsonc::parse_jsonc(&content) {
            Ok(v) => v,
            Err(_e) => {
                return handle_invalid_json_write(
                    &target.config_path,
                    &content,
                    "mcpServers",
                    "lean-ctx",
                    &desired,
                    opts.overwrite_invalid,
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

fn find_in_path(binary: &str) -> Option<std::path::PathBuf> {
    let path_var = std::env::var("PATH").ok()?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(binary);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn validate_claude_binary() -> Result<std::path::PathBuf, String> {
    let path = find_in_path("claude").ok_or("claude binary not found in PATH")?;

    let canonical =
        std::fs::canonicalize(&path).map_err(|e| format!("cannot resolve claude path: {e}"))?;

    let canonical_str = canonical.to_string_lossy();
    let is_trusted = canonical_str.contains("/.claude/")
        || canonical_str.contains("\\AppData\\")
        || canonical_str.contains("/usr/local/bin/")
        || canonical_str.contains("/opt/homebrew/")
        || canonical_str.contains("/nix/store/")
        || canonical_str.contains("/.npm/")
        || canonical_str.contains("/.nvm/")
        || canonical_str.contains("/node_modules/.bin/")
        || std::env::var("LEAN_CTX_TRUST_CLAUDE_PATH").is_ok();

    if !is_trusted {
        return Err(format!(
            "claude binary resolved to untrusted path: {canonical_str} — set LEAN_CTX_TRUST_CLAUDE_PATH=1 to override"
        ));
    }
    Ok(canonical)
}

fn try_claude_mcp_add(desired: &Value) -> Result<WriteResult, String> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let server_json = serde_json::to_string(desired).map_err(|e| e.to_string())?;

    let mut cmd = if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.args([
            "/C", "claude", "mcp", "add-json", "--scope", "user", "lean-ctx",
        ]);
        c
    } else {
        let claude_path = validate_claude_binary()?;
        let mut c = Command::new(claude_path);
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
        "command": binary,
        "args": [],
        "env": {}
    });

    if target.config_path.exists() {
        let content = std::fs::read_to_string(&target.config_path).map_err(|e| e.to_string())?;
        let mut json = match crate::core::jsonc::parse_jsonc(&content) {
            Ok(v) => v,
            Err(_e) => {
                return handle_invalid_json_write(
                    &target.config_path,
                    &content,
                    "context_servers",
                    "lean-ctx",
                    &desired,
                    opts.overwrite_invalid,
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
        .map_err(|_| "LEAN_CTX_DATA_DIR unavailable".to_string())?;
    let desired = serde_json::json!({ "type": "stdio", "command": binary, "args": [], "env": { "LEAN_CTX_DATA_DIR": data_dir } });

    if target.config_path.exists() {
        let content = std::fs::read_to_string(&target.config_path).map_err(|e| e.to_string())?;
        let mut json = match crate::core::jsonc::parse_jsonc(&content) {
            Ok(v) => v,
            Err(_e) => {
                return handle_invalid_json_write(
                    &target.config_path,
                    &content,
                    "servers",
                    "lean-ctx",
                    &desired,
                    opts.overwrite_invalid,
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
        .map_err(|_| "LEAN_CTX_DATA_DIR unavailable".to_string())?;
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

fn write_copilot_cli(
    target: &EditorTarget,
    binary: &str,
    opts: WriteOptions,
) -> Result<WriteResult, String> {
    let data_dir = crate::core::data_dir::lean_ctx_data_dir()
        .map(|d| d.to_string_lossy().to_string())
        .map_err(|_| "LEAN_CTX_DATA_DIR unavailable".to_string())?;
    let desired = serde_json::json!({
        "type": "local",
        "command": binary,
        "args": ["mcp"],
        "env": { "LEAN_CTX_DATA_DIR": data_dir },
        "tools": ["*"]
    });

    if target.config_path.exists() {
        let content = std::fs::read_to_string(&target.config_path).map_err(|e| e.to_string())?;
        let mut json = match crate::core::jsonc::parse_jsonc(&content) {
            Ok(v) => v,
            Err(_e) => {
                return handle_invalid_json_write(
                    &target.config_path,
                    &content,
                    "mcpServers",
                    "lean-ctx",
                    &desired,
                    opts.overwrite_invalid,
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
        let out = serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?;
        crate::config_io::write_atomic_with_backup(&target.config_path, &out)?;
        return Ok(WriteResult {
            action: WriteAction::Updated,
            note: None,
        });
    }

    // Fresh write
    if let Some(parent) = target.config_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let content = serde_json::to_string_pretty(&serde_json::json!({
        "mcpServers": {
            "lean-ctx": {
                "type": "local",
                "command": binary,
                "args": ["mcp"],
                "env": { "LEAN_CTX_DATA_DIR": data_dir },
                "tools": ["*"]
            }
        }
    }))
    .map_err(|e| e.to_string())?;
    crate::config_io::write_atomic_with_backup(&target.config_path, &content)?;
    Ok(WriteResult {
        action: WriteAction::Created,
        note: None,
    })
}

fn write_opencode_config(
    target: &EditorTarget,
    binary: &str,
    opts: WriteOptions,
) -> Result<WriteResult, String> {
    let data_dir = crate::core::data_dir::lean_ctx_data_dir()
        .map(|d| d.to_string_lossy().to_string())
        .map_err(|_| "LEAN_CTX_DATA_DIR unavailable".to_string())?;
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
            Err(_e) => {
                return handle_invalid_json_write(
                    &target.config_path,
                    &content,
                    "mcp",
                    "lean-ctx",
                    &desired,
                    opts.overwrite_invalid,
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
        .map_err(|_| "LEAN_CTX_DATA_DIR unavailable".to_string())?;
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
        .map_err(|_| "LEAN_CTX_DATA_DIR unavailable".to_string())?;
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
            Err(_e) => {
                return handle_invalid_json_write(
                    &target.config_path,
                    &content,
                    "mcpServers",
                    "lean-ctx",
                    &desired,
                    opts.overwrite_invalid,
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
        .map_err(|_| "LEAN_CTX_DATA_DIR unavailable".to_string())?;
    let entry = serde_json::json!({
        "command": binary,
        "env": { "LEAN_CTX_DATA_DIR": data_dir }
    });

    if target.config_path.exists() {
        let content = std::fs::read_to_string(&target.config_path).map_err(|e| e.to_string())?;
        let mut json = match crate::core::jsonc::parse_jsonc(&content) {
            Ok(v) => v,
            Err(_e) => {
                return handle_invalid_json_write(
                    &target.config_path,
                    &content,
                    "amp.mcpServers",
                    "lean-ctx",
                    &entry,
                    opts.overwrite_invalid,
                );
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
    let data_dir = crate::core::data_dir::lean_ctx_data_dir()
        .map(|d| d.to_string_lossy().to_string())
        .map_err(|_| "LEAN_CTX_DATA_DIR unavailable".to_string())?;
    let desired = serde_json::json!({
        "type": "stdio",
        "command": binary,
        "env": { "LEAN_CTX_DATA_DIR": data_dir }
    });

    if target.config_path.exists() {
        let content = std::fs::read_to_string(&target.config_path).map_err(|e| e.to_string())?;
        let mut json = match crate::core::jsonc::parse_jsonc(&content) {
            Ok(v) => v,
            Err(_e) => {
                return handle_invalid_json_write(
                    &target.config_path,
                    &content,
                    "mcp",
                    "lean-ctx",
                    &desired,
                    opts.overwrite_invalid,
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
    let mut inserted_parent_before_subtable = false;

    let parent_block = format!(
        "[mcp_servers.lean-ctx]\ncommand = {}\nargs = []\n\n",
        toml_quote(binary)
    );

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
            } else if !saw_section
                && !inserted_parent_before_subtable
                && trimmed.starts_with("[mcp_servers.lean-ctx.")
            {
                out.push_str(&parent_block);
                inserted_parent_before_subtable = true;
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

    if inserted_parent_before_subtable {
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
        .map_err(|_| "LEAN_CTX_DATA_DIR unavailable".to_string())?;
    let entry = serde_json::json!({
        "command": binary,
        "env": { "LEAN_CTX_DATA_DIR": data_dir },
        "trust": true,
    });

    if target.config_path.exists() {
        let content = std::fs::read_to_string(&target.config_path).map_err(|e| e.to_string())?;
        let mut json = match crate::core::jsonc::parse_jsonc(&content) {
            Ok(v) => v,
            Err(_e) => {
                return handle_invalid_json_write(
                    &target.config_path,
                    &content,
                    "mcpServers",
                    "lean-ctx",
                    &entry,
                    opts.overwrite_invalid,
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
            let has_correct_binary = content.contains(binary);
            let has_correct_data_dir = content.contains(&data_dir);
            if has_correct_binary && has_correct_data_dir {
                return Ok(WriteResult {
                    action: WriteAction::Already,
                    note: None,
                });
            }
            let cleaned = remove_hermes_yaml_lean_ctx_block(&content);
            let updated = upsert_hermes_yaml_mcp(&cleaned, &lean_ctx_block);
            crate::config_io::write_atomic_with_backup(&target.config_path, &updated)?;
            return Ok(WriteResult {
                action: WriteAction::Updated,
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

fn remove_hermes_yaml_lean_ctx_block(content: &str) -> String {
    let mut out = String::with_capacity(content.len());
    let mut skip = false;
    for line in content.lines() {
        if line.trim_start().starts_with("lean-ctx:")
            && (line.starts_with("  ") || line.starts_with('\t'))
        {
            skip = true;
            continue;
        }
        if skip {
            let indented = line.starts_with("    ") || line.starts_with("\t\t");
            let empty = line.trim().is_empty();
            if indented || empty {
                continue;
            }
            skip = false;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn write_qoder_settings(
    target: &EditorTarget,
    binary: &str,
    opts: WriteOptions,
) -> Result<WriteResult, String> {
    let data_dir = default_data_dir()?;
    let desired = serde_json::json!({
        "command": binary,
        "args": [],
        "env": {
            "LEAN_CTX_DATA_DIR": data_dir,
            "LEAN_CTX_FULL_TOOLS": "1"
        }
    });

    if target.config_path.exists() {
        let content = std::fs::read_to_string(&target.config_path).map_err(|e| e.to_string())?;
        let mut json = match crate::core::jsonc::parse_jsonc(&content) {
            Ok(v) => v,
            Err(_e) => {
                return handle_invalid_json_write(
                    &target.config_path,
                    &content,
                    "mcpServers",
                    "lean-ctx",
                    &desired,
                    opts.overwrite_invalid,
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

// ---------------------------------------------------------------------------
// Augment VS Code extension writer
//
// `augment.vscode-augment` persists registered MCP servers as a top-level JSON
// array under its globalStorage directory. The extension keys entries by the
// `id` field (a UUID), and we need that id to stay stable so repeated
// `init --agent augment` calls don't litter the list with duplicates.
//
// Schema (validated empirically 2026-05-21):
//   { type, id, name, disabled, command, args, env, useShellInterpolation }
// ---------------------------------------------------------------------------

/// Fixed UUIDv4-shaped id reserved for lean-ctx in Augment's VS Code MCP list.
/// The first segment hex-encodes "lean" (6c 65 61 6e) and the last segment
/// hex-encodes "leanct" (6c 65 61 6e 63 74) — a 6-byte ASCII tag that fits
/// exactly in the 12-hex-char node field. The middle bytes preserve the
/// version-4 / variant-RFC-4122 nibbles so the value parses as a valid UUID.
/// Only stability matters — the writer uses this id to locate and update its
/// own entry idempotently without colliding with user-added servers.
const LEAN_CTX_AUGMENT_VSCODE_ID: &str = "6c65616e-c747-4000-8000-6c65616e6374";

fn lean_ctx_augment_vscode_entry(binary: &str, data_dir: &str) -> Value {
    serde_json::json!({
        "type": "stdio",
        "id": LEAN_CTX_AUGMENT_VSCODE_ID,
        "name": "lean-ctx",
        "disabled": false,
        "command": binary,
        "args": [],
        "useShellInterpolation": false,
        "env": {
            "LEAN_CTX_DATA_DIR": data_dir
        }
    })
}

fn write_augment_vscode(
    target: &EditorTarget,
    binary: &str,
    opts: WriteOptions,
) -> Result<WriteResult, String> {
    let data_dir = default_data_dir()?;
    let desired = lean_ctx_augment_vscode_entry(binary, &data_dir);

    if !target.config_path.exists() {
        let arr = serde_json::Value::Array(vec![desired]);
        let content = serde_json::to_string_pretty(&arr).map_err(|e| e.to_string())?;
        crate::config_io::write_atomic_with_backup(&target.config_path, &content)?;
        return Ok(WriteResult {
            action: WriteAction::Created,
            note: None,
        });
    }

    let content = std::fs::read_to_string(&target.config_path).map_err(|e| e.to_string())?;
    let mut json: Value = match crate::core::jsonc::parse_jsonc(&content) {
        Ok(v) => v,
        Err(e) => {
            if !opts.overwrite_invalid {
                return Err(e.to_string());
            }
            eprintln!(
                "\x1b[33m⚠\x1b[0m  {} has JSON syntax errors — replacing with a clean array.",
                target.config_path.display()
            );
            backup_invalid_file(&target.config_path)?;
            let arr = serde_json::Value::Array(vec![desired]);
            let content = serde_json::to_string_pretty(&arr).map_err(|e| e.to_string())?;
            crate::config_io::write_atomic_with_backup(&target.config_path, &content)?;
            return Ok(WriteResult {
                action: WriteAction::Updated,
                note: Some("replaced invalid JSON with clean array".to_string()),
            });
        }
    };

    let arr = json.as_array_mut().ok_or_else(|| {
        "augment vscode mcpServers.json must contain a top-level JSON array".to_string()
    })?;

    if let Some(existing) = arr.iter_mut().find(|entry| {
        entry.get("name").and_then(|n| n.as_str()) == Some("lean-ctx")
            || entry.get("id").and_then(|i| i.as_str()) == Some(LEAN_CTX_AUGMENT_VSCODE_ID)
    }) {
        if *existing == desired {
            return Ok(WriteResult {
                action: WriteAction::Already,
                note: None,
            });
        }
        *existing = desired;
    } else {
        arr.push(desired);
    }

    let formatted = serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?;
    crate::config_io::write_atomic_with_backup(&target.config_path, &formatted)?;
    Ok(WriteResult {
        action: WriteAction::Updated,
        note: None,
    })
}

fn remove_lean_ctx_augment_vscode_server(
    path: &std::path::Path,
    opts: WriteOptions,
) -> Result<WriteResult, String> {
    if !path.exists() {
        return Ok(WriteResult {
            action: WriteAction::Already,
            note: Some("augment vscode mcpServers.json not found".to_string()),
        });
    }

    let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut json: Value = match crate::core::jsonc::parse_jsonc(&content) {
        Ok(v) => v,
        Err(e) => {
            if !opts.overwrite_invalid {
                return Err(e.to_string());
            }
            eprintln!(
                "\x1b[33m⚠\x1b[0m  {} has JSON syntax errors — skipping removal.",
                path.display()
            );
            return Ok(WriteResult {
                action: WriteAction::Already,
                note: Some("invalid JSON — cannot safely remove lean-ctx entry".to_string()),
            });
        }
    };

    let arr = json
        .as_array_mut()
        .ok_or_else(|| "augment vscode mcpServers.json must be a JSON array".to_string())?;

    let before = arr.len();
    arr.retain(|entry| {
        let name_match = entry.get("name").and_then(|n| n.as_str()) == Some("lean-ctx");
        let id_match = entry.get("id").and_then(|i| i.as_str()) == Some(LEAN_CTX_AUGMENT_VSCODE_ID);
        !(name_match || id_match)
    });
    if arr.len() == before {
        return Ok(WriteResult {
            action: WriteAction::Already,
            note: Some("lean-ctx not configured".to_string()),
        });
    }

    let formatted = serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?;
    crate::config_io::write_atomic_with_backup(path, &formatted)?;
    Ok(WriteResult {
        action: WriteAction::Updated,
        note: Some("removed lean-ctx from augment vscode mcp list".to_string()),
    })
}

fn backup_invalid_file(path: &std::path::Path) -> Result<std::path::PathBuf, String> {
    if !path.exists() {
        return Ok(path.to_path_buf());
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
    std::fs::copy(path, &bak).map_err(|e| e.to_string())?;
    Ok(bak)
}

/// Safe handler for invalid JSON config files. NEVER silently overwrites.
/// Strategy:
/// 1. If lean-ctx is already present in text → skip (no-op)
/// 2. Try text-based injection into the container key
/// 3. If injection fails → warn user with clear instructions, do NOT modify file
fn handle_invalid_json_write(
    path: &std::path::Path,
    content: &str,
    container_key: &str,
    entry_key: &str,
    value: &serde_json::Value,
    allow_inject: bool,
) -> Result<WriteResult, String> {
    if content.contains(&format!("\"{entry_key}\"")) {
        eprintln!(
            "\x1b[33m⚠\x1b[0m  {} has JSON syntax errors but already contains \"{entry_key}\".",
            path.display()
        );
        eprintln!("   Skipping — your config is untouched.");
        return Ok(WriteResult {
            action: WriteAction::Already,
            note: Some(format!("invalid JSON, {entry_key} already present")),
        });
    }

    if !allow_inject {
        return Err(format!(
            "{} contains invalid JSON. Fix the syntax and re-run lean-ctx setup.\n  Path: {}",
            path.display(),
            path.display()
        ));
    }

    // Try text-based injection
    if let Some(patched) = try_text_inject_mcp_entry(content, container_key, entry_key, value) {
        let bak = backup_invalid_file(path)?;
        crate::config_io::write_atomic_with_backup(path, &patched)?;
        eprintln!(
            "\x1b[32m✓\x1b[0m  Added {entry_key} to {} (text-based; file has syntax errors).",
            path.display()
        );
        eprintln!("   \x1b[33mNote:\x1b[0m Your config has JSON syntax errors — please fix them.");
        eprintln!("   Backup: {}", bak.display());
        return Ok(WriteResult {
            action: WriteAction::Updated,
            note: Some(format!(
                "text-injected into invalid JSON (backup: {})",
                bak.display()
            )),
        });
    }

    // Cannot safely modify — inform user
    eprintln!(
        "\x1b[33m⚠\x1b[0m  {} contains invalid JSON that lean-ctx cannot safely modify.",
        path.display()
    );
    eprintln!("   \x1b[1mYour config was NOT changed.\x1b[0m");
    eprintln!("   To fix:");
    eprintln!(
        "     1. Open {} and correct the JSON syntax errors",
        path.display()
    );
    eprintln!("     2. Re-run: lean-ctx setup");
    eprintln!("   (Common issue: trailing commas, missing quotes, unmatched braces)");
    Ok(WriteResult {
        action: WriteAction::Already,
        note: Some(format!(
            "invalid JSON — user must fix manually: {}",
            path.display()
        )),
    })
}

/// Attempt to inject an MCP entry into a JSON file using text manipulation.
/// Preserves the original file content even if it has syntax errors.
/// Returns None if text structure doesn't allow safe injection.
fn try_text_inject_mcp_entry(
    content: &str,
    container_key: &str,
    entry_key: &str,
    value: &serde_json::Value,
) -> Option<String> {
    let entry = serde_json::to_string_pretty(value).ok()?;
    let indented_entry = entry
        .lines()
        .enumerate()
        .map(|(i, line)| {
            if i == 0 {
                format!("    \"{entry_key}\": {line}")
            } else {
                format!("    {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    // Strategy 1: find the target container key and inject after its opening brace.
    // Prioritize the exact container_key, then fall back to common alternatives.
    let quoted_container = format!("\"{container_key}\"");
    let search_keys: Vec<&str> = std::iter::once(quoted_container.as_str())
        .chain(
            [
                "\"mcp\"",
                "\"mcpServers\"",
                "\"servers\"",
                "\"context_servers\"",
            ]
            .iter()
            .filter(|k| **k != quoted_container.as_str())
            .copied(),
        )
        .collect();

    for container in &search_keys {
        if let Some(pos) = content.find(container) {
            let after = &content[pos..];
            if let Some(brace_offset) = after.find('{') {
                let insert_pos = pos + brace_offset + 1;
                let before = &content[..insert_pos];
                let rest = &content[insert_pos..];
                let needs_comma = !rest.trim_start().starts_with('}');
                let injection = if needs_comma {
                    format!("\n{indented_entry},")
                } else {
                    format!("\n{indented_entry}\n  ")
                };
                return Some(format!("{before}{injection}{rest}"));
            }
        }
    }

    // Strategy 2: inject a new container block before the closing root brace
    if let Some(last_brace) = content.rfind('}') {
        let before = &content[..last_brace];
        let after = &content[last_brace..];
        let needs_comma = before.trim_end().ends_with('}')
            || before.trim_end().ends_with('"')
            || before.trim_end().ends_with(']');
        let comma = if needs_comma { "," } else { "" };
        let block = format!("{comma}\n  \"{container_key}\": {{\n{indented_entry}\n  }}\n");
        return Some(format!("{before}{block}{after}"));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
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
        assert!(json["mcpServers"]["lean-ctx"]["env"]["LEAN_CTX_DATA_DIR"]
            .as_str()
            .is_some_and(|s| !s.trim().is_empty()));
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
        let result =
            handle_invalid_json_write(&path, invalid_content, "mcp", "lean-ctx", &value, true);
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
}
