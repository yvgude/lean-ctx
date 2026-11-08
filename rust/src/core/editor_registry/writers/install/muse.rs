// SPDX-License-Identifier: Apache-2.0

//! Meta Muse Code MCP config writer (GH #2198).
//!
//! Muse Code reads user MCP servers from `settings.json` in its config dir
//! (`$XDG_CONFIG_HOME/muse`, default `~/.config/muse`). Documented contract
//! (<https://meta-models.github.io/muse-code-sdk/1.4/guides/extend/mcp-servers/>):
//!
//! * `"schema_version": 1` is required — without it Muse refuses to start.
//! * Canonical servers live under `mcpServers` as
//!   `{"type": "stdio", "command": …, "args": […]}`.
//! * The legacy spelling (`mcp_servers` + `"transport": "stdio"`, also shown on
//!   <https://dev.meta.ai/docs/muse-code/extending#mcp>) is still accepted in
//!   `settings.json`, but mixing a legacy key with its canonical counterpart —
//!   at the root or inside one entry — makes the config ambiguous and disables
//!   MCP for the whole session.
//!
//! So the writer merges into whichever root the user's file already uses, with
//! that root's matching entry spelling, and never adds the competing root. A
//! file that already carries both roots is ambiguous to Muse itself; we refuse
//! to touch it rather than guess.

use serde_json::{Map, Value};

#[allow(clippy::wildcard_imports)]
use super::super::shared::*;
use super::super::{WriteAction, WriteOptions, WriteResult};
use crate::core::editor_registry::types::EditorTarget;

/// Canonical MCP root in Muse Code `settings.json`.
pub(crate) const MUSE_MCP_ROOT: &str = "mcpServers";
/// Legacy MCP root, accepted in `settings.json` only.
pub(crate) const MUSE_LEGACY_MCP_ROOT: &str = "mcp_servers";
/// The only `schema_version` Muse Code 1.x accepts.
const MUSE_SCHEMA_VERSION: u64 = 1;

/// Entry in the canonical spelling (`mcpServers`).
fn muse_server_entry(binary: &str) -> Value {
    serde_json::json!({
        "type": "stdio",
        "command": binary,
        "args": []
    })
}

/// Entry in the legacy spelling (`mcp_servers`): `transport`, never `type`,
/// because mixing the two inside one entry disables MCP.
fn muse_legacy_server_entry(binary: &str) -> Value {
    serde_json::json!({
        "transport": "stdio",
        "command": binary,
        "args": []
    })
}

/// Pick the root (and matching entry spelling) to merge into.
fn muse_root_for(obj: &Map<String, Value>, path: &std::path::Path) -> Result<&'static str, String> {
    match (
        obj.contains_key(MUSE_MCP_ROOT),
        obj.contains_key(MUSE_LEGACY_MCP_ROOT),
    ) {
        (true, true) => Err(format!(
            "{} has both \"{MUSE_MCP_ROOT}\" and legacy \"{MUSE_LEGACY_MCP_ROOT}\"; Muse Code \
             disables MCP for ambiguous configs. Merge them into \"{MUSE_MCP_ROOT}\" and re-run \
             lean-ctx setup.",
            path.display()
        )),
        (false, true) => Ok(MUSE_LEGACY_MCP_ROOT),
        _ => Ok(MUSE_MCP_ROOT),
    }
}

