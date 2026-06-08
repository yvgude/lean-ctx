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
                if std::env::var("LEAN_CTX_ACTIVE").is_ok()
                    || std::env::var("LEAN_CTX_DISABLED").is_ok()
                {
                    passthrough(&command);
                }
                if raw {
                    std::env::set_var("LEAN_CTX_RAW", "1");
                } else {
                    std::env::set_var("LEAN_CTX_COMPRESS", "1");
                }
                let code = shell::exec(&command);
                core::stats::flush();
                core::heatmap::flush();
                std::process::exit(code);
            }
            "-t" | "--track" => {
                let cmd_args = &args[2..];
                let code = if cmd_args.len() > 1 {
                    shell::exec_argv(cmd_args)
                } else {
                    let command = cmd_args[0].clone();
                    if std::env::var("LEAN_CTX_ACTIVE").is_ok()
                        || std::env::var("LEAN_CTX_DISABLED").is_ok()
                    {
                        passthrough(&command);
                    }
                    shell::exec(&command)
                };
                core::stats::flush();
                core::heatmap::flush();
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
            "savings" => {
                cmd_savings(&rest);
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
            "plugin" | "plugins" => {
                crate::cli::plugin_cmd::cmd_plugin(&rest);
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
                println!("{}", crate::cli::audit_report::generate_report());
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
                core::stats::flush();
                return;
            }
            "diff" => {
                super::cmd_diff(&rest);
                core::stats::flush();
                return;
            }
            "grep" => {
                super::cmd_grep(&rest);
                core::stats::flush();
                return;
            }
            "find" => {
                super::cmd_find(&rest);
                core::stats::flush();
                return;
            }
            "ls" => {
                super::cmd_ls(&rest);
                core::stats::flush();
                return;
            }
            "deps" => {
                super::cmd_deps(&rest);
                core::stats::flush();
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
            "stats" => {
                super::cmd_stats(&rest);
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
                        eprintln!("Usage: lean-ctx hook <rewrite|redirect|observe|copilot|codex-pretooluse|codex-session-start|rewrite-inline>");
                        eprintln!("  Internal commands used by agent hooks (Claude, Cursor, Copilot, etc.)");
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
                let dry_run = rest.iter().any(|a| a == "--dry-run");
                let keep_config = rest.iter().any(|a| a == "--keep-config");
                let keep_binary = rest.iter().any(|a| a == "--keep-binary");
                uninstall::run(dry_run, keep_config, keep_binary);
                return;
            }
            "bypass" => {
                if rest.is_empty() {
                    eprintln!("Usage: lean-ctx bypass \"command\"");
                    eprintln!("Runs the command with zero compression (raw passthrough).");
                    std::process::exit(1);
                }
                let command = if rest.len() == 1 {
                    rest[0].clone()
                } else {
                    shell::join_command(&args[2..])
                };
                std::env::set_var("LEAN_CTX_RAW", "1");
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
                super::cloud::cmd_sync();
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

fn passthrough(command: &str) -> ! {
    let (shell, flag) = shell::shell_and_flag();
    let mut cmd = std::process::Command::new(&shell);
    cmd.arg(&flag).arg(command).env("LEAN_CTX_ACTIVE", "1");
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
        std::env::remove_var("LEAN_CTX_WORKER_THREADS");
        assert_eq!(resolve_worker_threads(1), 1);
    }

    #[test]
    #[serial]
    fn worker_threads_default_clamps_high() {
        std::env::remove_var("LEAN_CTX_WORKER_THREADS");
        assert_eq!(resolve_worker_threads(32), 4);
    }

    #[test]
    #[serial]
    fn worker_threads_default_passthrough() {
        std::env::remove_var("LEAN_CTX_WORKER_THREADS");
        assert_eq!(resolve_worker_threads(3), 3);
    }

    #[test]
    #[serial]
    fn worker_threads_env_override() {
        std::env::set_var("LEAN_CTX_WORKER_THREADS", "12");
        assert_eq!(resolve_worker_threads(2), 12);
        std::env::remove_var("LEAN_CTX_WORKER_THREADS");
    }

    #[test]
    #[serial]
    fn worker_threads_env_invalid_falls_back() {
        std::env::set_var("LEAN_CTX_WORKER_THREADS", "not_a_number");
        assert_eq!(resolve_worker_threads(3), 3);
        std::env::remove_var("LEAN_CTX_WORKER_THREADS");
    }
}
