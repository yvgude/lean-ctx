use serde_json::Value;

#[allow(clippy::wildcard_imports)]
use super::super::shared::*;
use super::super::{WriteAction, WriteOptions, WriteResult};
use crate::core::editor_registry::types::EditorTarget;

fn vscode_mcp_entry(binary: &str) -> Value {
    serde_json::json!({
        "type": "stdio",
        "command": binary,
        "args": [],
        "env": { "LEAN_CTX_PROJECT_ROOT": "${workspaceFolder}" }
    })
}

pub(crate) fn write_vscode_mcp(
    target: &EditorTarget,
    binary: &str,
    opts: WriteOptions,
) -> Result<WriteResult, String> {
    let desired = vscode_mcp_entry(binary);

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

pub(crate) fn write_vscode_mcp_fresh(
    path: &std::path::Path,
    binary: &str,
    note: Option<String>,
) -> Result<WriteResult, String> {
    let content = serde_json::to_string_pretty(&serde_json::json!({
        "servers": { "lean-ctx": vscode_mcp_entry(binary) }
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

#[cfg(test)]
mod tests {
    use super::vscode_mcp_entry;

    #[test]
    fn vscode_mcp_entry_scopes_server_to_workspace_folder() {
        let entry = vscode_mcp_entry("/bin/lean-ctx");

        assert_eq!(entry["env"]["LEAN_CTX_PROJECT_ROOT"], "${workspaceFolder}");
    }
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

pub(crate) fn lean_ctx_augment_vscode_entry(binary: &str) -> Value {
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

pub(crate) fn write_augment_vscode(
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
