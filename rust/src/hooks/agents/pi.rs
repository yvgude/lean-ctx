use std::path::PathBuf;

use super::super::{full_server_entry, resolve_binary_path, write_file};

pub(crate) fn install_pi_hook(global: bool) {
    let has_pi = std::process::Command::new("pi")
        .arg("--version")
        .output()
        .is_ok();

    if !has_pi {
        println!("Pi Coding Agent not found in PATH.");
        println!("Install Pi first: npm install -g @mariozechner/pi-coding-agent");
        println!();
    }

    println!("Installing pi-lean-ctx Pi Package...");
    println!();

    let install_result = std::process::Command::new("pi")
        .args(["install", "npm:pi-lean-ctx"])
        .status();

    match install_result {
        Ok(status) if status.success() => {
            println!("Installed pi-lean-ctx Pi Package.");
        }
        _ => {
            println!("Could not auto-install pi-lean-ctx. Install manually:");
            println!("  pi install npm:pi-lean-ctx");
            println!();
        }
    }

    write_pi_mcp_config();

    let scope = crate::core::config::Config::load().rules_scope_effective();
    let skip_project = global || scope == crate::core::config::RulesScope::Global;

    if skip_project {
        println!(
            "Global mode: skipping project-local AGENTS.md (use without --global in a project)."
        );
    } else {
        let agents_md = PathBuf::from("AGENTS.md");
        if !agents_md.exists()
            || !std::fs::read_to_string(&agents_md)
                .unwrap_or_default()
                .contains("lean-ctx")
        {
            let content = include_str!("../../templates/PI_AGENTS.md");
            write_file(&agents_md, content);
            println!("Created AGENTS.md in current project directory.");
        } else {
            println!("AGENTS.md already contains lean-ctx configuration.");
        }
    }

    println!();
    println!("Setup complete. All Pi tools (bash, read, grep, find, ls) route through lean-ctx.");
    println!("MCP tools (ctx_session, ctx_knowledge, ctx_semantic_search, ...) also available.");
    println!("Use /lean-ctx in Pi to verify the binary path and MCP status.");
}

fn write_pi_mcp_config() {
    let Some(home) = crate::core::home::resolve_home_dir() else {
        return;
    };

    let mcp_config_path = home.join(".pi/agent/mcp.json");

    if !home.join(".pi/agent").exists() {
        println!("  \x1b[2m○ ~/.pi/agent/ not found — skipping MCP config\x1b[0m");
        return;
    }

    if mcp_config_path.exists() {
        let Ok(content) = std::fs::read_to_string(&mcp_config_path) else {
            return;
        };
        if content.contains("lean-ctx") {
            println!("  \x1b[32m✓\x1b[0m Pi MCP config already contains lean-ctx");
            return;
        }

        if let Ok(mut json) = crate::core::jsonc::parse_jsonc(&content) {
            if let Some(obj) = json.as_object_mut() {
                let servers = obj
                    .entry("mcpServers")
                    .or_insert_with(|| serde_json::json!({}));
                if let Some(servers_obj) = servers.as_object_mut() {
                    servers_obj.insert("lean-ctx".to_string(), pi_mcp_server_entry());
                }
                if let Ok(formatted) = serde_json::to_string_pretty(&json) {
                    let _ = std::fs::write(&mcp_config_path, formatted);
                    println!(
                        "  \x1b[32m✓\x1b[0m Added lean-ctx to Pi MCP config (~/.pi/agent/mcp.json)"
                    );
                }
            }
        }
        return;
    }

    let content = serde_json::json!({
        "mcpServers": {
            "lean-ctx": pi_mcp_server_entry()
        }
    });
    if let Ok(formatted) = serde_json::to_string_pretty(&content) {
        let _ = std::fs::write(&mcp_config_path, formatted);
        println!("  \x1b[32m✓\x1b[0m Created Pi MCP config (~/.pi/agent/mcp.json)");
    }
}

fn pi_mcp_server_entry() -> serde_json::Value {
    let binary = resolve_binary_path();
    let mut entry = full_server_entry(&binary);
    if let Some(obj) = entry.as_object_mut() {
        obj.insert("lifecycle".to_string(), serde_json::json!("lazy"));
        obj.insert("directTools".to_string(), serde_json::json!(true));
    }
    entry
}
