#[allow(clippy::wildcard_imports)]
use super::super::shared::*;
use super::super::{WriteAction, WriteOptions, WriteResult};
use crate::core::editor_registry::types::EditorTarget;

pub(crate) fn write_opencode_config(
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
            // MCP entry unchanged — but stale deny permissions may linger (#1451).
            if strip_shadow_denies_if_disabled(obj) {
                let formatted = serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?;
                crate::config_io::write_atomic_with_backup(&target.config_path, &formatted)?;
                return Ok(WriteResult {
                    action: WriteAction::Updated,
                    note: Some("removed stale shadow-mode deny permissions".into()),
                });
            }
            return Ok(WriteResult {
                action: WriteAction::Already,
                note: None,
            });
        }
        mcp_obj.insert("lean-ctx".to_string(), desired);

        // #1451: strip shadow-mode deny entries when shadow_mode is off.
        // Without this, an MCP update preserves stale deny entries that
        // the hook installer would have removed — but only if it ran after.
        let _ = strip_shadow_denies_if_disabled(obj);

        let formatted = serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?;
        crate::config_io::write_atomic_with_backup(&target.config_path, &formatted)?;
        return Ok(WriteResult {
            action: WriteAction::Updated,
            note: None,
        });
    }

    write_opencode_fresh(&target.config_path, binary, None)
}

pub(crate) fn write_opencode_fresh(
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

/// Strip the `ask`/`deny` permissions lean-ctx set on native read/grep/glob/bash
/// unless the user chose Replace mode. In OpenCode `ask` prompts on every native
/// call and refuses it headless, so it is a refusal by another name — the
/// default mode keeps native tools usable. Keeps permission entries the user
/// set themselves (external_directory, rm *, allow, etc.).
fn strip_shadow_denies_if_disabled(obj: &mut serde_json::Map<String, serde_json::Value>) -> bool {
    if crate::hooks::replace_mode_chosen() {
        return false;
    }
    let Some(perms) = obj.get_mut("permission").and_then(|p| p.as_object_mut()) else {
        return false;
    };
    const SHADOW_TOOLS: &[&str] = &["read", "grep", "glob", "bash"];
    let mut changed = false;
    for &tool in SHADOW_TOOLS {
        if matches!(
            perms.get(tool).and_then(|v| v.as_str()),
            Some("ask" | "deny")
        ) {
            perms.remove(tool);
            changed = true;
        }
    }
    if changed && perms.is_empty() {
        obj.remove("permission");
    }
    changed
}
