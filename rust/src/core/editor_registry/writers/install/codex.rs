#[allow(clippy::wildcard_imports)]
use super::super::shared::*;
use super::super::{WriteAction, WriteResult};
use crate::core::editor_registry::types::EditorTarget;

pub(crate) fn write_codex_config(
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
        "[mcp_servers.lean-ctx]\ncommand = {}\nargs = [\"mcp\"]\n",
        toml_quote(binary)
    );
    crate::config_io::write_atomic_with_backup(&target.config_path, &content)?;
    Ok(WriteResult {
        action: WriteAction::Created,
        note: None,
    })
}

pub(crate) fn upsert_codex_toml(existing: &str, binary: &str) -> String {
    let mut out = String::with_capacity(existing.len() + 128);
    let mut in_section = false;
    let mut saw_section = false;
    let mut wrote_command = false;
    let mut wrote_args = false;
    let mut inserted_parent_before_subtable = false;
    // #594: drop a stale `[mcp_servers.lean-ctx.env]` table — older versions
    // pinned `LEAN_CTX_DATA_DIR` there, which forced the MCP server into
    // single-dir mode and collapsed config onto the data dir, diverging from
    // the CLI. The current entry never carries an env block, so stripping it is
    // safe and makes both read the same config.
    let mut in_env_subtable = false;

    let parent_block = format!(
        "[mcp_servers.lean-ctx]\ncommand = {}\nargs = [\"mcp\"]\n\n",
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
                out.push_str("args = [\"mcp\"]\n");
                wrote_args = true;
            }
            in_env_subtable = trimmed == "[mcp_servers.lean-ctx.env]"
                || trimmed.starts_with("[mcp_servers.lean-ctx.env.");
            if in_env_subtable {
                in_section = false;
                continue;
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

        if in_env_subtable {
            continue;
        }

        if in_section {
            if trimmed.starts_with("command") && trimmed.contains('=') {
                out.push_str(&format!("command = {}\n", toml_quote(binary)));
                wrote_command = true;
                continue;
            }
            if trimmed.starts_with("args") && trimmed.contains('=') {
                if trimmed.contains("\"mcp\"") || trimmed.contains("'mcp'") {
                    out.push_str(line);
                    out.push('\n');
                } else {
                    out.push_str("args = [\"mcp\"]\n");
                }
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
            out.push_str("args = [\"mcp\"]\n");
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
    out.push_str("args = [\"mcp\"]\n");
    out
}
