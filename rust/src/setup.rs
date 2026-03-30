use std::path::PathBuf;

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

    let binary = std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "lean-ctx".to_string());

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

        let has_config = target.config_path.exists()
            && std::fs::read_to_string(&target.config_path)
                .map(|c| c.contains("lean-ctx"))
                .unwrap_or(false);

        if has_config {
            terminal_ui::print_status_ok(&format!(
                "{:<20} \x1b[2m{short_path}\x1b[0m",
                target.name
            ));
            already_configured.push(target.name);
            continue;
        }

        match write_config(target, &binary) {
            Ok(()) => {
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

    // Step 3: Agent hooks
    terminal_ui::print_step_header(3, 5, "Agent Instructions");
    let mut agents_installed = 0;
    for target in &targets {
        if !target.detect_path.exists() || target.agent_key.is_empty() {
            continue;
        }
        crate::hooks::install_agent_hook(target.agent_key, true);
        agents_installed += 1;
    }
    if agents_installed == 0 {
        terminal_ui::print_status_skip("No agent instructions needed");
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
    crate::doctor::run();

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
    println!();
    println!("  \x1b[1mNext:\x1b[0m  {source_cmd}");
    if !newly_configured.is_empty() {
        println!(
            "         Restart {} to load MCP tools",
            newly_configured.join(", ")
        );
    }

    // Logo + commands
    println!();
    terminal_ui::print_logo_animated();
    terminal_ui::print_command_box();
}

fn shorten_path(path: &str, home: &str) -> String {
    if let Some(stripped) = path.strip_prefix(home) {
        format!("~{stripped}")
    } else {
        path.to_string()
    }
}

fn build_targets(home: &std::path::Path, _binary: &str) -> Vec<EditorTarget> {
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
            detect_path: home.join(".codex"),
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
            config_path: home.join(".config/opencode/opencode.json"),
            detect_path: home.join(".config/opencode"),
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

fn zed_settings_path(home: &std::path::Path) -> PathBuf {
    home.join(".config/zed/settings.json")
}

fn zed_config_dir(home: &std::path::Path) -> PathBuf {
    home.join(".config/zed")
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
    }
}

fn write_mcp_json(target: &EditorTarget, binary: &str) -> Result<(), String> {
    if target.config_path.exists() {
        let content = std::fs::read_to_string(&target.config_path).map_err(|e| e.to_string())?;

        if content.contains("lean-ctx") {
            return Ok(());
        }

        if let Ok(mut json) = serde_json::from_str::<serde_json::Value>(&content) {
            if let Some(obj) = json.as_object_mut() {
                let servers = obj
                    .entry("mcpServers")
                    .or_insert_with(|| serde_json::json!({}));
                if let Some(servers_obj) = servers.as_object_mut() {
                    servers_obj.insert(
                        "lean-ctx".to_string(),
                        serde_json::json!({ "command": binary }),
                    );
                }
                let formatted = serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?;
                std::fs::write(&target.config_path, formatted).map_err(|e| e.to_string())?;
                return Ok(());
            }
        }
        return Err(format!(
            "Could not parse existing config at {}. Please add lean-ctx manually:\n\
             Add to \"mcpServers\": \"lean-ctx\": {{ \"command\": \"{}\" }}",
            target.config_path.display(),
            binary
        ));
    }

    let content = serde_json::to_string_pretty(&serde_json::json!({
        "mcpServers": {
            "lean-ctx": {
                "command": binary
            }
        }
    }))
    .map_err(|e| e.to_string())?;

    std::fs::write(&target.config_path, content).map_err(|e| e.to_string())
}

fn write_zed_config(target: &EditorTarget, binary: &str) -> Result<(), String> {
    if target.config_path.exists() {
        let content = std::fs::read_to_string(&target.config_path).map_err(|e| e.to_string())?;

        if content.contains("lean-ctx") {
            return Ok(());
        }

        if let Ok(mut json) = serde_json::from_str::<serde_json::Value>(&content) {
            if let Some(obj) = json.as_object_mut() {
                let servers = obj
                    .entry("context_servers")
                    .or_insert_with(|| serde_json::json!({}));
                if let Some(servers_obj) = servers.as_object_mut() {
                    servers_obj.insert(
                        "lean-ctx".to_string(),
                        serde_json::json!({
                            "source": "custom",
                            "command": binary,
                            "args": [],
                            "env": {}
                        }),
                    );
                }
                let formatted = serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?;
                std::fs::write(&target.config_path, formatted).map_err(|e| e.to_string())?;
                return Ok(());
            }
        }
        return Err(format!(
            "Could not parse existing config at {}. Please add lean-ctx manually to \"context_servers\".",
            target.config_path.display()
        ));
    }

    let content = serde_json::to_string_pretty(&serde_json::json!({
        "context_servers": {
            "lean-ctx": {
                "source": "custom",
                "command": binary,
                "args": [],
                "env": {}
            }
        }
    }))
    .map_err(|e| e.to_string())?;

    std::fs::write(&target.config_path, content).map_err(|e| e.to_string())
}

fn write_codex_config(target: &EditorTarget, binary: &str) -> Result<(), String> {
    if target.config_path.exists() {
        let content = std::fs::read_to_string(&target.config_path).map_err(|e| e.to_string())?;

        if content.contains("lean-ctx") {
            return Ok(());
        }

        let mut new_content = content.clone();
        if !new_content.ends_with('\n') {
            new_content.push('\n');
        }
        new_content.push_str(&format!(
            "\n[mcp_servers.lean-ctx]\ncommand = \"{}\"\nargs = []\n",
            binary
        ));
        std::fs::write(&target.config_path, new_content).map_err(|e| e.to_string())?;
        return Ok(());
    }

    let content = format!(
        "[mcp_servers.lean-ctx]\ncommand = \"{}\"\nargs = []\n",
        binary
    );
    std::fs::write(&target.config_path, content).map_err(|e| e.to_string())
}

fn write_vscode_mcp(target: &EditorTarget, binary: &str) -> Result<(), String> {
    if target.config_path.exists() {
        let content = std::fs::read_to_string(&target.config_path).map_err(|e| e.to_string())?;
        if content.contains("lean-ctx") {
            return Ok(());
        }
        if let Ok(mut json) = serde_json::from_str::<serde_json::Value>(&content) {
            if let Some(obj) = json.as_object_mut() {
                let servers = obj
                    .entry("servers")
                    .or_insert_with(|| serde_json::json!({}));
                if let Some(servers_obj) = servers.as_object_mut() {
                    servers_obj.insert(
                        "lean-ctx".to_string(),
                        serde_json::json!({ "command": binary, "args": [] }),
                    );
                }
                let formatted = serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?;
                std::fs::write(&target.config_path, formatted).map_err(|e| e.to_string())?;
                return Ok(());
            }
        }
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

    std::fs::write(&target.config_path, content).map_err(|e| e.to_string())
}

fn write_opencode_config(target: &EditorTarget, binary: &str) -> Result<(), String> {
    if target.config_path.exists() {
        let content = std::fs::read_to_string(&target.config_path).map_err(|e| e.to_string())?;
        if content.contains("lean-ctx") {
            return Ok(());
        }
        if let Ok(mut json) = serde_json::from_str::<serde_json::Value>(&content) {
            if let Some(obj) = json.as_object_mut() {
                let mcp = obj.entry("mcp").or_insert_with(|| serde_json::json!({}));
                if let Some(mcp_obj) = mcp.as_object_mut() {
                    mcp_obj.insert(
                        "lean-ctx".to_string(),
                        serde_json::json!({
                            "type": "local",
                            "command": [binary],
                            "enabled": true
                        }),
                    );
                }
                let formatted = serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?;
                std::fs::write(&target.config_path, formatted).map_err(|e| e.to_string())?;
                return Ok(());
            }
        }
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

    std::fs::write(&target.config_path, content).map_err(|e| e.to_string())
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
