use crate::hooks::to_bash_compatible_path;

pub(crate) fn quiet_enabled() -> bool {
    matches!(std::env::var("LEAN_CTX_QUIET"), Ok(v) if v.trim() == "1")
}

macro_rules! qprintln {
    ($($t:tt)*) => {
        if !quiet_enabled() {
            println!($($t)*);
        }
    };
}

pub fn cmd_init(args: &[String]) {
    let global = args.iter().any(|a| a == "--global" || a == "-g");
    let project = args.iter().any(|a| a == "--project");
    let dry_run = args.iter().any(|a| a == "--dry-run");
    let no_hook = args.iter().any(|a| a == "--no-shell-hook")
        || crate::core::config::Config::load().shell_hook_disabled_effective();

    let explicit_mode = args
        .windows(2)
        .find(|w| w[0] == "--mode")
        .and_then(|w| crate::hooks::HookMode::from_str_loose(&w[1]));

    if args.windows(2).any(|w| w[0] == "--mode")
        && !args
            .windows(2)
            .any(|w| w[0] == "--mode" && crate::hooks::HookMode::from_str_loose(&w[1]).is_some())
    {
        let bad = args
            .windows(2)
            .find(|w| w[0] == "--mode")
            .map_or("?", |w| w[1].as_str());
        eprintln!("Unknown hook mode: '{bad}'. Valid: mcp, hybrid");
        std::process::exit(1);
    }

    let agents: Vec<&str> = args
        .windows(2)
        .filter(|w| w[0] == "--agent")
        .map(|w| w[1].as_str())
        .collect();

    if !agents.is_empty() {
        let cwd = std::env::current_dir().unwrap_or_default();
        for agent_name in &agents {
            let mode =
                explicit_mode.unwrap_or_else(|| crate::hooks::recommend_hook_mode(agent_name));
            let result = crate::setup::setup_single_agent(agent_name, global, mode);
            for name in &result.rules.injected {
                qprintln!("  ✓ {name} rules injected");
            }
            for name in &result.rules.updated {
                qprintln!("  ✓ {name} rules updated");
            }
            for name in &result.rules.already {
                qprintln!("  ✓ {name} rules up-to-date");
            }
            if result.skill_installed {
                qprintln!("  ✓ SKILL.md installed for {agent_name}");
            }
            if result.mcp_skipped {
                qprintln!("  • MCP registration skipped for {agent_name} (auto_update_mcp=false)");
            }
            for e in &result.errors {
                eprintln!("  ✗ {agent_name}: {e}");
            }
            if agent_name.eq_ignore_ascii_case("hermes") {
                qprintln!("\n  Beyond MCP, lean-ctx can be Hermes' active context engine");
                qprintln!("  (replaces the built-in ContextCompressor). Install the plugin from");
                qprintln!("  integrations/hermes-lean-ctx (scripts/install.sh), then set");
                qprintln!("  context.engine: \"lean-ctx\" in ~/.hermes/config.yaml.");
            }
            if project {
                crate::hooks::install_agent_project_hooks(agent_name, &cwd);
            }
        }
        if !global {
            crate::hooks::install_project_rules_for_agents(&agents);
        }
        qprintln!("\nRun 'lean-ctx gain' after using some commands to see your savings.");
        return;
    }

    let eval_shell = args
        .iter()
        .find(|a| matches!(a.as_str(), "bash" | "zsh" | "fish" | "powershell" | "pwsh"));
    if let Some(shell) = eval_shell
        && !global
    {
        super::shell_init::print_hook_stdout(shell);
        return;
    }

    let shell_name = std::env::var("SHELL").unwrap_or_default();
    let is_zsh = shell_name.contains("zsh");
    let is_fish = shell_name.contains("fish");
    let is_powershell = cfg!(windows) && shell_name.is_empty();

    let binary = crate::core::portable_binary::resolve_portable_binary();

    if dry_run {
        let rc = if is_powershell {
            dirs::home_dir().map_or_else(
                || "PowerShell profile".to_string(),
                |h| {
                    crate::shell::platform::powershell_profile_path(&h)
                        .to_string_lossy()
                        .into_owned()
                },
            )
        } else if is_fish {
            "~/.config/fish/config.fish".to_string()
        } else if is_zsh {
            "~/.zshrc".to_string()
        } else {
            "~/.bashrc".to_string()
        };
        qprintln!("\nlean-ctx init --dry-run\n");
        qprintln!("  Would modify:  {rc}");
        qprintln!("  Would backup:  {rc}.lean-ctx.bak");
        qprintln!("  Would alias:   git npm pnpm yarn cargo docker docker-compose kubectl");
        qprintln!("                 gh pip pip3 ruff go golangci-lint eslint prettier tsc");
        qprintln!("                 curl wget php composer (24 commands + k)");
        let data_dir = crate::core::data_dir::lean_ctx_data_dir().map_or_else(
            |_| "~/.config/lean-ctx/".to_string(),
            |p| p.to_string_lossy().to_string(),
        );
        qprintln!("  Would create:  {data_dir}");
        qprintln!("  Binary:        {binary}");
        qprintln!("\n  Safety: aliases auto-fallback to original command if lean-ctx is removed.");
        qprintln!("\n  Run without --dry-run to apply.");
        return;
    }

    if no_hook {
        qprintln!("Shell hook disabled (--no-shell-hook or shell_hook_disabled config).");
        qprintln!("MCP tools remain active. Set LEAN_CTX_NO_HOOK=1 to disable at runtime.");
    } else if is_powershell {
        super::shell_init::init_powershell(&binary);
    } else {
        let bash_binary = to_bash_compatible_path(&binary);
        if is_fish {
            super::shell_init::init_fish(&bash_binary);
        } else {
            super::shell_init::init_posix(is_zsh, &bash_binary);
        }
    }

    if let Ok(lean_dir) = crate::core::data_dir::lean_ctx_data_dir()
        && !lean_dir.exists()
    {
        let _ = std::fs::create_dir_all(&lean_dir);
        qprintln!("Created {}", lean_dir.display());
    }

    let rc = if is_powershell {
        "$PROFILE"
    } else if is_fish {
        "config.fish"
    } else if is_zsh {
        ".zshrc"
    } else {
        ".bashrc"
    };

    qprintln!("\nlean-ctx init complete (24 aliases installed)");
    qprintln!();
    qprintln!("  Disable temporarily:  lean-ctx-off");
    qprintln!("  Re-enable:            lean-ctx-on");
    qprintln!("  Check status:         lean-ctx-status");
    qprintln!("  Full uninstall:       lean-ctx uninstall");
    qprintln!("  Diagnose issues:      lean-ctx doctor");
    qprintln!("  Preview changes:      lean-ctx init --global --dry-run");
    qprintln!();
    if is_powershell {
        qprintln!("  Restart PowerShell or run: . {rc}");
    } else {
        qprintln!("  Restart your shell or run: source ~/{rc}");
    }
    qprintln!();
    qprintln!("For AI tool integration: lean-ctx init --agent <tool> [--mode <mode>]");
    qprintln!("  Supported: aider, amazonq, amp, antigravity, antigravity-cli, augment,");
    qprintln!("    claude, cline, codex, continue, copilot, crush, cursor, emacs, gemini,");
    qprintln!("    hermes, jetbrains, kiro, neovim, openclaw, opencode, pi, qoder,");
    qprintln!("    qoderwork, qwen, roo, sublime, trae, verdent, vscode, windsurf, zed");
    qprintln!("  Modes: mcp, hybrid  (auto-detected per agent, override with --mode)");
}

pub fn cmd_init_quiet(args: &[String]) {
    // SAFETY: the `init` CLI command is single-threaded; no other thread reads
    // the environment between this set and the matching remove below.
    unsafe { std::env::set_var("LEAN_CTX_QUIET", "1") };
    cmd_init(args);
    // SAFETY: single-threaded CLI command; pairs with the set above.
    unsafe { std::env::remove_var("LEAN_CTX_QUIET") };
}
