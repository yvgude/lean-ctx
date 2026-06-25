use super::super::resolve_binary_path;
use crate::core::editor_registry::{
    ConfigType, EditorTarget, WriteAction, WriteOptions, write_config_with_options,
};

/// Configure the `OpenClaw` MCP entry via the shared editor-registry writer —
/// the single source of truth for the `OpenClaw` schema (GitHub #390). The
/// writer handles version detection (`meta.lastTouchedVersion`), the nested
/// `mcp.servers` schema for >= 2026.6.1, legacy `mcpServers` migration and
/// idempotent re-runs.
pub(crate) fn install_openclaw_hook() {
    // #281: OpenClaw is configured purely via its MCP entry, so skip entirely
    // when MCP registration is disabled.
    if !super::super::should_register_mcp() {
        return;
    }
    let binary = resolve_binary_path();
    let home = crate::core::home::resolve_home_dir().unwrap_or_default();
    let display_path = "~/.openclaw/openclaw.json";

    let target = EditorTarget {
        name: "OpenClaw",
        agent_key: "openclaw".to_string(),
        config_path: home.join(".openclaw/openclaw.json"),
        detect_path: home.join(".openclaw"),
        config_type: ConfigType::OpenClaw,
    };

    match write_config_with_options(&target, &binary, WriteOptions::default()) {
        Ok(result) => {
            if super::super::mcp_server_quiet_mode() {
                return;
            }
            match result.action {
                WriteAction::Already => {
                    eprintln!("OpenClaw MCP already configured at {display_path}");
                }
                WriteAction::Created | WriteAction::Updated => {
                    eprintln!("  \x1b[32m✓\x1b[0m OpenClaw MCP configured at {display_path}");
                    if let Some(note) = result.note {
                        eprintln!("    ({note})");
                    }
                }
            }
        }
        Err(e) => {
            tracing::error!("Failed to configure OpenClaw: {e}");
            if !super::super::mcp_server_quiet_mode() {
                eprintln!("  \x1b[31m✗\x1b[0m OpenClaw MCP configuration failed: {e}");
            }
        }
    }
}
