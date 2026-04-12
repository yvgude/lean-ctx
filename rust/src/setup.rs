use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WriteAction {
    Created,
    Updated,
    Already,
}

struct EditorTarget {
    name: &'static str,
    agent_key: &'static str,
    config_path: PathBuf,
    detect_path: PathBuf,
    config_type: ConfigType,
}

enum ConfigType {
    McpJson,
    Zed,
    Codex,
    VsCodeMcp,
    OpenCode,
    Crush,
}

pub fn run_setup() {
    use crate::terminal_ui;

    let home = match dirs::home_dir() {
        Some(h) => h,
        None => {
            eprintln!("Cannot determine home directory");
            std::process::exit(1);
        }
    };

    let binary = resolve_portable_binary();

    let home_str = home.to_string_lossy().to_string();

    terminal_ui::print_setup_header();

    // Step 1: Shell hook
    terminal_ui::print_step_header(1, 5, "Shell Hook");
    crate::cli::cmd_init(&["--global".to_string()]);

    // Step 2: Editor auto-detection + configuration
    terminal_ui::print_step_header(2, 5, "AI Tool Detection");

    let targets = build_targets(&home, &binary);
    let mut newly_configured: Vec<&str> = Vec::new();
    let mut already_configured: Vec<&str> = Vec::new();
    let mut not_installed: Vec<&str> = Vec::new();
    let mut errors: Vec<&str> = Vec::new();

    for target in &targets {
        let short_path = shorten_path(&target.config_path.to_string_lossy(), &home_str);

        if !target.detect_path.exists() {
            not_installed.push(target.name);
            continue;
        }

        match write_config(target, &binary) {
            Ok(WriteAction::Already) => {
                terminal_ui::print_status_ok(&format!(
                    "{:<20} \x1b[2m{short_path}\x1b[0m",
                    target.name
                ));
                already_configured.push(target.name);
            }
            Ok(WriteAction::Created | WriteAction::Updated) => {
                terminal_ui::print_status_new(&format!(
                    "{:<20} \x1b[2m{short_path}\x1b[0m",
                    target.name
                ));
                newly_configured.push(target.name);
            }
            Err(e) => {
                terminal_ui::print_status_warn(&format!("{}: {e}", target.name));
                errors.push(target.name);
            }
        }
    }

    let total_ok = newly_configured.len() + already_configured.len();
    if total_ok == 0 && errors.is_empty() {
        terminal_ui::print_status_warn(
            "No AI tools detected. Install one and re-run: lean-ctx setup",
        );
    }

    if !not_installed.is_empty() {
        println!(
            "  \x1b[2m○ {} not detected: {}\x1b[0m",
            not_installed.len(),
            not_installed.join(", ")
        );
    }

    // Step 3: Agent rules injection
    terminal_ui::print_step_header(3, 5, "Agent Rules");
    let rules_result = crate::rules_inject::inject_all_rules(&home);
    for name in &rules_result.injected {
        terminal_ui::print_status_new(&format!("{name:<20} \x1b[2mrules injected\x1b[0m"));
    }
    for name in &rules_result.updated {
        terminal_ui::print_status_new(&format!("{name:<20} \x1b[2mrules updated\x1b[0m"));
    }
    for name in &rules_result.already {
        terminal_ui::print_status_ok(&format!("{name:<20} \x1b[2mrules up-to-date\x1b[0m"));
    }
    for err in &rules_result.errors {
        terminal_ui::print_status_warn(err);
    }
    if rules_result.injected.is_empty()
        && rules_result.updated.is_empty()
        && rules_result.already.is_empty()
        && rules_result.errors.is_empty()
    {
        terminal_ui::print_status_skip("No agent rules needed");
    }

    // Legacy agent hooks
    for target in &targets {
        if !target.detect_path.exists() || target.agent_key.is_empty() {
            continue;
        }
        crate::hooks::install_agent_hook(target.agent_key, true);
    }

    // Step 4: Data directory + diagnostics
    terminal_ui::print_step_header(4, 5, "Environment Check");
    let lean_dir = home.join(".lean-ctx");
    if !lean_dir.exists() {
        let _ = std::fs::create_dir_all(&lean_dir);
        terminal_ui::print_status_new("Created ~/.lean-ctx/");
    } else {
        terminal_ui::print_status_ok("~/.lean-ctx/ ready");
    }
    crate::doctor::run_compact();

    // Step 5: Data sharing
    terminal_ui::print_step_header(5, 5, "Help Improve lean-ctx");
    println!("  Share anonymous compression stats to make lean-ctx better.");
    println!("  \x1b[1mNo code, no file names, no personal data — ever.\x1b[0m");
    println!();
    print!("  Enable anonymous data sharing? \x1b[1m[Y/n]\x1b[0m ");
    use std::io::Write;
    std::io::stdout().flush().ok();

    let mut input = String::new();
    let contribute = if std::io::stdin().read_line(&mut input).is_ok() {
        let answer = input.trim().to_lowercase();
        answer.is_empty() || answer == "y" || answer == "yes"
    } else {
        false
    };

    if contribute {
        let config_dir = home.join(".lean-ctx");
        let _ = std::fs::create_dir_all(&config_dir);
        let config_path = config_dir.join("config.toml");
        let mut config_content = std::fs::read_to_string(&config_path).unwrap_or_default();
        if !config_content.contains("[cloud]") {
            if !config_content.is_empty() && !config_content.ends_with('\n') {
                config_content.push('\n');
            }
            config_content.push_str("\n[cloud]\ncontribute_enabled = true\n");
            let _ = std::fs::write(&config_path, config_content);
        }
        terminal_ui::print_status_ok("Enabled — thank you!");
    } else {
        terminal_ui::print_status_skip("Skipped — enable later with: lean-ctx config");
    }

    // Summary
    println!();
    println!(
        "  \x1b[1;32m✓ Setup complete!\x1b[0m  \x1b[1m{}\x1b[0m configured, \x1b[2m{} already set, {} skipped\x1b[0m",
        newly_configured.len(),
        already_configured.len(),
        not_installed.len()
    );

    if !errors.is_empty() {
        println!(
            "  \x1b[33m⚠ {} error{}: {}\x1b[0m",
            errors.len(),
            if errors.len() != 1 { "s" } else { "" },
            errors.join(", ")
        );
    }

    // Next steps
    let shell = std::env::var("SHELL").unwrap_or_default();
    let source_cmd = if shell.contains("zsh") {
        "source ~/.zshrc"
    } else if shell.contains("fish") {
        "source ~/.config/fish/config.fish"
    } else if shell.contains("bash") {
        "source ~/.bashrc"
    } else {
        "Restart your shell"
    };

    let dim = "\x1b[2m";
    let bold = "\x1b[1m";
    let cyan = "\x1b[36m";
    let yellow = "\x1b[33m";
    let rst = "\x1b[0m";

    println!();
    println!("  {bold}Next steps:{rst}");
    println!();
    println!("  {cyan}1.{rst} Reload your shell:");
    println!("     {bold}{source_cmd}{rst}");
    println!();

    let mut tools_to_restart: Vec<String> =
        newly_configured.iter().map(|s| s.to_string()).collect();
    for name in rules_result
        .injected
        .iter()
        .chain(rules_result.updated.iter())
    {
        if !tools_to_restart.iter().any(|t| t == name) {
            tools_to_restart.push(name.clone());
        }
    }

    if !tools_to_restart.is_empty() {
        println!("  {cyan}2.{rst} {yellow}{bold}Restart your IDE / AI tool:{rst}");
        println!("     {bold}{}{rst}", tools_to_restart.join(", "));
        println!(
            "     {dim}The MCP connection must be re-established for changes to take effect.{rst}"
        );
        println!("     {dim}Close and re-open the application completely.{rst}");
    } else if !already_configured.is_empty() {
        println!(
            "  {cyan}2.{rst} {dim}Your tools are already configured — no restart needed.{rst}"
        );
    }

    println!();
    println!(
        "  {dim}After restart, lean-ctx will automatically optimize every AI interaction.{rst}"
    );
    println!("  {dim}Verify with:{rst} {bold}lean-ctx gain{rst}");

    // Logo + commands
    println!();
    terminal_ui::print_logo_animated();
    terminal_ui::print_command_box();
}

