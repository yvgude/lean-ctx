// SPDX-License-Identifier: Apache-2.0
use lean_ctx_hooks::hooks::to_bash_compatible_path;

pub(crate) fn quiet_enabled() -> bool {
    lean_ctx::core::runtime_flags::quiet_enabled()
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
        let binary = lean_ctx::core::portable_binary::stable_shell_binary(
            &lean_ctx::core::portable_binary::resolve_portable_binary(),
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
        || lean_ctx::core::config::Config::load().shell_hook_disabled_effective();

    let explicit_mode = args
        .windows(2)
        .find(|w| w[0] == "--mode")
        .and_then(|w| lean_ctx_hooks::hooks::HookMode::from_str_loose(&w[1]));
    let governor_arm = args.windows(2).find(|w| w[0] == "--mode").and_then(|w| {
        lean_ctx_hooks::hooks::agents::claude_governor::GovernorArm::from_mode(&w[1])
    });

    if args.windows(2).any(|w| w[0] == "--mode")
        && explicit_mode.is_none()
        && governor_arm.is_none()
    {
        let bad = args
            .windows(2)
            .find(|w| w[0] == "--mode")
            .map_or("?", |w| w[1].as_str());
        eprintln!(
            "Unknown hook mode: '{bad}'. Valid: mcp, hybrid, replace, governor, governor-mcp"
        );
        std::process::exit(1);
    }

    let agents: Vec<&str> = args
        .windows(2)
        .filter(|w| w[0] == "--agent")
        .map(|w| w[1].as_str())
        .collect();

    // `--dry-run` with `--agent` must write nothing: the agent and governor
    // paths below run before the shell-hook dry run (reimplements community
    // PR #2169 by ZacKienzle2).
    if dry_run && !agents.is_empty() {
        let mode = args
            .windows(2)
            .find(|w| w[0] == "--mode")
            .map_or("default", |w| w[1].as_str());
        qprintln!("\nlean-ctx init --dry-run\n");
        qprintln!("  Would configure: {} (mode: {mode})", agents.join(", "));
        qprintln!("\n  Run without --dry-run to apply.");
        return;
    }

    if !agents.is_empty() {
        // An explicit agent setup: an older build's silent refresh yields to it.
        lean_ctx_hooks::hooks::setup_writer::record();
    }
    if let Some(arm) = governor_arm {
        init_governor(&agents, arm);
        return;
    }
    // An explicit standard mode is a pinned choice (no later update switches
    // it to the governor) and switches a governor install back first.
    if let Some(mode) = explicit_mode
        && let Some(home) = lean_ctx::core::home::resolve_home_dir()
    {
        let mode = mode.to_string().to_ascii_lowercase();
        for agent in &agents {
            let Some(host) = lean_ctx_hooks::hooks::governor_default::host(agent) else {
                continue;
            };
            lean_ctx_hooks::hooks::governor_default::pin(host, Some(&mode));
            let reverted = if host.key == "claude" {
                lean_ctx_hooks::hooks::agents::claude_governor::revert(&home)
            } else {
                lean_ctx_hooks::hooks::agents::governor_hosts::governor_available(agent)
                    .is_some_and(|spec| (spec.revert)(&home))
            };
            if reverted {
                qprintln!("  ✓ governor setup reverted for {}", host.name);
            }
        }
    }

    if !agents.is_empty() {
        let cwd = std::env::current_dir().unwrap_or_default();
        for agent_name in &agents {
            if explicit_mode.is_none() && init_governor_default(agent_name) {
                continue;
            }
            let mode = explicit_mode
                .unwrap_or_else(|| lean_ctx_hooks::hooks::recommend_hook_mode(agent_name));
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
            if result.errors.is_empty()
                && let Err(e) = crate::setup::agent_selection::remember_connected_agent(agent_name)
            {
                eprintln!("  ✗ {agent_name}: {e}");
            }
            if agent_name.eq_ignore_ascii_case("hermes") {
                qprintln!("\n  Beyond MCP, lean-ctx can be Hermes' active context engine");
                qprintln!("  (replaces the built-in ContextCompressor). Install the plugin from");
                qprintln!("  integrations/hermes-lean-ctx (scripts/install.sh), then set");
                qprintln!("  context.engine: \"lean-ctx\" in ~/.hermes/config.yaml.");
            }
            if project {
                lean_ctx_hooks::hooks::install_agent_project_hooks(agent_name, &cwd);
            }
        }
        if !global {
            lean_ctx_hooks::hooks::install_project_rules_for_agents(&agents);
        }
        qprintln!("\nRun 'lean-ctx cockpit' after using some commands to see your savings.");
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
    let binary = lean_ctx::core::portable_binary::stable_shell_binary(
        &lean_ctx::core::portable_binary::resolve_portable_binary(),
    );

    if dry_run {
        let rc = if is_powershell {
            dirs::home_dir().map_or_else(
                || "PowerShell profile".to_string(),
                |h| {
                    lean_ctx::shell::platform::resolve_powershell_profile_path(&h)
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
        let data_dir = lean_ctx::core::data_dir::lean_ctx_data_dir().map_or_else(
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

    if let Ok(lean_dir) = lean_ctx::core::data_dir::lean_ctx_data_dir()
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
        "    claude, cline, codewhale, codex, commandcode, continue, copilot, crush, cursor, emacs, gemini, goose,"
    );
    qprintln!("    grok, hermes, jetbrains, kiro, muse, neovim, omp, openclaw, opencode, pi,");
    qprintln!("    qoder, qodercli, qoderwork, qwen, roo, sublime, trae, verdent, vscode,");
    qprintln!("    windsurf, zed");
    qprintln!("  Modes: mcp, hybrid, replace  (auto-detected per agent, override with --mode)");
    qprintln!("  New Claude Code installs get governor-mcp; existing setups keep their mode");
}

pub fn cmd_init_quiet(args: &[String]) {
    let _quiet_guard = lean_ctx::core::runtime_flags::scoped_quiet();
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

fn is_claude_agent(agent: &str) -> bool {
    matches!(
        agent.to_ascii_lowercase().as_str(),
        "claude" | "claude-code"
    )
}

/// `init --agent <host> --mode governor|governor-mcp`: the zero-overhead
/// governor profile (`governor-mcp` is Claude Code only).
fn init_governor(
    agents: &[&str],
    arm: lean_ctx_hooks::hooks::agents::claude_governor::GovernorArm,
) {
    let Some(home) = lean_ctx::core::home::resolve_home_dir() else {
        eprintln!("  ✗ cannot resolve the home directory");
        std::process::exit(1);
    };
    if agents.is_empty() {
        eprintln!("  ✗ --mode governor needs --agent <host>");
        std::process::exit(1);
    }
    for agent in agents {
        if !is_claude_agent(agent) {
            init_host_governor(agent, arm, &home);
            continue;
        }
        lean_ctx_hooks::hooks::agents::claude_governor::install(&home, arm);
        if let Some(host) = lean_ctx_hooks::hooks::governor_default::host(agent) {
            // A chosen governor is pinned like a chosen standard mode: no
            // update or repair of the 3.12.1 migration switches it back.
            lean_ctx_hooks::hooks::governor_default::pin(host, Some(arm.label()));
        }
        qprintln!("  ✓ Claude Code governor setup installed (revert: --mode hybrid)");
        if let Err(e) = crate::setup::agent_selection::remember_connected_agent(agent) {
            eprintln!("  ✗ {agent}: {e}");
        }
    }
}

/// `init --agent <host>` without `--mode` on a host with a governor: apply the
/// default (a new Claude Code install gets governor-mcp; an automatic 3.12.1
/// migration is switched back). Returns whether the host is handled; `false`
/// runs the standard setup (in the mode `recommend_hook_mode` reads, which
/// includes the host's pin).
fn init_governor_default(agent: &str) -> bool {
    use lean_ctx_hooks::hooks::governor_default::{Outcome, apply_default, host};
    let (Some(host), Some(home)) = (host(agent), lean_ctx::core::home::resolve_home_dir()) else {
        return false;
    };
    let outcome = apply_default(host, &home);
    if outcome.ran_steps() {
        let _ = crate::setup::agent_selection::remember_connected_agent(host.key);
    }
    if let Some(notice) = outcome.notice(host) {
        qprintln!("  ✓ {notice}");
    } else if outcome == Outcome::AlreadyGovernor {
        (host.refresh)(&home);
        qprintln!("  ✓ {} governor setup up to date", host.name);
    }
    !outcome.standard_setup_runs()
}

/// `init --agent <host> --mode governor` for a host other than Claude Code.
fn init_host_governor(
    agent: &str,
    arm: lean_ctx_hooks::hooks::agents::claude_governor::GovernorArm,
    home: &std::path::Path,
) {
    use lean_ctx_hooks::hooks::agents::{claude_governor::GovernorArm, governor_hosts};
    let Some(spec) = governor_hosts::governor_available(agent) else {
        eprintln!("  • {agent}: no tool hooks LeanCTX can govern; keeps the MCP setup");
        return;
    };
    if arm == GovernorArm::McpDeferred {
        eprintln!("  • {agent}: governor-mcp is Claude Code only; use --mode governor");
        return;
    }
    governor_hosts::install(spec, home);
    // A chosen governor is pinned, so the repair of 3.12.1's automatic
    // migration never switches it back.
    if let Some(host) = lean_ctx_hooks::hooks::governor_default::host(agent) {
        lean_ctx_hooks::hooks::governor_default::pin(host, Some(arm.label()));
    }
    qprintln!(
        "  ✓ {} {} setup installed (switch back: {})",
        spec.name,
        spec.class.label(),
        spec.switch_back
    );
    if let Err(e) = crate::setup::agent_selection::remember_connected_agent(agent) {
        eprintln!("  ✗ {agent}: {e}");
    }
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
    println!("                      (auto-detected per agent when omitted). A new Claude");
    println!("                      Code install gets governor-mcp (governor hooks + MCP");
    println!("                      server); existing setups keep their mode. Claude Code,");
    println!("                      CodeBuddy, Cursor, Codex, Gemini and Copilot CLI also");
    println!("                      take governor; an explicit mode is kept by later updates");
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

    /// `init --agent claude` after an automatic 3.12.1 governor migration
    /// (backups next to the files, governor hooks, no MCP entry) restores the
    /// standard setup and pins it, so the next update leaves it alone.
    #[test]
    fn init_restores_an_automatically_migrated_setup_pinned() {
        use lean_ctx::test_env;
        use lean_ctx_hooks::hooks::agents::claude_governor;
        use lean_ctx_hooks::hooks::governor_default::{Outcome, apply_default, host, pinned_mode};

        let _iso = lean_ctx::core::data_dir::isolated_data_dir();
        let home = tempfile::tempdir().unwrap();
        let prev_home = std::env::var_os("HOME");
        test_env::set_var("HOME", home.path());
        test_env::set_var("LEAN_CTX_HOOK_BINARY", "lean-ctx");
        for var in ["CLAUDE_CONFIG_DIR", "LEAN_CTX_HOOK_MODE", "LEAN_CTX_QUIET"] {
            test_env::remove_var(var);
        }
        let h = home.path();
        let read = |p: &str| std::fs::read_to_string(h.join(p)).unwrap_or_default();
        let mcp = || serde_json::from_str::<serde_json::Value>(&read(".claude.json")).unwrap();
        // The standard install the migration found, backed up as 3.12.1 did.
        std::fs::create_dir_all(h.join(".claude")).unwrap();
        std::fs::write(
            h.join(".claude/settings.json"),
            r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"lean-ctx hook rewrite"}]},{"matcher":"Read|Grep|Glob","hooks":[{"type":"command","command":"lean-ctx hook redirect"}]}]},"permissions":{"deny":["Grep","Glob"]}}"#,
        )
        .unwrap();
        std::fs::write(
            h.join(".claude.json"),
            r#"{"projects":{"/x":{}},"mcpServers":{"lean-ctx":{"command":"lean-ctx","alwaysLoad":true},"other":{"command":"o"}}}"#,
        )
        .unwrap();
        std::fs::write(
            h.join(".claude/CLAUDE.md"),
            "# My notes\nkeep me\n\n<!-- lean-ctx -->\nstandard block: use ctx_read\n<!-- /lean-ctx -->\n",
        )
        .unwrap();
        for f in [".claude/settings.json", ".claude.json", ".claude/CLAUDE.md"] {
            std::fs::copy(h.join(f), h.join(format!("{f}.pre-governor-3.12.1.bak"))).unwrap();
        }
        claude_governor::install(h, claude_governor::GovernorArm::NoMcp);
        assert!(mcp().pointer("/mcpServers/lean-ctx").is_none(), "migrated");

        super::cmd_init_quiet(&args(&["--agent", "claude", "--global"]));

        assert!(!claude_governor::is_installed(h));
        let settings = read(".claude/settings.json");
        assert!(settings.contains("lean-ctx hook rewrite"), "{settings}");
        assert!(!settings.contains("hook govern"), "{settings}");
        assert!(mcp().pointer("/mcpServers/lean-ctx").is_some(), "MCP back");
        assert!(mcp().pointer("/mcpServers/other").is_some(), "foreign kept");
        assert!(
            settings.contains("\"Grep\""),
            "the deny returns with the MCP entry: {settings}"
        );
        assert!(read(".claude/CLAUDE.md").starts_with("# My notes\nkeep me\n"));
        assert_eq!(
            pinned_mode(host("claude").unwrap()).as_deref(),
            Some("hybrid")
        );
        // The next update leaves the pinned choice alone.
        let before = (read(".claude/settings.json"), read(".claude/CLAUDE.md"));
        assert_eq!(
            apply_default(host("claude").unwrap(), h),
            Outcome::Pinned("hybrid".into())
        );
        assert_eq!(
            (read(".claude/settings.json"), read(".claude/CLAUDE.md")),
            before
        );

        test_env::remove_var("LEAN_CTX_HOOK_BINARY");
        match prev_home {
            Some(p) => test_env::set_var("HOME", p),
            None => test_env::remove_var("HOME"),
        }
    }

    /// `init --agent claude` on a new home installs governor-mcp: governor
    /// hooks plus the lean-ctx MCP server with deferred tools.
    #[test]
    fn a_new_claude_install_gets_governor_mcp() {
        use lean_ctx::test_env;
        use lean_ctx_hooks::hooks::agents::claude_governor;

        let _iso = lean_ctx::core::data_dir::isolated_data_dir();
        let home = tempfile::tempdir().unwrap();
        let prev_home = std::env::var_os("HOME");
        test_env::set_var("HOME", home.path());
        test_env::set_var("LEAN_CTX_HOOK_BINARY", "lean-ctx");
        for var in ["CLAUDE_CONFIG_DIR", "LEAN_CTX_HOOK_MODE", "LEAN_CTX_QUIET"] {
            test_env::remove_var(var);
        }
        let h = home.path();
        std::fs::create_dir_all(h.join(".claude")).unwrap();

        super::cmd_init_quiet(&args(&["--agent", "claude", "--global"]));

        assert_eq!(
            claude_governor::installed_arm(h),
            Some(claude_governor::GovernorArm::McpDeferred)
        );
        let mcp: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(h.join(".claude.json")).unwrap())
                .unwrap();
        assert_eq!(mcp["mcpServers"]["lean-ctx"]["alwaysLoad"], false);

        test_env::remove_var("LEAN_CTX_HOOK_BINARY");
        match prev_home {
            Some(p) => test_env::set_var("HOME", p),
            None => test_env::remove_var("HOME"),
        }
    }
}