/// Write the lean-ctx server into Muse Code's user `settings.json`, keeping
/// every other key and server untouched. Idempotent: an unchanged file
/// reports `Already` and is not rewritten.
pub(crate) fn write_muse_settings(
    target: &EditorTarget,
    binary: &str,
    opts: WriteOptions,
) -> Result<WriteResult, String> {
    let path = &target.config_path;

    if path.exists() {
        let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let Ok(mut json) = crate::core::jsonc::parse_jsonc(&content) else {
            let legacy_only = content.contains(&format!("\"{MUSE_LEGACY_MCP_ROOT}\""))
                && !content.contains(&format!("\"{MUSE_MCP_ROOT}\""));
            let (root, desired) = if legacy_only {
                (MUSE_LEGACY_MCP_ROOT, muse_legacy_server_entry(binary))
            } else {
                (MUSE_MCP_ROOT, muse_server_entry(binary))
            };
            return handle_invalid_json_write(
                path,
                &content,
                root,
                "lean-ctx",
                &desired,
                opts.overwrite_invalid,
            );
        };
        let obj = json
            .as_object_mut()
            .ok_or_else(|| "root JSON must be an object".to_string())?;

        let root = muse_root_for(obj, path)?;
        let desired = if root == MUSE_LEGACY_MCP_ROOT {
            muse_legacy_server_entry(binary)
        } else {
            muse_server_entry(binary)
        };

        // `schema_version` is mandatory; a file without it is already rejected
        // by Muse, so adding the one supported version is a repair. A present
        // value is the user's (or a future Muse's) and is never rewritten.
        let schema_added = if obj.contains_key("schema_version") {
            false
        } else {
            obj.insert(
                "schema_version".to_string(),
                Value::from(MUSE_SCHEMA_VERSION),
            );
            true
        };

        let servers = obj.entry(root).or_insert_with(|| Value::Object(Map::new()));
        let servers_obj = servers
            .as_object_mut()
            .ok_or_else(|| format!("\"{root}\" must be an object"))?;

        if servers_obj.get("lean-ctx") == Some(&desired) && !schema_added {
            return Ok(WriteResult {
                action: WriteAction::Already,
                note: None,
            });
        }
        servers_obj.insert("lean-ctx".to_string(), desired);

        let formatted = serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?;
        crate::config_io::write_atomic_with_backup(path, &formatted)?;
        return Ok(WriteResult {
            action: WriteAction::Updated,
            note: None,
        });
    }

    let content = serde_json::to_string_pretty(&serde_json::json!({
        "schema_version": MUSE_SCHEMA_VERSION,
        "mcpServers": { "lean-ctx": muse_server_entry(binary) }
    }))
    .map_err(|e| e.to_string())?;
    crate::config_io::write_atomic_with_backup(path, &content)?;
    Ok(WriteResult {
        action: WriteAction::Created,
        note: None,
    })
}

#[cfg(test)]
mod tests {
    use super::super::super::{WriteAction, WriteOptions, remove_lean_ctx_server, write_config};
    use crate::core::editor_registry::types::{ConfigType, EditorTarget};
    use serde_json::Value;
    use std::path::{Path, PathBuf};

    const BIN: &str = "/usr/local/bin/lean-ctx";

    fn muse_target(path: PathBuf) -> EditorTarget {
        EditorTarget {
            name: "Muse Code",
            agent_key: "muse".to_string(),
            config_path: path,
            detect_path: PathBuf::from("/nonexistent"),
            config_type: ConfigType::MuseSettings,
        }
    }

    /// `<temp HOME>/.config/muse/settings.json` — the documented default path.
    fn settings_in(home: &Path) -> PathBuf {
        home.join(".config").join("muse").join("settings.json")
    }

    fn read(path: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    fn seed(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn muse_fresh_settings_get_schema_version_and_canonical_stdio_entry() {
        let home = tempfile::tempdir().unwrap();
        let path = settings_in(home.path());

        let res = write_config(&muse_target(path.clone()), BIN).unwrap();
        assert_eq!(res.action, WriteAction::Created);

        let json = read(&path);
        assert_eq!(
            json["schema_version"], 1,
            "Muse refuses to start without it"
        );
        assert_eq!(
            json["mcpServers"]["lean-ctx"],
            serde_json::json!({ "type": "stdio", "command": BIN, "args": [] })
        );
        assert!(json.get("mcp_servers").is_none());
    }

    #[test]
    fn muse_rerun_is_idempotent_and_leaves_the_file_untouched() {
        let home = tempfile::tempdir().unwrap();
        let path = settings_in(home.path());
        let t = muse_target(path.clone());

        assert_eq!(write_config(&t, BIN).unwrap().action, WriteAction::Created);
        let first = std::fs::read_to_string(&path).unwrap();
        assert_eq!(write_config(&t, BIN).unwrap().action, WriteAction::Already);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), first);
    }

    #[test]
    fn muse_merge_preserves_user_servers_and_settings() {
        let home = tempfile::tempdir().unwrap();
        let path = settings_in(home.path());
        seed(
            &path,
            r#"{
  "schema_version": 1,
  "permissions": { "default_profile": "ask" },
  "mcpServers": {
    "docs": { "type": "stdio", "command": "python3", "args": ["/opt/docs.py"], "env": { "T": "${T}" } },
    "lean-ctx": { "type": "stdio", "command": "/old/lean-ctx", "args": [] }
  }
}"#,
        );

        let res = write_config(&muse_target(path.clone()), BIN).unwrap();
        assert_eq!(res.action, WriteAction::Updated);

