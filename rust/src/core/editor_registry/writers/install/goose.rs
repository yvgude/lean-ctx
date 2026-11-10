// SPDX-License-Identifier: Apache-2.0
//! Goose (`~/.config/goose/config.yaml`): LeanCTX as a stdio extension under
//! the top-level `extensions` map. Line-based like the Hermes writer, so the
//! user's comments and other extensions stay untouched.

use super::super::uninstall::remove_yaml_section_child;
use super::super::{WriteAction, WriteOptions, WriteResult};
use super::hermes::upsert_yaml_section_child;
use crate::core::editor_registry::types::EditorTarget;

/// The `extensions` key Goose reads.
pub(crate) const GOOSE_SECTION: &str = "extensions";

/// The `lean-ctx` entry for `binary`, indented as a child of `extensions`.
pub(crate) fn goose_extension_block(binary: &str) -> String {
    let cmd = serde_json::to_string(binary).unwrap_or_else(|_| format!("\"{binary}\""));
    format!(
        "  lean-ctx:\n    name: lean-ctx\n    type: stdio\n    cmd: {cmd}\n    args: []\n    enabled: true\n    envs: {{}}\n    timeout: 300\n    bundled: null\n    description: LeanCTX context gateway"
    )
}

/// `existing` with the `lean-ctx` extension for `binary` (inserted or
/// replaced); `extensions: {}` is opened into a block map first. Any other
/// form of the top-level `extensions` key (an inline map, a comment after
/// it, a flow value) is refused: the line-based writer would otherwise add a
/// second `extensions:` key, and YAML keeps only one of them.
pub(crate) fn upsert_goose_extension(existing: &str, binary: &str) -> Result<String, String> {
    let header = format!("{GOOSE_SECTION}:");
    let empty = format!("{GOOSE_SECTION}: {{}}");
    if let Some(line) = existing.lines().find(|l| {
        let t = l.trim_end();
        l.starts_with(header.as_str()) && t != header && t != empty
    }) {
        return Err(format!(
            "Goose config not changed: `{}` is not a block map LeanCTX can edit; \
             write it as `{header}` with one extension per indented line, then run setup again",
            line.trim_end()
        ));
    }
    let opened: String = existing
        .lines()
        .map(|line| {
            if line.trim_end() == format!("{GOOSE_SECTION}: {{}}") {
                format!("{GOOSE_SECTION}:")
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let cleaned = remove_yaml_section_child(&opened, GOOSE_SECTION, "lean-ctx");
    Ok(upsert_yaml_section_child(
        &cleaned,
        GOOSE_SECTION,
        &goose_extension_block(binary),
    ))
}

pub(crate) fn write_goose_yaml(
    target: &EditorTarget,
    binary: &str,
    _opts: WriteOptions,
) -> Result<WriteResult, String> {
    if !target.config_path.exists() {
        let content = format!("{GOOSE_SECTION}:\n{}\n", goose_extension_block(binary));
        crate::config_io::write_atomic_with_backup(&target.config_path, &content)?;
        return Ok(WriteResult {
            action: WriteAction::Created,
            note: None,
        });
    }
    let content = std::fs::read_to_string(&target.config_path).map_err(|e| e.to_string())?;
    let updated = upsert_goose_extension(&content, binary)?;
    if updated == content || updated.trim_end() == content.trim_end() {
        return Ok(WriteResult {
            action: WriteAction::Already,
            note: None,
        });
    }
    crate::config_io::write_atomic_with_backup(&target.config_path, &updated)?;
    Ok(WriteResult {
        action: WriteAction::Updated,
        note: None,
    })
}
