// SPDX-License-Identifier: Apache-2.0
//! `setup`, `onboard`, `install` and `bootstrap`: argument parsing (including
//! the `--agents` selection and `--dry-run`) and the setup entry points.

use crate::{core, setup};

pub(super) fn handle_setup(rest: &[String]) {
    if rest.first().map(String::as_str) == Some("codex-protected") {
        let code = crate::cli::cmd_setup_codex_protected(&rest[1..]);
        if code != 0 {
            std::process::exit(code);
        }
        return;
    }
    // Safety (#476 class): `--help`/`-h` — or any unknown flag — must NEVER
    // fall through to a real setup run that mutates shell + agent configs.
    if rest.iter().any(|a| a == "--help" || a == "-h") {
        print_setup_help();
        return;
    }
    if rest == ["runtime", "configure"] || rest == ["runtime", "configure", "--staging"] {
        if !setup::configure_runtime() {
            std::process::exit(2);
        }
        return;
    }
    if rest.first().map(String::as_str) == Some("runtime") {
        crate::cli::cmd_engine(rest);
        return;
    }
    const KNOWN: &[&str] = &[
        "--non-interactive",
        "--yes",
        "-y",
        "--fix",
        "--json",
        "--no-auto-approve",
        "--skip-rules",
        "--no-agent-aliases",
        "--dry-run",
    ];
    let (rest, agents) = take_agents_arg("setup", rest);
    if let Some(unknown) = rest.iter().find(|a| !KNOWN.contains(&a.as_str())) {
        eprintln!("setup: unknown flag or argument '{unknown}'\n");
        print_setup_help();
        std::process::exit(2);
    }
    let non_interactive = rest.iter().any(|a| a == "--non-interactive");
    let yes = rest.iter().any(|a| a == "--yes" || a == "-y");
    let fix = rest.iter().any(|a| a == "--fix");
    let json = rest.iter().any(|a| a == "--json");
    let no_auto_approve = rest.iter().any(|a| a == "--no-auto-approve");
    let skip_rules = rest.iter().any(|a| a == "--skip-rules");
    let no_agent_aliases = rest.iter().any(|a| a == "--no-agent-aliases");
    let dry_run = rest.iter().any(|a| a == "--dry-run");

    if no_agent_aliases && !dry_run {
        let _ = crate::core::config::setter::set_by_key("skip_agent_aliases", "true");
    }

    if dry_run {
        // A plan never prompts: it shows the recorded (or --agents) selection.
        let opts = setup::SetupOptions {
            non_interactive: true,
            yes: true,
            fix,
            json,
            no_auto_approve,
            skip_rules,
            agents,
            dry_run,
            ..Default::default()
        };
        match setup::run_setup_with_options(&opts) {
            Ok(report) if json => println!(
                "{}",
                serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".to_string())
            ),
            Ok(report) => setup::print_plan(&report),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
    } else if non_interactive || fix || json || yes {
        let opts = setup::SetupOptions {
            non_interactive,
            yes,
            fix,
            json,
            no_auto_approve,
            skip_rules,
            agents,
            ..Default::default()
        };
        run_setup_options(&opts, json);
    } else {
        setup::run_setup_with_agents(agents);
    }
}

/// Split `--agents <list>` off `args` and validate it against the known agent
/// keys. A missing value or an unknown name exits with status 2 before
/// anything is written.
fn take_agents_arg(
    command: &str,
    args: &[String],
) -> (Vec<String>, Option<std::collections::BTreeSet<String>>) {
    use setup::agent_selection::{known_agent_keys, parse_agent_list, take_agents_flag};

    let fail = |e: String| -> ! {
        eprintln!("{command}: {e}");
        std::process::exit(2);
    };
    let (rest, raw) = take_agents_flag(args).unwrap_or_else(|e| fail(e));
    let Some(raw) = raw else {
        return (rest, None);
    };
    let home = core::home::resolve_home_dir()
        .unwrap_or_else(|| fail("cannot determine home directory".to_string()));
    let known = known_agent_keys(&core::editor_registry::build_targets(&home));
    let keys = parse_agent_list(&raw, &known).unwrap_or_else(|e| fail(e));
    (rest, Some(keys))
}

pub(super) fn handle_onboard(rest: &[String]) {
    if rest.iter().any(|a| a == "--help" || a == "-h") {
        println!("Usage: lean-ctx onboard [--agents <list>] [--dry-run] [--no-agent-aliases]");
        println!("Connect your AI tools with one command: detects installed");
        println!("agents, installs hooks/rules/MCP registrations, verifies.");
        println!("On a terminal it first asks which detected tools to connect.");
        println!();
        println!("  --agents <list>     Connect only these tools (e.g. claude,cursor; or none)");
        println!("                      and remember the choice for updates; `all` connects");
        println!("                      (and remembers) every tool installed now");
        println!("  --dry-run           Show what would be written; change nothing");
        println!("  --yes, -y           Do not ask; connect every detected tool (or the");
        println!("                      recorded choice)");
        println!("  --non-interactive   Do not ask; act as without a terminal");
        println!("  --no-agent-aliases  Do not install claude/codex/gemini shell aliases");
        println!();
        println!("Without a terminal and without a recorded choice (npm install, CI) onboard");
        println!("configures no new tool: it refreshes only tools that already use lean-ctx.");
        println!("Environment: LEAN_CTX_AGENTS=claude,cursor (or all) works like --agents.");
        println!("Fine-grained control: lean-ctx setup --help");
        return;
    }
    let mut args = rest.to_vec();
    // install.sh and the npm postinstall pass the selection through the
    // environment; an explicit --agents wins.
    if let Ok(env_agents) = std::env::var("LEAN_CTX_AGENTS")
        && !env_agents.trim().is_empty()
        && !args
            .iter()
            .any(|a| a == "--agents" || a.starts_with("--agents="))
    {
        args.push(format!("--agents={env_agents}"));
    }
    // `all` is onboard's explicit opt-in to every tool installed right now; it
    // is recorded as that list, so later updates still never pick up a tool
    // installed afterwards.
    let wants_all = setup::agent_selection::take_agents_flag(&args)
        .ok()
        .and_then(|(_, raw)| raw)
        .is_some_and(|raw| raw.trim().eq_ignore_ascii_case("all"));
    let (rest, agents) = if wants_all {
        let (rest, _) = setup::agent_selection::take_agents_flag(&args).unwrap_or_default();
        (rest, None)
    } else {
        take_agents_arg("onboard", &args)
    };
    if let Some(unknown) = rest.iter().find(|a| {
        !matches!(
            a.as_str(),
            "--no-agent-aliases" | "--dry-run" | "--yes" | "-y" | "--non-interactive"
        )
    }) {
        eprintln!("onboard: unknown flag or argument '{unknown}' (see: lean-ctx onboard --help)");
        std::process::exit(2);
    }
    let dry_run = rest.iter().any(|a| a == "--dry-run");
    let yes = rest.iter().any(|a| matches!(a.as_str(), "--yes" | "-y"));
    let no_prompt = rest.iter().any(|a| a == "--non-interactive");
    if !dry_run && rest.iter().any(|a| a == "--no-agent-aliases") {
        let _ = crate::core::config::setter::set_by_key("skip_agent_aliases", "true");
    }
    let request = match agents {
        Some(keys) => setup::OnboardAgents::Only(keys),
        None if wants_all => setup::OnboardAgents::AllDetected,
        None if yes => setup::OnboardAgents::Yes,
        None => setup::OnboardAgents::Unspecified,
    };
    setup::run_onboard_with(&request, dry_run, no_prompt);
}

pub(super) fn handle_install(rest: &[String]) {
    // Plain `lean-ctx install` is a natural thing to type after installing the
    // binary; keep it as guided setup unless repair mode was explicitly asked.
    let repair = rest.iter().any(|a| a == "--repair" || a == "--fix");
    let json = rest.iter().any(|a| a == "--json");
    if !repair {
        setup::run_setup();
        return;
    }
    run_repair_setup(json);
}

pub(super) fn handle_bootstrap(rest: &[String]) {
    let json = rest.iter().any(|a| a == "--json");
    run_repair_setup(json);
}

fn run_repair_setup(json: bool) {
    let opts = setup::SetupOptions {
        non_interactive: true,
        yes: true,
        fix: true,
        json,
        ..Default::default()
    };
    run_setup_options(&opts, json);
}

fn run_setup_options(opts: &setup::SetupOptions, json: bool) {
    match setup::run_setup_with_options(opts) {
        Ok(report) => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".to_string())
                );
            }
            if !report.success {
                std::process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

/// Help for `lean-ctx setup`. Printed for `--help`/`-h` and unknown flags so
/// asking about setup can never accidentally *run* setup (#476 class, #658).
fn print_setup_help() {
    println!("Usage: lean-ctx setup [options]");
    println!();
    println!("Connect LeanCTX to your AI tools: registers the MCP server, installs agent");
    println!("hooks and rules, and adds the shell hook. Interactive by default; runs");
    println!("non-interactively without a TTY.");
    println!();
    println!("Options:");
    println!("  --non-interactive   No prompts; apply defaults");
    println!("  --yes, -y           Assume yes for all prompts");
    println!("  --fix               Repair an existing installation");
    println!("  --json              Machine-readable report (implies non-interactive)");
    println!("  --no-auto-approve   Skip auto-approve configuration");
    println!("  --skip-rules        Do not write agent rules files");
    println!("  --agents <list>     Configure only these tools (e.g. claude,cursor; or none)");
    println!("                      and remember the choice; skips the selection prompt");
    println!("  --dry-run           Show what would be written; change nothing");
    println!("  --help, -h          Show this help (never runs setup)");
    println!();
    println!("Interactive setup asks which detected AI tools to connect and remembers the");
    println!("choice (setup.agents). Updates refresh only those tools — or, without a");
    println!("recorded choice, only tools that are already connected. A newly installed");
    println!("tool is never configured by an update: run setup or init --agent <name>.");
    println!("Without a TTY (or with --non-interactive/--yes) setup connects all detected");
    println!("tools, or the recorded choice; use --agents to choose.");
    println!();
    println!("Subcommands:");
    println!("  codex-protected --project <DIR> [--policy-pack <NAME>] [--start]");
    println!("                      Check (or start) a separate protected Codex session");
    println!("  runtime <configure|status|sync-configured|rollback-configured|");
    println!("           activate-configured|activate-personal|renew-configured|deactivate>");
    println!("                      Optional proprietary runtime (Pro analyses, Cockpit);");
    println!("                      installing needs --accept-proprietary, staging-only");
    println!("                      operator commands also --staging");
    println!("  share-identity | share-invite | share-publish | share-continue");
    println!("                      Context sharing (needs --accept-proprietary):");
    println!(
        "                      share-invite --recipient ACCOUNT_UUID --output ABSOLUTE_PRIVATE_FILE"
    );
    println!("                        [--context-id UUID --revision N --expires-at RFC3339];");
    println!("                      share-publish|share-continue --project ABSOLUTE_DIRECTORY");
    println!("                        --invitation ABSOLUTE_PRIVATE_FILE [--expected-head SHA256]");
    println!("                      (omit --expected-head only for an empty destination; send");
    println!("                      invitation files confidentially to their named recipient)");
    println!();
    println!("See also: lean-ctx onboard (one-command setup), lean-ctx doctor");
}
