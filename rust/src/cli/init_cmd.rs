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
    let dry_run = args.iter().any(|a| a == "--dry-run");
    let no_hook = args.iter().any(|a| a == "--no-shell-hook")
        || crate::core::config::Config::load().shell_hook_disabled_effective();

    let agents: Vec<&str> = args
        .windows(2)
        .filter(|w| w[0] == "--agent")
        .map(|w| w[1].as_str())
        .collect();

    if !agents.is_empty() {
        for agent_name in &agents {
            crate::hooks::install_agent_hook(agent_name, global);
            if let Err(e) = crate::setup::configure_agent_mcp(agent_name) {
                eprintln!("MCP config for '{agent_name}' not updated: {e}");
            }
        }
        if !global {
            crate::hooks::install_project_rules();
        }
        qprintln!("\nRun 'lean-ctx gain' after using some commands to see your savings.");
        return;
    }

    let eval_shell = args
        .iter()
        .find(|a| matches!(a.as_str(), "bash" | "zsh" | "fish" | "powershell" | "pwsh"));
    if let Some(shell) = eval_shell {
        if !global {
            super::shell_init::print_hook_stdout(shell);
            return;
        }
    }

    let shell_name = std::env::var("SHELL").unwrap_or_default();
    let is_zsh = shell_name.contains("zsh");
    let is_fish = shell_name.contains("fish");
    let is_powershell = cfg!(windows) && shell_name.is_empty();

    let binary = crate::core::portable_binary::resolve_portable_binary();

    if dry_run {
        let rc = if is_powershell {
            "Documents/PowerShell/Microsoft.PowerShell_profile.ps1".to_string()
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

    if let Ok(lean_dir) = crate::core::data_dir::lean_ctx_data_dir() {
        if !lean_dir.exists() {
            let _ = std::fs::create_dir_all(&lean_dir);
            qprintln!("Created {}", lean_dir.display());
        }
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
    qprintln!("For AI tool integration: lean-ctx init --agent <tool>");
    qprintln!("  Supported: aider, amazonq, amp, antigravity, claude, cline, codex, copilot,");
    qprintln!("    crush, cursor, emacs, gemini, hermes, jetbrains, kiro, neovim, opencode,");
    qprintln!("    pi, qoder, qoderwork, qwen, roo, sublime, trae, verdent, windsurf");
}

pub fn cmd_init_quiet(args: &[String]) {
    std::env::set_var("LEAN_CTX_QUIET", "1");
    cmd_init(args);
    std::env::remove_var("LEAN_CTX_QUIET");
}
