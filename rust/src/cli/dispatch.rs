use crate::{
    core, dashboard, doctor, heatmap, hook_handlers, mcp_stdio, report, setup, shell, status,
    token_report, tools, tui, uninstall,
};
use anyhow::Result;

pub fn run() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() > 1 {
        let rest = args[2..].to_vec();

        match args[1].as_str() {
            "-c" | "exec" => {
                let raw = rest.first().map(|a| a == "--raw").unwrap_or(false);
                let cmd_args = if raw { &args[3..] } else { &args[2..] };
                let command = if cmd_args.len() == 1 {
                    cmd_args[0].clone()
                } else {
                    shell::join_command(cmd_args)
                };
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
                std::process::exit(code);
            }
            "-t" | "--track" => {
                let cmd_args = &args[2..];
                let command = if cmd_args.len() == 1 {
                    cmd_args[0].clone()
                } else {
                    shell::join_command(cmd_args)
                };
                if std::env::var("LEAN_CTX_ACTIVE").is_ok()
                    || std::env::var("LEAN_CTX_DISABLED").is_ok()
                {
                    passthrough(&command);
                }
                let code = shell::exec(&command);
                core::stats::flush();
                std::process::exit(code);
            }
            "shell" | "--shell" => {
                shell::interactive();
                return;
            }
            "gain" => {
                if rest.iter().any(|a| a == "--reset") {
                    core::stats::reset_all();
                    println!("Stats reset. All token savings data cleared.");
                    return;
                }
                if rest.iter().any(|a| a == "--live" || a == "--watch") {
                    core::stats::gain_live();
                    return;
                }
                let model = rest.iter().enumerate().find_map(|(i, a)| {
                    if let Some(v) = a.strip_prefix("--model=") {
                        return Some(v.to_string());
                    }
                    if a == "--model" {
                        return rest.get(i + 1).cloned();
                    }
                    None
                });
                let period = rest
                    .iter()
                    .enumerate()
                    .find_map(|(i, a)| {
                        if let Some(v) = a.strip_prefix("--period=") {
                            return Some(v.to_string());
                        }
                        if a == "--period" {
                            return rest.get(i + 1).cloned();
                        }
                        None
                    })
                    .unwrap_or_else(|| "all".to_string());
                let limit = rest
                    .iter()
                    .enumerate()
                    .find_map(|(i, a)| {
                        if let Some(v) = a.strip_prefix("--limit=") {
                            return v.parse::<usize>().ok();
                        }
                        if a == "--limit" {
                            return rest.get(i + 1).and_then(|v| v.parse::<usize>().ok());
                        }
                        None
                    })
                    .unwrap_or(10);

                if rest.iter().any(|a| a == "--graph") {
                    println!("{}", core::stats::format_gain_graph());
                } else if rest.iter().any(|a| a == "--daily") {
                    println!("{}", core::stats::format_gain_daily());
                } else if rest.iter().any(|a| a == "--json") {
                    println!(
                        "{}",
                        tools::ctx_gain::handle(
                            "json",
                            Some(&period),
                            model.as_deref(),
                            Some(limit)
                        )
                    );
                } else if rest.iter().any(|a| a == "--score") {
                    println!(
                        "{}",
                        tools::ctx_gain::handle("score", None, model.as_deref(), Some(limit))
                    );
                } else if rest.iter().any(|a| a == "--cost") {
                    println!(
                        "{}",
                        tools::ctx_gain::handle("cost", None, model.as_deref(), Some(limit))
                    );
                } else if rest.iter().any(|a| a == "--tasks") {
                    println!(
                        "{}",
                        tools::ctx_gain::handle("tasks", None, None, Some(limit))
                    );
                } else if rest.iter().any(|a| a == "--agents") {
                    println!(
                        "{}",
                        tools::ctx_gain::handle("agents", None, None, Some(limit))
                    );
                } else if rest.iter().any(|a| a == "--heatmap") {
                    println!(
                        "{}",
                        tools::ctx_gain::handle("heatmap", None, None, Some(limit))
                    );
                } else if rest.iter().any(|a| a == "--wrapped") {
                    println!(
                        "{}",
                        tools::ctx_gain::handle(
                            "wrapped",
                            Some(&period),
                            model.as_deref(),
                            Some(limit)
                        )
                    );
                } else if rest.iter().any(|a| a == "--pipeline") {
                    let stats_path = dirs::home_dir()
                        .unwrap_or_default()
                        .join(".lean-ctx")
                        .join("pipeline_stats.json");
                    if let Ok(data) = std::fs::read_to_string(&stats_path) {
                        if let Ok(stats) =
                            serde_json::from_str::<core::pipeline::PipelineStats>(&data)
                        {
                            println!("{}", stats.format_summary());
                        } else {
                            println!("No pipeline stats available yet (corrupt data).");
                        }
                    } else {
                        println!(
                            "No pipeline stats available yet. Use MCP tools to generate data."
                        );
                    }
                } else if rest.iter().any(|a| a == "--deep") {
                    println!(
                        "{}\n{}\n{}\n{}\n{}",
                        tools::ctx_gain::handle("report", None, model.as_deref(), Some(limit)),
                        tools::ctx_gain::handle("tasks", None, None, Some(limit)),
                        tools::ctx_gain::handle("cost", None, model.as_deref(), Some(limit)),
                        tools::ctx_gain::handle("agents", None, None, Some(limit)),
                        tools::ctx_gain::handle("heatmap", None, None, Some(limit))
                    );
                } else {
                    println!("{}", core::stats::format_gain());
                }
                return;
            }
            "token-report" | "report-tokens" => {
                let code = token_report::run_cli(&rest);
                if code != 0 {
                    std::process::exit(code);
                }
                return;
            }
            "cep" => {
                println!("{}", tools::ctx_gain::handle("score", None, None, Some(10)));
                return;
            }
            "dashboard" => {
                let port = rest
                    .iter()
                    .find_map(|p| p.strip_prefix("--port=").or_else(|| p.strip_prefix("-p=")))
                    .and_then(|p| p.parse().ok());
                let host = rest
                    .iter()
                    .find_map(|p| p.strip_prefix("--host=").or_else(|| p.strip_prefix("-H=")))
                    .map(String::from);
                let project = rest
                    .iter()
                    .find_map(|p| p.strip_prefix("--project="))
                    .map(String::from);
                if let Some(ref p) = project {
                    std::env::set_var("LEAN_CTX_DASHBOARD_PROJECT", p);
                }
                run_async(dashboard::start(port, host));
                return;
            }
            "serve" => {
                #[cfg(feature = "http-server")]
                {
                    let mut cfg = crate::http_server::HttpServerConfig::default();
                    let mut i = 0;
                    while i < rest.len() {
                        match rest[i].as_str() {
                            "--host" | "-H" => {
                                i += 1;
                                if i < rest.len() {
                                    cfg.host = rest[i].clone();
                                }
                            }
                            arg if arg.starts_with("--host=") => {
                                cfg.host = arg["--host=".len()..].to_string();
                            }
                            "--port" | "-p" => {
                                i += 1;
                                if i < rest.len() {
                                    if let Ok(p) = rest[i].parse::<u16>() {
                                        cfg.port = p;
                                    }
                                }
                            }
                            arg if arg.starts_with("--port=") => {
                                if let Ok(p) = arg["--port=".len()..].parse::<u16>() {
                                    cfg.port = p;
                                }
                            }
                            "--project-root" => {
                                i += 1;
                                if i < rest.len() {
                                    cfg.project_root = std::path::PathBuf::from(&rest[i]);
                                }
                            }
                            arg if arg.starts_with("--project-root=") => {
                                cfg.project_root =
                                    std::path::PathBuf::from(&arg["--project-root=".len()..]);
                            }
                            "--auth-token" => {
                                i += 1;
                                if i < rest.len() {
                                    cfg.auth_token = Some(rest[i].clone());
                                }
                            }
                            arg if arg.starts_with("--auth-token=") => {
                                cfg.auth_token = Some(arg["--auth-token=".len()..].to_string());
                            }
                            "--stateful" => cfg.stateful_mode = true,
                            "--stateless" => cfg.stateful_mode = false,
                            "--json" => cfg.json_response = true,
                            "--sse" => cfg.json_response = false,
                            "--disable-host-check" => cfg.disable_host_check = true,
                            "--allowed-host" => {
                                i += 1;
                                if i < rest.len() {
                                    cfg.allowed_hosts.push(rest[i].clone());
                                }
                            }
                            arg if arg.starts_with("--allowed-host=") => {
                                cfg.allowed_hosts
                                    .push(arg["--allowed-host=".len()..].to_string());
                            }
                            "--max-body-bytes" => {
                                i += 1;
                                if i < rest.len() {
                                    if let Ok(n) = rest[i].parse::<usize>() {
                                        cfg.max_body_bytes = n;
                                    }
                                }
                            }
                            arg if arg.starts_with("--max-body-bytes=") => {
                                if let Ok(n) = arg["--max-body-bytes=".len()..].parse::<usize>() {
                                    cfg.max_body_bytes = n;
                                }
                            }
                            "--max-concurrency" => {
                                i += 1;
                                if i < rest.len() {
                                    if let Ok(n) = rest[i].parse::<usize>() {
                                        cfg.max_concurrency = n;
                                    }
                                }
                            }
                            arg if arg.starts_with("--max-concurrency=") => {
                                if let Ok(n) = arg["--max-concurrency=".len()..].parse::<usize>() {
                                    cfg.max_concurrency = n;
                                }
                            }
                            "--max-rps" => {
                                i += 1;
                                if i < rest.len() {
                                    if let Ok(n) = rest[i].parse::<u32>() {
                                        cfg.max_rps = n;
                                    }
                                }
                            }
                            arg if arg.starts_with("--max-rps=") => {
                                if let Ok(n) = arg["--max-rps=".len()..].parse::<u32>() {
                                    cfg.max_rps = n;
                                }
                            }
                            "--rate-burst" => {
                                i += 1;
                                if i < rest.len() {
                                    if let Ok(n) = rest[i].parse::<u32>() {
                                        cfg.rate_burst = n;
                                    }
                                }
                            }
                            arg if arg.starts_with("--rate-burst=") => {
                                if let Ok(n) = arg["--rate-burst=".len()..].parse::<u32>() {
                                    cfg.rate_burst = n;
                                }
                            }
                            "--request-timeout-ms" => {
                                i += 1;
                                if i < rest.len() {
                                    if let Ok(n) = rest[i].parse::<u64>() {
                                        cfg.request_timeout_ms = n;
                                    }
                                }
                            }
                            arg if arg.starts_with("--request-timeout-ms=") => {
                                if let Ok(n) = arg["--request-timeout-ms=".len()..].parse::<u64>() {
                                    cfg.request_timeout_ms = n;
                                }
                            }
                            "--help" | "-h" => {
                                eprintln!(
                                    "Usage: lean-ctx serve [--host H] [--port N] [--project-root DIR]\\n\\
                                     \\n\\
                                     Options:\\n\\
                                       --host, -H            Bind host (default: 127.0.0.1)\\n\\
                                       --port, -p            Bind port (default: 8080)\\n\\
                                       --project-root        Resolve relative paths against this root (default: cwd)\\n\\
                                       --auth-token          Require Authorization: Bearer <token> (required for non-loopback binds)\\n\\
                                       --stateful/--stateless  Streamable HTTP session mode (default: stateless)\\n\\
                                       --json/--sse          Response framing in stateless mode (default: json)\\n\\
                                       --max-body-bytes      Max request body size in bytes (default: 2097152)\\n\\
                                       --max-concurrency     Max concurrent requests (default: 32)\\n\\
                                       --max-rps             Max requests/sec (global, default: 50)\\n\\
                                       --rate-burst          Rate limiter burst (global, default: 100)\\n\\
                                       --request-timeout-ms  REST tool-call timeout (default: 30000)\\n\\
                                       --allowed-host        Add allowed Host header (repeatable)\\n\\
                                       --disable-host-check  Disable Host header validation (unsafe)"
                                );
                                return;
                            }
                            _ => {}
                        }
                        i += 1;
                    }

                    if cfg.auth_token.is_none() {
                        if let Ok(v) = std::env::var("LEAN_CTX_HTTP_TOKEN") {
                            if !v.trim().is_empty() {
                                cfg.auth_token = Some(v);
                            }
                        }
                    }

                    if let Err(e) = run_async(crate::http_server::serve(cfg)) {
                        eprintln!("HTTP server error: {e}");
                        std::process::exit(1);
                    }
                    return;
                }
                #[cfg(not(feature = "http-server"))]
                {
                    eprintln!("lean-ctx serve is not available in this build");
                    std::process::exit(1);
                }
            }
            "watch" => {
                if let Err(e) = tui::run() {
                    eprintln!("TUI error: {e}");
                    std::process::exit(1);
                }
                return;
            }
            "proxy" => {
                #[cfg(feature = "http-server")]
                {
                    let sub = rest.first().map(|s| s.as_str()).unwrap_or("help");
                    match sub {
                        "start" => {
                            let port: u16 = rest
                                .iter()
                                .find_map(|p| {
                                    p.strip_prefix("--port=").or_else(|| p.strip_prefix("-p="))
                                })
                                .and_then(|p| p.parse().ok())
                                .unwrap_or(4444);
                            let autostart = rest.iter().any(|a| a == "--autostart");
                            if autostart {
                                crate::proxy_autostart::install(port, false);
                                return;
                            }
                            if let Err(e) = run_async(crate::proxy::start_proxy(port)) {
                                eprintln!("Proxy error: {e}");
                                std::process::exit(1);
                            }
                        }
                        "stop" => {
                            match ureq::get(&format!(
                                "http://127.0.0.1:{}/health",
                                rest.iter()
                                    .find_map(|p| p.strip_prefix("--port="))
                                    .and_then(|p| p.parse::<u16>().ok())
                                    .unwrap_or(4444)
                            ))
                            .call()
                            {
                                Ok(_) => {
                                    println!("Proxy is running. Use Ctrl+C or kill the process.");
                                }
                                Err(_) => {
                                    println!("No proxy running on that port.");
                                }
                            }
                        }
                        "status" => {
                            let port: u16 = rest
                                .iter()
                                .find_map(|p| p.strip_prefix("--port="))
                                .and_then(|p| p.parse().ok())
                                .unwrap_or(4444);
                            match ureq::get(&format!("http://127.0.0.1:{port}/status")).call() {
                                Ok(resp) => {
                                    let body =
                                        resp.into_body().read_to_string().unwrap_or_default();
                                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body)
                                    {
                                        println!("lean-ctx proxy status:");
                                        println!("  Requests:    {}", v["requests_total"]);
                                        println!("  Compressed:  {}", v["requests_compressed"]);
                                        println!("  Tokens saved: {}", v["tokens_saved"]);
                                        println!(
                                            "  Compression: {}%",
                                            v["compression_ratio_pct"].as_str().unwrap_or("0.0")
                                        );
                                    } else {
                                        println!("{body}");
                                    }
                                }
                                Err(_) => {
                                    println!("No proxy running on port {port}.");
                                    println!("Start with: lean-ctx proxy start");
                                }
                            }
                        }
                        _ => {
                            println!("Usage: lean-ctx proxy <start|stop|status> [--port=4444]");
                        }
                    }
                    return;
                }
                #[cfg(not(feature = "http-server"))]
                {
                    eprintln!("lean-ctx proxy is not available in this build");
                    std::process::exit(1);
                }
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

                if non_interactive || fix || json || yes {
                    let opts = setup::SetupOptions {
                        non_interactive,
                        yes,
                        fix,
                        json,
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
            "bootstrap" => {
                let json = rest.iter().any(|a| a == "--json");
                let opts = setup::SetupOptions {
                    non_interactive: true,
                    yes: true,
                    fix: true,
                    json,
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
                return;
            }
            "diff" => {
                super::cmd_diff(&rest);
                return;
            }
            "grep" => {
                super::cmd_grep(&rest);
                return;
            }
            "find" => {
                super::cmd_find(&rest);
                return;
            }
            "ls" => {
                super::cmd_ls(&rest);
                return;
            }
            "deps" => {
                super::cmd_deps(&rest);
                return;
            }
            "discover" => {
                super::cmd_discover(&rest);
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
                let mut action = "build";
                let mut path_arg: Option<&str> = None;
                for arg in &rest {
                    if arg == "build" {
                        action = "build";
                    } else {
                        path_arg = Some(arg.as_str());
                    }
                }
                let root = path_arg
                    .map(String::from)
                    .or_else(|| {
                        std::env::current_dir()
                            .ok()
                            .map(|p| p.to_string_lossy().to_string())
                    })
                    .unwrap_or_else(|| ".".to_string());
                match action {
                    "build" => {
                        let index = core::graph_index::load_or_build(&root);
                        println!(
                            "Graph built: {} files, {} edges",
                            index.files.len(),
                            index.edges.len()
                        );
                    }
                    _ => {
                        eprintln!("Usage: lean-ctx graph [build] [path]");
                    }
                }
                return;
            }
            "session" => {
                super::cmd_session();
                return;
            }
            "wrapped" => {
                super::cmd_wrapped(&rest);
                return;
            }
            "sessions" => {
                super::cmd_sessions(&rest);
                return;
            }
            "benchmark" => {
                super::cmd_benchmark(&rest);
                return;
            }
            "config" => {
                super::cmd_config(&rest);
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
            "terse" => {
                super::cmd_terse(&rest);
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
            "doctor" => {
                let code = doctor::run_cli(&rest);
                if code != 0 {
                    std::process::exit(code);
                }
                return;
            }
            "gotchas" | "bugs" => {
                super::cloud::cmd_gotchas(&rest);
                return;
            }
            "buddy" | "pet" => {
                super::cloud::cmd_buddy(&rest);
                return;
            }
            "hook" => {
                let action = rest.first().map(|s| s.as_str()).unwrap_or("help");
                match action {
                    "rewrite" => hook_handlers::handle_rewrite(),
                    "redirect" => hook_handlers::handle_redirect(),
                    "copilot" => hook_handlers::handle_copilot(),
                    "codex-pretooluse" => hook_handlers::handle_codex_pretooluse(),
                    "codex-session-start" => hook_handlers::handle_codex_session_start(),
                    "rewrite-inline" => hook_handlers::handle_rewrite_inline(),
                    _ => {
                        eprintln!("Usage: lean-ctx hook <rewrite|redirect|copilot|codex-pretooluse|codex-session-start|rewrite-inline>");
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
                uninstall::run();
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
            "--help" | "-h" => {
                print_help();
                return;
            }
            "mcp" => {}
            _ => {
                eprintln!("lean-ctx: unknown command '{}'\n", args[1]);
                print_help();
                std::process::exit(1);
            }
        }
    }

    if let Err(e) = run_mcp_server() {
        eprintln!("lean-ctx: {e}");
        std::process::exit(1);
    }
}

fn passthrough(command: &str) -> ! {
    let (shell, flag) = shell::shell_and_flag();
    let status = std::process::Command::new(&shell)
        .arg(&flag)
        .arg(command)
        .env("LEAN_CTX_ACTIVE", "1")
        .status()
        .map(|s| s.code().unwrap_or(1))
        .unwrap_or(127);
    std::process::exit(status);
}

fn run_async<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Runtime::new()
        .expect("failed to create async runtime")
        .block_on(future)
}

fn run_mcp_server() -> Result<()> {
    use rmcp::ServiceExt;
    use tracing_subscriber::EnvFilter;

    std::env::set_var("LEAN_CTX_MCP_SERVER", "1");

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::from_default_env())
            .with_writer(std::io::stderr)
            .init();

        tracing::info!(
            "lean-ctx v{} MCP server starting",
            env!("CARGO_PKG_VERSION")
        );

        let server = tools::create_server();
        let transport =
            mcp_stdio::HybridStdioTransport::new_server(tokio::io::stdin(), tokio::io::stdout());
        let service = server.serve(transport).await?;
        service.waiting().await?;

        core::stats::flush();
        core::mode_predictor::ModePredictor::flush();
        core::feedback::FeedbackStore::flush();

        Ok(())
    })
}

fn print_help() {
    println!(
        "lean-ctx {version} — Context Runtime for AI Agents

90+ compression patterns | 46 MCP tools | Context Continuity Protocol

USAGE:
    lean-ctx                       Start MCP server (stdio)
    lean-ctx serve                 Start MCP server (Streamable HTTP)
    lean-ctx -t \"command\"          Track command (full output + stats, no compression)
    lean-ctx -c \"command\"          Execute with compressed output (used by AI hooks)
    lean-ctx -c --raw \"command\"    Execute without compression (full output)
    lean-ctx exec \"command\"        Same as -c
    lean-ctx shell                 Interactive shell with compression

COMMANDS:
    gain                           Visual dashboard (colors, bars, sparklines, USD)
    gain --live                    Live mode: auto-refreshes every 1s in-place
    gain --graph                   30-day savings chart
    gain --daily                   Bordered day-by-day table with USD
    gain --json                    Raw JSON export of all stats
         token-report [--json]          Token + memory report (project + session + CEP)
    cep                            CEP impact report (score trends, cache, modes)
    watch                          Live TUI dashboard (real-time event stream)
    dashboard [--port=N] [--host=H] Open web dashboard (default: http://localhost:3333)
    serve [--host H] [--port N]    MCP over HTTP (Streamable HTTP, local-first)
    proxy start [--port=4444]      API proxy: compress tool_results before LLM API
    proxy status                   Show proxy statistics
    cache [list|clear|stats]       Show/manage file read cache
    wrapped [--week|--month|--all] Savings report card (shareable)
    sessions [list|show|cleanup]   Manage CCP sessions (~/.lean-ctx/sessions/)
    benchmark run [path] [--json]  Run real benchmark on project files
    benchmark report [path]        Generate shareable Markdown report
    cheatsheet                     Command cheat sheet & workflow quick reference
    setup                          One-command setup: shell + editor + verify
    bootstrap                      Non-interactive setup + fix (zero-config)
    status [--json]                Show setup + MCP + rules status
    init [--global]                Install shell aliases (zsh/bash/fish/PowerShell)
    init --agent <name>            Configure MCP for specific editor/agent
    read <file> [-m mode]          Read file with compression
    diff <file1> <file2>           Compressed file diff
    grep <pattern> [path]          Search with compressed output
    find <pattern> [path]          Find files with compressed output
    ls [path]                      Directory listing with compression
    deps [path]                    Show project dependencies
    discover                       Find uncompressed commands in shell history
    filter [list|validate|init]    Manage custom compression filters (~/.lean-ctx/filters/)
    session                        Show adoption statistics
    config                         Show/edit configuration (~/.lean-ctx/config.toml)
    theme [list|set|export|import] Customize terminal colors and themes
    tee [list|clear|show <file>|last] Manage output tee files (~/.lean-ctx/tee/)
    terse [off|lite|full|ultra]    Set agent output verbosity (saves 25-65% output tokens)
    slow-log [list|clear]          Show/clear slow command log (~/.lean-ctx/slow-commands.log)
    update [--check]               Self-update lean-ctx binary from GitHub Releases
    gotchas [list|clear|export|stats] Bug Memory: view/manage auto-detected error patterns
    buddy [show|stats|ascii|json]  Token Guardian: your data-driven coding companion
    doctor [--fix] [--json]        Run diagnostics (and optionally repair)
    uninstall                      Remove shell hook, MCP configs, and data directory

SHELL HOOK PATTERNS (90+):
    git       status, log, diff, add, commit, push, pull, fetch, clone,
              branch, checkout, switch, merge, stash, tag, reset, remote
    docker    build, ps, images, logs, compose, exec, network
    npm/pnpm  install, test, run, list, outdated, audit
    cargo     build, test, check, clippy
    gh        pr list/view/create, issue list/view, run list/view
    kubectl   get pods/services/deployments, logs, describe, apply
    python    pip install/list/outdated, ruff check/format, poetry, uv
    linters   eslint, biome, prettier, golangci-lint
    builds    tsc, next build, vite build
    ruby      rubocop, bundle install/update, rake test, rails test
    tests     jest, vitest, pytest, go test, playwright, rspec, minitest
    iac       terraform, make, maven, gradle, dotnet, flutter, dart
    utils     curl, grep/rg, find, ls, wget, env
    data      JSON schema extraction, log deduplication

READ MODES:
    auto                           Auto-select optimal mode (default)
    full                           Full content (cached re-reads = 13 tokens)
    map                            Dependency graph + API signatures
    signatures                     tree-sitter AST extraction (18 languages)
    task                           Task-relevant filtering (requires ctx_session task)
    reference                      One-line reference stub (cheap cache key)
    aggressive                     Syntax-stripped content
    entropy                        Shannon entropy filtered
    diff                           Changed lines only
    lines:N-M                      Specific line ranges (e.g. lines:10-50,80)

ENVIRONMENT:
    LEAN_CTX_DISABLED=1            Bypass ALL compression + prevent shell hook from loading
    LEAN_CTX_ENABLED=0             Prevent shell hook auto-start (lean-ctx-on still works)
    LEAN_CTX_RAW=1                 Same as --raw for current command
    LEAN_CTX_AUTONOMY=false        Disable autonomous features
    LEAN_CTX_COMPRESS=1            Force compression (even for excluded commands)

OPTIONS:
    --version, -V                  Show version
    --help, -h                     Show this help

EXAMPLES:
    lean-ctx -c \"git status\"       Compressed git output
    lean-ctx -c \"kubectl get pods\" Compressed k8s output
    lean-ctx -c \"gh pr list\"       Compressed GitHub CLI output
    lean-ctx gain                  Visual terminal dashboard
    lean-ctx gain --live           Live auto-updating terminal dashboard
    lean-ctx gain --graph          30-day savings chart
    lean-ctx gain --daily          Day-by-day breakdown with USD
         lean-ctx token-report --json   Machine-readable token + memory report
    lean-ctx dashboard             Open web dashboard at localhost:3333
    lean-ctx dashboard --host=0.0.0.0  Bind to all interfaces (remote access)
    lean-ctx wrapped               Weekly savings report card
    lean-ctx wrapped --month       Monthly savings report card
    lean-ctx sessions list         List all CCP sessions
    lean-ctx sessions show         Show latest session state
    lean-ctx discover              Find missed savings in shell history
    lean-ctx setup                 One-command setup (shell + editors + verify)
    lean-ctx bootstrap             Non-interactive setup + fix (zero-config)
    lean-ctx bootstrap --json      Machine-readable bootstrap report
    lean-ctx init --global         Install shell aliases (includes lean-ctx-on/off/mode/status)
    lean-ctx-on                    Enable shell aliases in track mode (full output + stats)
    lean-ctx-off                   Disable all shell aliases
    lean-ctx-mode track            Track mode: full output, stats recorded (default)
    lean-ctx-mode compress         Compress mode: all output compressed (power users)
    lean-ctx-mode off              Same as lean-ctx-off
    lean-ctx-status                Show whether compression is active
    lean-ctx init --agent pi       Install Pi Coding Agent extension
    lean-ctx doctor                Check PATH, config, MCP, and dashboard port
    lean-ctx doctor --fix --json   Repair + machine-readable report
    lean-ctx status --json         Machine-readable current status
    lean-ctx read src/main.rs -m map
    lean-ctx grep \"pub fn\" src/
    lean-ctx deps .

CLOUD:
    cloud status                   Show cloud connection status
    login <email>                  Log into existing LeanCTX Cloud account
    register <email>               Create a new LeanCTX Cloud account
    forgot-password <email>        Send password reset email
    sync                           Upload local stats to cloud dashboard
    contribute                     Share anonymized compression data

TROUBLESHOOTING:
    Commands broken?     lean-ctx-off             (fixes current session)
    Permanent fix?       lean-ctx uninstall       (removes all hooks)
    Manual fix?          Edit ~/.zshrc, remove the \"lean-ctx shell hook\" block
    Binary missing?      Aliases auto-fallback to original commands (safe)
    Preview init?        lean-ctx init --global --dry-run

WEBSITE: https://leanctx.com
GITHUB:  https://github.com/yvgude/lean-ctx
",
        version = env!("CARGO_PKG_VERSION"),
    );
}
