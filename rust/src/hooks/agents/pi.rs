use std::path::PathBuf;

use crate::hooks::HookMode;

use super::super::write_file;

pub(crate) fn install_pi_hook_with_mode(global: bool, mode: HookMode) {
    let has_pi = std::process::Command::new("pi")
        .arg("--version")
        .output()
        .is_ok();

    if !has_pi {
        println!("Pi Coding Agent not found in PATH.");
        println!("Install Pi first: npm install -g @earendil-works/pi-coding-agent");
        println!();
    }

    println!("Installing pi-lean-ctx Pi Package...");
    println!();

    let install_result = std::process::Command::new("pi")
        .args(["install", "npm:pi-lean-ctx"])
        .status();

    match install_result {
        Ok(status) if status.success() => {
            eprintln!("Installed pi-lean-ctx Pi Package.");
        }
        _ => {
            eprintln!("Could not auto-install pi-lean-ctx. Install manually:");
            eprintln!("  pi install npm:pi-lean-ctx");
            eprintln!();
        }
    }

    match mode {
        HookMode::Mcp | HookMode::Hybrid => remove_stale_pi_mcp_entry(),
    }

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
    println!(
        "Setup complete. Prefer the ctx_* tools (ctx_read/ctx_shell/ctx_search/ctx_glob/ctx_tree) — \
         only those are compressed; native read/bash/grep are not."
    );
    match mode {
        HookMode::Mcp | HookMode::Hybrid => {
            println!(
                "Embedded MCP bridge (session cache) is on by default. Use /lean-ctx in Pi to verify \
                 it reports 'connected'."
            );
        }
    }
}

/// Pi has no native MCP adapter: a `lean-ctx` entry in `~/.pi/agent/mcp.json`
/// is never served by anything, but older pi-lean-ctx versions read it as
/// "an adapter is configured" and disabled their embedded MCP bridge — the
/// session cache silently never engaged (GitHub #361, found by the tokbench
/// independent benchmark). Earlier installers wrote that entry by default, so
/// setup now removes it instead.
fn remove_stale_pi_mcp_entry() {
    let Some(home) = crate::core::home::resolve_home_dir() else {
        return;
    };

    let mcp_config_path = home.join(".pi/agent/mcp.json");
    let Ok(content) = std::fs::read_to_string(&mcp_config_path) else {
        return;
    };
    if !content.contains("lean-ctx") {
        return;
    }

    let Ok(mut json) = crate::core::jsonc::parse_jsonc(&content) else {
        return;
    };
    let Some(servers) = json
        .get_mut("mcpServers")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return;
    };
    if servers.remove("lean-ctx").is_none() {
        return;
    }

    let only_empty_servers = servers.is_empty()
        && json
            .as_object()
            .is_some_and(|o| o.keys().all(|k| k == "mcpServers"));
    if only_empty_servers {
        let _ = std::fs::remove_file(&mcp_config_path);
        println!(
            "  \x1b[32m✓\x1b[0m Removed stale Pi MCP config (~/.pi/agent/mcp.json) — \
             the embedded pi-lean-ctx bridge serves MCP instead"
        );
        return;
    }
    if let Ok(formatted) = serde_json::to_string_pretty(&json) {
        let _ = std::fs::write(&mcp_config_path, formatted);
        println!(
            "  \x1b[32m✓\x1b[0m Removed stale lean-ctx entry from ~/.pi/agent/mcp.json — \
             the embedded pi-lean-ctx bridge serves MCP instead"
        );
    }
}
