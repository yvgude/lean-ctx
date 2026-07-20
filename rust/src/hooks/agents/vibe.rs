use std::path::Path;

use super::super::{
    mcp_server_quiet_mode, resolve_binary_path, resolve_hook_command_binary, should_register_mcp,
};
use crate::core::editor_registry::{
    ConfigType, EditorTarget, WriteAction, WriteOptions, vibe_config_path,
    write_config_with_options,
};

/// Name of the lean-ctx entry inside `~/.vibe/hooks.toml`. Stable so re-runs
/// upsert instead of appending duplicates (Vibe rejects duplicate hook names).
pub(crate) const VIBE_HOOK_NAME: &str = "lean-ctx-redirect";

/// Tools the pre_tool hook intercepts: `bash` (arg-rewritten through the
/// compression CLI) plus `read_file`/`grep` (denied → MCP tools in Replace
/// mode). `re:` regex is fullmatch + case-insensitive in Vibe's `name_matches`.
pub(crate) const VIBE_HOOK_MATCH: &str = "re:(bash|read_file|grep)";

/// Configure Mistral Vibe: the MCP server (exposes `ctx_*` tools) **and** the
/// `pre_tool` hook (arg-rewrites native `bash` onto lean-ctx transparently).
/// Both are MCP-gated — Vibe has no non-MCP surface for lean-ctx.
pub(crate) fn install_vibe_hook() {
    if !should_register_mcp() {
        return;
    }
    let home = crate::core::home::resolve_home_dir().unwrap_or_default();
    install_vibe_mcp(&home);
    install_vibe_hooks_toml(&home);
}

/// Register the MCP server via the shared editor-registry writer
/// (`ConfigType::VibeToml`) — the single source of truth for the Vibe schema.
/// It parses `~/.vibe/config.toml` with `toml_edit`, upserts the
/// `[[mcp_servers]]` `lean-ctx` entry, creates the parent dir, and writes
/// atomically with a backup (no clobbering of a config that lacks the array).
fn install_vibe_mcp(home: &Path) {
    let binary = resolve_binary_path();
    let display_path = "~/.vibe/config.toml";
    let target = EditorTarget {
        name: "Mistral Vibe",
        agent_key: "vibe".to_string(),
        config_path: vibe_config_path(home),
        detect_path: home.join(".vibe"),
        config_type: ConfigType::VibeToml,
    };

    match write_config_with_options(&target, &binary, WriteOptions::default()) {
        Ok(result) => {
            if mcp_server_quiet_mode() {
                return;
            }
            match result.action {
                WriteAction::Already => {
                    eprintln!("Vibe MCP already configured at {display_path}");
                }
                WriteAction::Created | WriteAction::Updated => {
                    eprintln!("  \x1b[32m✓\x1b[0m Vibe MCP configured at {display_path}");
                }
            }
        }
        Err(e) => {
            tracing::error!("Failed to configure Vibe MCP: {e}");
            if !mcp_server_quiet_mode() {
                eprintln!("  \x1b[31m✗\x1b[0m Vibe MCP configuration failed: {e}");
            }
        }
    }
}

/// Upsert the lean-ctx `pre_tool` hook into `~/.vibe/hooks.toml`. Idempotent:
/// re-runs update the existing `lean-ctx-redirect` entry in place, preserving
/// any other user hooks and (via `toml_edit`) their comments/formatting.
fn install_vibe_hooks_toml(home: &Path) {
    let hooks_path = home.join(".vibe/hooks.toml");
    let display_path = "~/.vibe/hooks.toml";
    // `hook <agent>-<event>` dispatch, honoring the portable-binary override (#708).
    let command = format!("{} hook vibe-pre-tool", resolve_hook_command_binary());

    // Refuse to clobber a config we cannot parse (#443-style guard).
    let doc_res: Result<toml_edit::DocumentMut, String> = match std::fs::read_to_string(&hooks_path)
    {
        Ok(existing) if !existing.trim().is_empty() => existing
            .parse::<toml_edit::DocumentMut>()
            .map_err(|e| e.to_string()),
        _ => Ok(toml_edit::DocumentMut::new()),
    };
    let mut doc = match doc_res {
        Ok(doc) => doc,
        Err(e) => {
            tracing::error!("Failed to parse Vibe hooks.toml: {e}");
            if !mcp_server_quiet_mode() {
                eprintln!("  \x1b[31m✗\x1b[0m Vibe hooks.toml is invalid TOML — left untouched");
            }
            return;
        }
    };

    if !upsert_vibe_hook(&mut doc, &command) {
        if !mcp_server_quiet_mode() {
            eprintln!("Vibe hook already configured in {display_path}");
        }
        return;
    }

    if let Err(e) = crate::config_io::write_atomic_with_backup(&hooks_path, &doc.to_string()) {
        tracing::error!("Failed to write Vibe hooks.toml: {e}");
        if !mcp_server_quiet_mode() {
            eprintln!("  \x1b[31m✗\x1b[0m Vibe hook configuration failed: {e}");
        }
        return;
    }
    if !mcp_server_quiet_mode() {
        eprintln!("  \x1b[32m✓\x1b[0m Vibe pre_tool hook configured at {display_path}");
    }
}

