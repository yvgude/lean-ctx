use serde_json::Value;

#[allow(clippy::wildcard_imports)]
use super::super::shared::*;
use super::super::{WriteAction, WriteOptions, WriteResult};
use crate::core::editor_registry::types::EditorTarget;

const OMP_MCP_SCHEMA: &str = "https://raw.githubusercontent.com/can1357/oh-my-pi/main/packages/coding-agent/src/config/mcp-schema.json";

fn omp_server_entry(binary: &str) -> Value {
    serde_json::json!({
        "type": "stdio",
        "command": binary,
        "args": []
    })
}

/// Write OMP's native MCP entry without touching other user servers.
///
/// OMP's native MCP client defers connection/tool loading itself; the config
/// schema has no `lifecycle` key, so keep the valid stdio shape here.
pub(crate) fn write_omp_mcp(
    target: &EditorTarget,
    binary: &str,
    opts: WriteOptions,
) -> Result<WriteResult, String> {
    let desired = omp_server_entry(binary);

    if target.config_path.exists() {
        let content = std::fs::read_to_string(&target.config_path).map_err(|e| e.to_string())?;
        let Ok(mut json) = crate::core::jsonc::parse_jsonc(&content) else {
            return handle_invalid_json_write(
                &target.config_path,
                &content,
                "mcpServers",
                "lean-ctx",
                &desired,
                opts.overwrite_invalid,
            );
        };
        let obj = json
            .as_object_mut()
            .ok_or_else(|| "root JSON must be an object".to_string())?;
        let schema_added = if obj.contains_key("$schema") {
            false
        } else {
            obj.insert(
                "$schema".to_string(),
                Value::String(OMP_MCP_SCHEMA.to_string()),
            );
            true
        };
        let servers = obj
            .entry("mcpServers")
            .or_insert_with(|| serde_json::json!({}));
        let servers_obj = servers
            .as_object_mut()
            .ok_or_else(|| "\"mcpServers\" must be an object".to_string())?;

        if servers_obj.get("lean-ctx") == Some(&desired) && !schema_added {
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

    let content = serde_json::to_string_pretty(&serde_json::json!({
        "$schema": OMP_MCP_SCHEMA,
        "mcpServers": { "lean-ctx": desired }
    }))
    .map_err(|e| e.to_string())?;
    crate::config_io::write_atomic_with_backup(&target.config_path, &content)?;
    Ok(WriteResult {
        action: WriteAction::Created,
        note: None,
    })
}