pub fn configure_agent_mcp(agent: &str) -> Result<(), String> {
    let home = dirs::home_dir().ok_or_else(|| "Cannot determine home directory".to_string())?;
    let binary = resolve_portable_binary();

    let mut targets = Vec::<EditorTarget>::new();

    let push = |targets: &mut Vec<EditorTarget>,
                name: &'static str,
                config_path: PathBuf,
                config_type: ConfigType| {
        targets.push(EditorTarget {
            name,
            agent_key: agent,
            detect_path: PathBuf::from("/nonexistent"), // not used in direct agent config
            config_path,
            config_type,
        });
    };

    match agent {
        "cursor" => push(
            &mut targets,
            "Cursor",
            home.join(".cursor/mcp.json"),
            ConfigType::McpJson,
        ),
        "claude" | "claude-code" => push(
            &mut targets,
            "Claude Code",
            home.join(".claude.json"),
            ConfigType::McpJson,
        ),
        "windsurf" => push(
            &mut targets,
            "Windsurf",
            home.join(".codeium/windsurf/mcp_config.json"),
            ConfigType::McpJson,
        ),
        "codex" => push(
            &mut targets,
            "Codex CLI",
            home.join(".codex/config.toml"),
            ConfigType::Codex,
        ),
        "gemini" => {
            push(
                &mut targets,
                "Gemini CLI",
                home.join(".gemini/settings/mcp.json"),
                ConfigType::McpJson,
            );
            push(
                &mut targets,
                "Antigravity",
                home.join(".gemini/antigravity/mcp_config.json"),
                ConfigType::McpJson,
            );
        }
        "copilot" => push(
            &mut targets,
            "VS Code / Copilot",
            vscode_mcp_path(),
            ConfigType::VsCodeMcp,
        ),
        "crush" => push(
            &mut targets,
            "Crush",
            home.join(".config/crush/crush.json"),
            ConfigType::Crush,
        ),
        "pi" => push(
            &mut targets,
            "Pi Coding Agent",
            home.join(".pi/agent/mcp.json"),
            ConfigType::McpJson,
        ),
        "cline" => push(&mut targets, "Cline", cline_mcp_path(), ConfigType::McpJson),
        "roo" => push(
            &mut targets,
            "Roo Code",
            roo_mcp_path(),
            ConfigType::McpJson,
        ),
        "kiro" => push(
            &mut targets,
            "AWS Kiro",
            home.join(".kiro/settings/mcp.json"),
            ConfigType::McpJson,
        ),
        "verdent" => push(
            &mut targets,
            "Verdent",
            home.join(".verdent/mcp.json"),
            ConfigType::McpJson,
        ),
        "jetbrains" => push(
            &mut targets,
            "JetBrains IDEs",
            home.join(".jb-mcp.json"),
            ConfigType::McpJson,
        ),
        _ => {
            return Err(format!("Unknown agent '{agent}'"));
        }
    }

    for t in &targets {
        let _ = write_config(t, &binary)?;
    }

    Ok(())
}

