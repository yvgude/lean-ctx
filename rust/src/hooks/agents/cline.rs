use std::path::PathBuf;

use super::super::{mcp_server_quiet_mode, resolve_binary_path, write_file};
use super::shared::prepare_project_rules_path;

pub(crate) fn install_cline_rules(global: bool) {
    if global {
        let vscode_mcp = crate::core::editor_registry::vscode_mcp_path();
        if vscode_mcp.as_os_str() != "/nonexistent" {
            install_vscode_mcp_for_cline(&vscode_mcp);
        }
    } else {
        let vscode_dir = PathBuf::from(".vscode");
        let _ = std::fs::create_dir_all(&vscode_dir);
        install_vscode_mcp_for_cline(&vscode_dir.join("mcp.json"));
    }

    let Some(rules_path) = prepare_project_rules_path(global, ".clinerules") else {
        return;
    };

    let shadow = crate::core::config::Config::load().shadow_mode;
    write_file(&rules_path, &cline_rules_body(shadow));
    if !mcp_server_quiet_mode() {
        eprintln!("Installed .clinerules in current project.");
    }
}

/// Builds the `.clinerules` body from `rules_canonical` (the single source of
/// truth) via the canonical `Dedicated` render.
///
/// Cline and Roo get the lean-ctx **MCP server** installed above, and the shell
/// hook already wraps real terminal commands, so the rules must steer the agent
/// to the `ctx_*` MCP tools — NOT tell it to hand-prefix every command with
/// `lean-ctx -c`. The old guidance did exactly that, which double-wraps an
/// already-wrapped command and trips the re-entry passthrough, so output came
/// back uncompressed (GH #603).
///
/// Rendered at [`CompressionLevel::Off`](crate::core::config::CompressionLevel::Off)
/// on purpose: the per-turn output-style payload is delivered by the MCP
/// instructions channel and the deduped global `.cline/rules/lean-ctx.md`
/// carrier. `.clinerules` is a project file the dedup pass does not scan, so
/// emitting the compression block here too would only duplicate it (#684). The
/// `START_MARK`/`END_MARK` wrapper is what `uninstall` strips.
fn cline_rules_body(shadow: bool) -> String {
    crate::core::rules_canonical::render(
        shadow,
        crate::core::rules_canonical::Wrapper::Dedicated,
        crate::core::config::CompressionLevel::Off,
    )
}

fn install_vscode_mcp_for_cline(mcp_path: &std::path::Path) {
    let binary = resolve_binary_path();
    let entry = serde_json::json!({
        "type": "stdio",
        "command": binary,
        "args": [],
        "env": super::super::mcp_server_env_json()
    });

    crate::hooks::install_named_json_server(
        "Cline/Roo",
        &mcp_path.display().to_string(),
        mcp_path,
        "servers",
        entry,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::rules_canonical::{COMPRESSION_BLOCK_START, END_MARK, START_MARK};

    /// #603: the old `.clinerules` told the agent to hand-prefix every shell
    /// command with `lean-ctx -c`, which re-wraps an already-wrapped command and
    /// passes through uncompressed. The rules must be MCP-first and carry NO
    /// `lean-ctx -c` prefix guidance, derived from `rules_canonical`.
    #[test]
    fn cline_rules_are_mcp_first_without_lean_ctx_dash_c() {
        let body = cline_rules_body(false);
        assert!(
            body.contains(START_MARK),
            "must carry the canonical markers"
        );
        assert!(body.contains(END_MARK));
        assert!(
            body.contains("ctx_shell"),
            "must steer to the ctx_* MCP tools:\n{body}"
        );
        assert!(
            !body.contains("lean-ctx -c"),
            "must NOT tell the agent to hand-prefix lean-ctx -c (#603):\n{body}"
        );
    }

    /// #684: `.clinerules` is not scanned by the dedup pass, so it must never
    /// carry the per-turn compression payload (delivered via MCP + the deduped
    /// global carrier) — in either shadow or non-shadow mode.
    #[test]
    fn cline_rules_omit_per_turn_compression_block() {
        for shadow in [false, true] {
            let body = cline_rules_body(shadow);
            assert!(
                !body.contains(COMPRESSION_BLOCK_START),
                "shadow={shadow}: .clinerules must not duplicate the compression block:\n{body}"
            );
            assert!(
                !body.contains("lean-ctx -c"),
                "shadow={shadow}: no lean-ctx -c prefix guidance:\n{body}"
            );
        }
    }
}
