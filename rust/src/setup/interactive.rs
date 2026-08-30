use crate::core::editor_registry::{WriteAction, WriteOptions};
use crate::core::portable_binary::resolve_portable_binary;
use crate::hooks::{HookMode, recommend_hook_mode};

use super::first_run::{first_run_setup_level, persist_setup_choice};
use super::helpers::{
    configure_plan_mode_settings, configure_premium_features, configure_tool_profile,
    install_skill_files, shorten_path,
};
use super::index_build::spawn_index_build_background;
use super::options::SetupOptions;
use super::with_options::run_setup_with_options;

pub fn run_setup() {
    use crate::terminal_ui;

    if crate::shell::is_non_interactive() {
        eprintln!("Non-interactive terminal detected (no TTY on stdin).");
        eprintln!(
            "Running in non-interactive mode (equivalent to: lean-ctx setup --non-interactive --yes)"
        );
        eprintln!();
        let opts = SetupOptions {
            non_interactive: true,
            yes: true,
            ..Default::default()
        };
        match run_setup_with_options(opts) {
            Ok(report) => {
                for w in &report.warnings {
                    tracing::warn!("{w}");
                }
            }
            Err(e) => tracing::error!("Setup error: {e}"),
        }
        return;
    }

    let Some(home) = crate::core::home::resolve_home_dir() else {
        tracing::error!("Cannot determine home directory");
        std::process::exit(1);
    };

    let binary = resolve_portable_binary();

    let home_str = home.to_string_lossy().to_string();

    terminal_ui::print_setup_header();

    let (inject_rules, inject_skills) = first_run_setup_level();
    persist_setup_choice(inject_rules, inject_skills);

    terminal_ui::print_step_header(1, 13, "Shell Hook");
    crate::cli::cmd_init(&["--global".to_string()]);
    crate::shell_hook::install_all(false);

    terminal_ui::print_step_header(2, 13, "Daemon");
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

    terminal_ui::print_step_header(3, 13, "AI Tool Detection");

    let targets = crate::core::editor_registry::build_targets(&home);
    // #281: in MCP-disabled environments (`auto_update_mcp = false`) editors are
    // still detected and hooks/rules still install, but the MCP server is never
    // written into their configs.
    let update_mcp = crate::core::config::Config::load()
        .setup
        .should_update_mcp();
    let mut newly_configured: Vec<&str> = Vec::new();
    let mut already_configured: Vec<&str> = Vec::new();
    let mut not_installed: Vec<&str> = Vec::new();
    let mut mcp_skipped: Vec<&str> = Vec::new();
    let mut errors: Vec<&str> = Vec::new();

    for target in &targets {
        let short_path = shorten_path(&target.config_path.to_string_lossy(), &home_str);

        if !target.detect_path.exists() {
            not_installed.push(target.name);
            continue;
        }

        if !update_mcp {
            terminal_ui::print_status_ok(&format!(
                "{:<20} \x1b[2mMCP registration skipped (auto_update_mcp=false)\x1b[0m",
                target.name
            ));
            mcp_skipped.push(target.name);
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
    if total_ok == 0 && errors.is_empty() && mcp_skipped.is_empty() {
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

    configure_plan_mode_settings(&newly_configured, &already_configured);

    terminal_ui::print_step_header(4, 13, "Agent Rules");
    let rules_result = if inject_rules {
        let r = crate::rules_inject::inject_all_rules(&home);
        for name in &r.injected {
            terminal_ui::print_status_new(&format!("{name:<20} \x1b[2mrules injected\x1b[0m"));
        }
        for name in &r.updated {
            terminal_ui::print_status_new(&format!("{name:<20} \x1b[2mrules updated\x1b[0m"));
        }
        for name in &r.already {
            terminal_ui::print_status_ok(&format!("{name:<20} \x1b[2mrules up-to-date\x1b[0m"));
        }
        for err in &r.errors {
            terminal_ui::print_status_warn(err);
        }
        if !r.backed_up.is_empty() {
            for bak in &r.backed_up {
                println!("  \x1b[2m  ↳ backup: {bak}\x1b[0m");
            }
        }
        if r.injected.is_empty()
            && r.updated.is_empty()
            && r.already.is_empty()
            && r.errors.is_empty()
        {
            terminal_ui::print_status_skip("No agent rules needed");
        }
        r
    } else {
        terminal_ui::print_status_skip(
            "Skipped (run `lean-ctx setup` or set auto_inject_rules = true in config)",
        );
        crate::rules_inject::InjectResult::default()
    };

    for target in &targets {
        if !target.detect_path.exists() || target.agent_key.is_empty() {
            continue;
        }
        let mode = recommend_hook_mode(&target.agent_key);
        crate::hooks::install_agent_hook_with_mode(&target.agent_key, true, mode);
    }

    terminal_ui::print_step_header(5, 13, "API Proxy (optional)");
    {
        let cfg = crate::core::config::Config::load();
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
                if let Err(e) =
                    crate::core::config::Config::update_global(|c| c.proxy_enabled = Some(answer))
                {
                    tracing::warn!("could not persist proxy choice: {e}");
                }
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

    terminal_ui::print_step_header(6, 13, "IDE Config Access (optional)");
    {
        let cfg = crate::core::config::Config::load();
        match cfg.allow_ide_config_dirs {
            Some(true) => {
                terminal_ui::print_status_ok(
                    "Enabled — the agent can read your editors' config dirs",
                );
            }
            Some(false) => {
                terminal_ui::print_status_skip(
                    "Off (enable: lean-ctx config set allow_ide_config_dirs true)",
                );
            }
            None => {
                println!(
                    "  \x1b[2mlean-ctx tools are jailed to the current project. Enabling this lets\x1b[0m"
                );
                println!(
                    "  \x1b[2mthe agent read every supported editor's config dir (~/.cursor, VS Code,\x1b[0m"
                );
                println!(
                    "  \x1b[2mCline/Roo, JetBrains, …) to manage MCP setup across editors.\x1b[0m"
                );
                println!();
                println!(
                    "  \x1b[33mTrade-off:\x1b[0m \x1b[2mthose dirs can hold other agents' sessions & credentials.\x1b[0m"
                );
                println!();
                print!("  Allow the agent to read IDE config dirs? [y/N] ");
                let _ = std::io::Write::flush(&mut std::io::stdout());
                let mut input = String::new();
                let _ = std::io::stdin().read_line(&mut input);
                let answer = matches!(input.trim().to_lowercase().as_str(), "y" | "yes");
                if let Err(e) = crate::core::config::Config::update_global(|c| {
                    c.allow_ide_config_dirs = Some(answer);
                }) {
                    tracing::warn!("could not persist IDE-config-access choice: {e}");
                }
                if answer {
                    terminal_ui::print_status_new("IDE config access enabled");
                } else {
                    terminal_ui::print_status_skip(
                        "Skipped (enable later: lean-ctx config set allow_ide_config_dirs true)",
                    );
                }
            }
        }
    }

    terminal_ui::print_step_header(7, 13, "Skill Files");
    if inject_skills {
        let skill_result = install_skill_files(&home);
        for (name, installed) in &skill_result {
            if *installed {
                terminal_ui::print_status_new(&format!(
                    "{name:<20} \x1b[2mSKILL.md installed\x1b[0m"
                ));
            } else {
                terminal_ui::print_status_ok(&format!(
                    "{name:<20} \x1b[2mSKILL.md up-to-date\x1b[0m"
                ));
            }
        }
        if skill_result.is_empty() {
            terminal_ui::print_status_skip("No skill directories to install");
        }
    } else {
        terminal_ui::print_status_skip(
            "Skipped (skill files install with the rules opt-in; choose Standard/Full in `lean-ctx setup`)",
        );
    }

    terminal_ui::print_step_header(8, 13, "Environment Check");
    let lean_dir = crate::core::data_dir::lean_ctx_data_dir()
        .unwrap_or_else(|_| home.join(".config/lean-ctx"));
    if lean_dir.exists() {
        terminal_ui::print_status_ok(&format!("{} ready", lean_dir.display()));
    } else {
        let _ = std::fs::create_dir_all(&lean_dir);
        terminal_ui::print_status_new(&format!("Created {}", lean_dir.display()));
    }
    if let Some(report) = crate::core::data_consolidate::consolidate()
        && report.files_moved > 0
    {
        terminal_ui::print_status_new(&format!(
            "Consolidated {} file(s) from a split data dir into {}",
            report.files_moved,
            report.canonical.display()
        ));
    }
    // #594: relocate a `config.toml` that an old MCP env (LEAN_CTX_DATA_DIR)
    // stranded in the data dir, so CLI and MCP read the same config from now on.
    if let Some(report) = crate::core::config_heal::heal() {
        match report.action {
            crate::core::config_heal::HealAction::Adopted => {
                terminal_ui::print_status_new(&format!(
                    "Recovered your config into {}",
                    report.to.display()
                ));
            }
            crate::core::config_heal::HealAction::Superseded => {
                terminal_ui::print_status_ok("Unified config (archived a stale data-dir copy)");
            }
        }
    }
    crate::doctor::run_compact();

    // Commit to the XDG layout (and drain any residual ~/.lean-ctx) so a stray
    // marker can never re-collapse config/data/state/cache later (GL #623).
    crate::core::layout_pin::heal();

    terminal_ui::print_step_header(9, 13, "Help Improve lean-ctx");
    println!("  Share anonymous telemetry to make lean-ctx better:");
    println!("  [2m  • Version, OS, architecture, random install ID[0m");
    println!("  [2m  • Compression patterns: file-type, size bucket, mode, ratio[0m");
    println!("  [1mNo code, no file names, no personal data — ever.[0m");
    println!("  [2mInspect anytime: lean-ctx telemetry show[0m");
    println!();
    print!("  Enable anonymous telemetry? [1m[y/N][0m ");
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
        let config_path = crate::core::config::Config::path()
            .unwrap_or_else(|| home.join(".config/lean-ctx").join("config.toml"));
        if let Some(dir) = config_path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let mut config_content = std::fs::read_to_string(&config_path).unwrap_or_default();
        if !config_content.contains("[telemetry]") {
            if !config_content.ends_with('\n') {
                config_content.push('\n');
            }
            config_content.push_str("\n[telemetry]\nenabled = true\n");
        }
        let _ = crate::config_io::write_atomic_with_backup(&config_path, &config_content);
        terminal_ui::print_status_ok("Enabled — thank you!");
    } else {
        terminal_ui::print_status_skip("Skipped — enable later with: lean-ctx telemetry on");
    }

    terminal_ui::print_step_header(10, 13, "Auto-Updates");
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

    terminal_ui::print_step_header(11, 13, "Tool Profile");
    configure_tool_profile();

    terminal_ui::print_step_header(12, 13, "Advanced Tuning (optional)");
    configure_premium_features(&home);

    terminal_ui::print_step_header(13, 13, "Code Intelligence");
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
                terminal_ui::print_status_skip(
                    "No project root set. Set later: lean-ctx config set project_root /path/to/project",
                );
            } else {
                let root_path = std::path::Path::new(root_trimmed);
                if root_path.exists() && root_path.is_dir() {
                    let config_path = crate::core::config::Config::path()
                        .unwrap_or_else(|| home.join(".config/lean-ctx").join("config.toml"));
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
                    let _ = crate::config_io::write_atomic_with_backup(&config_path, &content);
                    terminal_ui::print_status_ok(&format!("Project root set: {root_trimmed}"));
                    if crate::core::pathutil::has_project_marker(root_path) {
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
        let is_project = cwd
            .as_ref()
            .is_some_and(|d| crate::core::pathutil::has_project_marker(d));
        if is_project {
            println!("  \x1b[2mBuilding code graph for graph-aware reads, impact analysis,\x1b[0m");
            println!("  \x1b[2mand smart search fusion in the background...\x1b[0m");
            if let Some(ref root) = cwd {
                spawn_index_build_background(root);
            }
            terminal_ui::print_status_ok("Graph build started (background)");
        } else {
            println!("  \x1b[2mRun `lean-ctx graph build` inside any git project to enable\x1b[0m");
            println!(
                "  \x1b[2mgraph-aware reads, impact analysis, and smart search fusion.\x1b[0m"
            );
        }
    }
    println!();

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

    let source_cmd = crate::shell_hook::shell_source_command().unwrap_or("Restart your shell");

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
        if !tools_to_restart.contains(name) {
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

    println!();
    terminal_ui::print_logo_animated();
    terminal_ui::print_command_box();

    crate::cli::show_first_run_wow();
}