fn shorten_path(path: &str, home: &str) -> String {
    if let Some(stripped) = path.strip_prefix(home) {
        format!("~{stripped}")
    } else {
        path.to_string()
    }
}

fn build_targets(home: &std::path::Path, _binary: &str) -> Vec<EditorTarget> {
    #[cfg(windows)]
    let opencode_cfg = if let Ok(appdata) = std::env::var("APPDATA") {
        std::path::PathBuf::from(appdata)
            .join("opencode")
            .join("opencode.json")
    } else {
        home.join(".config/opencode/opencode.json")
    };
    #[cfg(not(windows))]
    let opencode_cfg = home.join(".config/opencode/opencode.json");

    #[cfg(windows)]
    let opencode_detect = opencode_cfg
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| home.join(".config/opencode"));
    #[cfg(not(windows))]
    let opencode_detect = home.join(".config/opencode");

    vec![
        EditorTarget {
            name: "Cursor",
            agent_key: "cursor",
            config_path: home.join(".cursor/mcp.json"),
            detect_path: home.join(".cursor"),
            config_type: ConfigType::McpJson,
        },
        EditorTarget {
            name: "Claude Code",
            agent_key: "claude",
            config_path: home.join(".claude.json"),
            detect_path: detect_claude_path(),
            config_type: ConfigType::McpJson,
        },
        EditorTarget {
            name: "Windsurf",
            agent_key: "windsurf",
            config_path: home.join(".codeium/windsurf/mcp_config.json"),
            detect_path: home.join(".codeium/windsurf"),
            config_type: ConfigType::McpJson,
        },
        EditorTarget {
            name: "Codex CLI",
            agent_key: "codex",
            config_path: home.join(".codex/config.toml"),
            detect_path: detect_codex_path(home),
            config_type: ConfigType::Codex,
        },
        EditorTarget {
            name: "Gemini CLI",
            agent_key: "gemini",
            config_path: home.join(".gemini/settings/mcp.json"),
            detect_path: home.join(".gemini"),
            config_type: ConfigType::McpJson,
        },
        EditorTarget {
            name: "Antigravity",
            agent_key: "gemini",
            config_path: home.join(".gemini/antigravity/mcp_config.json"),
            detect_path: home.join(".gemini/antigravity"),
            config_type: ConfigType::McpJson,
        },
        EditorTarget {
            name: "Zed",
            agent_key: "",
            config_path: zed_settings_path(home),
            detect_path: zed_config_dir(home),
            config_type: ConfigType::Zed,
        },
        EditorTarget {
            name: "VS Code / Copilot",
            agent_key: "copilot",
            config_path: vscode_mcp_path(),
            detect_path: detect_vscode_path(),
            config_type: ConfigType::VsCodeMcp,
        },
        EditorTarget {
            name: "OpenCode",
            agent_key: "",
            config_path: opencode_cfg,
            detect_path: opencode_detect,
            config_type: ConfigType::OpenCode,
        },
        EditorTarget {
            name: "Qwen Code",
            agent_key: "qwen",
            config_path: home.join(".qwen/mcp.json"),
            detect_path: home.join(".qwen"),
            config_type: ConfigType::McpJson,
        },
        EditorTarget {
            name: "Trae",
            agent_key: "trae",
            config_path: home.join(".trae/mcp.json"),
            detect_path: home.join(".trae"),
            config_type: ConfigType::McpJson,
        },
        EditorTarget {
            name: "Amazon Q Developer",
            agent_key: "amazonq",
            config_path: home.join(".aws/amazonq/mcp.json"),
            detect_path: home.join(".aws/amazonq"),
            config_type: ConfigType::McpJson,
        },
        EditorTarget {
            name: "JetBrains IDEs",
            agent_key: "jetbrains",
            config_path: home.join(".jb-mcp.json"),
            detect_path: detect_jetbrains_path(home),
            config_type: ConfigType::McpJson,
        },
        EditorTarget {
            name: "Cline",
            agent_key: "cline",
            config_path: cline_mcp_path(),
            detect_path: detect_cline_path(),
            config_type: ConfigType::McpJson,
        },
        EditorTarget {
            name: "Roo Code",
            agent_key: "roo",
            config_path: roo_mcp_path(),
            detect_path: detect_roo_path(),
            config_type: ConfigType::McpJson,
        },
        EditorTarget {
            name: "AWS Kiro",
            agent_key: "kiro",
            config_path: home.join(".kiro/settings/mcp.json"),
            detect_path: home.join(".kiro"),
            config_type: ConfigType::McpJson,
        },
        EditorTarget {
            name: "Verdent",
            agent_key: "verdent",
            config_path: home.join(".verdent/mcp.json"),
            detect_path: home.join(".verdent"),
            config_type: ConfigType::McpJson,
        },
        EditorTarget {
            name: "Crush",
            agent_key: "crush",
            config_path: home.join(".config/crush/crush.json"),
            detect_path: home.join(".config/crush"),
            config_type: ConfigType::Crush,
        },
        EditorTarget {
            name: "Pi Coding Agent",
            agent_key: "pi",
            config_path: home.join(".pi/agent/mcp.json"),
            detect_path: home.join(".pi/agent"),
            config_type: ConfigType::McpJson,
        },
    ]
}

