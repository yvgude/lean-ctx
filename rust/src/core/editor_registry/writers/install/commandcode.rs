use serde_json::Value;

#[allow(clippy::wildcard_imports)]
use super::super::shared::*;
use super::super::{WriteAction, WriteOptions, WriteResult};
use crate::core::editor_registry::types::EditorTarget;

/// Instructions block for Command Code MCP hosts (shadow-mode rules).
pub(crate) const COMMANDCODE_MCP_INSTRUCTIONS: &str = "\
lean-ctx shadow mode: native file/search/shell calls auto-route to ctx_* — no tool-mapping needed.\n\
Exclusive tools (no native trigger): ctx_compose (search + read + symbols), \
ctx_search(action=symbol) (exact symbol), ctx_search(action=semantic) (by meaning), \
ctx_callgraph (callers), ctx_knowledge / ctx_session (memory).\n\
OUTPUT STYLE: concise\n\
- Bullet points over paragraphs\n\
- Skip filler words and hedging (\"I think\", \"probably\", \"it seems\")\n\
- 1-sentence explanations max, then code/action\n\
- No repeating what the user said";

pub(crate) fn write_commandcode_config_with_rule_steering(
    target: &EditorTarget,
    binary: &str,
    opts: WriteOptions,
    allow_rule_steering: bool,
) -> Result<WriteResult, String> {
    // Command Code is unusual: model steering lives directly inside the MCP
    // server entry. Setup may therefore need to configure MCP without creating
    // or refreshing that steering.
    let mut desired = serde_json::json!({
        "transport": "stdio",
        "enabled": true,
        "command": binary,
    });

    if allow_rule_steering && let Some(obj) = desired.as_object_mut() {
        obj.insert(
            "instructions".to_string(),
            serde_json::Value::String(COMMANDCODE_MCP_INSTRUCTIONS.to_string()),
        );
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

        // A setup-level opt-out (`auto_inject_rules=false` / `--skip-rules`)
        // means "do not create or refresh steering", so existing instructions
        // are preserved. `rules_injection=off` is stronger: remove only the
        // lean-ctx-owned Command Code steering while leaving user-authored
        // instructions untouched.
        if !allow_rule_steering
            && let Some(instructions) = existing
                .as_ref()
                .and_then(|entry| entry.get("instructions"))
                .cloned()
        {
            let rules_off = crate::core::config::Config::load().rules_injection_effective()
                == crate::core::config::RulesInjection::Off;

            let lean_ctx_owned = instructions.as_str().is_some_and(|text| {
                text == COMMANDCODE_MCP_INSTRUCTIONS || text.starts_with("lean-ctx shadow mode:")
            });

            if !rules_off || !lean_ctx_owned {
                if let Some(obj) = desired.as_object_mut() {
                    obj.insert("instructions".to_string(), instructions);
                }
            }
        }

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

    write_commandcode_fresh(&target.config_path, &desired, None)
}

pub(crate) fn write_commandcode_fresh(
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
