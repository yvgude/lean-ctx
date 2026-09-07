//! CodeWhale MCP config writer (GH #1402).
//!
//! CodeWhale reads its user-level server list from `~/.codewhale/mcp.json`
//! (legacy: `~/.deepseek/mcp.json`, path resolution in
//! [`crate::core::editor_registry::codewhale_mcp_json_path`]) and accepts two
//! alternative root keys: `servers` (upstream's own preferred name) and
//! `mcpServers` (the cross-client standard). Writing a second root next to the
//! one the user already has is the failure mode this writer exists to avoid —
//! so we adopt whichever root is already present and only fall back to the
//! standard `mcpServers` for a file we create ourselves.
//!
//! The entry stays a bare `{"command": "<binary>"}`: CodeWhale runs lean-ctx
//! with no args, and its schema validation fails closed on unknown fields, so
//! no `instructions` / `autoApprove` decoration is emitted (see
//! `client_constraints`).

use serde_json::Value;

#[allow(clippy::wildcard_imports)]
use super::super::shared::*;
use super::super::{WriteAction, WriteOptions, WriteResult};
use crate::core::editor_registry::types::EditorTarget;

/// The root key CodeWhale prefers upstream.
pub(crate) const CODEWHALE_SERVERS_KEY: &str = "servers";
/// The cross-client standard root, also accepted by CodeWhale, and what we
/// create for a fresh config.
pub(crate) const CODEWHALE_MCP_SERVERS_KEY: &str = "mcpServers";

/// Pick the root key to merge into for an already-parsed CodeWhale config.
///
/// Preference order is "whatever the user already uses" — an existing
/// `servers` object wins over `mcpServers` because that is the name upstream
/// documents as preferred, and a config carrying both is already the user's
/// choice to keep. Only a config with neither gets the standard key.
pub(crate) fn codewhale_root_key(root: &serde_json::Map<String, Value>) -> &'static str {
    if root
        .get(CODEWHALE_SERVERS_KEY)
        .is_some_and(Value::is_object)
    {
        return CODEWHALE_SERVERS_KEY;
    }
    if root
        .get(CODEWHALE_MCP_SERVERS_KEY)
        .is_some_and(Value::is_object)
    {
        return CODEWHALE_MCP_SERVERS_KEY;
    }
    CODEWHALE_MCP_SERVERS_KEY
}

pub(crate) fn write_codewhale_config(
    target: &EditorTarget,
    binary: &str,
    opts: WriteOptions,
) -> Result<WriteResult, String> {
    // Bare binary, no args, no extra fields — see module docs.
    let desired = lean_ctx_server_entry(binary, supports_auto_approve(target));

    if target.config_path.exists() {
        let content = std::fs::read_to_string(&target.config_path).map_err(|e| e.to_string())?;
        let mut json = match crate::core::jsonc::parse_jsonc(&content) {
            Ok(v) => v,
            Err(_e) => {
                return handle_invalid_json_write(
                    &target.config_path,
                    &content,
                    CODEWHALE_MCP_SERVERS_KEY,
                    "lean-ctx",
                    &desired,
                    opts.overwrite_invalid,
                );
            }
        };
        let obj = json
            .as_object_mut()
            .ok_or_else(|| "root JSON must be an object".to_string())?;

        let root_key = codewhale_root_key(obj);
        let servers = obj
            .entry(root_key.to_string())
            .or_insert_with(|| serde_json::json!({}));
        let servers_obj = servers
            .as_object_mut()
            .ok_or_else(|| format!("\"{root_key}\" must be an object"))?;

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
            note: (root_key == CODEWHALE_SERVERS_KEY)
                .then(|| "merged into existing \"servers\" root".to_string()),
        });
    }

    // Literal key, asserted equal to the const so the two can never drift.
    debug_assert_eq!(CODEWHALE_MCP_SERVERS_KEY, "mcpServers");
    let content = serde_json::to_string_pretty(&serde_json::json!({
        "mcpServers": { "lean-ctx": desired }
    }))
    .map_err(|e| e.to_string())?;
    crate::config_io::write_atomic_with_backup(&target.config_path, &content)?;
    Ok(WriteResult {
        action: WriteAction::Created,
        note: None,
    })
}
