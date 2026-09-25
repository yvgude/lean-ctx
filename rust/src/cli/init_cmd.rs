use crate::hooks::to_bash_compatible_path;

pub(crate) fn quiet_enabled() -> bool {
    crate::core::runtime_flags::quiet_enabled()
}

macro_rules! qprintln {
    ($($t:tt)*) => {
        if !quiet_enabled() {
            println!($($t)*);
        }
    };
}

pub fn cmd_init(args: &[String]) {
    // Safety (#476 class, #1849): asking about init must never *run* it —
    // `init --agent claude --help` used to write the agent's rules file. The
    // guard sits here rather than in the dispatcher so every entry point
    // (`cmd_init_quiet`, `doctor --fix`) inherits it.
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_init_help();
        return;
    }
    // Opt-in value surfaces: each is its own step, never part of the alias setup.
    if let Some(enable) = opt_in_flag(args, "--prompt") {
        let binary = crate::core::portable_binary::stable_shell_binary(
            &crate::core::portable_binary::resolve_portable_binary(),
        );
        super::prompt_init::cmd_init_prompt(enable, &binary);
        return;
    }
    if let Some(enable) = opt_in_flag(args, "--git-trailer") {
        super::git_trailer::cmd_init_git_trailer(enable);
        return;
    }
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
        eprintln!("Unknown hook mode: '{bad}'. Valid: mcp, hybrid, replace");
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

    // Shell hooks outlive the build that wrote them: embed the stable PATH
    // entry, not a versioned package-manager directory (#1851).
    let binary = crate::core::portable_binary::stable_shell_binary(
        &crate::core::portable_binary::resolve_portable_binary(),
    );

    if dry_run {
        let rc = if is_powershell {
            dirs::home_dir().map_or_else(
                || "PowerShell profile".to_string(),
                |h| {
                    crate::shell::platform::resolve_powershell_profile_path(&h)
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
    qprintln!(
        "    claude, cline, codewhale, codex, commandcode, continue, copilot, crush, cursor, emacs, gemini,"
    );
    qprintln!("    grok, hermes, jetbrains, kiro, neovim, omp, openclaw, opencode, pi,");
    qprintln!("    qoder, qodercli, qoderwork, qwen, roo, sublime, trae, verdent, vscode,");
    qprintln!("    windsurf, zed");
    qprintln!("  Modes: mcp, hybrid, replace  (auto-detected per agent, override with --mode)");
}

pub fn cmd_init_quiet(args: &[String]) {
    let _quiet_guard = crate::core::runtime_flags::scoped_quiet();
    cmd_init(args);
}

/// `--flag` → enable, `--flag off` / `--flag=off` → disable, absent → `None`.
fn opt_in_flag(args: &[String], flag: &str) -> Option<bool> {
    let pos = args
        .iter()
        .position(|a| a == flag || a.starts_with(&format!("{flag}=")))?;
    let value = args[pos]
        .strip_prefix(&format!("{flag}="))
        .or_else(|| args.get(pos + 1).map(String::as_str));
    Some(!matches!(value, Some("off" | "false" | "0")))
}

/// Help for `lean-ctx init`. Printed for `--help`/`-h`, whatever else is on
/// the line, and never followed by an init.
fn print_init_help() {
    println!("Usage: lean-ctx init [options]");
    println!("       lean-ctx init <bash|zsh|fish|powershell|pwsh>");
    println!();
    println!("Installs the shell aliases, or connects an AI tool with --agent.");
    println!("The second form prints the shell hook to stdout for `eval` and writes nothing.");
    println!();
    println!("Options:");
    println!("  --global, -g        Install the shell aliases into your shell profile");
    println!("  --agent <tool>      Configure an AI tool: MCP, hooks and rules (repeatable)");
    println!("  --mode <mode>       Hook mode for --agent: mcp, hybrid or replace");
    println!("                      (auto-detected per agent when omitted)");
    println!("  --project           With --agent: also install project-local hooks");
    println!("  --no-shell-hook     Skip the shell aliases; MCP tools stay active");
    println!("  --dry-run           Show what the shell setup would change, change nothing");
    println!("  --prompt [off]      Show what lean-ctx did in your shell prompt (zsh, bash,");
    println!("                      fish; prints a module for Starship). `off` removes it");
    println!("  --git-trailer [off] Add a `lean-ctx:` trailer to this repo's commit messages.");
    println!("                      `off` removes the hook");
    println!("  --help, -h          Show this help (never runs init)");
    println!();
    println!("Examples:");
    println!("  lean-ctx init --global --dry-run");
    println!("  lean-ctx init --global");
    println!("  lean-ctx init --prompt");
    println!("  lean-ctx init --agent claude");
    println!("  lean-ctx init --agent codex --mode hybrid");
}

#[cfg(test)]
mod tests {
    use super::opt_in_flag;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn opt_in_flags_parse_on_and_off() {
        assert_eq!(opt_in_flag(&args(&["--prompt"]), "--prompt"), Some(true));
        assert_eq!(
            opt_in_flag(&args(&["--prompt", "off"]), "--prompt"),
            Some(false)
        );
        assert_eq!(
            opt_in_flag(&args(&["--prompt=false"]), "--prompt"),
            Some(false)
        );
        assert_eq!(
            opt_in_flag(&args(&["--git-trailer", "0"]), "--git-trailer"),
            Some(false)
        );
        assert_eq!(opt_in_flag(&args(&["--global"]), "--prompt"), None);
        // `--prompt-foo` is not `--prompt`.
        assert_eq!(opt_in_flag(&args(&["--prompt-foo"]), "--prompt"), None);
    }
}