fn detect_claude_path() -> PathBuf {
    if let Ok(output) = std::process::Command::new("which").arg("claude").output() {
        if output.status.success() {
            return PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
        }
    }
    if let Some(home) = dirs::home_dir() {
        let claude_json = home.join(".claude.json");
        if claude_json.exists() {
            return claude_json;
        }
    }
    PathBuf::from("/nonexistent")
}

fn detect_codex_path(home: &std::path::Path) -> PathBuf {
    let codex_dir = home.join(".codex");
    if codex_dir.exists() {
        return codex_dir;
    }
    if let Ok(output) = std::process::Command::new("which").arg("codex").output() {
        if output.status.success() {
            return codex_dir;
        }
    }
    PathBuf::from("/nonexistent")
}

fn zed_settings_path(home: &std::path::Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Zed/settings.json")
    } else {
        home.join(".config/zed/settings.json")
    }
}

fn zed_config_dir(home: &std::path::Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Zed")
    } else {
        home.join(".config/zed")
    }
}

fn write_config(target: &EditorTarget, binary: &str) -> Result<(), String> {
    if let Some(parent) = target.config_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    match target.config_type {
        ConfigType::McpJson => write_mcp_json(target, binary),
        ConfigType::Zed => write_zed_config(target, binary),
        ConfigType::Codex => write_codex_config(target, binary),
        ConfigType::VsCodeMcp => write_vscode_mcp(target, binary),
        ConfigType::OpenCode => write_opencode_config(target, binary),
        ConfigType::Crush => write_crush_config(target, binary),
    }
}

