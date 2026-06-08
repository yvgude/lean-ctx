use std::path::PathBuf;

use super::super::{mcp_server_quiet_mode, resolve_binary_path, write_file};

pub(crate) fn install_copilot_hook(global: bool) {
    let binary = resolve_binary_path();

    if global {
        // Copilot CLI loads global MCP servers from `~/.copilot/mcp-config.json`,
        // independent of any project/repo config. Only register lean-ctx there
        // for global installs (the repo-scoped `.github/mcp.json` covers local).
        write_copilot_cli_home_mcp();

        let mcp_path = crate::core::editor_registry::vscode_mcp_path();
        if mcp_path.as_os_str() == "/nonexistent" {
            if !mcp_server_quiet_mode() {
                eprintln!("  \x1b[2mVS Code not found — skipping global Copilot config\x1b[0m");
            }
            return;
        }
        write_vscode_mcp_file(&mcp_path, &binary, "global VS Code User MCP");
        install_copilot_pretooluse_hook(true);
    } else {
        let vscode_dir = PathBuf::from(".vscode");
        let _ = std::fs::create_dir_all(&vscode_dir);
        let mcp_path = vscode_dir.join("mcp.json");
        write_vscode_mcp_file(&mcp_path, &binary, ".vscode/mcp.json");

        let github_dir = PathBuf::from(".github");
        let _ = std::fs::create_dir_all(&github_dir);
        let copilot_mcp = github_dir.join("mcp.json");
        write_copilot_cli_mcp_file(&copilot_mcp, &binary, ".github/mcp.json");

        install_copilot_pretooluse_hook(false);
    }
}

/// Register lean-ctx in the Copilot CLI's global MCP config at
/// `~/.copilot/mcp-config.json`. Reuses the canonical `CopilotCli` writer so the
/// entry format and merge behavior match `configure_agent_mcp`, and uses the
/// portable binary path so repeated installs stay idempotent (no churn).
fn write_copilot_cli_home_mcp() {
    let Some(home) = dirs::home_dir() else {
        return;
    };

    let binary = crate::core::portable_binary::resolve_portable_binary();
    let target = crate::core::editor_registry::EditorTarget {
        name: "Copilot CLI",
        agent_key: "copilot".to_string(),
        detect_path: PathBuf::from("/nonexistent"),
        config_path: home.join(".copilot/mcp-config.json"),
        config_type: crate::core::editor_registry::ConfigType::CopilotCli,
    };

    match crate::core::editor_registry::write_config_with_options(
        &target,
        &binary,
        crate::core::editor_registry::WriteOptions {
            overwrite_invalid: true,
        },
    ) {
        Ok(result) => {
            if !mcp_server_quiet_mode() {
                use crate::core::editor_registry::WriteAction;
                let label = "~/.copilot/mcp-config.json";
                let msg = match result.action {
                    WriteAction::Created => format!("Created {label} with lean-ctx MCP server"),
                    WriteAction::Updated => format!("Added lean-ctx to {label}"),
                    WriteAction::Already => format!("lean-ctx already configured in {label}"),
                };
                eprintln!("  \x1b[32m✓\x1b[0m {msg}");
            }
        }
        Err(e) => {
            if !mcp_server_quiet_mode() {
                eprintln!(
                    "  \x1b[33m⚠\x1b[0m  Could not configure {}: {e}",
                    target.config_path.display()
                );
            }
        }
    }
}