        let json = read(&path);
        assert_eq!(json["schema_version"], 1);
        assert_eq!(json["permissions"]["default_profile"], "ask");
        assert_eq!(json["mcpServers"]["docs"]["command"], "python3");
        assert_eq!(json["mcpServers"]["docs"]["args"][0], "/opt/docs.py");
        assert_eq!(json["mcpServers"]["docs"]["env"]["T"], "${T}");
        assert_eq!(json["mcpServers"]["lean-ctx"]["command"], BIN);
    }

    #[test]
    fn muse_adds_missing_schema_version_but_keeps_an_existing_one() {
        let home = tempfile::tempdir().unwrap();
        let path = settings_in(home.path());
        seed(&path, r#"{ "model": "x" }"#);
        let t = muse_target(path.clone());

        assert_eq!(write_config(&t, BIN).unwrap().action, WriteAction::Updated);
        let json = read(&path);
        assert_eq!(json["schema_version"], 1);
        assert_eq!(json["model"], "x");

        seed(
            &path,
            &format!(
                r#"{{ "schema_version": 2, "mcpServers": {{ "lean-ctx": {{ "type": "stdio", "command": "{BIN}", "args": [] }} }} }}"#
            ),
        );
        assert_eq!(write_config(&t, BIN).unwrap().action, WriteAction::Already);
        assert_eq!(read(&path)["schema_version"], 2);
    }

    #[test]
    fn muse_legacy_root_gets_legacy_entry_and_no_competing_canonical_root() {
        let home = tempfile::tempdir().unwrap();
        let path = settings_in(home.path());
        seed(
            &path,
            r#"{ "schema_version": 1, "mcp_servers": { "my-tools": { "transport": "stdio", "command": "my-mcp-server", "args": [] } } }"#,
        );
        let t = muse_target(path.clone());

        assert_eq!(write_config(&t, BIN).unwrap().action, WriteAction::Updated);
        let json = read(&path);
        assert!(
            json.get("mcpServers").is_none(),
            "mixing mcpServers with mcp_servers disables MCP in Muse"
        );
        assert_eq!(
            json["mcp_servers"]["lean-ctx"],
            serde_json::json!({ "transport": "stdio", "command": BIN, "args": [] })
        );
        assert_eq!(json["mcp_servers"]["my-tools"]["command"], "my-mcp-server");
        assert_eq!(write_config(&t, BIN).unwrap().action, WriteAction::Already);
    }

    #[test]
    fn muse_refuses_a_settings_file_that_already_mixes_both_roots() {
        let home = tempfile::tempdir().unwrap();
        let path = settings_in(home.path());
        let original = r#"{ "schema_version": 1, "mcpServers": {}, "mcp_servers": {} }"#;
        seed(&path, original);

        let err = write_config(&muse_target(path.clone()), BIN).unwrap_err();
        assert!(err.contains("mcp_servers"), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn muse_uninstall_removes_only_the_lean_ctx_entry() {
        let home = tempfile::tempdir().unwrap();
        let path = settings_in(home.path());
        seed(
            &path,
            r#"{ "schema_version": 1, "theme": "dark", "mcpServers": { "docs": { "command": "python3" } } }"#,
        );
        let t = muse_target(path.clone());
        write_config(&t, BIN).unwrap();

        let res = remove_lean_ctx_server(&t, WriteOptions::default()).unwrap();
        assert_eq!(res.action, WriteAction::Updated);
        let json = read(&path);
        assert!(json["mcpServers"].get("lean-ctx").is_none());
        assert_eq!(json["mcpServers"]["docs"]["command"], "python3");
        assert_eq!(json["schema_version"], 1);
        assert_eq!(json["theme"], "dark");

        let again = remove_lean_ctx_server(&t, WriteOptions::default()).unwrap();
        assert_eq!(again.action, WriteAction::Already);
    }

    #[test]
    fn muse_uninstall_also_removes_a_legacy_root_entry() {
        let home = tempfile::tempdir().unwrap();
        let path = settings_in(home.path());
        seed(
            &path,
            r#"{ "schema_version": 1, "mcp_servers": { "lean-ctx": { "transport": "stdio", "command": "lean-ctx" }, "keep": { "command": "k" } } }"#,
        );

        let res =
            remove_lean_ctx_server(&muse_target(path.clone()), WriteOptions::default()).unwrap();
        assert_eq!(res.action, WriteAction::Updated);
        let json = read(&path);
        assert!(json["mcp_servers"].get("lean-ctx").is_none());
        assert_eq!(json["mcp_servers"]["keep"]["command"], "k");
    }
}