fn lean_ctx_server_entry(binary: &str) -> serde_json::Value {
    serde_json::json!({
        "command": binary,
        "autoApprove": [
            "ctx_read", "ctx_shell", "ctx_search", "ctx_tree",
            "ctx_overview", "ctx_compress", "ctx_metrics", "ctx_session",
            "ctx_knowledge", "ctx_agent", "ctx_analyze", "ctx_benchmark",
            "ctx_cache", "ctx_discover", "ctx_smart_read", "ctx_delta",
            "ctx_edit", "ctx_dedup", "ctx_fill", "ctx_intent", "ctx_response",
            "ctx_context", "ctx_graph", "ctx_wrapped", "ctx_multi_read",
            "ctx_semantic_search", "ctx"
        ]
    })
}

fn write_mcp_json(target: &EditorTarget, binary: &str) -> Result<WriteAction, String> {
    let desired = lean_ctx_server_entry(binary);
    if target.config_path.exists() {
        let content = std::fs::read_to_string(&target.config_path).map_err(|e| e.to_string())?;
        let mut json =
            serde_json::from_str::<serde_json::Value>(&content).map_err(|e| e.to_string())?;
        let obj = json
            .as_object_mut()
            .ok_or_else(|| "root JSON must be an object".to_string())?;
        let servers = obj
            .entry("mcpServers")
            .or_insert_with(|| serde_json::json!({}));
        let servers_obj = servers
            .as_object_mut()
            .ok_or_else(|| "\"mcpServers\" must be an object".to_string())?;

        let existing = servers_obj.get("lean-ctx").cloned();
        if existing.as_ref() == Some(&desired) {
            return Ok(WriteAction::Already);
        }
        servers_obj.insert("lean-ctx".to_string(), desired);
        let formatted = serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?;
        crate::config_io::write_atomic_with_backup(&target.config_path, &formatted)?;
        return Ok(if existing.is_some() {
            WriteAction::Updated
        } else {
            WriteAction::Updated
        });
        return Err(format!(
            "Could not parse existing config at {}. Please add lean-ctx manually:\n\
             Add to \"mcpServers\": \"lean-ctx\": {{ \"command\": \"{}\" }}",
            target.config_path.display(),
            binary
        ));
    }

    let content = serde_json::to_string_pretty(&serde_json::json!({
        "mcpServers": {
            "lean-ctx": desired
        }
    }))
    .map_err(|e| e.to_string())?;

    crate::config_io::write_atomic_with_backup(&target.config_path, &content)?;
    Ok(WriteAction::Created)
}

fn write_zed_config(target: &EditorTarget, binary: &str) -> Result<WriteAction, String> {
    let desired = serde_json::json!({
        "source": "custom",
        "command": binary,
        "args": [],
        "env": {}
    });
    if target.config_path.exists() {
        let content = std::fs::read_to_string(&target.config_path).map_err(|e| e.to_string())?;
        let mut json =
            serde_json::from_str::<serde_json::Value>(&content).map_err(|e| e.to_string())?;
        let obj = json
            .as_object_mut()
            .ok_or_else(|| "root JSON must be an object".to_string())?;
        let servers = obj
            .entry("context_servers")
            .or_insert_with(|| serde_json::json!({}));
        let servers_obj = servers
            .as_object_mut()
            .ok_or_else(|| "\"context_servers\" must be an object".to_string())?;

        let existing = servers_obj.get("lean-ctx").cloned();
        if existing.as_ref() == Some(&desired) {
            return Ok(WriteAction::Already);
        }
        servers_obj.insert("lean-ctx".to_string(), desired);
        let formatted = serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?;
        crate::config_io::write_atomic_with_backup(&target.config_path, &formatted)?;
        return Ok(WriteAction::Updated);
        return Err(format!(
            "Could not parse existing config at {}. Please add lean-ctx manually to \"context_servers\".",
            target.config_path.display()
        ));
    }

    let content = serde_json::to_string_pretty(&serde_json::json!({
        "context_servers": {
            "lean-ctx": desired
        }
    }))
    .map_err(|e| e.to_string())?;

    crate::config_io::write_atomic_with_backup(&target.config_path, &content)?;
    Ok(WriteAction::Created)
}

fn write_codex_config(target: &EditorTarget, binary: &str) -> Result<WriteAction, String> {
    if target.config_path.exists() {
        let content = std::fs::read_to_string(&target.config_path).map_err(|e| e.to_string())?;
        let updated = upsert_codex_toml(&content, binary);
        if updated == content {
            return Ok(WriteAction::Already);
        }
        crate::config_io::write_atomic_with_backup(&target.config_path, &updated)?;
        return Ok(WriteAction::Updated);
    }

    let content = format!(
        "[mcp_servers.lean-ctx]\ncommand = \"{}\"\nargs = []\n",
        binary
    );
    crate::config_io::write_atomic_with_backup(&target.config_path, &content)?;
    Ok(WriteAction::Created)
}