fn install_copilot_pretooluse_hook(global: bool) {
    let binary = resolve_binary_path();
    let rewrite_cmd = format!("{binary} hook rewrite");
    let redirect_cmd = format!("{binary} hook redirect");
    let observe_cmd = format!("{binary} hook observe");

    let hook_config = serde_json::json!({
        "version": 1,
        "hooks": {
            "preToolUse": [
                {
                    "type": "command",
                    "bash": rewrite_cmd,
                    "timeoutSec": 15
                },
                {
                    "type": "command",
                    "bash": redirect_cmd,
                    "timeoutSec": 5
                }
            ],
            "postToolUse": [
                {
                    "type": "command",
                    "bash": observe_cmd,
                    "timeoutSec": 5
                }
            ]
        }
    });

    let hook_path = if global {
        let Some(home) = crate::core::home::resolve_home_dir() else {
            return;
        };
        let dir = home.join(".github").join("hooks");
        let _ = std::fs::create_dir_all(&dir);
        dir.join("hooks.json")
    } else {
        let dir = PathBuf::from(".github").join("hooks");
        let _ = std::fs::create_dir_all(&dir);
        dir.join("hooks.json")
    };

    let needs_write = if hook_path.exists() {
        let content = std::fs::read_to_string(&hook_path).unwrap_or_default();
        !content.contains("hook rewrite")
            || content.contains("\"PreToolUse\"")
            || !content.contains("hook observe")
    } else {
        true
    };

    if !needs_write {
        return;
    }

    if hook_path.exists() {
        if let Ok(mut existing) = crate::core::jsonc::parse_jsonc(
            &std::fs::read_to_string(&hook_path).unwrap_or_default(),
        ) {
            if let Some(obj) = existing.as_object_mut() {
                obj.insert("version".to_string(), serde_json::json!(1));
                let hooks = obj
                    .entry("hooks".to_string())
                    .or_insert_with(|| serde_json::json!({}));
                if let Some(hooks_obj) = hooks.as_object_mut() {
                    if let Some(desired_hooks) = hook_config["hooks"].as_object() {
                        for (event, entries) in desired_hooks {
                            hooks_obj.insert(event.clone(), entries.clone());
                        }
                    }
                }
                write_file(
                    &hook_path,
                    &serde_json::to_string_pretty(&existing).unwrap_or_default(),
                );
                if !mcp_server_quiet_mode() {
                    eprintln!("Updated Copilot hooks at {}", hook_path.display());
                }
                return;
            }
        }
    }

    write_file(
        &hook_path,
        &serde_json::to_string_pretty(&hook_config).unwrap_or_default(),
    );
    if !mcp_server_quiet_mode() {
        eprintln!("Installed Copilot hooks at {}", hook_path.display());
    }
}

fn server_entry(binary: &str) -> serde_json::Value {
    let data_dir = crate::core::data_dir::lean_ctx_data_dir()
        .map(|d| d.to_string_lossy().to_string())
        .unwrap_or_default();
    serde_json::json!({
        "type": "stdio",
        "command": binary,
        "args": [],
        "env": { "LEAN_CTX_DATA_DIR": data_dir }
    })
}

/// VS Code uses `"servers"` as the top-level key (not `"mcpServers"`).
fn write_vscode_mcp_file(mcp_path: &PathBuf, binary: &str, label: &str) {
    write_mcp_config(mcp_path, binary, label, "servers", server_entry(binary));
}

/// Copilot CLI uses `"mcpServers"` as the top-level key in `.github/mcp.json`.
fn write_copilot_cli_mcp_file(mcp_path: &PathBuf, binary: &str, label: &str) {
    let data_dir = crate::core::data_dir::lean_ctx_data_dir()
        .map(|d| d.to_string_lossy().to_string())
        .unwrap_or_default();
    let entry = serde_json::json!({
        "command": binary,
        "args": [],
        "env": { "LEAN_CTX_DATA_DIR": data_dir }
    });
    write_mcp_config(mcp_path, binary, label, "mcpServers", entry);
}

fn write_mcp_config(
    mcp_path: &PathBuf,
    binary: &str,
    label: &str,
    root_key: &str,
    desired: serde_json::Value,
) {
    if mcp_path.exists() {
        let content = std::fs::read_to_string(mcp_path).unwrap_or_default();
        match crate::core::jsonc::parse_jsonc(&content) {
            Ok(mut json) => {
                if let Some(obj) = json.as_object_mut() {
                    let servers = obj.entry(root_key).or_insert_with(|| serde_json::json!({}));
                    if let Some(servers_obj) = servers.as_object_mut() {
                        if servers_obj.get("lean-ctx") == Some(&desired) {
                            if !mcp_server_quiet_mode() {
                                eprintln!(
                                    "  \x1b[32m✓\x1b[0m lean-ctx already configured in {label}"
                                );
                            }
                            return;
                        }
                        servers_obj.insert("lean-ctx".to_string(), desired);
                    }
                    write_file(
                        mcp_path,
                        &serde_json::to_string_pretty(&json).unwrap_or_default(),
                    );
                    if !mcp_server_quiet_mode() {
                        eprintln!("  \x1b[32m✓\x1b[0m Added lean-ctx to {label}");
                    }
                    return;
                }
            }
            Err(e) => {
                tracing::warn!(
                    "Could not parse MCP config at {}: {e}\nAdd to \"{root_key}\": \"lean-ctx\": {{ \"command\": \"{}\", \"args\": [] }}",
                    mcp_path.display(),
                    binary
                );
                return;
            }
        }
    }

    if let Some(parent) = mcp_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let config = serde_json::json!({
        root_key: {
            "lean-ctx": desired
        }
    });

    write_file(
        mcp_path,
        &serde_json::to_string_pretty(&config).unwrap_or_default(),
    );
    if !mcp_server_quiet_mode() {
        eprintln!("  \x1b[32m✓\x1b[0m Created {label} with lean-ctx MCP server");
    }
}
