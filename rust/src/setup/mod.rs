use std::path::PathBuf;

use crate::core::editor_registry::{ConfigType, EditorTarget, WriteAction, WriteOptions};
use crate::core::portable_binary::resolve_portable_binary;
use crate::core::setup_report::{PlatformInfo, SetupItem, SetupReport, SetupStepReport};
use crate::hooks::{HookMode, recommend_hook_mode};
use chrono::Utc;
use std::ffi::OsString;
mod mcp;
pub use mcp::*;
mod helpers;
pub use helpers::*;

#[must_use]
pub fn claude_config_json_path(home: &std::path::Path) -> PathBuf {
    crate::core::editor_registry::claude_mcp_json_path(home)
}

#[must_use]
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
        // SAFETY: `EnvVarGuard` is only used in single-threaded setup/doctor CLI
        // flows (and serial-gated tests), so no other thread reads the
        // environment while the guard mutates it.
        unsafe { std::env::set_var(key, value) };
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(previous) = &self.previous {
            // SAFETY: see `EnvVarGuard::set` — restoration runs on the same
            // single-threaded setup/doctor path that created the guard.
            unsafe { std::env::set_var(self.key, previous) };
        } else {
            // SAFETY: see `EnvVarGuard::set` — restoration runs on the same
            // single-threaded setup/doctor path that created the guard.
            unsafe { std::env::remove_var(self.key) };
        }
    }
}

/// Determine the setup level from a first-run interactive menu.
/// Returns (`inject_rules`, `inject_skills`).
fn first_run_setup_level() -> (bool, bool) {
    use std::io::Write;

    let cfg = crate::core::config::Config::load();
    if cfg.setup.auto_inject_rules.is_some() {
        return (
            cfg.setup.should_inject_rules(),
            cfg.setup.should_inject_skills(),
        );
    }

    println!();
    println!("  \x1b[1mWelcome to lean-ctx!\x1b[0m");
    println!();
    println!("  lean-ctx compresses AI context by 60-99%, saving tokens and money.");
    println!();
    println!("  Choose your setup level:");
    println!(
        "    \x1b[36m[1]\x1b[0m Minimal  \x1b[2m— Just MCP tools, no config file changes (recommended)\x1b[0m"
    );
    println!(
        "    \x1b[36m[2]\x1b[0m Standard \x1b[2m— MCP tools + agent instructions for optimal mode selection\x1b[0m"
    );
    println!(
        "    \x1b[36m[3]\x1b[0m Full     \x1b[2m— Everything (tools + rules + skills + shell hooks)\x1b[0m"
    );
    println!();
    print!("  Your choice \x1b[1m[1]\x1b[0m: ");
    std::io::stdout().flush().ok();

    let mut input = String::new();
    let choice = if std::io::stdin().read_line(&mut input).is_ok() {
        input.trim().parse::<u8>().unwrap_or(1)
    } else {
        1
    };

    match choice {
        3 => (true, true),
        2 => (true, false),
        _ => (false, false),
    }
}