fn write_vscode_mcp(target: &EditorTarget, binary: &str) -> Result<WriteAction, String> {
    let desired = serde_json::json!({ "command": binary, "args": [] });
    if target.config_path.exists() {
        let content = std::fs::read_to_string(&target.config_path).map_err(|e| e.to_string())?;
        let mut json =
            serde_json::from_str::<serde_json::Value>(&content).map_err(|e| e.to_string())?;
        let obj = json
            .as_object_mut()
            .ok_or_else(|| "root JSON must be an object".to_string())?;
        let servers = obj
            .entry("servers")
            .or_insert_with(|| serde_json::json!({}));
        let servers_obj = servers
            .as_object_mut()
            .ok_or_else(|| "\"servers\" must be an object".to_string())?;

        let existing = servers_obj.get("lean-ctx").cloned();
        if existing.as_ref() == Some(&desired) {
            return Ok(WriteAction::Already);
        }
        servers_obj.insert("lean-ctx".to_string(), desired);
        let formatted = serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?;
        crate::config_io::write_atomic_with_backup(&target.config_path, &formatted)?;
        return Ok(WriteAction::Updated);
        return Err(format!(
            "Could not parse existing config at {}. Please add lean-ctx manually to \"servers\".",
            target.config_path.display()
        ));
    }

    if let Some(parent) = target.config_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let content = serde_json::to_string_pretty(&serde_json::json!({
        "servers": {
            "lean-ctx": {
                "command": binary,
                "args": []
            }
        }
    }))
    .map_err(|e| e.to_string())?;

    crate::config_io::write_atomic_with_backup(&target.config_path, &content)?;
    Ok(WriteAction::Created)
}

fn write_opencode_config(target: &EditorTarget, binary: &str) -> Result<WriteAction, String> {
    let desired = serde_json::json!({
        "type": "local",
        "command": [binary],
        "enabled": true
    });
    if target.config_path.exists() {
        let content = std::fs::read_to_string(&target.config_path).map_err(|e| e.to_string())?;
        let mut json =
            serde_json::from_str::<serde_json::Value>(&content).map_err(|e| e.to_string())?;
        let obj = json
            .as_object_mut()
            .ok_or_else(|| "root JSON must be an object".to_string())?;
        let mcp = obj.entry("mcp").or_insert_with(|| serde_json::json!({}));
        let mcp_obj = mcp
            .as_object_mut()
            .ok_or_else(|| "\"mcp\" must be an object".to_string())?;
        let existing = mcp_obj.get("lean-ctx").cloned();
        if existing.as_ref() == Some(&desired) {
            return Ok(WriteAction::Already);
        }
        mcp_obj.insert("lean-ctx".to_string(), desired);
        let formatted = serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?;
        crate::config_io::write_atomic_with_backup(&target.config_path, &formatted)?;
        return Ok(WriteAction::Updated);
        return Err(format!(
            "Could not parse existing config at {}. Please add lean-ctx manually:\n\
             Add to the \"mcp\" section: \"lean-ctx\": {{ \"type\": \"local\", \"command\": [\"{}\"], \"enabled\": true }}",
            target.config_path.display(),
            binary
        ));
    }

    if let Some(parent) = target.config_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let content = serde_json::to_string_pretty(&serde_json::json!({
        "$schema": "https://opencode.ai/config.json",
        "mcp": {
            "lean-ctx": {
                "type": "local",
                "command": [binary],
                "enabled": true
            }
        }
    }))
    .map_err(|e| e.to_string())?;

    crate::config_io::write_atomic_with_backup(&target.config_path, &content)?;
    Ok(WriteAction::Created)
}

fn write_crush_config(target: &EditorTarget, binary: &str) -> Result<WriteAction, String> {
    let desired = serde_json::json!({ "type": "stdio", "command": binary });
    if target.config_path.exists() {
        let content = std::fs::read_to_string(&target.config_path).map_err(|e| e.to_string())?;
        let mut json =
            serde_json::from_str::<serde_json::Value>(&content).map_err(|e| e.to_string())?;
        let obj = json
            .as_object_mut()
            .ok_or_else(|| "root JSON must be an object".to_string())?;
        let mcp = obj.entry("mcp").or_insert_with(|| serde_json::json!({}));
        let mcp_obj = mcp
            .as_object_mut()
            .ok_or_else(|| "\"mcp\" must be an object".to_string())?;

        let existing = mcp_obj.get("lean-ctx").cloned();
        if existing.as_ref() == Some(&desired) {
            return Ok(WriteAction::Already);
        }
        mcp_obj.insert("lean-ctx".to_string(), desired);
        let formatted = serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?;
        crate::config_io::write_atomic_with_backup(&target.config_path, &formatted)?;
        return Ok(WriteAction::Updated);
    }

    let content = serde_json::to_string_pretty(&serde_json::json!({
        "mcp": { "lean-ctx": desired }
    }))
    .map_err(|e| e.to_string())?;

    crate::config_io::write_atomic_with_backup(&target.config_path, &content)?;
    Ok(WriteAction::Created)
}

