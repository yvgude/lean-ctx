use super::super::uninstall::remove_hermes_yaml_lean_ctx_block;
use super::super::{WriteAction, WriteOptions, WriteResult};
use crate::core::editor_registry::types::EditorTarget;

pub(crate) fn write_hermes_yaml(
    target: &EditorTarget,
    binary: &str,
    _opts: WriteOptions,
) -> Result<WriteResult, String> {
    let lean_ctx_block = format!("  lean-ctx:\n    command: \"{binary}\"");

    if target.config_path.exists() {
        let content = std::fs::read_to_string(&target.config_path).map_err(|e| e.to_string())?;

        if content.contains("lean-ctx") {
            let has_correct_binary = content.contains(binary);
            // #594: a stale `LEAN_CTX_DATA_DIR` env (from older versions) must be
            // rewritten out even when the binary already matches, so the MCP
            // server stops collapsing config onto the data dir.
            let has_stale_env = content.contains("LEAN_CTX_DATA_DIR");
            if has_correct_binary && !has_stale_env {
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

pub(crate) fn upsert_hermes_yaml_mcp(existing: &str, lean_ctx_block: &str) -> String {
    upsert_yaml_section_child(existing, "mcp_servers", lean_ctx_block)
}

/// Insert `block` (an indented child entry) into the top-level map
/// `section`, creating the section at the end when it is missing.
pub(crate) fn upsert_yaml_section_child(existing: &str, section: &str, block: &str) -> String {
    let lean_ctx_block = block;
    let header = format!("{section}:");
    let mut out = String::with_capacity(existing.len() + lean_ctx_block.len() + 32);
    let mut in_mcp_section = false;
    let mut saw_mcp_child = false;
    let mut inserted = false;
    let lines: Vec<&str> = existing.lines().collect();

    for line in &lines {
        if !inserted && line.trim_end() == header {
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

            // An empty section ends at the next top-level key: insert before
            // it, never at the end of the file under another key.
            if is_toplevel || (saw_mcp_child && line.trim().is_empty()) {
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
        out.push('\n');
        out.push_str(&header);
        out.push('\n');
        out.push_str(lean_ctx_block);
        out.push('\n');
    }

    out
}
