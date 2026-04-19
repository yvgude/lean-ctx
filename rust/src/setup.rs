use std::path::PathBuf;

use crate::core::editor_registry::{ConfigType, EditorTarget, WriteAction, WriteOptions};
use crate::core::portable_binary::resolve_portable_binary;
use crate::core::setup_report::{PlatformInfo, SetupItem, SetupReport, SetupStepReport};
use chrono::Utc;

pub fn claude_config_json_path(home: &std::path::Path) -> PathBuf {
    crate::core::editor_registry::claude_mcp_json_path(home)
}

pub fn claude_config_dir(home: &std::path::Path) -> PathBuf {
    crate::core::editor_registry::claude_state_dir(home)
}

pub fn run_setup() {
    use crate::terminal_ui;

    if crate::shell::is_non_interactive() {
        eprintln!("Non-interactive terminal detected (no TTY on stdin).");
        eprintln!("Running in non-interactive mode (equivalent to: lean-ctx setup --non-interactive --yes)");
        eprintln!();
        let opts = SetupOptions {
            non_interactive: true,
            yes: true,
            fix: false,
            json: false,
        };
        match run_setup_with_options(opts) {
            Ok(report) => {
                if !report.warnings.is_empty() {
                    for w in &report.warnings {
                        eprintln!("  warning: {w}");
                    }
                }
            }
            Err(e) => eprintln!("Setup error: {e}"),
        }
        return;
    }

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

    let targets = crate::core::editor_registry::build_targets(&home);
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

        match crate::core::editor_registry::write_config_with_options(
            target,
            &binary,
            WriteOptions {
                overwrite_invalid: false,
            },
        ) {
            Ok(res) if res.action == WriteAction::Already => {
                terminal_ui::print_status_ok(&format!(
                    "{:<20} \x1b[2m{short_path}\x1b[0m",
                    target.name
                ));
                already_configured.push(target.name);
            }
            Ok(_) => {
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
        crate::hooks::install_agent_hook(&target.agent_key, true);
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

#[derive(Debug, Clone, Copy, Default)]
pub struct SetupOptions {
    pub non_interactive: bool,
    pub yes: bool,
    pub fix: bool,
    pub json: bool,
}

pub fn run_setup_with_options(opts: SetupOptions) -> Result<SetupReport, String> {
    let started_at = Utc::now();
    let home = dirs::home_dir().ok_or_else(|| "Cannot determine home directory".to_string())?;
    let binary = resolve_portable_binary();
    let home_str = home.to_string_lossy().to_string();

    let mut steps: Vec<SetupStepReport> = Vec::new();

    // Step: Shell Hook
    let mut shell_step = SetupStepReport {
        name: "shell_hook".to_string(),
        ok: true,
        items: Vec::new(),
        warnings: Vec::new(),
        errors: Vec::new(),
    };
    if !opts.non_interactive || opts.yes {
        if opts.json {
            crate::cli::cmd_init_quiet(&["--global".to_string()]);
        } else {
            crate::cli::cmd_init(&["--global".to_string()]);
        }
        shell_step.items.push(SetupItem {
            name: "init --global".to_string(),
            status: "ran".to_string(),
            path: None,
            note: None,
        });
    } else {
        shell_step
            .warnings
            .push("non_interactive_without_yes: shell hook not installed (use --yes)".to_string());
        shell_step.ok = false;
        shell_step.items.push(SetupItem {
            name: "init --global".to_string(),
            status: "skipped".to_string(),
            path: None,
            note: Some("requires --yes in --non-interactive mode".to_string()),
        });
    }
    steps.push(shell_step);

    // Step: Editor MCP config
    let mut editor_step = SetupStepReport {
        name: "editors".to_string(),
        ok: true,
        items: Vec::new(),
        warnings: Vec::new(),
        errors: Vec::new(),
    };

    let targets = crate::core::editor_registry::build_targets(&home);
    for target in &targets {
        let short_path = shorten_path(&target.config_path.to_string_lossy(), &home_str);
        if !target.detect_path.exists() {
            editor_step.items.push(SetupItem {
                name: target.name.to_string(),
                status: "not_detected".to_string(),
                path: Some(short_path),
                note: None,
            });
            continue;
        }

        let res = crate::core::editor_registry::write_config_with_options(
            target,
            &binary,
            WriteOptions {
                overwrite_invalid: opts.fix,
            },
        );
        match res {
            Ok(w) => {
                editor_step.items.push(SetupItem {
                    name: target.name.to_string(),
                    status: match w.action {
                        WriteAction::Created => "created".to_string(),
                        WriteAction::Updated => "updated".to_string(),
                        WriteAction::Already => "already".to_string(),
                    },
                    path: Some(short_path),
                    note: w.note,
                });
            }
            Err(e) => {
                editor_step.ok = false;
                editor_step.items.push(SetupItem {
                    name: target.name.to_string(),
                    status: "error".to_string(),
                    path: Some(short_path),
                    note: Some(e),
                });
            }
        }
    }
    steps.push(editor_step);

    // Step: Agent rules
    let mut rules_step = SetupStepReport {
        name: "agent_rules".to_string(),
        ok: true,
        items: Vec::new(),
        warnings: Vec::new(),
        errors: Vec::new(),
    };
    let rules_result = crate::rules_inject::inject_all_rules(&home);
    for n in rules_result.injected {
        rules_step.items.push(SetupItem {
            name: n,
            status: "injected".to_string(),
            path: None,
            note: None,
        });
    }
    for n in rules_result.updated {
        rules_step.items.push(SetupItem {
            name: n,
            status: "updated".to_string(),
            path: None,
            note: None,
        });
    }
    for n in rules_result.already {
        rules_step.items.push(SetupItem {
            name: n,
            status: "already".to_string(),
            path: None,
            note: None,
        });
    }
    for e in rules_result.errors {
        rules_step.ok = false;
        rules_step.errors.push(e);
    }
    steps.push(rules_step);

    // Step: Environment / doctor (compact)
    let mut env_step = SetupStepReport {
        name: "doctor_compact".to_string(),
        ok: true,
        items: Vec::new(),
        warnings: Vec::new(),
        errors: Vec::new(),
    };
    let (passed, total) = crate::doctor::compact_score();
    env_step.items.push(SetupItem {
        name: "doctor".to_string(),
        status: format!("{passed}/{total}"),
        path: None,
        note: None,
    });
    if passed != total {
        env_step.warnings.push(format!(
            "doctor compact not fully passing: {passed}/{total}"
        ));
    }
    steps.push(env_step);

    let finished_at = Utc::now();
    let success = steps.iter().all(|s| s.ok);
    let report = SetupReport {
        schema_version: 1,
        started_at,
        finished_at,
        success,
        platform: PlatformInfo {
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
        },
        steps,
        warnings: Vec::new(),
        errors: Vec::new(),
    };

    let path = SetupReport::default_path()?;
    let mut content =
        serde_json::to_string_pretty(&report).map_err(|e| format!("serialize report: {e}"))?;
    content.push('\n');
    crate::config_io::write_atomic(&path, &content)?;

    Ok(report)
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
            agent_key: agent.to_string(),
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
            crate::core::editor_registry::claude_mcp_json_path(&home),
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
        "antigravity" => push(
            &mut targets,
            "Antigravity",
            home.join(".gemini/antigravity/mcp_config.json"),
            ConfigType::McpJson,
        ),
        "copilot" => push(
            &mut targets,
            "VS Code / Copilot",
            crate::core::editor_registry::vscode_mcp_path(),
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
        "cline" => push(
            &mut targets,
            "Cline",
            crate::core::editor_registry::cline_mcp_path(),
            ConfigType::McpJson,
        ),
        "roo" => push(
            &mut targets,
            "Roo Code",
            crate::core::editor_registry::roo_mcp_path(),
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
        crate::core::editor_registry::write_config_with_options(
            t,
            &binary,
            WriteOptions {
                overwrite_invalid: true,
            },
        )?;
    }

    if agent == "kiro" {
        install_kiro_steering(&home);
    }

    Ok(())
}

fn install_kiro_steering(home: &std::path::Path) {
    let cwd = std::env::current_dir().unwrap_or_else(|_| home.to_path_buf());
    let steering_dir = cwd.join(".kiro").join("steering");
    let steering_file = steering_dir.join("lean-ctx.md");

    if steering_file.exists()
        && std::fs::read_to_string(&steering_file)
            .unwrap_or_default()
            .contains("lean-ctx")
    {
        println!("  Kiro steering file already exists at .kiro/steering/lean-ctx.md");
        return;
    }

    let _ = std::fs::create_dir_all(&steering_dir);
    let _ = std::fs::write(&steering_file, crate::hooks::KIRO_STEERING_TEMPLATE);
    println!("  \x1b[32m✓\x1b[0m Created .kiro/steering/lean-ctx.md (Kiro will now prefer lean-ctx tools)");
}

fn shorten_path(path: &str, home: &str) -> String {
    if let Some(stripped) = path.strip_prefix(home) {
        format!("~{stripped}")
    } else {
        path.to_string()
    }
}
