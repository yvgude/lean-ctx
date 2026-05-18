use std::path::PathBuf;

use crate::core::editor_registry::{ConfigType, EditorTarget, WriteAction, WriteOptions};
use crate::core::portable_binary::resolve_portable_binary;
use crate::core::setup_report::{PlatformInfo, SetupItem, SetupReport, SetupStepReport};
use crate::hooks::{recommend_hook_mode, HookMode};
use chrono::Utc;
use std::ffi::OsString;

pub fn claude_config_json_path(home: &std::path::Path) -> PathBuf {
    crate::core::editor_registry::claude_mcp_json_path(home)
}

pub fn claude_config_dir(home: &std::path::Path) -> PathBuf {
    crate::core::editor_registry::claude_state_dir(home)
}

pub(crate) struct EnvVarGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvVarGuard {
    pub(crate) fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(previous) = &self.previous {
            std::env::set_var(self.key, previous);
        } else {
            std::env::remove_var(self.key);
        }
    }
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
            ..Default::default()
        };
        match run_setup_with_options(opts) {
            Ok(report) => {
                if !report.warnings.is_empty() {
                    for w in &report.warnings {
                        tracing::warn!("{w}");
                    }
                }
            }
            Err(e) => tracing::error!("Setup error: {e}"),
        }
        return;
    }

    let Some(home) = dirs::home_dir() else {
        tracing::error!("Cannot determine home directory");
        std::process::exit(1);
    };

    let binary = resolve_portable_binary();

    let home_str = home.to_string_lossy().to_string();

    terminal_ui::print_setup_header();

    // Step 1: Shell hook (legacy aliases + universal shell hook)
    terminal_ui::print_step_header(1, 11, "Shell Hook");
    crate::cli::cmd_init(&["--global".to_string()]);
    crate::shell_hook::install_all(false);

    // Step 2: Daemon (optional acceleration for CLI routing)
    terminal_ui::print_step_header(2, 11, "Daemon");
    if crate::daemon::is_daemon_running() {
        terminal_ui::print_status_ok("Daemon running — restarting with current binary…");
        let _ = crate::daemon::stop_daemon();
        std::thread::sleep(std::time::Duration::from_millis(500));
        if let Err(e) = crate::daemon::start_daemon(&[]) {
            terminal_ui::print_status_warn(&format!("Daemon restart failed: {e}"));
        }
    } else if let Err(e) = crate::daemon::start_daemon(&[]) {
        terminal_ui::print_status_warn(&format!("Daemon start failed: {e}"));
    }

    // Step 3: Editor auto-detection + configuration
    terminal_ui::print_step_header(3, 11, "AI Tool Detection");

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

        let mode = if target.agent_key.is_empty() {
            HookMode::Mcp
        } else {
            recommend_hook_mode(&target.agent_key)
        };

        match crate::core::editor_registry::write_config_with_options(
            target,
            &binary,
            WriteOptions {
                overwrite_invalid: false,
            },
        ) {
            Ok(res) if res.action == WriteAction::Already => {
                terminal_ui::print_status_ok(&format!(
                    "{:<20} \x1b[36m{mode}\x1b[0m  \x1b[2m{short_path}\x1b[0m",
                    target.name
                ));
                already_configured.push(target.name);
            }
            Ok(_) => {
                terminal_ui::print_status_new(&format!(
                    "{:<20} \x1b[36m{mode}\x1b[0m  \x1b[2m{short_path}\x1b[0m",
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

    // Step 4: Agent rules injection
    terminal_ui::print_step_header(4, 11, "Agent Rules");
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

    // Agent hooks (mode-aware)
    for target in &targets {
        if !target.detect_path.exists() || target.agent_key.is_empty() {
            continue;
        }
        let mode = recommend_hook_mode(&target.agent_key);
        crate::hooks::install_agent_hook_with_mode(&target.agent_key, true, mode);
    }

    // Step 5: API Proxy (opt-in)
    terminal_ui::print_step_header(5, 11, "API Proxy (optional)");
    {
        let mut cfg = crate::core::config::Config::load();
        let proxy_port = crate::proxy_setup::default_port();

        match cfg.proxy_enabled {
            Some(true) => {
                crate::proxy_autostart::install(proxy_port, false);
                std::thread::sleep(std::time::Duration::from_millis(500));
                crate::proxy_setup::install_proxy_env(&home, proxy_port, false);
                terminal_ui::print_status_ok("Proxy active (opted in)");
            }
            Some(false) => {
                terminal_ui::print_status_skip(
                    "Proxy disabled (run `lean-ctx proxy enable` to change)",
                );
            }
            None => {
                println!(
                    "  \x1b[2mThe API proxy routes LLM requests through lean-ctx for additional\x1b[0m"
                );
                println!(
                    "  \x1b[2mtool-result compression and precise token analytics in the dashboard.\x1b[0m"
                );
                println!();
                println!(
                    "  \x1b[2mWithout it: MCP tools, shell hooks, gain tracking, and memory\x1b[0m"
                );
                println!(
                    "  \x1b[2mall work normally. The proxy adds ~5-15% extra savings on top.\x1b[0m"
                );
                println!();
                print!("  Enable the API proxy? [y/N] ");
                let _ = std::io::Write::flush(&mut std::io::stdout());
                let mut input = String::new();
                let _ = std::io::stdin().read_line(&mut input);
                let answer = matches!(input.trim().to_lowercase().as_str(), "y" | "yes");
                cfg.proxy_enabled = Some(answer);
                let _ = cfg.save();
                if answer {
                    crate::proxy_autostart::install(proxy_port, false);
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    crate::proxy_setup::install_proxy_env(&home, proxy_port, false);
                    terminal_ui::print_status_new("Proxy enabled");
                } else {
                    terminal_ui::print_status_skip(
                        "Proxy skipped (run `lean-ctx proxy enable` anytime)",
                    );
                }
            }
        }
    }

    // Step 6: SKILL.md installation
    terminal_ui::print_step_header(6, 11, "Skill Files");
    let skill_result = install_skill_files(&home);
    for (name, installed) in &skill_result {
        if *installed {
            terminal_ui::print_status_new(&format!("{name:<20} \x1b[2mSKILL.md installed\x1b[0m"));
        } else {
            terminal_ui::print_status_ok(&format!("{name:<20} \x1b[2mSKILL.md up-to-date\x1b[0m"));
        }
    }
    if skill_result.is_empty() {
        terminal_ui::print_status_skip("No skill directories to install");
    }

    // Step 7: Data directory + diagnostics
    terminal_ui::print_step_header(7, 11, "Environment Check");
    let lean_dir = crate::core::data_dir::lean_ctx_data_dir()
        .unwrap_or_else(|_| home.join(".config/lean-ctx"));
    if lean_dir.exists() {
        terminal_ui::print_status_ok(&format!("{} ready", lean_dir.display()));
    } else {
        let _ = std::fs::create_dir_all(&lean_dir);
        terminal_ui::print_status_new(&format!("Created {}", lean_dir.display()));
    }
    if let Some(tokens) = crate::core::data_dir::migrate_if_split() {
        terminal_ui::print_status_new(&format!(
            "Migrated stats from split data dir ({tokens} tokens recovered)"
        ));
    }
    crate::doctor::run_compact();

    // Step 8: Data sharing
    terminal_ui::print_step_header(8, 11, "Help Improve lean-ctx");
    println!("  Share anonymous compression stats to make lean-ctx better.");
    println!("  \x1b[1mNo code, no file names, no personal data — ever.\x1b[0m");
    println!();
    print!("  Enable anonymous data sharing? \x1b[1m[y/N]\x1b[0m ");
    use std::io::Write;
    std::io::stdout().flush().ok();

    let mut input = String::new();
    let contribute = if std::io::stdin().read_line(&mut input).is_ok() {
        let answer = input.trim().to_lowercase();
        answer == "y" || answer == "yes"
    } else {
        false
    };

    if contribute {
        let config_dir = crate::core::data_dir::lean_ctx_data_dir()
            .unwrap_or_else(|_| home.join(".config/lean-ctx"));
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

    // Step 9: Auto-Update opt-in
    terminal_ui::print_step_header(9, 11, "Auto-Updates");
    println!("  Keep lean-ctx up to date automatically.");
    println!("  \x1b[1mChecks GitHub every 6h, installs only when a new release exists.\x1b[0m");
    println!(
        "  \x1b[2mNo restarts mid-session. Change anytime: lean-ctx update --schedule off\x1b[0m"
    );
    println!();
    print!("  Enable automatic updates? \x1b[1m[y/N]\x1b[0m ");
    std::io::stdout().flush().ok();

    let mut auto_input = String::new();
    let auto_update = if std::io::stdin().read_line(&mut auto_input).is_ok() {
        let answer = auto_input.trim().to_lowercase();
        answer == "y" || answer == "yes"
    } else {
        false
    };

    if auto_update {
        let cfg = crate::core::config::Config::load();
        let hours = cfg.updates.check_interval_hours;
        match crate::core::update_scheduler::install_schedule(hours) {
            Ok(info) => {
                crate::core::update_scheduler::set_auto_update(true, false, hours);
                terminal_ui::print_status_ok(&format!("Enabled — {info}"));
            }
            Err(e) => {
                terminal_ui::print_status_warn(&format!("Scheduler setup failed: {e}"));
                terminal_ui::print_status_skip("Enable later: lean-ctx update --schedule");
            }
        }
    } else {
        crate::core::update_scheduler::set_auto_update(false, false, 6);
        terminal_ui::print_status_skip("Skipped — enable later: lean-ctx update --schedule");
    }

    // Step 10: Premium Features Configuration
    terminal_ui::print_step_header(10, 11, "Premium Features");
    configure_premium_features(&home);

    // Step 11: Code Intelligence — build graph in background
    terminal_ui::print_step_header(11, 11, "Code Intelligence");
    let cwd = std::env::current_dir().ok();
    let cwd_is_home = cwd
        .as_ref()
        .is_some_and(|d| dirs::home_dir().is_some_and(|h| d.as_path() == h.as_path()));
    if cwd_is_home {
        terminal_ui::print_status_warn(
            "Running from $HOME — graph build skipped to avoid scanning your entire home directory.",
        );
        println!();
        println!("  \x1b[1mSet a default project root to avoid this:\x1b[0m");
        println!("  \x1b[2mEnter your main project path (or press Enter to skip):\x1b[0m");
        print!("  \x1b[1m>\x1b[0m ");
        use std::io::Write;
        std::io::stdout().flush().ok();
        let mut root_input = String::new();
        if std::io::stdin().read_line(&mut root_input).is_ok() {
            let root_trimmed = root_input.trim();
            if root_trimmed.is_empty() {
                terminal_ui::print_status_skip("No project root set. Set later: lean-ctx config set project_root /path/to/project");
            } else {
                let root_path = std::path::Path::new(root_trimmed);
                if root_path.exists() && root_path.is_dir() {
                    let config_path = crate::core::data_dir::lean_ctx_data_dir()
                        .unwrap_or_else(|_| home.join(".config/lean-ctx"))
                        .join("config.toml");
                    let mut content = std::fs::read_to_string(&config_path).unwrap_or_default();
                    if content.contains("project_root") {
                        if let Ok(re) = regex::Regex::new(r#"(?m)^project_root\s*=\s*"[^"]*""#) {
                            content = re
                                .replace(&content, &format!("project_root = \"{root_trimmed}\""))
                                .to_string();
                        }
                    } else {
                        if !content.is_empty() && !content.ends_with('\n') {
                            content.push('\n');
                        }
                        content.push_str(&format!("project_root = \"{root_trimmed}\"\n"));
                    }
                    let _ = std::fs::write(&config_path, &content);
                    terminal_ui::print_status_ok(&format!("Project root set: {root_trimmed}"));
                    if root_path.join(".git").exists()
                        || root_path.join("Cargo.toml").exists()
                        || root_path.join("package.json").exists()
                    {
                        spawn_index_build_background(root_path);
                        terminal_ui::print_status_ok("Graph build started (background)");
                    }
                } else {
                    terminal_ui::print_status_warn(&format!(
                        "Path not found: {root_trimmed} — skipped"
                    ));
                }
            }
        }
    } else {
        let is_project = cwd.as_ref().is_some_and(|d| {
            d.join(".git").exists()
                || d.join("Cargo.toml").exists()
                || d.join("package.json").exists()
                || d.join("go.mod").exists()
        });
        if is_project {
            println!("  \x1b[2mBuilding code graph for graph-aware reads, impact analysis,\x1b[0m");
            println!("  \x1b[2mand smart search fusion in the background...\x1b[0m");
            if let Some(ref root) = cwd {
                spawn_index_build_background(root);
            }
            terminal_ui::print_status_ok("Graph build started (background)");
        } else {
            println!(
                "  \x1b[2mRun `lean-ctx impact build` inside any git project to enable\x1b[0m"
            );
            println!(
                "  \x1b[2mgraph-aware reads, impact analysis, and smart search fusion.\x1b[0m"
            );
        }
    }
    println!();

    // Auto-approve transparency banner
    {
        let tools = crate::core::editor_registry::writers::auto_approve_tools();
        println!();
        println!(
            "  \x1b[33m⚡ Auto-approved tools ({} total):\x1b[0m",
            tools.len()
        );
        for chunk in tools.chunks(6) {
            let names: Vec<_> = chunk.iter().map(|t| format!("\x1b[2m{t}\x1b[0m")).collect();
            println!("    {}", names.join(", "));
        }
        println!("  \x1b[2mDisable with: lean-ctx setup --no-auto-approve\x1b[0m");
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
            if errors.len() == 1 { "" } else { "s" },
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

    let mut tools_to_restart: Vec<String> = newly_configured
        .iter()
        .map(std::string::ToString::to_string)
        .collect();
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
            "     {dim}Changes take effect after a full restart (MCP may be enabled or disabled depending on mode).{rst}"
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
    pub no_auto_approve: bool,
    pub skip_proxy: bool,
}

pub fn run_setup_with_options(opts: SetupOptions) -> Result<SetupReport, String> {
    let _quiet_guard = opts.json.then(|| EnvVarGuard::set("LEAN_CTX_QUIET", "1"));
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
        crate::shell_hook::install_all(opts.json);
        #[cfg(not(windows))]
        {
            let hook_content = crate::cli::generate_hook_posix(&binary);
            if crate::shell::is_container() {
                crate::cli::write_env_sh_for_containers(&hook_content);
                shell_step.items.push(SetupItem {
                    name: "env_sh".to_string(),
                    status: "created".to_string(),
                    path: Some("~/.lean-ctx/env.sh".to_string()),
                    note: Some("Docker/CI helper (BASH_ENV / CLAUDE_ENV_FILE)".to_string()),
                });
            } else {
                shell_step.items.push(SetupItem {
                    name: "env_sh".to_string(),
                    status: "skipped".to_string(),
                    path: None,
                    note: Some("not a container environment".to_string()),
                });
            }
        }
        shell_step.items.push(SetupItem {
            name: "init --global".to_string(),
            status: "ran".to_string(),
            path: None,
            note: None,
        });
        shell_step.items.push(SetupItem {
            name: "universal_shell_hook".to_string(),
            status: "installed".to_string(),
            path: None,
            note: Some("~/.zshenv, ~/.bashenv, agent aliases".to_string()),
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

    // Step: Daemon (optional acceleration for CLI routing)
    let mut daemon_step = SetupStepReport {
        name: "daemon".to_string(),
        ok: true,
        items: Vec::new(),
        warnings: Vec::new(),
        errors: Vec::new(),
    };
    {
        let was_running = crate::daemon::is_daemon_running();
        if was_running {
            let _ = crate::daemon::stop_daemon();
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        match crate::daemon::start_daemon(&[]) {
            Ok(()) => {
                let action = if was_running { "restarted" } else { "started" };
                daemon_step.items.push(SetupItem {
                    name: "serve --daemon".to_string(),
                    status: action.to_string(),
                    path: Some(crate::daemon::daemon_addr().display()),
                    note: Some("CLI commands can route via IPC when running".to_string()),
                });
            }
            Err(e) => {
                daemon_step
                    .warnings
                    .push(format!("daemon start failed (non-fatal): {e}"));
                daemon_step.items.push(SetupItem {
                    name: "serve --daemon".to_string(),
                    status: "skipped".to_string(),
                    path: None,
                    note: Some(format!("optional — {e}")),
                });
            }
        }
    }
    steps.push(daemon_step);

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

        let mode = if target.agent_key.is_empty() {
            HookMode::Mcp
        } else {
            recommend_hook_mode(&target.agent_key)
        };

        let res = crate::core::editor_registry::write_config_with_options(
            target,
            &binary,
            WriteOptions {
                overwrite_invalid: opts.fix,
            },
        );
        match res {
            Ok(w) => {
                let note_parts: Vec<String> = [Some(format!("mode={mode}")), w.note]
                    .into_iter()
                    .flatten()
                    .collect();
                editor_step.items.push(SetupItem {
                    name: target.name.to_string(),
                    status: match w.action {
                        WriteAction::Created => "created".to_string(),
                        WriteAction::Updated => "updated".to_string(),
                        WriteAction::Already => "already".to_string(),
                    },
                    path: Some(short_path),
                    note: Some(note_parts.join("; ")),
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

    // Step: Skill files
    let mut skill_step = SetupStepReport {
        name: "skill_files".to_string(),
        ok: true,
        items: Vec::new(),
        warnings: Vec::new(),
        errors: Vec::new(),
    };
    let skill_results = crate::rules_inject::install_all_skills(&home);
    for (name, is_new) in &skill_results {
        skill_step.items.push(SetupItem {
            name: name.clone(),
            status: if *is_new { "installed" } else { "already" }.to_string(),
            path: None,
            note: Some("SKILL.md".to_string()),
        });
    }
    if !skill_step.items.is_empty() {
        steps.push(skill_step);
    }

    // Step: Agent-specific hooks (all detected agents)
    let mut hooks_step = SetupStepReport {
        name: "agent_hooks".to_string(),
        ok: true,
        items: Vec::new(),
        warnings: Vec::new(),
        errors: Vec::new(),
    };
    for target in &targets {
        if !target.detect_path.exists() || target.agent_key.is_empty() {
            continue;
        }
        let mode = recommend_hook_mode(&target.agent_key);
        crate::hooks::install_agent_hook_with_mode(&target.agent_key, true, mode);
        let mcp_note = match configure_agent_mcp(&target.agent_key) {
            Ok(()) => "; MCP config updated".to_string(),
            Err(e) => format!("; MCP config skipped: {e}"),
        };
        hooks_step.items.push(SetupItem {
            name: format!("{} hooks", target.name),
            status: "installed".to_string(),
            path: Some(target.detect_path.to_string_lossy().to_string()),
            note: Some(format!(
                "mode={mode}; merge-based install/repair (preserves other hooks/plugins){mcp_note}"
            )),
        });
    }
    if !hooks_step.items.is_empty() {
        steps.push(hooks_step);
    }

    // Step: Proxy autostart + env vars (respects opt-in)
    let mut proxy_step = SetupStepReport {
        name: "proxy".to_string(),
        ok: true,
        items: Vec::new(),
        warnings: Vec::new(),
        errors: Vec::new(),
    };
    if opts.skip_proxy {
        proxy_step.items.push(SetupItem {
            name: "proxy".to_string(),
            status: "skipped".to_string(),
            path: None,
            note: Some("Proxy not enabled (run `lean-ctx proxy enable`)".to_string()),
        });
    } else {
        let proxy_port = crate::proxy_setup::default_port();
        crate::proxy_autostart::install(proxy_port, true);
        std::thread::sleep(std::time::Duration::from_millis(500));
        crate::proxy_setup::install_proxy_env(&home, proxy_port, opts.json);
        proxy_step.items.push(SetupItem {
            name: "proxy_autostart".to_string(),
            status: "installed".to_string(),
            path: None,
            note: Some("LaunchAgent/systemd auto-start on login".to_string()),
        });
        proxy_step.items.push(SetupItem {
            name: "proxy_env".to_string(),
            status: "configured".to_string(),
            path: None,
            note: Some("ANTHROPIC_BASE_URL, OPENAI_BASE_URL, GEMINI_API_BASE_URL".to_string()),
        });
    }
    steps.push(proxy_step);

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

    // Project root validation: warn if no root is configured and cwd is broad
    {
        let has_env_root = std::env::var("LEAN_CTX_PROJECT_ROOT")
            .ok()
            .is_some_and(|v| !v.is_empty());
        let cfg = crate::core::config::Config::load();
        let has_cfg_root = cfg.project_root.as_ref().is_some_and(|v| !v.is_empty());
        if !has_env_root && !has_cfg_root {
            if let Ok(cwd) = std::env::current_dir() {
                let is_home = dirs::home_dir().is_some_and(|h| cwd == h);
                if is_home {
                    let mut root_step = SetupStepReport {
                        name: "project_root".to_string(),
                        ok: true,
                        items: Vec::new(),
                        warnings: vec![
                            "No project_root configured. Running from $HOME can cause excessive scanning. \
                             Set via: lean-ctx config set project_root /path/to/project".to_string()
                        ],
                        errors: Vec::new(),
                    };
                    root_step.items.push(SetupItem {
                        name: "project_root".to_string(),
                        status: "unconfigured".to_string(),
                        path: None,
                        note: Some(
                            "Set LEAN_CTX_PROJECT_ROOT or add project_root to config.toml"
                                .to_string(),
                        ),
                    });
                    steps.push(root_step);
                }
            }
        }
    }

    // Auto-build property graph if inside any recognized project
    if let Ok(cwd) = std::env::current_dir() {
        let is_project = cwd.join(".git").exists()
            || cwd.join("Cargo.toml").exists()
            || cwd.join("package.json").exists()
            || cwd.join("go.mod").exists();
        if is_project {
            spawn_index_build_background(&cwd);
        }
    }

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

fn spawn_index_build_background(root: &std::path::Path) {
    if std::env::var("LEAN_CTX_DISABLED").is_ok()
        || matches!(std::env::var("LEAN_CTX_QUIET"), Ok(v) if v.trim() == "1")
    {
        return;
    }
    let root_str = crate::core::graph_index::normalize_project_root(&root.to_string_lossy());
    if !crate::core::graph_index::is_safe_scan_root_public(&root_str) {
        tracing::info!("[setup: skipping background graph build for unsafe root {root_str}]");
        return;
    }

    let binary = std::env::current_exe().map_or_else(
        |_| resolve_portable_binary(),
        |p| p.to_string_lossy().to_string(),
    );

    #[cfg(unix)]
    {
        let mut cmd = std::process::Command::new("nice");
        cmd.args(["-n", "19"]);
        if which_ionice_available() {
            cmd.arg("ionice").args(["-c", "3"]);
        }
        cmd.arg(&binary)
            .args(["index", "build-graph", "--root"])
            .arg(root)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .stdin(std::process::Stdio::null());
        let _ = cmd.spawn();
    }

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let _ = std::process::Command::new(&binary)
            .args(["index", "build-graph", "--root"])
            .arg(root)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .stdin(std::process::Stdio::null())
            .creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW)
            .spawn();
    }
}

#[cfg(unix)]
fn which_ionice_available() -> bool {
    std::process::Command::new("ionice")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
}

/// Result of setting up a single agent with all steps.
#[derive(Debug, Default)]
pub struct AgentSetupResult {
    pub mcp_ok: bool,
    pub rules: crate::rules_inject::InjectResult,
    pub skill_installed: bool,
    pub errors: Vec<String>,
}

/// Complete per-agent setup: MCP config + global rules + skill + hook.
/// Single source of truth — called by both `init --agent` and `setup`.
pub fn setup_single_agent(
    agent_name: &str,
    global: bool,
    mode: crate::hooks::HookMode,
) -> AgentSetupResult {
    let home = dirs::home_dir().unwrap_or_default();
    let mut result = AgentSetupResult::default();

    crate::hooks::install_agent_hook_with_mode(agent_name, global, mode);

    match configure_agent_mcp(agent_name) {
        Ok(()) => result.mcp_ok = true,
        Err(e) => result.errors.push(format!("MCP config: {e}")),
    }

    result.rules = crate::rules_inject::inject_rules_for_agent(&home, agent_name);

    if let Ok(path) = crate::rules_inject::install_skill_for_agent(&home, agent_name) {
        result.skill_installed = path.exists();
    }

    result
}

pub fn configure_agent_mcp(agent: &str) -> Result<(), String> {
    let home = dirs::home_dir().ok_or_else(|| "Cannot determine home directory".to_string())?;
    let binary = resolve_portable_binary();

    let targets = agent_mcp_targets(agent, &home)?;

    let mut errors = Vec::new();
    for t in &targets {
        if let Err(e) = crate::core::editor_registry::write_config_with_options(
            t,
            &binary,
            WriteOptions {
                overwrite_invalid: true,
            },
        ) {
            eprintln!(
                "\x1b[33m⚠\x1b[0m  Could not configure {}: {}",
                t.config_path.display(),
                e
            );
            errors.push(e);
        }
    }

    if agent == "kiro" {
        install_kiro_steering(&home);
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} config(s) could not be written. See warnings above.",
            errors.len()
        ))
    }
}

fn agent_mcp_targets(agent: &str, home: &std::path::Path) -> Result<Vec<EditorTarget>, String> {
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

    let pi_cfg = home.join(".pi").join("agent").join("mcp.json");

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
            crate::core::editor_registry::claude_mcp_json_path(home),
            ConfigType::McpJson,
        ),
        "windsurf" => push(
            &mut targets,
            "Windsurf",
            home.join(".codeium/windsurf/mcp_config.json"),
            ConfigType::McpJson,
        ),
        "codex" => {
            let codex_dir =
                crate::core::home::resolve_codex_dir().unwrap_or_else(|| home.join(".codex"));
            push(
                &mut targets,
                "Codex CLI",
                codex_dir.join("config.toml"),
                ConfigType::Codex,
            );
        }
        "gemini" => {
            push(
                &mut targets,
                "Gemini CLI",
                home.join(".gemini/settings.json"),
                ConfigType::GeminiSettings,
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
            "Copilot CLI",
            home.join(".copilot/mcp-config.json"),
            ConfigType::CopilotCli,
        ),
        "crush" => push(
            &mut targets,
            "Crush",
            home.join(".config/crush/crush.json"),
            ConfigType::Crush,
        ),
        "pi" => push(&mut targets, "Pi Coding Agent", pi_cfg, ConfigType::McpJson),
        "qoder" => {
            for path in crate::core::editor_registry::qoder_all_mcp_paths(home) {
                push(&mut targets, "Qoder", path, ConfigType::QoderSettings);
            }
        }
        "qoderwork" => push(
            &mut targets,
            "QoderWork",
            crate::core::editor_registry::qoderwork_mcp_path(home),
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
        "jetbrains" | "amp" => {
            // Handled by dedicated install hooks (servers[] array / amp.mcpServers)
        }
        "qwen" => push(
            &mut targets,
            "Qwen Code",
            home.join(".qwen/settings.json"),
            ConfigType::McpJson,
        ),
        "trae" => push(
            &mut targets,
            "Trae",
            home.join(".trae/mcp.json"),
            ConfigType::McpJson,
        ),
        "amazonq" => push(
            &mut targets,
            "Amazon Q Developer",
            home.join(".aws/amazonq/default.json"),
            ConfigType::McpJson,
        ),
        "opencode" => {
            #[cfg(windows)]
            let opencode_path = if let Ok(appdata) = std::env::var("APPDATA") {
                std::path::PathBuf::from(appdata)
                    .join("opencode")
                    .join("opencode.json")
            } else {
                home.join(".config/opencode/opencode.json")
            };
            #[cfg(not(windows))]
            let opencode_path = home.join(".config/opencode/opencode.json");
            push(
                &mut targets,
                "OpenCode",
                opencode_path,
                ConfigType::OpenCode,
            );
        }
        "hermes" => push(
            &mut targets,
            "Hermes Agent",
            home.join(".hermes/config.yaml"),
            ConfigType::HermesYaml,
        ),
        "vscode" => push(
            &mut targets,
            "VS Code",
            crate::core::editor_registry::vscode_mcp_path(),
            ConfigType::VsCodeMcp,
        ),
        "zed" => push(
            &mut targets,
            "Zed",
            crate::core::editor_registry::zed_settings_path(home),
            ConfigType::Zed,
        ),
        "aider" => push(
            &mut targets,
            "Aider",
            home.join(".aider/mcp.json"),
            ConfigType::McpJson,
        ),
        "continue" => push(
            &mut targets,
            "Continue",
            home.join(".continue/mcp.json"),
            ConfigType::McpJson,
        ),
        "neovim" => push(
            &mut targets,
            "Neovim (mcphub.nvim)",
            home.join(".config/mcphub/servers.json"),
            ConfigType::McpJson,
        ),
        "emacs" => push(
            &mut targets,
            "Emacs (mcp.el)",
            home.join(".emacs.d/mcp.json"),
            ConfigType::McpJson,
        ),
        "sublime" => push(
            &mut targets,
            "Sublime Text",
            home.join(".config/sublime-text/mcp.json"),
            ConfigType::McpJson,
        ),
        _ => {
            return Err(format!("Unknown agent '{agent}'"));
        }
    }

    Ok(targets)
}

pub fn disable_agent_mcp(agent: &str, overwrite_invalid: bool) -> Result<(), String> {
    let home = dirs::home_dir().ok_or_else(|| "Cannot determine home directory".to_string())?;

    let mut targets = Vec::<EditorTarget>::new();

    let push = |targets: &mut Vec<EditorTarget>,
                name: &'static str,
                config_path: PathBuf,
                config_type: ConfigType| {
        targets.push(EditorTarget {
            name,
            agent_key: agent.to_string(),
            detect_path: PathBuf::from("/nonexistent"),
            config_path,
            config_type,
        });
    };

    let pi_cfg = home.join(".pi").join("agent").join("mcp.json");

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
        "codex" => {
            let codex_dir =
                crate::core::home::resolve_codex_dir().unwrap_or_else(|| home.join(".codex"));
            push(
                &mut targets,
                "Codex CLI",
                codex_dir.join("config.toml"),
                ConfigType::Codex,
            );
        }
        "gemini" => {
            push(
                &mut targets,
                "Gemini CLI",
                home.join(".gemini/settings.json"),
                ConfigType::GeminiSettings,
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
            "Copilot CLI",
            home.join(".copilot/mcp-config.json"),
            ConfigType::CopilotCli,
        ),
        "crush" => push(
            &mut targets,
            "Crush",
            home.join(".config/crush/crush.json"),
            ConfigType::Crush,
        ),
        "pi" => push(&mut targets, "Pi Coding Agent", pi_cfg, ConfigType::McpJson),
        "qoder" => {
            for path in crate::core::editor_registry::qoder_all_mcp_paths(&home) {
                push(&mut targets, "Qoder", path, ConfigType::QoderSettings);
            }
        }
        "qoderwork" => push(
            &mut targets,
            "QoderWork",
            crate::core::editor_registry::qoderwork_mcp_path(&home),
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
        "jetbrains" | "amp" => {
            // Not supported for disable via this helper.
        }
        "qwen" => push(
            &mut targets,
            "Qwen Code",
            home.join(".qwen/settings.json"),
            ConfigType::McpJson,
        ),
        "trae" => push(
            &mut targets,
            "Trae",
            home.join(".trae/mcp.json"),
            ConfigType::McpJson,
        ),
        "amazonq" => push(
            &mut targets,
            "Amazon Q Developer",
            home.join(".aws/amazonq/default.json"),
            ConfigType::McpJson,
        ),
        "opencode" => {
            #[cfg(windows)]
            let opencode_path = if let Ok(appdata) = std::env::var("APPDATA") {
                std::path::PathBuf::from(appdata)
                    .join("opencode")
                    .join("opencode.json")
            } else {
                home.join(".config/opencode/opencode.json")
            };
            #[cfg(not(windows))]
            let opencode_path = home.join(".config/opencode/opencode.json");
            push(
                &mut targets,
                "OpenCode",
                opencode_path,
                ConfigType::OpenCode,
            );
        }
        "hermes" => push(
            &mut targets,
            "Hermes Agent",
            home.join(".hermes/config.yaml"),
            ConfigType::HermesYaml,
        ),
        "vscode" => push(
            &mut targets,
            "VS Code",
            crate::core::editor_registry::vscode_mcp_path(),
            ConfigType::VsCodeMcp,
        ),
        "zed" => push(
            &mut targets,
            "Zed",
            crate::core::editor_registry::zed_settings_path(&home),
            ConfigType::Zed,
        ),
        "aider" => push(
            &mut targets,
            "Aider",
            home.join(".aider/mcp.json"),
            ConfigType::McpJson,
        ),
        "continue" => push(
            &mut targets,
            "Continue",
            home.join(".continue/mcp.json"),
            ConfigType::McpJson,
        ),
        "neovim" => push(
            &mut targets,
            "Neovim (mcphub.nvim)",
            home.join(".config/mcphub/servers.json"),
            ConfigType::McpJson,
        ),
        "emacs" => push(
            &mut targets,
            "Emacs (mcp.el)",
            home.join(".emacs.d/mcp.json"),
            ConfigType::McpJson,
        ),
        "sublime" => push(
            &mut targets,
            "Sublime Text",
            home.join(".config/sublime-text/mcp.json"),
            ConfigType::McpJson,
        ),
        _ => {
            return Err(format!("Unknown agent '{agent}'"));
        }
    }

    for t in &targets {
        crate::core::editor_registry::remove_lean_ctx_server(
            t,
            WriteOptions { overwrite_invalid },
        )?;
    }

    Ok(())
}

pub fn install_skill_files(home: &std::path::Path) -> Vec<(String, bool)> {
    crate::rules_inject::install_all_skills(home)
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

fn upsert_toml_key(content: &mut String, key: &str, value: &str) {
    let pattern = format!("{key} = ");
    if let Some(start) = content.find(&pattern) {
        let line_end = content[start..]
            .find('\n')
            .map_or(content.len(), |p| start + p);
        content.replace_range(start..line_end, &format!("{key} = \"{value}\""));
    } else {
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str(&format!("{key} = \"{value}\"\n"));
    }
}

fn remove_toml_key(content: &mut String, key: &str) {
    let pattern = format!("{key} = ");
    if let Some(start) = content.find(&pattern) {
        let line_end = content[start..]
            .find('\n')
            .map_or(content.len(), |p| start + p + 1);
        content.replace_range(start..line_end, "");
    }
}

fn configure_premium_features(home: &std::path::Path) {
    use crate::terminal_ui;
    use std::io::Write;

    let config_dir = crate::core::data_dir::lean_ctx_data_dir()
        .unwrap_or_else(|_| home.join(".config/lean-ctx"));
    let _ = std::fs::create_dir_all(&config_dir);
    let config_path = config_dir.join("config.toml");
    let mut config_content = std::fs::read_to_string(&config_path).unwrap_or_default();

    let dim = "\x1b[2m";
    let bold = "\x1b[1m";
    let cyan = "\x1b[36m";
    let rst = "\x1b[0m";

    // Unified Compression Level (replaces terse_agent + output_density)
    println!("\n  {bold}Compression Level{rst} {dim}(controls all token optimization layers){rst}");
    println!("  {dim}Applies to tool output, agent prompts, and protocol mode.{rst}");
    println!();
    println!("  {cyan}off{rst}      — No compression (full verbose output)");
    println!("  {cyan}lite{rst}     — Light: concise output, basic terse filtering {dim}(~25% savings){rst}");
    println!("  {cyan}standard{rst} — Dense output + compact protocol + pattern-aware {dim}(~45% savings){rst}");
    println!("  {cyan}max{rst}      — Expert mode: TDD protocol, all layers active {dim}(~65% savings){rst}");
    println!();
    print!("  Compression level? {bold}[off/lite/standard/max]{rst} {dim}(default: off){rst} ");
    std::io::stdout().flush().ok();

    let mut level_input = String::new();
    let level = if std::io::stdin().read_line(&mut level_input).is_ok() {
        match level_input.trim().to_lowercase().as_str() {
            "lite" => "lite",
            "standard" | "std" => "standard",
            "max" => "max",
            _ => "off",
        }
    } else {
        "off"
    };

    let effective_level = if level != "off" {
        upsert_toml_key(&mut config_content, "compression_level", level);
        remove_toml_key(&mut config_content, "terse_agent");
        remove_toml_key(&mut config_content, "output_density");
        terminal_ui::print_status_ok(&format!("Compression: {level}"));
        crate::core::config::CompressionLevel::from_str_label(level)
    } else if config_content.contains("compression_level") {
        upsert_toml_key(&mut config_content, "compression_level", "off");
        terminal_ui::print_status_ok("Compression: off");
        Some(crate::core::config::CompressionLevel::Off)
    } else {
        terminal_ui::print_status_skip(
            "Compression: off (change later with: lean-ctx compression <level>)",
        );
        Some(crate::core::config::CompressionLevel::Off)
    };

    if let Some(lvl) = effective_level {
        let n = crate::core::terse::rules_inject::inject(&lvl);
        if n > 0 {
            terminal_ui::print_status_ok(&format!(
                "Updated {n} rules file(s) with compression prompt"
            ));
        }
    }

    // Tool Result Archive (unchanged)
    println!(
        "\n  {bold}Tool Result Archive{rst} {dim}(zero-loss: large outputs archived, retrievable via ctx_expand){rst}"
    );
    print!("  Enable auto-archive? {bold}[Y/n]{rst} ");
    std::io::stdout().flush().ok();

    let mut archive_input = String::new();
    let archive_on = if std::io::stdin().read_line(&mut archive_input).is_ok() {
        let a = archive_input.trim().to_lowercase();
        a.is_empty() || a == "y" || a == "yes"
    } else {
        true
    };

    if archive_on && !config_content.contains("[archive]") {
        if !config_content.is_empty() && !config_content.ends_with('\n') {
            config_content.push('\n');
        }
        config_content.push_str("\n[archive]\nenabled = true\n");
        terminal_ui::print_status_ok("Tool Result Archive: enabled");
    } else if !archive_on {
        terminal_ui::print_status_skip("Archive: off (enable later in config.toml)");
    }

    let _ = std::fs::write(&config_path, config_content);
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    #[cfg(target_os = "macos")]
    fn qoder_agent_targets_include_all_macos_mcp_locations() {
        let home = std::path::Path::new("/Users/tester");
        let targets = agent_mcp_targets("qoder", home).unwrap();
        let paths: Vec<_> = targets.iter().map(|t| t.config_path.as_path()).collect();

        assert_eq!(
            paths,
            vec![
                home.join(".qoder/mcp.json").as_path(),
                home.join("Library/Application Support/Qoder/User/mcp.json")
                    .as_path(),
                home.join("Library/Application Support/Qoder/SharedClientCache/mcp.json")
                    .as_path(),
            ]
        );
        assert!(targets
            .iter()
            .all(|t| t.config_type == ConfigType::QoderSettings));
    }
}
