// SPDX-License-Identifier: Apache-2.0
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
        HookMode::Mcp | HookMode::Hybrid | HookMode::Replace => remove_stale_pi_mcp_entry(),
    }

    match mode {
        HookMode::Replace => propagate_pi_replace_mode(),
        HookMode::Hybrid => propagate_pi_hybrid_mode(),
        HookMode::Mcp => {}
    }

    let scope = lean_ctx::core::config::Config::load().rules_scope_effective();
    let skip_project = global || scope == lean_ctx::core::config::RulesScope::Global;

    if skip_project {
        println!(
            "Global mode: skipping project-local AGENTS.md (use without --global in a project)."
        );
    } else {
        let agents_md = PathBuf::from("AGENTS.md");
        let content = match mode {
            HookMode::Replace => lean_ctx::core::agent_templates::PI_AGENTS_REPLACE_MD,
            HookMode::Mcp | HookMode::Hybrid => lean_ctx::core::agent_templates::PI_AGENTS_MD,
        };
        if !agents_md.exists()
            || !std::fs::read_to_string(&agents_md)
                .unwrap_or_default()
                .contains("lean-ctx")
        {
            write_file(&agents_md, content);
            println!("Created AGENTS.md in current project directory.");
        } else {
            println!("AGENTS.md already contains lean-ctx configuration.");
        }
    }

    println!();
    match mode {
        HookMode::Replace => {
            println!(
                "Setup complete (Replace mode). Native read/bash/grep/find/ls are suppressed — \
                 only ctx_* tools are available."
            );
        }
        _ => {
            println!(
                "Setup complete. Prefer the ctx_* tools (ctx_read/ctx_shell/ctx_search/ctx_glob/ctx_tree) — \
                 only those are compressed; native read/bash/grep are not."
            );
        }
    }
    println!(
        "Embedded MCP bridge (session cache) is on by default. Use /lean-ctx in Pi to verify \
         it reports 'connected'."
    );
}

/// Write the Pi extension config.json with `"mode": "replace"` so the embedded
/// MCP bridge suppresses all native Pi builtins (read/bash/ls/find/grep) and
/// only exposes ctx_* tools.
fn propagate_pi_replace_mode() {
    let Some(home) = lean_ctx::core::home::resolve_home_dir() else {
        return;
    };
    let config_dir = home
        .join(".pi")
        .join("agent")
        .join("extensions")
        .join("pi-lean-ctx");
    let _ = std::fs::create_dir_all(&config_dir);
    let config_path = config_dir.join("config.json");

    let mut json = if config_path.exists() {
        std::fs::read_to_string(&config_path)
            .ok()
            .and_then(|c| serde_json::from_str::<serde_json::Value>(&c).ok())
            .unwrap_or_else(|| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    let obj = json
        .as_object_mut()
        .expect("Pi config root must be a JSON object");
    let current_mode = obj.get("mode").and_then(|v| v.as_str()).unwrap_or_default();
    if current_mode == "replace" {
        return;
    }
    obj.insert(
        "mode".to_string(),
        serde_json::Value::String("replace".to_string()),
    );

    // Propagate engine settings from config.toml (#793).
    let cfg = lean_ctx::core::config::Config::load();
    let env_block = obj.entry("env").or_insert_with(|| serde_json::json!({}));
    if let Some(env_obj) = env_block.as_object_mut() {
        let level_str = format!("{:?}", cfg.compression_level).to_lowercase();
        env_obj.insert(
            "LEAN_CTX_COMPRESSION_LEVEL".to_string(),
            serde_json::Value::String(level_str),
        );
        let footer_str = serde_json::to_string(&cfg.savings_footer)
            .unwrap_or_default()
            .trim_matches('"')
            .to_string();
        env_obj.insert(
            "LEAN_CTX_SAVINGS_FOOTER".to_string(),
            serde_json::Value::String(footer_str),
        );
    }

    if let Ok(out) = serde_json::to_string_pretty(&json) {
        write_file(&config_path, &out);
        println!(
            "  \x1b[32m✓\x1b[0m Pi config: set mode=replace + LeanCTX settings in {}",
            config_path.display()
        );
    }
}

/// Write the Pi extension config.json with `"routeShell": true` so shell
/// commands are routed through lean-ctx for compression even in Hybrid mode
/// (#793). Unlike Replace mode, native builtins remain available.
fn propagate_pi_hybrid_mode() {
    let Some(home) = lean_ctx::core::home::resolve_home_dir() else {
        return;
    };
    let config_dir = home
        .join(".pi")
        .join("agent")
        .join("extensions")
        .join("pi-lean-ctx");
    let _ = std::fs::create_dir_all(&config_dir);
    let config_path = config_dir.join("config.json");

    let mut json = if config_path.exists() {
        std::fs::read_to_string(&config_path)
            .ok()
            .and_then(|c| serde_json::from_str::<serde_json::Value>(&c).ok())
            .unwrap_or_else(|| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    let obj = json
        .as_object_mut()
        .expect("Pi config root must be a JSON object");

    let mut changed = false;

    if obj.get("routeShell").and_then(serde_json::Value::as_bool) != Some(true) {
        obj.insert("routeShell".to_string(), serde_json::Value::Bool(true));
        changed = true;
    }

    // Propagate engine settings from config.toml (#793).
    let cfg = lean_ctx::core::config::Config::load();
    let env_block = obj.entry("env").or_insert_with(|| serde_json::json!({}));
    if let Some(env_obj) = env_block.as_object_mut() {
        let level_str = format!("{:?}", cfg.compression_level).to_lowercase();
        if env_obj
            .get("LEAN_CTX_COMPRESSION_LEVEL")
            .and_then(|v| v.as_str())
            != Some(&level_str)
        {
            env_obj.insert(
                "LEAN_CTX_COMPRESSION_LEVEL".to_string(),
                serde_json::Value::String(level_str),
            );
            changed = true;
        }
        let footer_str = serde_json::to_string(&cfg.savings_footer)
            .unwrap_or_default()
            .trim_matches('"')
            .to_string();
        if env_obj
            .get("LEAN_CTX_SAVINGS_FOOTER")
            .and_then(|v| v.as_str())
            != Some(&footer_str)
        {
            env_obj.insert(
                "LEAN_CTX_SAVINGS_FOOTER".to_string(),
                serde_json::Value::String(footer_str),
            );
            changed = true;
        }
    }

    if !changed {
        return;
    }

    if let Ok(out) = serde_json::to_string_pretty(&json) {
        write_file(&config_path, &out);
        println!(
            "  \x1b[32m✓\x1b[0m Pi config: set routeShell=true + LeanCTX settings in {}",
            config_path.display()
        );
    }
}

/// Pi has no native MCP adapter: a `lean-ctx` entry in `~/.pi/agent/mcp.json`
/// is never served by anything, but older pi-lean-ctx versions read it as
/// "an adapter is configured" and disabled their embedded MCP bridge — the
/// session cache silently never engaged (GitHub #361, found by the tokbench
/// independent benchmark). Earlier installers wrote that entry by default, so
/// setup now removes it instead.
fn remove_stale_pi_mcp_entry() {
    let Some(home) = lean_ctx::core::home::resolve_home_dir() else {
        return;
    };

    let mcp_config_path = home.join(".pi/agent/mcp.json");
    let Ok(content) = std::fs::read_to_string(&mcp_config_path) else {
        return;
    };
    if !content.contains("lean-ctx") {
        return;
    }

    let Ok(mut json) = lean_ctx::core::jsonc::parse_jsonc(&content) else {
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