/// Build the lean-ctx `[[hooks]]` entry for `~/.vibe/hooks.toml`.
fn vibe_hook_entry(command: &str) -> toml_edit::Table {
    let mut entry = toml_edit::Table::new();
    entry.insert("name", toml_edit::value(VIBE_HOOK_NAME));
    entry.insert("type", toml_edit::value("pre_tool"));
    entry.insert("match", toml_edit::value(VIBE_HOOK_MATCH));
    entry.insert("command", toml_edit::value(command));
    entry.insert("timeout", toml_edit::value(60.0));
    entry.insert(
        "description",
        toml_edit::value("Route native bash through lean-ctx; steer read_file/grep to ctx_* tools"),
    );
    entry
}

/// Upsert the lean-ctx pre_tool hook into a parsed `hooks.toml` document,
/// keyed by `name`. Returns `true` if the document changed (a write is
/// needed), `false` if the existing entry already matches. Pure — no I/O — so
/// the on-disk format and idempotency are unit-testable.
fn upsert_vibe_hook(doc: &mut toml_edit::DocumentMut, command: &str) -> bool {
    // Ensure a top-level `hooks` array-of-tables exists.
    if !matches!(doc.get("hooks"), Some(toml_edit::Item::ArrayOfTables(_))) {
        doc.insert(
            "hooks",
            toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new()),
        );
    }
    let Some(toml_edit::Item::ArrayOfTables(hooks)) = doc.get_mut("hooks") else {
        return false;
    };

    let entry = vibe_hook_entry(command);
    for table in hooks.iter_mut() {
        if table.get("name").and_then(|n| n.as_str()) == Some(VIBE_HOOK_NAME) {
            // Already current → no write.
            if table.get("command").and_then(|c| c.as_str()) == Some(command)
                && table.get("match").and_then(|m| m.as_str()) == Some(VIBE_HOOK_MATCH)
            {
                return false;
            }
            *table = entry;
            return true;
        }
    }
    hooks.push(entry);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    const CMD: &str = "/usr/local/bin/lean-ctx hook vibe-pre-tool";

    #[test]
    fn fresh_doc_gets_well_formed_hook_entry() {
        let mut doc = toml_edit::DocumentMut::new();
        assert!(upsert_vibe_hook(&mut doc, CMD));

        // Re-parse the rendered TOML to assert the on-disk shape Vibe will read.
        let round: toml_edit::DocumentMut = doc.to_string().parse().unwrap();
        let hooks = round.get("hooks").unwrap().as_array_of_tables().unwrap();
        assert_eq!(hooks.len(), 1);
        let t = hooks.get(0).unwrap();
        assert_eq!(t.get("name").unwrap().as_str(), Some(VIBE_HOOK_NAME));
        assert_eq!(t.get("type").unwrap().as_str(), Some("pre_tool"));
        assert_eq!(t.get("match").unwrap().as_str(), Some(VIBE_HOOK_MATCH));
        assert_eq!(t.get("command").unwrap().as_str(), Some(CMD));
        assert_eq!(t.get("timeout").unwrap().as_float(), Some(60.0));
    }

    #[test]
    fn rerun_is_idempotent() {
        let mut doc = toml_edit::DocumentMut::new();
        assert!(upsert_vibe_hook(&mut doc, CMD));
        // Second run with identical command → no change.
        assert!(!upsert_vibe_hook(&mut doc, CMD));
        let hooks = doc.get("hooks").unwrap().as_array_of_tables().unwrap();
        assert_eq!(hooks.len(), 1, "must not append a duplicate");
    }

    #[test]
    fn changed_command_updates_in_place() {
        let mut doc = toml_edit::DocumentMut::new();
        upsert_vibe_hook(&mut doc, CMD);
        assert!(upsert_vibe_hook(
            &mut doc,
            "/new/path/lean-ctx hook vibe-pre-tool"
        ));
        let hooks = doc.get("hooks").unwrap().as_array_of_tables().unwrap();
        assert_eq!(hooks.len(), 1, "same name → update, not append");
        assert_eq!(
            hooks.get(0).unwrap().get("command").unwrap().as_str(),
            Some("/new/path/lean-ctx hook vibe-pre-tool")
        );
    }

    #[test]
    fn preserves_unrelated_user_hooks() {
        let existing = r#"[[hooks]]
name = "my-linter"
type = "post_tool"
match = "edit"
command = "eslint --quiet"
"#;
        let mut doc: toml_edit::DocumentMut = existing.parse().unwrap();
        assert!(upsert_vibe_hook(&mut doc, CMD));
        let hooks = doc.get("hooks").unwrap().as_array_of_tables().unwrap();
        assert_eq!(hooks.len(), 2, "user's own hook must survive");
        let names: Vec<_> = hooks
            .iter()
            .filter_map(|t| t.get("name").and_then(|n| n.as_str()))
            .collect();
        assert!(names.contains(&"my-linter"));
        assert!(names.contains(&VIBE_HOOK_NAME));
    }
}
