use crate::{
    core, doctor, heatmap, hook_handlers, report, setup, shell, status, token_report, uninstall,
};

mod analytics;
mod help;
mod lifecycle;
mod network;
mod server;

#[allow(clippy::wildcard_imports)]
use analytics::*;
#[allow(clippy::wildcard_imports)]
use help::*;
#[allow(clippy::wildcard_imports)]
use lifecycle::*;
#[allow(clippy::wildcard_imports)]
use network::*;
#[allow(clippy::wildcard_imports)]
use server::*;

pub fn run() {
    let mut args: Vec<String> = std::env::args().collect();

    // On Linux, if the binary was replaced while running, systemd may write
    // the path with " (deleted)" suffix into ExecStart, causing "(deleted)"
    // to appear as an argument. Strip it defensively.
    if args.get(1).is_some_and(|a| a == "(deleted)") {
        args.remove(1);
    }

    if !is_server_mode(&args) {
        restore_sigpipe_default();
    }

    let enters_mcp = args.len() == 1 || args.get(1).is_some_and(|a| a == "mcp");
    if !enters_mcp {
        crate::core::logging::init_logging();
    }

    if args.len() > 1 {
        let rest = args[2..].to_vec();

        match args[1].as_str() {
            "-c" | "exec" => {
                let raw = rest.first().is_some_and(|a| a == "--raw");
                let cmd_args = if raw { &args[3..] } else { &args[2..] };
                let command = if cmd_args.len() == 1 {
                    cmd_args[0].clone()
                } else {
                    shell::join_command(cmd_args)
                };
                // The `lean-ctx -c` wrapper runs inside the agent shell, which
                // carries runtime/session vars the MCP server never sees. Bridge
                // them so ctx_shell can forward them too (#370).
                core::agent_runtime_env::capture();
                if crate::shell::reentry::should_pass_through() {
                    passthrough(&command);
                }
                if raw {
                    // SAFETY: CLI dispatch is single-threaded; this runs before any
                    // worker threads start (the process hands off to shell::exec below).
                    unsafe { std::env::set_var("LEAN_CTX_RAW", "1") };
                } else {
                    // SAFETY: CLI dispatch is single-threaded; this runs before any
                    // worker threads start (the process hands off to shell::exec below).
                    unsafe { std::env::set_var("LEAN_CTX_COMPRESS", "1") };
                }
                let code = shell::exec(&command);
                core::tool_lifecycle::flush_all();
                std::process::exit(code);
            }
            "-t" | "--track" => {
                let cmd_args = &args[2..];
                let code = if cmd_args.len() > 1 {
                    shell::exec_argv(cmd_args)
                } else {
                    let command = cmd_args[0].clone();
                    if crate::shell::reentry::should_pass_through() {
                        passthrough(&command);
                    }
                    shell::exec(&command)
                };
                core::tool_lifecycle::flush_all();
                std::process::exit(code);
            }
            "shell" | "--shell" => {
                shell::interactive();
                return;
            }
            "gain" => {
                cmd_gain(&rest);
                return;
            }
            "spend" => {
                cmd_spend(&rest);
                return;
            }
            "savings" => {
                cmd_savings(&rest);
                return;
            }
            "learning" => {
                cmd_learning(&rest);
                return;
            }
            "conformance" | "selftest" => {
                cmd_conformance(&rest);
                return;
            }
            "billing" => {
                cmd_billing(&rest);
                return;
            }
            "finops" => {
                cmd_finops(&rest);
                return;
            }
            "roi" => {
                // Local ROI is individual + free. The team roll-up lives on its own
                // surface (`savings team` / the web account), not under `roi`.
                super::cmd_roi(&rest);
                return;
            }
            "token-report" | "report-tokens" => {
                let code = token_report::run_cli(&rest);
                if code != 0 {
                    std::process::exit(code);
                }
                return;
            }
            "pack" => {
                crate::cli::cmd_pack(&rest);
                return;
            }
            "policy" => {
                crate::cli::cmd_policy(&rest);
                return;
            }
            "plugin" | "plugins" => {
                crate::cli::plugin_cmd::cmd_plugin(&rest);
                return;
            }
            "addon" | "addons" => {
                crate::cli::addon_cmd::cmd_addon(&rest);
                return;
            }
            "rules" => {
                crate::cli::rules_cmd::cmd_rules(&rest);
                return;
            }
            "proof" => {
                crate::cli::cmd_proof(&rest);
                return;
            }
            "verify" => {
                crate::cli::cmd_verify(&rest);
                return;
            }
            "eval" => {
                crate::cli::eval_cmd::cmd_eval(&rest);
                return;
            }
            "verify-cache" | "cache-selftest" => {
                let code = crate::cli::verify_cache_cmd::cmd_verify_cache(&rest);
                if code != 0 {
                    std::process::exit(code);
                }
                return;
            }
            "visualize" => {
                super::cmd_visualize(&rest);
                return;
            }
            "audit" => {
                if rest.first().map(String::as_str) == Some("evidence") {
                    crate::cli::audit_report::cmd_evidence(&rest[1..]);
                } else {
                    println!("{}", crate::cli::audit_report::generate_report());
                }
                return;
            }
            "compliance" => {
                crate::cli::cmd_compliance(&rest);
                return;
            }
            "agent" => {
                crate::cli::cmd_agent(&rest);
                return;
            }
            "instructions" => {
                crate::cli::cmd_instructions(&rest);
                return;
            }
            "index" => {
                crate::cli::cmd_index(&rest);
                return;
            }
            "semantic-search" | "search-code" => {
                crate::cli::cmd_semantic_search(&rest);
                core::stats::flush();
                return;
            }
            "repomap" | "repo-map" => {
                crate::cli::cmd_repomap(&rest);
                core::stats::flush();
                return;
            }
            "cep" => {
                println!("{}", core::stats::format_cep_report());
                return;
            }
            "dashboard" => {
                cmd_dashboard(&rest);
                return;
            }
            "team" => {
                cmd_team(&rest);
                return;
            }
            "provider" => {
                cmd_provider(&rest);
                return;
            }
            "serve" => {
                cmd_serve(&rest);
                return;
            }
            "watch" => {
                cmd_watch(&rest);
                return;
            }
            "proxy" => {
                cmd_proxy(&rest);
                return;
            }
            "daemon" => {
                cmd_daemon(&rest);
                return;
            }
            "init" => {
                super::cmd_init(&rest);
                return;
            }
            "setup" => {
                let non_interactive = rest.iter().any(|a| a == "--non-interactive");
                let yes = rest.iter().any(|a| a == "--yes" || a == "-y");
                let fix = rest.iter().any(|a| a == "--fix");
                let json = rest.iter().any(|a| a == "--json");
                let no_auto_approve = rest.iter().any(|a| a == "--no-auto-approve");
                let skip_rules = rest.iter().any(|a| a == "--skip-rules");

                if non_interactive || fix || json || yes {
                    let opts = setup::SetupOptions {
                        non_interactive,
                        yes,
                        fix,
                        json,
                        no_auto_approve,
                        skip_rules,
                        ..Default::default()
                    };
                    match setup::run_setup_with_options(opts) {
                        Ok(report) => {
                            if json {
                                println!(
                                    "{}",
                                    serde_json::to_string_pretty(&report)
                                        .unwrap_or_else(|_| "{}".to_string())
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
                } else {
                    setup::run_setup();
                }
                return;
            }
            "onboard" => {
                setup::run_onboard();
                return;
            }
            "install" => {
                // Plain `lean-ctx install` is a natural thing to type after
                // installing the binary — treat it as the guided setup rather
                // than failing with a usage error. `--repair`/`--fix` keeps the
                // non-interactive, merge-based repair path.
                let repair = rest.iter().any(|a| a == "--repair" || a == "--fix");
                let json = rest.iter().any(|a| a == "--json");
                if !repair {
                    setup::run_setup();
                    return;
                }
                let opts = setup::SetupOptions {
                    non_interactive: true,
                    yes: true,
                    fix: true,
                    json,
                    ..Default::default()
                };
                match setup::run_setup_with_options(opts) {
                    Ok(report) => {
                        if json {
                            println!(
                                "{}",
                                serde_json::to_string_pretty(&report)
                                    .unwrap_or_else(|_| "{}".to_string())
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
                return;
            }
            "bootstrap" => {
                let json = rest.iter().any(|a| a == "--json");
                let opts = setup::SetupOptions {
                    non_interactive: true,
                    yes: true,
                    fix: true,
                    json,
                    ..Default::default()
                };
                match setup::run_setup_with_options(opts) {
                    Ok(report) => {
                        if json {
                            println!(
                                "{}",
                                serde_json::to_string_pretty(&report)
                                    .unwrap_or_else(|_| "{}".to_string())
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
                return;
            }
            "status" => {
                let code = status::run_cli(&rest);
                if code != 0 {
                    std::process::exit(code);
                }
                return;
            }
            "read" => {
                super::cmd_read(&rest);
                core::tool_lifecycle::flush_all();
                return;
            }
            "call" => {
                super::cmd_call(&rest);
                return;
            }
            "diff" => {
                super::cmd_diff(&rest);
                core::tool_lifecycle::flush_all();
                return;
            }
            "grep" => {
                super::cmd_grep(&rest);
                core::tool_lifecycle::flush_all();
                return;
            }
            "glob" => {
                super::cmd_glob(&rest);
                core::stats::flush();
                return;
            }
            "find" => {
                super::cmd_find(&rest);
                core::tool_lifecycle::flush_all();
                return;
            }
            "ls" => {
                super::cmd_ls(&rest);
                core::tool_lifecycle::flush_all();
                return;
            }
            "deps" => {
                super::cmd_deps(&rest);
                core::tool_lifecycle::flush_all();
                return;
            }
            "discover" => {
                super::cmd_discover(&rest);
                return;
            }
            "ghost" => {
                super::cmd_ghost(&rest);
                return;
            }
            "filter" => {
                super::cmd_filter(&rest);
                return;
            }
            "heatmap" => {
                heatmap::cmd_heatmap(&rest);
                return;
            }
            "graph" => {
                cmd_graph(&rest);
                return;
            }
            "smells" => {
                cmd_smells(&rest);
                return;
            }
            "session" => {
                super::cmd_session_action(&rest);
                return;
            }
            "ledger" => {
                super::cmd_ledger(&rest);
                return;
            }
            "control" | "context-control" => {
                super::cmd_control(&rest);
                return;
            }
            "plan" | "context-plan" => {
                super::cmd_plan(&rest);
                return;
            }
            "compile" | "context-compile" => {
                super::cmd_compile(&rest);
                return;
            }
            "knowledge" => {
                super::cmd_knowledge(&rest);
                return;
            }
            "skillify" => {
                super::cmd_skillify(&rest);
                return;
            }
            "summary" => {
                super::cmd_summary(&rest);
                return;
            }
            "overview" => {
                super::cmd_overview(&rest);
                return;
            }
            "compress" => {
                super::cmd_compress(&rest);
                return;
            }
            "wrapped" => {
                eprintln!("'lean-ctx wrapped' has been removed. Use: lean-ctx gain --wrapped");
                std::process::exit(1);
            }
            "sessions" | "session-store" => {
                super::cmd_sessions(&rest);
                return;
            }
            "benchmark" => {
                super::cmd_benchmark(&rest);
                return;
            }
            "compact" => {
                cmd_compact(&rest);
                return;
            }
            "profile" => {
                super::cmd_profile(&rest);
                return;
            }
            "tools" => {
                // `tools health` is the token-budget / rot report (#848); it is
                // distinct from tool *profiles* and routed before the forward.
                if rest.first().map(String::as_str) == Some("health") {
                    super::cmd_tools_health(&rest[1..]);
                    return;
                }
                // Canonical, unambiguous entry point for MCP *tool* profiles
                // (how many tools the agent sees). Disambiguates from
                // `lean-ctx profile`, which manages *context* profiles.
                let mut forwarded = vec!["tools".to_string()];
                forwarded.extend(rest.iter().cloned());
                super::cmd_profile(&forwarded);
                return;
            }
            "config" => {
                super::cmd_config(&rest);
                return;
            }
            "allow" => {
                super::cmd_allow(&rest);
                return;
            }
            "security" => {
                super::cmd_security(&rest);
                return;
            }
            "yolo" => {
                super::cmd_yolo(&rest);
                return;
            }
            "secure" | "lockdown" => {
                super::cmd_secure(&rest);
                return;
            }
            "trust" => {
                super::cmd_trust(&rest);
                return;
            }
            "untrust" => {
                super::cmd_untrust(&rest);
                return;
            }
            "stats" => {
                super::cmd_stats(&rest);
                return;
            }
            "introspect" => {
                super::cmd_introspect(&rest);
                return;
            }
            "cache" => {
                super::cmd_cache(&rest);
                return;
            }
            "theme" => {
                super::cmd_theme(&rest);
                return;
            }
            "tee" => {
                super::cmd_tee(&rest);
                return;
            }
            "terse" | "compression" => {
                super::cmd_compression(&rest);
                return;
            }
            "slow-log" => {
                super::cmd_slow_log(&rest);
                return;
            }
            "debug-log" => {
                super::cmd_debug_log(&rest);
                return;
            }
            // Editor focus ingress (#500): called by the VS Code extension on
            // tab change; <10ms, no daemon required.
            "editor-signal" => {
                let file = rest
                    .iter()
                    .position(|a| a == "--file")
                    .and_then(|i| rest.get(i + 1));
                if let Some(path) = file {
                    if let Err(e) = core::editor_signal::record_focus(path) {
                        eprintln!("editor-signal: {e}");
                        std::process::exit(1);
                    }
                } else {
                    eprintln!("usage: lean-ctx editor-signal --file <path>");
                    std::process::exit(2);
                }
                return;
            }
            "update" | "--self-update" => {
                core::updater::run(&rest);
                return;
            }
            "restart" => {
                cmd_restart();
                return;
            }
            "stop" => {
                cmd_stop();
                return;
            }
            "dev-install" => {
                cmd_dev_install();
                return;
            }
            "codesign-setup" => {
                cmd_codesign_setup();
                return;
            }
            "doctor" => {
                let code = doctor::run_cli(&rest);
                if code != 0 {
                    std::process::exit(code);
                }
                return;
            }
            "harden" => {
                super::harden::run(&rest);
                return;
            }
            "export-rules" => {
                super::export_rules::run(&rest);
                return;
            }
            "gotchas" | "bugs" => {
                super::cloud::cmd_gotchas(&rest);
                return;
            }
            "learn" => {
                super::cmd_learn(&rest);
                return;
            }
            "buddy" | "pet" => {
                super::cloud::cmd_buddy(&rest);
                return;
            }
            "hook" => {
                hook_handlers::mark_hook_environment();
                // Hooks run inside the agent shell environment, so they can see
                // runtime/session vars (e.g. CODEX_THREAD_ID) that the long-lived
                // MCP server process never receives. Bridge them for ctx_shell (#370).
                core::agent_runtime_env::capture();
                hook_handlers::arm_watchdog(std::time::Duration::from_secs(5));
                let action = rest.first().map_or("help", std::string::String::as_str);
                match action {
                    "rewrite" => hook_handlers::handle_rewrite(),
                    "redirect" => hook_handlers::handle_redirect(),
                    "observe" => hook_handlers::handle_observe(),
                    "copilot" => hook_handlers::handle_copilot(),
                    "codex-pretooluse" => hook_handlers::handle_codex_pretooluse(),
                    "codex-session-start" => hook_handlers::handle_codex_session_start(),
                    "rewrite-inline" => hook_handlers::handle_rewrite_inline(),
                    _ => {
                        eprintln!(
                            "Usage: lean-ctx hook <rewrite|redirect|observe|copilot|codex-pretooluse|codex-session-start|rewrite-inline>"
                        );
                        eprintln!(
                            "  Internal commands used by agent hooks (Claude, Cursor, Copilot, etc.)"
                        );
                        std::process::exit(1);
                    }
                }
                return;
            }
            "report-issue" | "report" => {
                report::run(&rest);
                return;
            }
            "uninstall" => {
                // Safety: `--help`/`-h` must NEVER fall through to a real
                // uninstall (issue #476). Short-circuit before any removal.
                if rest.iter().any(|a| a == "--help" || a == "-h") {
                    uninstall::print_help();
                    return;
                }
                let dry_run = rest.iter().any(|a| a == "--dry-run");
                let keep_config = rest.iter().any(|a| a == "--keep-config");
                let keep_binary = rest.iter().any(|a| a == "--keep-binary");
                uninstall::run(dry_run, keep_config, keep_binary);
                return;
            }
            // `raw` is the primary name; `bypass` is kept as a back-compat alias.
            // The old "bypass" wording read to a model like a *security* bypass,
            // but this only skips compression — the shell allowlist and path jail
            // still apply (GH security audit, finding 5).
            "raw" | "bypass" => {
                if rest.is_empty() {
                    eprintln!("Usage: lean-ctx raw \"command\"");
                    eprintln!(
                        "Runs the command with output passed through unchanged (no \
                         compression). The shell allowlist still applies."
                    );
                    std::process::exit(1);
                }
                let command = if rest.len() == 1 {
                    rest[0].clone()
                } else {
                    shell::join_command(&args[2..])
                };
                // SAFETY: CLI dispatch is single-threaded; this runs before the
                // process hands off to shell::exec and exits below.
                unsafe { std::env::set_var("LEAN_CTX_RAW", "1") };
                let code = shell::exec(&command);
                std::process::exit(code);
            }
            "safety-levels" | "safety" => {
                println!("{}", core::compression_safety::format_safety_table());
                return;
            }
            "cheat" | "cheatsheet" | "cheat-sheet" => {
                super::cmd_cheatsheet();
                return;
            }
            "login" => {
                super::cloud::cmd_login(&rest);
                return;
            }
            "register" => {
                super::cloud::cmd_register(&rest);
                return;
            }
            "forgot-password" => {
                super::cloud::cmd_forgot_password(&rest);
                return;
            }
            "sync" => {
                super::cloud::cmd_sync(&rest);
                return;
            }
            "contribute" => {
                super::cloud::cmd_contribute();
                return;
            }
            "cloud" => {
                super::cloud::cmd_cloud(&rest);
                return;
            }
            "upgrade" => {
                super::cloud::cmd_upgrade();
                return;
            }
            "--version" | "-V" => {
                println!("{}", core::integrity::origin_line());
                return;
            }
            "help" => {
                let want_all = rest
                    .iter()
                    .any(|a| matches!(a.as_str(), "all" | "full" | "--all" | "-a"));
                if want_all {
                    print_help();
                } else {
                    print_help_concise();
                }
                return;
            }
            "--help" | "-h" => {
                if rest
                    .iter()
                    .any(|a| matches!(a.as_str(), "all" | "full" | "--all" | "-a"))
                {
                    print_help();
                } else {
                    print_help_concise();
                }
                return;
            }
            "mcp" => {}
            _ => {
                tracing::error!("lean-ctx: unknown command '{}'", args[1]);
                print_help_concise();
                std::process::exit(1);
            }
        }
    }

    // Bare `lean-ctx` in an interactive terminal: a human almost certainly did
    // not mean to start a silent stdio MCP server (which just hangs waiting for
    // JSON-RPC). Show a short quickstart instead. MCP clients pipe stdin (not a
    // TTY) so they still get the server, and explicit `lean-ctx mcp` always
    // serves regardless of TTY.
    if args.len() == 1 && std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        print_quickstart();
        return;
    }

    if let Err(e) = run_mcp_server() {
        tracing::error!("lean-ctx: {e}");
        std::process::exit(1);
    }
}

/// Long-lived server entry points keep Rust's default ignored SIGPIPE: they
/// must survive peers closing sockets/pipes early. Bare `lean-ctx` counts as
/// a server because MCP clients spawn the binary without a subcommand.
fn is_server_mode(args: &[String]) -> bool {
    args.len() == 1
        || args.get(1).is_some_and(|a| {
            matches!(
                a.as_str(),
                "mcp" | "daemon" | "proxy" | "serve" | "watch" | "dashboard"
            )
        })
}

/// Restore the default SIGPIPE disposition for short-lived CLI invocations.
///
/// Rust's runtime ignores SIGPIPE process-wide, so `lean-ctx doctor | head`
/// made `println!` panic with BrokenPipe; the LineWriter flush in stdout's
/// Drop then panicked again *during unwinding*, which aborts — the SIGABRT
/// (exit 134) of upstream #378 / GL#436. Real CLIs (cat, grep, rg) terminate
/// silently with exit 141 instead; SIG_DFL gives us exactly that. Children
/// spawned via std::process::Command are unaffected either way (std resets
/// their SIGPIPE disposition since Rust 1.65).
#[cfg(unix)]
fn restore_sigpipe_default() {
    // SAFETY: signal(2) with SIG_DFL has no preconditions and is called once
    // during single-threaded startup, before any I/O.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

#[cfg(not(unix))]
fn restore_sigpipe_default() {}

fn passthrough(command: &str) -> ! {
    let (shell, flag) = shell::shell_and_flag();
    let mut cmd = std::process::Command::new(&shell);
    cmd.arg(&flag).arg(command);
    shell::reentry::mark_child(&mut cmd);
    shell::platform::apply_utf8_locale(&mut cmd);
    let status = cmd.status().map_or(127, |s| s.code().unwrap_or(1));
    std::process::exit(status);
}

pub(super) fn run_async<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Runtime::new()
        .expect("failed to create async runtime")
        .block_on(future)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    fn args_of(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn server_modes_keep_ignored_sigpipe() {
        for mode in ["mcp", "daemon", "proxy", "serve", "watch", "dashboard"] {
            assert!(
                is_server_mode(&args_of(&["lean-ctx", mode])),
                "{mode} must count as server mode"
            );
        }
        // Bare invocation = MCP server spawned by a client.
        assert!(is_server_mode(&args_of(&["lean-ctx"])));
    }

    #[test]
    fn cli_modes_restore_default_sigpipe() {
        for mode in ["doctor", "-c", "status", "ls", "grep", "gain", "help"] {
            assert!(
                !is_server_mode(&args_of(&["lean-ctx", mode])),
                "{mode} must count as CLI mode (SIGPIPE default)"
            );
        }
    }

    #[test]
    fn quickstart_is_short_and_points_to_setup() {
        let q = quickstart_text();
        assert!(
            q.contains("lean-ctx onboard"),
            "quickstart must point to onboard"
        );
        assert!(q.contains("lean-ctx help"), "quickstart must point to help");
        // Must stay a *quickstart*, not the full reference — keep it tight.
        assert!(
            q.lines().count() <= 16,
            "quickstart should be short; got {} lines",
            q.lines().count()
        );
        assert!(
            !q.contains("COMMANDS:"),
            "quickstart must not inline the full command reference"
        );
    }

    #[test]
    fn concise_help_is_short_and_points_to_full() {
        let h = concise_help_text();
        assert!(h.contains("lean-ctx onboard"), "must lead with onboard");
        assert!(
            h.contains("lean-ctx help all"),
            "must point to full reference"
        );
        assert!(
            h.contains("lean-ctx tools"),
            "must surface the tools profile command"
        );
        // Concise means concise — keep it well under the full reference.
        assert!(
            h.lines().count() <= 40,
            "concise help should stay short; got {} lines",
            h.lines().count()
        );
        assert!(
            !h.contains("SHELL HOOK PATTERNS"),
            "concise help must not inline the full pattern catalog"
        );
    }

    #[test]
    fn capability_banner_tool_count_matches_registry() {
        let n = crate::server::registry::tool_count();
        let banner = capability_banner();
        assert!(
            banner.contains(&format!("{n} MCP tools")),
            "banner must show the live registry count ({n}); got: {banner}"
        );
    }

    #[test]
    #[serial]
    fn worker_threads_default_clamps_low() {
        crate::test_env::remove_var("LEAN_CTX_WORKER_THREADS");
        assert_eq!(resolve_worker_threads(1), 1);
    }

    #[test]
    #[serial]
    fn worker_threads_default_clamps_high() {
        crate::test_env::remove_var("LEAN_CTX_WORKER_THREADS");
        assert_eq!(resolve_worker_threads(32), 4);
    }

    #[test]
    #[serial]
    fn worker_threads_default_passthrough() {
        crate::test_env::remove_var("LEAN_CTX_WORKER_THREADS");
        assert_eq!(resolve_worker_threads(3), 3);
    }

    #[test]
    #[serial]
    fn worker_threads_env_override() {
        crate::test_env::set_var("LEAN_CTX_WORKER_THREADS", "12");
        assert_eq!(resolve_worker_threads(2), 12);
        crate::test_env::remove_var("LEAN_CTX_WORKER_THREADS");
    }

    #[test]
    #[serial]
    fn worker_threads_env_invalid_falls_back() {
        crate::test_env::set_var("LEAN_CTX_WORKER_THREADS", "not_a_number");
        assert_eq!(resolve_worker_threads(3), 3);
        crate::test_env::remove_var("LEAN_CTX_WORKER_THREADS");
    }
}