/// Persist the user's setup level choice to config.toml.
fn persist_setup_choice(inject_rules: bool, inject_skills: bool) {
    if let Err(e) = crate::core::config::Config::update_global(|cfg| {
        cfg.setup.auto_inject_rules = Some(inject_rules);
        cfg.setup.auto_inject_skills = Some(inject_skills);
    }) {
        tracing::warn!("could not persist setup choice: {e}");
    }
}

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

    let Some(home) = dirs::home_dir() else {
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
        terminal_ui::print_status_skip("Skipped (run `lean-ctx setup --inject-rules` to enable)");
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
    crate::doctor::run_compact();

    // Commit to the XDG layout (and drain any residual ~/.lean-ctx) so a stray
    // marker can never re-collapse config/data/state/cache later (GL #623).
    crate::core::layout_pin::heal();

    terminal_ui::print_step_header(9, 13, "Help Improve lean-ctx");
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
        let config_path = crate::core::config::Config::path()
            .unwrap_or_else(|| home.join(".config/lean-ctx").join("config.toml"));
        if let Some(dir) = config_path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let mut config_content = std::fs::read_to_string(&config_path).unwrap_or_default();
        if !config_content.contains("[cloud]") {
            if !config_content.is_empty() && !config_content.ends_with('\n') {
                config_content.push('\n');
            }
            config_content.push_str("\n[cloud]\ncontribute_enabled = true\n");
            let _ = crate::config_io::write_atomic_with_backup(&config_path, &config_content);
        }
        terminal_ui::print_status_ok("Enabled — thank you!");
    } else {
        terminal_ui::print_status_skip("Skipped — enable later with: lean-ctx config");
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

/// Friendly, non-interactive "golden path" onboarding.
///
/// Unlike `run_setup` (the full 12-step interactive wizard), `onboard` makes
/// every decision for the user with sensible defaults — connect detected AI
/// tools, install the shell hook, set the `standard` tool profile — then prints
/// one clear "you're all set" message with a single obvious next step. This is
/// the recommended first-run path: time-to-value in seconds, zero prompts.
pub fn run_onboard() {
    use crate::terminal_ui;

    let dim = "\x1b[2m";
    let bold = "\x1b[1m";
    let cyan = "\x1b[36m";
    let green = "\x1b[1;32m";
    let yellow = "\x1b[33m";
    let rst = "\x1b[0m";

    println!();
    println!("  {bold}Connecting lean-ctx to your AI tools…{rst}");
    println!(
        "  {dim}No questions — using recommended defaults. Run `lean-ctx setup` for full control.{rst}"
    );
    println!();

    let opts = SetupOptions {
        non_interactive: true,
        yes: true,
        fix: true,
        ..Default::default()
    };

    let report = match run_setup_with_options(opts) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("  {yellow}Onboarding could not complete: {e}{rst}");
            eprintln!("  {dim}Try the guided setup instead: lean-ctx setup{rst}");
            std::process::exit(1);
        }
    };

    let connected: Vec<String> = report
        .steps
        .iter()
        .find(|s| s.name == "editors")
        .map(|s| {
            s.items
                .iter()
                .filter(|i| matches!(i.status.as_str(), "created" | "updated" | "already"))
                .map(|i| i.name.clone())
                .collect()
        })
        .unwrap_or_default();

    let data_dir = crate::core::data_dir::lean_ctx_data_dir()
        .map_or_else(|_| "~/.lean-ctx".to_string(), |p| p.display().to_string());

    println!();
    if connected.is_empty() {
        println!("  {yellow}No AI tools detected yet.{rst}");
        println!(
            "  {dim}Install Cursor, Claude Code, VS Code, etc., then re-run: lean-ctx onboard{rst}"
        );
    } else {
        println!("  {green}✓ lean-ctx is connected.{rst}");
        println!();
        println!("  {bold}Connected:{rst} {}", connected.join(", "));
    }
    println!("  {dim}Data dir:{rst}  {data_dir}");

    let source_cmd = crate::shell_hook::shell_source_command().unwrap_or("Restart your shell");
    println!();
    println!("  {bold}One last step:{rst}");
    println!("  {cyan}1.{rst} Reload your shell:  {bold}{source_cmd}{rst}");
    if !connected.is_empty() {
        println!(
            "  {cyan}2.{rst} {yellow}Fully restart your AI tool{rst} {dim}(so it reconnects to lean-ctx){rst}"
        );
        println!(
            "  {cyan}3.{rst} Ask your AI to read a file — lean-ctx optimizes it automatically."
        );
    }
    println!();
    println!(
        "  {dim}Check anytime:{rst}  {bold}lean-ctx doctor{rst}  {dim}·{rst}  {bold}lean-ctx gain{rst}"
    );
    println!();
    terminal_ui::print_command_box();

    crate::cli::show_first_run_wow();
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SetupOptions {
    pub non_interactive: bool,
    pub yes: bool,
    pub fix: bool,
    pub json: bool,
    pub no_auto_approve: bool,
    pub skip_proxy: bool,
    pub skip_rules: bool,
    /// Explicitly request rules injection (overrides config).
    pub force_inject_rules: bool,
}

