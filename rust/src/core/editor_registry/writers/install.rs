// Auto-split from the former monolithic writers.rs. Grouped by operation
// (install/uninstall) + shared helpers; behavior is unchanged.

use serde_json::Value;

#[allow(clippy::wildcard_imports)]
use super::shared::*;
use super::uninstall::remove_hermes_yaml_lean_ctx_block;
use super::{WriteAction, WriteOptions, WriteResult};
use crate::core::editor_registry::types::EditorTarget;

pub(super) fn write_mcp_json(
    target: &EditorTarget,
    binary: &str,
    opts: WriteOptions,
) -> Result<WriteResult, String> {
    let include_aa = supports_auto_approve(target);
    let desired = if target.agent_key.is_empty() {
        lean_ctx_server_entry(binary, include_aa)
    } else {
        lean_ctx_server_entry_with_instructions(binary, include_aa, &target.agent_key)
    };

    // Claude Code manages ~/.claude.json and may overwrite it on first start.
    // Prefer the official CLI integration when available.
    // Skip when LEAN_CTX_QUIET=1 (bootstrap --json / setup --json) to avoid
    // spawning `claude mcp add-json` which can stall in non-interactive CI.
    if (target.agent_key == "claude" || target.name == "Claude Code")
        && !matches!(std::env::var("LEAN_CTX_QUIET"), Ok(v) if v.trim() == "1")
        && let Ok(result) = try_claude_mcp_add(&desired)
    {
        return Ok(result);
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

pub(super) fn find_in_path(binary: &str) -> Option<std::path::PathBuf> {
    let path_var = std::env::var("PATH").ok()?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(binary);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

pub(super) fn validate_claude_binary() -> Result<std::path::PathBuf, String> {
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

pub(super) fn try_claude_mcp_add(desired: &Value) -> Result<WriteResult, String> {
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

pub(super) fn write_mcp_json_fresh(
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

pub(super) fn write_zed_config(
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

pub(super) fn write_codex_config(
    target: &EditorTarget,
    binary: &str,
) -> Result<WriteResult, String> {
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

pub(super) fn write_zed_config_fresh(
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

pub(super) fn write_vscode_mcp(
    target: &EditorTarget,
    binary: &str,
    opts: WriteOptions,
) -> Result<WriteResult, String> {
    let desired = serde_json::json!({ "type": "stdio", "command": binary, "args": [] });

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

pub(super) fn write_vscode_mcp_fresh(
    path: &std::path::Path,
    binary: &str,
    note: Option<String>,
) -> Result<WriteResult, String> {
    let content = serde_json::to_string_pretty(&serde_json::json!({
        "servers": { "lean-ctx": { "type": "stdio", "command": binary, "args": [] } }
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

pub(super) fn write_copilot_cli(
    target: &EditorTarget,
    binary: &str,
    opts: WriteOptions,
) -> Result<WriteResult, String> {
    let desired = serde_json::json!({
        "type": "local",
        "command": binary,
        "args": ["mcp"],
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

pub(super) fn write_opencode_config(
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

pub(super) fn write_opencode_fresh(
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

pub(super) fn write_jetbrains_config(
    target: &EditorTarget,
    binary: &str,
    opts: WriteOptions,
) -> Result<WriteResult, String> {
    // JetBrains AI Assistant expects an "mcpServers" mapping in the JSON snippet
    // you paste into Settings | Tools | AI Assistant | Model Context Protocol (MCP).
    // We write that snippet to a file for easy copy/paste.
    let desired = serde_json::json!({
        "command": binary,
        "args": []
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

pub(super) fn write_amp_config(
    target: &EditorTarget,
    binary: &str,
    opts: WriteOptions,
) -> Result<WriteResult, String> {
    let entry = serde_json::json!({
        "command": binary
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

pub(super) fn write_crush_config(
    target: &EditorTarget,
    binary: &str,
    opts: WriteOptions,
) -> Result<WriteResult, String> {
    let desired = serde_json::json!({
        "type": "stdio",
        "command": binary
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

pub(super) fn write_crush_fresh(
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

pub(super) fn upsert_codex_toml(existing: &str, binary: &str) -> String {
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

pub(super) fn write_gemini_settings(
    target: &EditorTarget,
    binary: &str,
    opts: WriteOptions,
) -> Result<WriteResult, String> {
    let entry = serde_json::json!({
        "command": binary,
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

pub(super) fn write_hermes_yaml(
    target: &EditorTarget,
    binary: &str,
    _opts: WriteOptions,
) -> Result<WriteResult, String> {
    let lean_ctx_block = format!("  lean-ctx:\n    command: \"{binary}\"");

    if target.config_path.exists() {
        let content = std::fs::read_to_string(&target.config_path).map_err(|e| e.to_string())?;

        if content.contains("lean-ctx") {
            let has_correct_binary = content.contains(binary);
            if has_correct_binary {
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

pub(super) fn upsert_hermes_yaml_mcp(existing: &str, lean_ctx_block: &str) -> String {
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

pub(super) fn write_qoder_settings(
    target: &EditorTarget,
    binary: &str,
    opts: WriteOptions,
) -> Result<WriteResult, String> {
    // Core toolset by default — no LEAN_CTX_FULL_TOOLS (GitHub #385, see
    // hooks::full_server_entry).
    let desired = serde_json::json!({
        "command": binary,
        "args": []
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
// OpenClaw writer (GitHub #390)
//
// OpenClaw changed its config schema in 2026.6.1: MCP servers moved from a
// top-level `mcpServers` (camelCase) object to a nested `mcp.servers` object,
// and the new validator *rejects* unknown top-level keys — a re-injected
// `mcpServers` block makes every hot-reload fail ("Unrecognized key") and, if
// it wins on restart, takes the gateway down.
//
// Strategy:
//   - Detect the installed version via `meta.lastTouchedVersion`.
//   - >= 2026.6.1 (or unknown/missing): write nested `mcp.servers` and
//     migrate away our legacy `mcpServers.lean-ctx` entry (dropping the
//     `mcpServers` key entirely once it is empty).
//   - < 2026.6.1: keep writing the legacy camelCase schema.
//   - Idempotent: if the entry already matches and no stale legacy entry
//     exists, nothing is written (no watchdog reload-tick churn).
// ---------------------------------------------------------------------------

/// First `OpenClaw` version that requires the nested `mcp.servers` schema.
const OPENCLAW_NESTED_SCHEMA_VERSION: (u64, u64, u64) = (2026, 6, 1);

/// Parse an `OpenClaw` version string ("2026.6.1") into a comparable triple.
/// Tolerates missing components ("2026.6" -> (2026, 6, 0)) and pre-release
/// suffixes ("2026.6.1-beta.2" -> (2026, 6, 1)).
pub(super) fn parse_openclaw_version(raw: &str) -> Option<(u64, u64, u64)> {
    let core = raw.trim().split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.trim().parse::<u64>().ok()?;
    let minor = parts
        .next()
        .and_then(|p| p.trim().parse::<u64>().ok())
        .unwrap_or(0);
    let patch = parts
        .next()
        .and_then(|p| p.trim().parse::<u64>().ok())
        .unwrap_or(0);
    Some((major, minor, patch))
}

/// Whether this `OpenClaw` config requires the nested `mcp.servers` schema.
///
/// Defaults to nested when the version is unknown: current `OpenClaw` releases
/// are all >= 2026.6.1, the legacy key actively breaks them, and a fresh
/// install has no `meta` block at all. An existing `mcp.servers` object is
/// also treated as proof of the new schema (the user or `OpenClaw` itself
/// migrated already).
pub(super) fn openclaw_uses_nested_schema(root: &serde_json::Map<String, Value>) -> bool {
    if root
        .get("mcp")
        .and_then(|m| m.get("servers"))
        .is_some_and(Value::is_object)
    {
        return true;
    }
    let version = root
        .get("meta")
        .and_then(|m| m.get("lastTouchedVersion"))
        .and_then(Value::as_str)
        .and_then(parse_openclaw_version);
    match version {
        Some(v) => v >= OPENCLAW_NESTED_SCHEMA_VERSION,
        None => true,
    }
}

/// Remove our legacy top-level `mcpServers.lean-ctx` entry. Drops the whole
/// `mcpServers` key when it becomes empty (`OpenClaw` >= 2026.6.1 rejects even
/// an empty unknown key). Foreign entries under `mcpServers` are preserved.
/// Returns true when the document was modified.
pub(super) fn remove_legacy_openclaw_entry(root: &mut serde_json::Map<String, Value>) -> bool {
    let Some(servers) = root.get_mut("mcpServers").and_then(Value::as_object_mut) else {
        return false;
    };
    if servers.remove("lean-ctx").is_none() {
        return false;
    }
    if servers.is_empty() {
        root.remove("mcpServers");
    }
    true
}

pub(super) fn write_openclaw_config(
    target: &EditorTarget,
    binary: &str,
    _opts: WriteOptions,
) -> Result<WriteResult, String> {
    let desired = serde_json::json!({
        "command": binary
    });

    if !target.config_path.exists() {
        let content = serde_json::to_string_pretty(&serde_json::json!({
            "mcp": { "servers": { "lean-ctx": desired } }
        }))
        .map_err(|e| e.to_string())?;
        crate::config_io::write_atomic_with_backup(&target.config_path, &content)?;
        return Ok(WriteResult {
            action: WriteAction::Created,
            note: None,
        });
    }

    let content = std::fs::read_to_string(&target.config_path).map_err(|e| e.to_string())?;
    let mut json = match crate::core::jsonc::parse_jsonc(&content) {
        Ok(v) => v,
        Err(_e) => {
            // Never text-inject into openclaw.json: the nested `mcp.servers`
            // shape cannot be patched safely with flat text injection, and a
            // malformed write would take the strict 2026.6.1 validator (and
            // with it the gateway) down. `allow_inject=false` keeps the
            // existing "already present? -> skip, else -> clear error" flow.
            return handle_invalid_json_write(
                &target.config_path,
                &content,
                "mcp",
                "lean-ctx",
                &desired,
                false,
            );
        }
    };
    let root = json
        .as_object_mut()
        .ok_or_else(|| "root JSON must be an object".to_string())?;

    if !openclaw_uses_nested_schema(root) {
        // Legacy OpenClaw (< 2026.6.1): keep the camelCase schema it expects.
        let servers = root
            .entry("mcpServers")
            .or_insert_with(|| serde_json::json!({}));
        let servers_obj = servers
            .as_object_mut()
            .ok_or_else(|| "\"mcpServers\" must be an object".to_string())?;
        if servers_obj.get("lean-ctx") == Some(&desired) {
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
            note: Some("legacy mcpServers schema (OpenClaw < 2026.6.1)".to_string()),
        });
    }

    let migrated_legacy = remove_legacy_openclaw_entry(root);

    let mcp = root.entry("mcp").or_insert_with(|| serde_json::json!({}));
    let mcp_obj = mcp
        .as_object_mut()
        .ok_or_else(|| "\"mcp\" must be an object".to_string())?;
    let servers = mcp_obj
        .entry("servers")
        .or_insert_with(|| serde_json::json!({}));
    let servers_obj = servers
        .as_object_mut()
        .ok_or_else(|| "\"mcp.servers\" must be an object".to_string())?;

    let entry_current = servers_obj.get("lean-ctx") == Some(&desired);
    if entry_current && !migrated_legacy {
        return Ok(WriteResult {
            action: WriteAction::Already,
            note: None,
        });
    }
    servers_obj.insert("lean-ctx".to_string(), desired);

    let formatted = serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?;
    crate::config_io::write_atomic_with_backup(&target.config_path, &formatted)?;
    Ok(WriteResult {
        action: WriteAction::Updated,
        note: migrated_legacy
            .then(|| "migrated legacy mcpServers entry to mcp.servers".to_string()),
    })
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

pub(super) fn lean_ctx_augment_vscode_entry(binary: &str) -> Value {
    serde_json::json!({
        "type": "stdio",
        "id": LEAN_CTX_AUGMENT_VSCODE_ID,
        "name": "lean-ctx",
        "disabled": false,
        "command": binary,
        "args": [],
        "useShellInterpolation": false
    })
}

pub(super) fn write_augment_vscode(
    target: &EditorTarget,
    binary: &str,
    opts: WriteOptions,
) -> Result<WriteResult, String> {
    let desired = lean_ctx_augment_vscode_entry(binary);

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