fn upsert_codex_toml(existing: &str, binary: &str) -> String {
    let mut out = String::with_capacity(existing.len() + 128);
    let mut in_section = false;
    let mut saw_section = false;
    let mut wrote_command = false;
    let mut wrote_args = false;

    for line in existing.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            if in_section && !wrote_command {
                out.push_str(&format!("command = \"{}\"\n", binary));
                wrote_command = true;
            }
            if in_section && !wrote_args {
                out.push_str("args = []\n");
                wrote_args = true;
            }
            in_section = trimmed == "[mcp_servers.lean-ctx]";
            if in_section {
                saw_section = true;
            }
            out.push_str(line);
            out.push('\n');
            continue;
        }

        if in_section {
            if trimmed.starts_with("command") && trimmed.contains('=') {
                out.push_str(&format!("command = \"{}\"\n", binary));
                wrote_command = true;
                continue;
            }
            if trimmed.starts_with("args") && trimmed.contains('=') {
                out.push_str("args = []\n");
                wrote_args = true;
                continue;
            }
        }

        out.push_str(line);
        out.push('\n');
    }

    if saw_section {
        if in_section && !wrote_command {
            out.push_str(&format!("command = \"{}\"\n", binary));
        }
        if in_section && !wrote_args {
            out.push_str("args = []\n");
        }
        return out;
    }

    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("\n[mcp_servers.lean-ctx]\n");
    out.push_str(&format!("command = \"{}\"\n", binary));
    out.push_str("args = []\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(path: PathBuf, ty: ConfigType) -> EditorTarget {
        EditorTarget {
            name: "test",
            agent_key: "test",
            config_path: path,
            detect_path: PathBuf::from("/nonexistent"),
            config_type: ty,
        }
    }

    #[test]
    fn mcp_json_upserts_and_preserves_other_servers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        std::fs::write(
            &path,
            r#"{ "mcpServers": { "other": { "command": "other-bin" }, "lean-ctx": { "command": "/old/path/lean-ctx", "autoApprove": [] } } }"#,
        )
        .unwrap();

        let t = target(path.clone(), ConfigType::McpJson);
        let action = write_mcp_json(&t, "/new/path/lean-ctx").unwrap();
        assert_eq!(action, WriteAction::Updated);

        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(json["mcpServers"]["other"]["command"], "other-bin");
        assert_eq!(
            json["mcpServers"]["lean-ctx"]["command"],
            "/new/path/lean-ctx"
        );
        assert!(json["mcpServers"]["lean-ctx"]["autoApprove"].is_array());
        assert!(
            json["mcpServers"]["lean-ctx"]["autoApprove"]
                .as_array()
                .unwrap()
                .len()
                > 5
        );
    }

    #[test]
    fn crush_config_writes_mcp_root() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("crush.json");
        std::fs::write(
            &path,
            r#"{ "mcp": { "lean-ctx": { "type": "stdio", "command": "old" } } }"#,
        )
        .unwrap();

        let t = target(path.clone(), ConfigType::Crush);
        let action = write_crush_config(&t, "new").unwrap();
        assert_eq!(action, WriteAction::Updated);

        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(json["mcp"]["lean-ctx"]["type"], "stdio");
        assert_eq!(json["mcp"]["lean-ctx"]["command"], "new");
    }

    #[test]
    fn codex_toml_upserts_existing_section() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"[mcp_servers.lean-ctx]
command = "old"
args = ["x"]
"#,
        )
        .unwrap();

        let t = target(path.clone(), ConfigType::Codex);
        let action = write_codex_config(&t, "new").unwrap();
        assert_eq!(action, WriteAction::Updated);

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains(r#"command = "new""#));
        assert!(content.contains("args = []"));
    }
}

fn detect_vscode_path() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = dirs::home_dir() {
            let vscode = home.join("Library/Application Support/Code/User/settings.json");
            if vscode.exists() {
                return vscode;
            }
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Some(home) = dirs::home_dir() {
            let vscode = home.join(".config/Code/User/settings.json");
            if vscode.exists() {
                return vscode;
            }
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            let vscode = PathBuf::from(appdata).join("Code/User/settings.json");
            if vscode.exists() {
                return vscode;
            }
        }
    }
    if let Ok(output) = std::process::Command::new("which").arg("code").output() {
        if output.status.success() {
            return PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
        }
    }
    PathBuf::from("/nonexistent")
}

