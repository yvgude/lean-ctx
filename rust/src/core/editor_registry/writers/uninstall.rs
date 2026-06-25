// Auto-split from the former monolithic writers.rs. Grouped by operation
// (install/uninstall) + shared helpers; behavior is unchanged.

use serde_json::Value;

use super::shared::LEAN_CTX_AUGMENT_VSCODE_ID;
use super::{WriteAction, WriteOptions, WriteResult};

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

pub(super) fn remove_lean_ctx_vscode_server(
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

pub(super) fn remove_lean_ctx_amp_server(
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

/// `OpenClaw` uninstall (GitHub #390): remove lean-ctx from BOTH schemas — the
/// nested `mcp.servers` (>= 2026.6.1) and the legacy top-level `mcpServers`.
/// Containers emptied by the removal are dropped entirely so the strict
/// 2026.6.1 validator never sees a leftover unknown key.
pub(super) fn remove_lean_ctx_openclaw_server(
    path: &std::path::Path,
    opts: WriteOptions,
) -> Result<WriteResult, String> {
    if !path.exists() {
        return Ok(WriteResult {
            action: WriteAction::Already,
            note: Some("openclaw.json not found".to_string()),
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

    let removed_legacy = super::install::remove_legacy_openclaw_entry(obj);

    let mut removed_nested = false;
    if let Some(mcp_obj) = obj.get_mut("mcp").and_then(Value::as_object_mut) {
        if let Some(servers_obj) = mcp_obj.get_mut("servers").and_then(Value::as_object_mut) {
            removed_nested = servers_obj.remove("lean-ctx").is_some();
            if servers_obj.is_empty() {
                mcp_obj.remove("servers");
            }
        }
        if mcp_obj.is_empty() {
            obj.remove("mcp");
        }
    }

    if !removed_legacy && !removed_nested {
        return Ok(WriteResult {
            action: WriteAction::Already,
            note: Some("lean-ctx not configured".to_string()),
        });
    }

    let formatted = serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?;
    crate::config_io::write_atomic_with_backup(path, &formatted)?;
    Ok(WriteResult {
        action: WriteAction::Updated,
        note: Some("removed lean-ctx from openclaw.json".to_string()),
    })
}

pub(super) fn remove_lean_ctx_named_json_server(
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

pub(super) fn remove_lean_ctx_codex_server(path: &std::path::Path) -> Result<WriteResult, String> {
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

pub(super) fn remove_codex_toml_section(existing: &str, header: &str) -> String {
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

pub(super) fn remove_lean_ctx_hermes_yaml_server(
    path: &std::path::Path,
) -> Result<WriteResult, String> {
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

pub(super) fn remove_hermes_yaml_mcp_server_block(existing: &str, name: &str) -> String {
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

pub(super) fn remove_hermes_yaml_lean_ctx_block(content: &str) -> String {
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

pub(super) fn remove_lean_ctx_augment_vscode_server(
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