pub fn run_setup_with_options(opts: SetupOptions) -> Result<SetupReport, String> {
    let _quiet_guard = opts.json.then(|| EnvVarGuard::set("LEAN_CTX_QUIET", "1"));
    let started_at = Utc::now();
    let home = dirs::home_dir().ok_or_else(|| "Cannot determine home directory".to_string())?;
    let binary = resolve_portable_binary();
    let home_str = home.to_string_lossy().to_string();

    // Commit to the XDG layout (and drain any residual ~/.lean-ctx) so a stray
    // marker can never re-collapse config/data/state/cache later (GL #623).
    crate::core::layout_pin::heal();

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
                    path: Some(crate::core::paths::config_dir().map_or_else(
                        |_| "~/.config/lean-ctx/env.sh".to_string(),
                        |d| d.join("env.sh").to_string_lossy().to_string(),
                    )),
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
    // #281: honor `auto_update_mcp = false` — editors are still detected and
    // reported, but the MCP server is never registered in their configs.
    let update_mcp = crate::core::config::Config::load()
        .setup
        .should_update_mcp();
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

        if !update_mcp {
            editor_step.items.push(SetupItem {
                name: target.name.to_string(),
                status: "skipped".to_string(),
                path: Some(short_path),
                note: Some(format!(
                    "mode={mode}; MCP registration skipped (auto_update_mcp=false)"
                )),
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

    // Step: Agent rules — respect config unless explicitly forced or skipped
    let mut rules_step = SetupStepReport {
        name: "agent_rules".to_string(),
        ok: true,
        items: Vec::new(),
        warnings: Vec::new(),
        errors: Vec::new(),
    };
    let setup_cfg = crate::core::config::Config::load().setup;
    let should_inject = if opts.skip_rules {
        false
    } else if opts.force_inject_rules {
        true
    } else if opts.yes && opts.non_interactive {
        setup_cfg.should_inject_rules()
    } else {
        true
    };

    if should_inject {
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
        if !rules_result.backed_up.is_empty() {
            for bak in &rules_result.backed_up {
                rules_step.items.push(SetupItem {
                    name: "backup".to_string(),
                    status: "created".to_string(),
                    path: Some(bak.clone()),
                    note: Some("previous version backed up".to_string()),
                });
            }
        }
        for e in rules_result.errors {
            rules_step.ok = false;
            rules_step.errors.push(e);
        }
    } else {
        let reason = if opts.skip_rules {
            "--skip-rules flag set"
        } else {
            "auto_inject_rules not enabled (run `lean-ctx setup --inject-rules`)"
        };
        rules_step.items.push(SetupItem {
            name: "agent_rules".to_string(),
            status: "skipped".to_string(),
            path: None,
            note: Some(reason.to_string()),
        });
    }
    steps.push(rules_step);

    // Step: Skill files — respect config
    let mut skill_step = SetupStepReport {
        name: "skill_files".to_string(),
        ok: true,
        items: Vec::new(),
        warnings: Vec::new(),
        errors: Vec::new(),
    };
    let should_install_skills = if opts.skip_rules {
        false
    } else if opts.force_inject_rules {
        true
    } else if opts.yes && opts.non_interactive {
        setup_cfg.should_inject_skills()
    } else {
        true
    };
    if should_install_skills {
        let skill_results = crate::rules_inject::install_all_skills(&home);
        for (name, is_new) in &skill_results {
            skill_step.items.push(SetupItem {
                name: name.clone(),
                status: if *is_new { "installed" } else { "already" }.to_string(),
                path: None,
                note: Some("SKILL.md".to_string()),
            });
        }
    } else {
        skill_step.items.push(SetupItem {
            name: "skill_files".to_string(),
            status: "skipped".to_string(),
            path: None,
            note: Some("auto_inject_skills not enabled".to_string()),
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
        // #281: honor `[setup] auto_update_mcp = false` — register MCP only when
        // enabled; hooks above always install.
        let mcp_note = if setup_cfg.should_update_mcp() {
            match configure_agent_mcp(&target.agent_key) {
                Ok(()) => "; MCP config updated".to_string(),
                Err(e) => format!("; MCP config skipped: {e}"),
            }
        } else {
            "; MCP registration skipped (auto_update_mcp=false)".to_string()
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

    // Step: Tool profile. Deliberately does NOT write a default profile:
    // writing `tool_profile = "standard"` made every install "explicit", which
    // disables the lazy-core advertisement (the lazy core) and ships the full
    // profile schema set (~5-15k tokens) to every session (#575). The lean
    // default needs no config key — all tools stay reachable via ctx_call.
    let mut tool_profile_step = SetupStepReport {
        name: "tool_profile".to_string(),
        ok: true,
        items: Vec::new(),
        warnings: Vec::new(),
        errors: Vec::new(),
    };
    {
        let cfg = crate::core::config::Config::load();
        if cfg.tool_profile.is_none() && std::env::var("LEAN_CTX_TOOL_PROFILE").is_err() {
            let lazy_count = crate::tool_defs::core_tool_names().len();
            tool_profile_step.items.push(SetupItem {
                name: "tool_profile".to_string(),
                status: "lean default".to_string(),
                path: None,
                note: Some(format!(
                    "{lazy_count} tools advertised, all reachable via ctx_call \
                     (pin more with: lean-ctx tools standard|power)"
                )),
            });
        } else {
            let profile = cfg.tool_profile_effective();
            let overhead_hint = match profile {
                crate::core::tool_profiles::ToolProfile::Power => {
                    "; advertises ALL tool schemas — `lean-ctx tools lean` cuts this to the lazy core"
                }
                _ => "",
            };
            tool_profile_step.items.push(SetupItem {
                name: "tool_profile".to_string(),
                status: "already".to_string(),
                path: None,
                note: Some(format!("profile={}{overhead_hint}", profile.as_str())),
            });
        }
    }
    steps.push(tool_profile_step);

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
        let proxy_cfg = crate::core::config::Config::load();
        if proxy_cfg.proxy_enabled == Some(true) {
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
        } else {
            proxy_step.items.push(SetupItem {
                name: "proxy".to_string(),
                status: "skipped".to_string(),
                path: None,
                note: Some(
                    "Proxy not opted-in (run `lean-ctx proxy enable` to activate)".to_string(),
                ),
            });
        }
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
        let has_env_root = std::env::var("LEAN_CTX_PROJECT_ROOT").is_ok_and(|v| !v.is_empty());
        let cfg = crate::core::config::Config::load();
        let has_cfg_root = cfg.project_root.as_ref().is_some_and(|v| !v.is_empty());
        if !has_env_root
            && !has_cfg_root
            && let Ok(cwd) = std::env::current_dir()
        {
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
                        "Set LEAN_CTX_PROJECT_ROOT or add project_root to config.toml".to_string(),
                    ),
                });
                steps.push(root_step);
            }
        }
    }

    // Auto-build property graph if inside any recognized project. The marker
    // probe is TCC-guarded (#356): a launchd-standalone setup run never stats
    // markers under ~/Documents — and `may_autoindex_cwd` additionally skips the
    // probe for a non-standalone CLI refresh whose cwd is in a protected dir.
    if let Ok(cwd) = std::env::current_dir()
        && may_autoindex_cwd(&cwd)
        && crate::core::pathutil::has_project_marker(&cwd)
    {
        spawn_index_build_background(&cwd);
    }

    // IDE config access: the interactive `setup` prompts for informed consent
    // (see run_setup). An explicit `--yes` is itself consent, so enable the
    // registry-derived opt-in if the user has never decided. `--fix` repair runs
    // must never silently widen the jail, so they are left untouched.
    if opts.yes
        && !opts.fix
        && crate::core::config::Config::load()
            .allow_ide_config_dirs
            .is_none()
        && let Err(e) =
            crate::core::config::Config::update_global(|c| c.allow_ide_config_dirs = Some(true))
    {
        tracing::warn!("could not enable IDE config access: {e}");
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

/// #356: decide whether a setup refresh may auto-index `cwd`. Returns `false`
/// for any cwd inside a macOS TCC-protected home dir (`~/Documents`, `~/Desktop`,
/// `~/Downloads`) so a `lean-ctx update` run from a project there never stats
/// marker files in it. That stat pops the macOS privacy prompt when lean-ctx is
/// its own TCC responsible process, and a maintenance refresh has no need to
/// trigger it — the graph builds on the next real tool use anyway. On non-macOS
/// hosts `is_under_tcc_protected_dir` is always `false`, so behaviour is
/// unchanged.
fn may_autoindex_cwd(cwd: &std::path::Path) -> bool {
    !crate::core::pathutil::is_under_tcc_protected_dir(cwd)
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

    let binary = resolve_portable_binary();

    #[cfg(unix)]
    {
        let mut cmd = std::process::Command::new("nice");
        cmd.args(["-n", "19"]);
        if which_ionice_available() {
            cmd.arg("ionice").args(["-c", "3"]);
        }
        cmd.arg(&binary)
            .args(["index", "build", "--root"])
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
            .args(["index", "build", "--root"])
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

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    // #356: a setup refresh (e.g. via `lean-ctx update`) must not auto-index a
    // cwd inside a TCC-protected home dir, or it stats marker files there and
    // pops the macOS privacy prompt. Projects elsewhere index normally.
    #[test]
    fn may_autoindex_cwd_skips_tcc_protected_dirs() {
        let Some(home) = dirs::home_dir() else {
            return;
        };
        assert!(!may_autoindex_cwd(&home.join("Documents/proj")));
        assert!(!may_autoindex_cwd(&home.join("Desktop/proj")));
        assert!(!may_autoindex_cwd(&home.join("Downloads/proj")));
        assert!(may_autoindex_cwd(&home.join("code/proj")));
        assert!(may_autoindex_cwd(std::path::Path::new("/tmp/proj")));
    }

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
        assert!(
            targets
                .iter()
                .all(|t| t.config_type == ConfigType::QoderSettings)
        );
    }
}