fn vscode_mcp_path() -> PathBuf {
    if let Some(home) = dirs::home_dir() {
        #[cfg(target_os = "macos")]
        {
            return home.join("Library/Application Support/Code/User/mcp.json");
        }
        #[cfg(target_os = "linux")]
        {
            return home.join(".config/Code/User/mcp.json");
        }
        #[cfg(target_os = "windows")]
        {
            if let Ok(appdata) = std::env::var("APPDATA") {
                return PathBuf::from(appdata).join("Code/User/mcp.json");
            }
        }
        #[allow(unreachable_code)]
        home.join(".config/Code/User/mcp.json")
    } else {
        PathBuf::from("/nonexistent")
    }
}

fn detect_jetbrains_path(home: &std::path::Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        let lib = home.join("Library/Application Support/JetBrains");
        if lib.exists() {
            return lib;
        }
    }
    #[cfg(target_os = "linux")]
    {
        let cfg = home.join(".config/JetBrains");
        if cfg.exists() {
            return cfg;
        }
    }
    if home.join(".jb-mcp.json").exists() {
        return home.join(".jb-mcp.json");
    }
    PathBuf::from("/nonexistent")
}

fn cline_mcp_path() -> PathBuf {
    if let Some(home) = dirs::home_dir() {
        #[cfg(target_os = "macos")]
        {
            return home.join("Library/Application Support/Code/User/globalStorage/saoudrizwan.claude-dev/settings/cline_mcp_settings.json");
        }
        #[cfg(target_os = "linux")]
        {
            return home.join(".config/Code/User/globalStorage/saoudrizwan.claude-dev/settings/cline_mcp_settings.json");
        }
        #[cfg(target_os = "windows")]
        {
            if let Ok(appdata) = std::env::var("APPDATA") {
                return PathBuf::from(appdata).join("Code/User/globalStorage/saoudrizwan.claude-dev/settings/cline_mcp_settings.json");
            }
        }
    }
    PathBuf::from("/nonexistent")
}

fn detect_cline_path() -> PathBuf {
    if let Some(home) = dirs::home_dir() {
        #[cfg(target_os = "macos")]
        {
            let p = home
                .join("Library/Application Support/Code/User/globalStorage/saoudrizwan.claude-dev");
            if p.exists() {
                return p;
            }
        }
        #[cfg(target_os = "linux")]
        {
            let p = home.join(".config/Code/User/globalStorage/saoudrizwan.claude-dev");
            if p.exists() {
                return p;
            }
        }
    }
    PathBuf::from("/nonexistent")
}

fn roo_mcp_path() -> PathBuf {
    if let Some(home) = dirs::home_dir() {
        #[cfg(target_os = "macos")]
        {
            return home.join("Library/Application Support/Code/User/globalStorage/rooveterinaryinc.roo-cline/settings/cline_mcp_settings.json");
        }
        #[cfg(target_os = "linux")]
        {
            return home.join(".config/Code/User/globalStorage/rooveterinaryinc.roo-cline/settings/cline_mcp_settings.json");
        }
        #[cfg(target_os = "windows")]
        {
            if let Ok(appdata) = std::env::var("APPDATA") {
                return PathBuf::from(appdata).join("Code/User/globalStorage/rooveterinaryinc.roo-cline/settings/cline_mcp_settings.json");
            }
        }
    }
    PathBuf::from("/nonexistent")
}

fn detect_roo_path() -> PathBuf {
    if let Some(home) = dirs::home_dir() {
        #[cfg(target_os = "macos")]
        {
            let p = home.join(
                "Library/Application Support/Code/User/globalStorage/rooveterinaryinc.roo-cline",
            );
            if p.exists() {
                return p;
            }
        }
        #[cfg(target_os = "linux")]
        {
            let p = home.join(".config/Code/User/globalStorage/rooveterinaryinc.roo-cline");
            if p.exists() {
                return p;
            }
        }
    }
    PathBuf::from("/nonexistent")
}

fn resolve_portable_binary() -> String {
    let which_cmd = if cfg!(windows) { "where" } else { "which" };
    if let Ok(status) = std::process::Command::new(which_cmd)
        .arg("lean-ctx")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
    {
        if status.success() {
            return "lean-ctx".to_string();
        }
    }
    std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "lean-ctx".to_string())
}
