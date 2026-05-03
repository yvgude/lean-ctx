use std::path::PathBuf;

use super::super::{mcp_server_quiet_mode, resolve_binary_path, write_file};

pub(crate) fn install_copilot_hook(global: bool) {
    let binary = resolve_binary_path();

    if global {
        let mcp_path = crate::core::editor_registry::vscode_mcp_path();
        if mcp_path.as_os_str() == "/nonexistent" {
            println!("  \x1b[2mVS Code not found — skipping global Copilot config\x1b[0m");
            return;
        }
        write_vscode_mcp_file(&mcp_path, &binary, "global VS Code User MCP");
        install_copilot_pretooluse_hook(true);
    } else {
        let vscode_dir = PathBuf::from(".vscode");
        let _ = std::fs::create_dir_all(&vscode_dir);
        let mcp_path = vscode_dir.join("mcp.json");
        write_vscode_mcp_file(&mcp_path, &binary, ".vscode/mcp.json");
        install_copilot_pretooluse_hook(false);
    }
}

fn install_copilot_pretooluse_hook(global: bool) {
    let binary = resolve_binary_path();
    let rewrite_cmd = format!("{binary} hook rewrite");
    let redirect_cmd = format!("{binary} hook redirect");

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
        !content.contains("hook rewrite") || content.contains("\"PreToolUse\"")
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
                obj.insert("hooks".to_string(), hook_config["hooks"].clone());
                write_file(
                    &hook_path,
                    &serde_json::to_string_pretty(&existing).unwrap_or_default(),
                );
                if !mcp_server_quiet_mode() {
                    println!("Updated Copilot hooks at {}", hook_path.display());
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
        println!("Installed Copilot hooks at {}", hook_path.display());
    }
}

fn write_vscode_mcp_file(mcp_path: &PathBuf, binary: &str, label: &str) {
    let data_dir = crate::core::data_dir::lean_ctx_data_dir()
        .map(|d| d.to_string_lossy().to_string())
        .unwrap_or_default();
    let desired = serde_json::json!({ "type": "stdio", "command": binary, "args": [], "env": { "LEAN_CTX_DATA_DIR": data_dir } });
    if mcp_path.exists() {
        let content = std::fs::read_to_string(mcp_path).unwrap_or_default();
        match crate::core::jsonc::parse_jsonc(&content) {
            Ok(mut json) => {
                if let Some(obj) = json.as_object_mut() {
                    let servers = obj
                        .entry("servers")
                        .or_insert_with(|| serde_json::json!({}));
                    if let Some(servers_obj) = servers.as_object_mut() {
                        if servers_obj.get("lean-ctx") == Some(&desired) {
                            println!("  \x1b[32m✓\x1b[0m Copilot already configured in {label}");
                            return;
                        }
                        servers_obj.insert("lean-ctx".to_string(), desired);
                    }
                    write_file(
                        mcp_path,
                        &serde_json::to_string_pretty(&json).unwrap_or_default(),
                    );
                    println!("  \x1b[32m✓\x1b[0m Added lean-ctx to {label}");
                    return;
                }
            }
            Err(e) => {
                tracing::warn!(
                    "Could not parse VS Code MCP config at {}: {e}\nAdd to \"servers\": \"lean-ctx\": {{ \"command\": \"{}\", \"args\": [] }}",
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

    let data_dir = crate::core::data_dir::lean_ctx_data_dir()
        .map(|d| d.to_string_lossy().to_string())
        .unwrap_or_default();
    let config = serde_json::json!({
        "servers": {
            "lean-ctx": {
                "type": "stdio",
                "command": binary,
                "args": [],
                "env": { "LEAN_CTX_DATA_DIR": data_dir }
            }
        }
    });

    write_file(
        mcp_path,
        &serde_json::to_string_pretty(&config).unwrap_or_default(),
    );
    println!("  \x1b[32m✓\x1b[0m Created {label} with lean-ctx MCP server");
}
