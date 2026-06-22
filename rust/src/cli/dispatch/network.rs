use crate::{dashboard, tui};

pub(super) fn cmd_team(rest: &[String]) {
    let sub = rest.first().map_or("help", std::string::String::as_str);
    match sub {
        "serve" => {
            let cfg_path = rest
                .iter()
                .enumerate()
                .find_map(|(i, a)| {
                    if let Some(v) = a.strip_prefix("--config=") {
                        return Some(v.to_string());
                    }
                    if a == "--config" {
                        return rest.get(i + 1).cloned();
                    }
                    None
                })
                .unwrap_or_default();

            if cfg_path.trim().is_empty() {
                eprintln!("Usage: lean-ctx team serve --config <path>");
                std::process::exit(1);
            }

            let cfg =
                crate::http_server::team::TeamServerConfig::load(std::path::Path::new(&cfg_path))
                    .unwrap_or_else(|e| {
                        eprintln!("Invalid team config: {e}");
                        std::process::exit(1);
                    });

            if let Err(e) = super::run_async(crate::http_server::team::serve_team(cfg)) {
                tracing::error!("Team server error: {e}");
                std::process::exit(1);
            }
        }
        "token" => {
            let action = rest.get(1).map_or("help", std::string::String::as_str);
            if action == "create" {
                let args = &rest[2..];
                let cfg_path = args
                    .iter()
                    .enumerate()
                    .find_map(|(i, a)| {
                        if let Some(v) = a.strip_prefix("--config=") {
                            return Some(v.to_string());
                        }
                        if a == "--config" {
                            return args.get(i + 1).cloned();
                        }
                        None
                    })
                    .unwrap_or_default();
                let token_id = args
                    .iter()
                    .enumerate()
                    .find_map(|(i, a)| {
                        if let Some(v) = a.strip_prefix("--id=") {
                            return Some(v.to_string());
                        }
                        if a == "--id" {
                            return args.get(i + 1).cloned();
                        }
                        None
                    })
                    .unwrap_or_default();
                let scopes_csv = args
                    .iter()
                    .enumerate()
                    .find_map(|(i, a)| {
                        if let Some(v) = a.strip_prefix("--scopes=") {
                            return Some(v.to_string());
                        }
                        if let Some(v) = a.strip_prefix("--scope=") {
                            return Some(v.to_string());
                        }
                        if a == "--scopes" || a == "--scope" {
                            return args.get(i + 1).cloned();
                        }
                        None
                    })
                    .unwrap_or_default();
                let role_arg = args.iter().enumerate().find_map(|(i, a)| {
                    if let Some(v) = a.strip_prefix("--role=") {
                        return Some(v.to_string());
                    }
                    if a == "--role" {
                        return args.get(i + 1).cloned();
                    }
                    None
                });

                // EPIC 13.2: a token may be granted via explicit scopes and/or a
                // coarse role (viewer/member/admin/owner).
                if cfg_path.trim().is_empty()
                    || token_id.trim().is_empty()
                    || (scopes_csv.trim().is_empty() && role_arg.is_none())
                {
                    eprintln!(
                        "Usage: lean-ctx team token create --config <path> --id <id> (--scopes <csv> | --role <viewer|member|admin|owner>)"
                    );
                    std::process::exit(1);
                }

                let role = match role_arg.as_deref() {
                    Some(r) => {
                        let Some(role) = crate::http_server::team::TeamRole::parse(r) else {
                            eprintln!("Unknown role: {r}. Valid: viewer, member, admin, owner");
                            std::process::exit(1);
                        };
                        Some(role)
                    }
                    None => None,
                };

                let cfg_p = std::path::PathBuf::from(&cfg_path);
                let mut cfg = crate::http_server::team::TeamServerConfig::load(cfg_p.as_path())
                    .unwrap_or_else(|e| {
                        eprintln!("Invalid team config: {e}");
                        std::process::exit(1);
                    });

                let mut scopes = Vec::new();
                for part in scopes_csv.split(',') {
                    let p = part.trim().to_ascii_lowercase();
                    if p.is_empty() {
                        continue;
                    }
                    let scope = match p.as_str() {
                        "search" => crate::http_server::team::TeamScope::Search,
                        "graph" => crate::http_server::team::TeamScope::Graph,
                        "artifacts" => crate::http_server::team::TeamScope::Artifacts,
                        "index" => crate::http_server::team::TeamScope::Index,
                        "events" => crate::http_server::team::TeamScope::Events,
                        "sessionmutations" | "session_mutations" => {
                            crate::http_server::team::TeamScope::SessionMutations
                        }
                        "knowledge" => crate::http_server::team::TeamScope::Knowledge,
                        "audit" => crate::http_server::team::TeamScope::Audit,
                        _ => {
                            eprintln!(
                                "Unknown scope: {p}. Valid: search, graph, artifacts, index, events, sessionmutations, knowledge, audit"
                            );
                            std::process::exit(1);
                        }
                    };
                    if !scopes.contains(&scope) {
                        scopes.push(scope);
                    }
                }
                if scopes.is_empty() && role.is_none() {
                    eprintln!("At least 1 scope or a role is required");
                    std::process::exit(1);
                }

                let (token, hash) = crate::http_server::team::create_token().unwrap_or_else(|e| {
                    eprintln!("Token generation failed: {e}");
                    std::process::exit(1);
                });

                cfg.tokens.push(crate::http_server::team::TeamTokenConfig {
                    id: token_id,
                    sha256_hex: hash,
                    scopes,
                    role,
                });

                cfg.save(cfg_p.as_path()).unwrap_or_else(|e| {
                    eprintln!("Failed to write config: {e}");
                    std::process::exit(1);
                });

                println!("{token}");
                return;
            }
            eprintln!("Usage: lean-ctx team token create --config <path> --id <id> --scopes <csv>");
            std::process::exit(1);
        }
        "slo-report" => {
            cmd_team_slo_report(&rest[1..]);
        }
        "sync" => {
            let args = &rest[1..];
            let cfg_path = args
                .iter()
                .enumerate()
                .find_map(|(i, a)| {
                    if let Some(v) = a.strip_prefix("--config=") {
                        return Some(v.to_string());
                    }
                    if a == "--config" {
                        return args.get(i + 1).cloned();
                    }
                    None
                })
                .unwrap_or_default();
            if cfg_path.trim().is_empty() {
                eprintln!("Usage: lean-ctx team sync --config <path> [--workspace <id>]");
                std::process::exit(1);
            }
            let only_ws = args.iter().enumerate().find_map(|(i, a)| {
                if let Some(v) = a.strip_prefix("--workspace=") {
                    return Some(v.to_string());
                }
                if let Some(v) = a.strip_prefix("--workspace-id=") {
                    return Some(v.to_string());
                }
                if a == "--workspace" || a == "--workspace-id" {
                    return args.get(i + 1).cloned();
                }
                None
            });

            let cfg =
                crate::http_server::team::TeamServerConfig::load(std::path::Path::new(&cfg_path))
                    .unwrap_or_else(|e| {
                        eprintln!("Invalid team config: {e}");
                        std::process::exit(1);
                    });

            for ws in &cfg.workspaces {
                if let Some(ref only) = only_ws
                    && ws.id != *only
                {
                    continue;
                }
                let git_dir = ws.root.join(".git");
                if !git_dir.exists() {
                    eprintln!(
                        "workspace '{}' root is not a git repo: {}",
                        ws.id,
                        ws.root.display()
                    );
                    std::process::exit(1);
                }
                let status = std::process::Command::new("git")
                    .arg("-C")
                    .arg(&ws.root)
                    .args(["fetch", "--all", "--prune"])
                    .status()
                    .unwrap_or_else(|e| {
                        eprintln!("git fetch failed for workspace '{}': {e}", ws.id);
                        std::process::exit(1);
                    });
                if !status.success() {
                    eprintln!(
                        "git fetch failed for workspace '{}' (exit={})",
                        ws.id,
                        status.code().unwrap_or(1)
                    );
                    std::process::exit(1);
                }
            }
        }
        _ => {
            eprintln!(
                "Usage:\n  lean-ctx team serve --config <path>\n  lean-ctx team token create --config <path> --id <id> --scopes <csv>\n  lean-ctx team sync --config <path> [--workspace <id>]\n  lean-ctx team slo-report --server <url> --token <token> [--json]"
            );
            std::process::exit(1);
        }
    }
}

/// `lean-ctx team slo-report` — fetches `/v1/metrics` from a team server and
/// renders the hosted-index SLO gate (GL #391). Exit code 0 = all objectives
/// green, 1 = at least one violated (CI-friendly for the 30-day GA gate).
fn cmd_team_slo_report(args: &[String]) {
    let flag = |name: &str| -> Option<String> {
        args.iter().enumerate().find_map(|(i, a)| {
            if let Some(v) = a.strip_prefix(&format!("--{name}=")) {
                return Some(v.to_string());
            }
            if a == format!("--{name}").as_str() {
                return args.get(i + 1).cloned();
            }
            None
        })
    };
    let server = flag("server").unwrap_or_default();
    let token = flag("token")
        .or_else(|| std::env::var("LEAN_CTX_TEAM_TOKEN").ok())
        .unwrap_or_default();
    let json_out = args.iter().any(|a| a == "--json");

    if server.trim().is_empty() || token.trim().is_empty() {
        eprintln!(
            "Usage: lean-ctx team slo-report --server <url> --token <token> [--json]\n  (token also via LEAN_CTX_TEAM_TOKEN)"
        );
        std::process::exit(1);
    }

    let url = format!("{}/v1/metrics", server.trim_end_matches('/'));
    let body = match ureq::get(&url)
        .header("Authorization", &format!("Bearer {token}"))
        .call()
    {
        Ok(resp) => resp.into_body().read_to_string().unwrap_or_default(),
        Err(e) => {
            eprintln!("\x1b[31m✗\x1b[0m Could not reach team server at {url}: {e}");
            std::process::exit(1);
        }
    };
    let v: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("\x1b[31m✗\x1b[0m Invalid /v1/metrics response: {e}");
            std::process::exit(1);
        }
    };
    let Some(slo) = v.get("slo") else {
        eprintln!(
            "\x1b[31m✗\x1b[0m Server response has no `slo` block — server predates GL #391; upgrade the team server."
        );
        std::process::exit(1);
    };

    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(slo).unwrap_or_else(|_| slo.to_string())
        );
    }

    let read_f64 = |key: &str| slo.get(key).and_then(serde_json::Value::as_f64);
    let availability = read_f64("availability_pct").unwrap_or(100.0);
    let p95 = read_f64("p95_ms").unwrap_or(0.0);
    let lag = read_f64("index_lag_seconds");
    let window = slo.get("window_len").and_then(serde_json::Value::as_u64);
    let uptime = slo
        .get("uptime_seconds")
        .and_then(serde_json::Value::as_u64);

    // The three GA-gate objectives (docs/examples/team-slos.toml).
    let avail_ok = availability >= 99.5;
    let p95_ok = p95 < 500.0;
    let lag_ok = lag.is_none_or(|secs| secs < 300.0);

    if !json_out {
        let mark = |ok: bool| {
            if ok {
                "\x1b[32mOK\x1b[0m"
            } else {
                "\x1b[31mVIOLATED\x1b[0m"
            }
        };
        println!("Hosted Index SLO Report — {server}");
        println!(
            "  Availability  {availability:7.2} %   (target ≥ 99.5)   {}",
            mark(avail_ok)
        );
        println!(
            "  Query p95     {p95:7.0} ms   (target < 500)    {}",
            mark(p95_ok)
        );
        match lag {
            Some(secs) => println!(
                "  Index lag     {secs:7.0} s    (target < 300)    {}",
                mark(lag_ok)
            ),
            None => println!("  Index lag         n/a    (no index write observed yet)"),
        }
        if let (Some(win), Some(up)) = (window, uptime) {
            let (days, hours, mins) = (up / 86_400, (up % 86_400) / 3_600, (up % 3_600) / 60);
            println!("  Window        {win} requests · uptime {days}d {hours}h {mins}m");
        }
        if avail_ok && p95_ok && lag_ok {
            println!("  \x1b[32m→ GA gate: PASS (all objectives green)\x1b[0m");
        } else {
            println!("  \x1b[31m→ GA gate: FAIL\x1b[0m   Runbook: docs/guides/hosted-index-slo.md");
        }
    }

    if !(avail_ok && p95_ok && lag_ok) {
        std::process::exit(1);
    }
}

pub(super) fn cmd_dashboard(rest: &[String]) {
    if rest.iter().any(|a| a == "--help" || a == "-h") {
        println!(
            "Usage: lean-ctx dashboard [--port=N] [--host=H] [--base-path=PREFIX] [--auth-token=TOKEN] [--project=PATH] [--export]"
        );
        println!("Examples:");
        println!("  lean-ctx dashboard");
        println!("  lean-ctx dashboard --port=3333");
        println!("  lean-ctx dashboard --host=0.0.0.0");
        println!(
            "  lean-ctx dashboard --base-path=/dashboard   Mount behind a reverse proxy subpath"
        );
        println!(
            "  lean-ctx dashboard --auth-token=<token>     Pin the Bearer token (alias --token; overrides LEAN_CTX_HTTP_TOKEN)"
        );
        println!("  lean-ctx dashboard --export        Export HTML report (replaces visualize)");
        println!(
            "  lean-ctx dashboard --open=none      Start without launching a browser (also --no-open)"
        );
        println!(
            "  lean-ctx dashboard --open=vscode    Don't launch external browser; show how to open the native VS Code tab"
        );
        println!("Environment:");
        println!(
            "  LEAN_CTX_DASHBOARD_OPEN=browser|none|vscode  Default reveal mode (overridden by --open=)."
        );
        println!(
            "  LEAN_CTX_HTTP_TOKEN=<token>   Pin the dashboard Bearer token (stable across restarts — ideal behind a reverse proxy). Overridden by --auth-token. Unset → a random token is generated each start."
        );
        println!(
            "  LEAN_CTX_SCRAPE_TOKEN=<token> Read-only token accepted ONLY for GET /metrics — hand this to Prometheus/Datadog agents instead of the dashboard token (docs/integrations/datadog.md)."
        );
        return;
    }
    if rest.iter().any(|a| a == "--export") {
        let output = rest
            .iter()
            .find_map(|a| a.strip_prefix("--output="))
            .unwrap_or("lean-ctx-report.html");
        let open = rest.iter().any(|a| a == "--open");
        crate::cli::cmd_visualize(&[
            format!("--output={output}"),
            if open {
                "--open".to_string()
            } else {
                String::new()
            },
        ]);
        return;
    }
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
        // SAFETY: runs during single-threaded CLI argument parsing, before the
        // dashboard server (and its threads) starts.
        unsafe { std::env::set_var("LEAN_CTX_DASHBOARD_PROJECT", p) };
    }
    // `--base-path` / `--prefix`: mount the dashboard behind a reverse-proxy
    // subpath (e.g. `/dashboard`). See dashboard::base_path (#355).
    let base_path = rest
        .iter()
        .find_map(|p| {
            p.strip_prefix("--base-path=")
                .or_else(|| p.strip_prefix("--prefix="))
        })
        .map(String::from);
    // `--auth-token` / `--token`: pin the dashboard Bearer token from the CLI.
    // Takes precedence over LEAN_CTX_HTTP_TOKEN so it survives container/service
    // environments that strip or fail to inherit the env var (#377).
    let auth_token = rest
        .iter()
        .find_map(|p| {
            p.strip_prefix("--auth-token=")
                .or_else(|| p.strip_prefix("--token="))
        })
        .map(String::from);
    // `--open=<browser|none|vscode>`: how to reveal the URL once the server is
    // up. `--no-open` is shorthand for `--open=none` (#424). Overrides
    // LEAN_CTX_DASHBOARD_OPEN.
    let open_mode = if rest.iter().any(|a| a == "--no-open") {
        Some("none".to_string())
    } else {
        rest.iter()
            .find_map(|p| p.strip_prefix("--open="))
            .map(String::from)
    };
    // GH #450: pin the XDG layout before serving, exactly like the daemon/server
    // start paths do. Without this the dashboard was the only writer that could
    // land config.toml in a divergent (unpinned/legacy) dir while the runtime
    // read another — so a saved quick-setting silently "reset" on the next read.
    crate::core::layout_pin::heal();
    super::spawn_proxy_if_needed();
    super::run_async(dashboard::start(
        port, host, base_path, auth_token, open_mode,
    ));
}

pub(super) fn cmd_watch(rest: &[String]) {
    if rest.iter().any(|a| a == "--help" || a == "-h") {
        println!("Usage: lean-ctx watch");
        println!("  Live TUI dashboard (real-time event stream).");
        return;
    }
    if let Err(e) = tui::run() {
        tracing::error!("TUI error: {e}");
        std::process::exit(1);
    }
}

/// Parse `--port=N` from proxy args, falling back to the configured default.
#[cfg(feature = "http-server")]
fn parse_proxy_port(rest: &[String]) -> u16 {
    rest.iter()
        .find_map(|p| p.strip_prefix("--port="))
        .and_then(|p| p.parse().ok())
        .unwrap_or_else(crate::proxy_setup::default_port)
}

/// Stops a standalone/foreground proxy by reading its PID from `/health` and
/// terminating it (graceful, then force). Returns true if a proxy was reachable
/// on `port`, false if nothing was listening. Shared by `stop` and `restart`.
#[cfg(feature = "http-server")]
fn stop_proxy_process(port: u16) -> bool {
    let health_url = format!("http://127.0.0.1:{port}/health");
    let Ok(resp) = ureq::get(&health_url).call() else {
        return false;
    };
    let pid = resp.into_body().read_to_string().ok().and_then(|body| {
        body.split("pid\":")
            .nth(1)
            .and_then(|s| s.split([',', '}']).next())
            .and_then(|s| s.trim().parse::<u32>().ok())
    });
    match pid {
        Some(pid) => {
            let _ = crate::ipc::process::terminate_gracefully(pid);
            std::thread::sleep(std::time::Duration::from_millis(500));
            if crate::ipc::process::is_alive(pid) {
                let _ = crate::ipc::process::force_kill(pid);
            }
            println!("Proxy on port {port} stopped (PID {pid}).");
        }
        None => {
            println!(
                "Proxy on port {port} running but could not parse PID. Use `lean-ctx stop` to kill all."
            );
        }
    }
    true
}

/// Prints the proxy's live upstreams (from `/status`) and warns when they drift
/// from what the operator expects. Covers both #449 cases: a shell-exported
/// `LEAN_CTX_*_UPSTREAM` that never reached the MCP/service-spawned proxy, and a
/// proxy started with an env override that now masks a later config.toml edit.
#[cfg(feature = "http-server")]
fn print_live_upstreams_and_drift(v: &serde_json::Value, cfg: &crate::core::config::Config) {
    use crate::core::config::{
        ProxyProvider, UpstreamDrift, diagnose_drift, env_upstream_override,
    };

    let Some(up) = v.get("upstreams").and_then(|u| u.as_object()) else {
        return;
    };
    let disk = cfg.proxy.resolve_all_disk();
    println!("  Upstreams (live):");
    let mut notes = Vec::new();
    for (label, key, provider, disk_val) in [
        (
            "Anthropic",
            "anthropic",
            ProxyProvider::Anthropic,
            &disk.anthropic,
        ),
        ("OpenAI", "openai", ProxyProvider::OpenAi, &disk.openai),
        ("Gemini", "gemini", ProxyProvider::Gemini, &disk.gemini),
    ] {
        let live = up.get(key).and_then(|x| x.as_str()).unwrap_or("?");
        println!("    {label:<10} {live}");
        if live == "?" {
            continue;
        }
        let env = env_upstream_override(provider);
        match diagnose_drift(env.as_deref(), disk_val, live) {
            Some(UpstreamDrift::EnvNotApplied) => {
                let want = env.as_deref().unwrap_or("");
                notes.push(format!(
                    "  \x1b[33m⚠ {label}: LEAN_CTX_{}_UPSTREAM is set in this shell ({want})\x1b[0m\n  \
                       \x1b[33m  but the running proxy serves {live}. Environment variables do not reach\x1b[0m\n  \
                       \x1b[33m  an MCP/service-spawned proxy (#449). Persist it — applies live:\x1b[0m\n  \
                       \x1b[33m    lean-ctx config set proxy.{key}_upstream {want}\x1b[0m",
                    label.to_uppercase(),
                ));
            }
            Some(UpstreamDrift::ConfigNotApplied) => {
                notes.push(format!(
                    "  \x1b[33m⚠ {label}: proxy serves {live} but config.toml resolves to {disk_val}.\x1b[0m\n  \
                       \x1b[33m  Apply it: lean-ctx proxy restart\x1b[0m",
                ));
            }
            None => {}
        }
    }
    for note in notes {
        println!();
        println!("{note}");
    }
}

pub(super) fn cmd_proxy(rest: &[String]) {
    #[cfg(feature = "http-server")]
    {
        // `--help` anywhere must never execute the verb (GH #393).
        if wants_help(rest) {
            println!(
                "Usage: lean-ctx proxy <start|stop|restart|status|enable|disable|cleanup> [--port=4444]"
            );
            println!();
            println!("Commands:");
            println!(
                "  start     Run the compression proxy (foreground; --autostart installs a service)"
            );
            println!("  stop      Stop the proxy on the given port");
            println!(
                "  restart   Restart the managed proxy (re-reads config.toml; drops env overrides)"
            );
            println!("  status    Show proxy config, process, live upstreams and stats");
            println!("  enable    Enable the proxy: config flag, autostart service, env wiring");
            println!("  disable   Disable the proxy and restore the original endpoint");
            println!("  cleanup   Remove stale proxy URLs from AI tool configs");
            return;
        }
        let sub = rest.first().map_or("help", std::string::String::as_str);
        match sub {
            "start" => {
                let port: u16 = rest
                    .iter()
                    .find_map(|p| p.strip_prefix("--port=").or_else(|| p.strip_prefix("-p=")))
                    .and_then(|p| p.parse().ok())
                    .unwrap_or_else(crate::proxy_setup::default_port);
                let autostart = rest.iter().any(|a| a == "--autostart");
                if autostart {
                    crate::proxy_autostart::install(port, false);
                    return;
                }
                if let Err(e) = super::run_async(crate::proxy::start_proxy(port)) {
                    tracing::error!("Proxy error: {e}");
                    std::process::exit(1);
                }
            }
            "stop" => {
                let port = parse_proxy_port(rest);
                if !stop_proxy_process(port) {
                    println!("No proxy running on port {port}.");
                }
            }
            "restart" => {
                let port = parse_proxy_port(rest);
                if crate::proxy_autostart::is_installed() {
                    // Managed service (LaunchAgent / systemd): a clean bootout +
                    // bootstrap restarts the proxy so it re-reads config.toml. It
                    // deliberately drops any `LEAN_CTX_*_UPSTREAM` env override
                    // (the service context has none), making config.toml the
                    // single source of truth for the long-lived proxy (#449).
                    crate::proxy_autostart::stop();
                    std::thread::sleep(std::time::Duration::from_millis(700));
                    crate::proxy_autostart::start();
                    println!("\x1b[32m✓\x1b[0m Proxy restarted (managed service).");
                    println!("  Verify active upstreams: lean-ctx proxy status");
                } else if stop_proxy_process(port) {
                    println!();
                    println!("  No autostart service installed — start the proxy again:");
                    println!("    lean-ctx proxy start --port={port}");
                } else {
                    println!("No proxy running on port {port} and no autostart service installed.");
                    println!("  Start it now:       lean-ctx proxy start --port={port}");
                    println!("  Or install service: lean-ctx proxy enable");
                }
            }
            "status" => {
                let port = parse_proxy_port(rest);
                let cfg = crate::core::config::Config::load();
                println!("lean-ctx proxy:");
                match cfg.proxy_enabled {
                    Some(true) => println!("  Config:  enabled"),
                    Some(false) => println!("  Config:  disabled"),
                    None => println!("  Config:  undecided (not yet configured)"),
                }
                println!("  Port:    {port}");
                // Liveness comes from the *public* /health endpoint so a running
                // proxy is never misreported as down — even mid-upgrade when the
                // managed proxy still holds an old session token (#449). The rich
                // detail (stats + live upstreams) comes from the authenticated
                // /status; if that 401s while /health is up, we still report it as
                // running and point at `proxy restart`.
                let alive = ureq::get(&format!("http://127.0.0.1:{port}/health"))
                    .call()
                    .is_ok();
                if alive {
                    println!("  Process: running");
                    let token =
                        crate::core::session_token::resolve_proxy_token("LEAN_CTX_PROXY_TOKEN");
                    let status = ureq::get(&format!("http://127.0.0.1:{port}/status"))
                        .header("Authorization", &format!("Bearer {token}"))
                        .call();
                    if let Ok(resp) = status {
                        let body = resp.into_body().read_to_string().unwrap_or_default();
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
                            println!("  Requests:    {}", v["requests_total"]);
                            println!("  Compressed:  {}", v["requests_compressed"]);
                            println!("  Tokens saved: {}", v["tokens_saved"]);
                            println!(
                                "  Compression: {}%",
                                v["compression_ratio_pct"].as_str().unwrap_or("0.0")
                            );
                            print_live_upstreams_and_drift(&v, &cfg);
                        }
                    } else {
                        println!(
                            "  \x1b[33m⚠ Live details unavailable: the running proxy rejects this\x1b[0m"
                        );
                        println!(
                            "  \x1b[33m  shell's session token. Re-sync it: lean-ctx proxy restart\x1b[0m"
                        );
                    }
                } else {
                    println!("  Process: not running");
                }
                if cfg.proxy_enabled == Some(false) || cfg.proxy_enabled.is_none() {
                    println!();
                    println!("  Enable: lean-ctx proxy enable");

                    let home = dirs::home_dir().unwrap_or_default();
                    if crate::proxy_setup::has_stale_proxy_url(&home) {
                        println!();
                        println!(
                            "  \x1b[33m⚠ WARNING: Claude Code ANTHROPIC_BASE_URL points to the local proxy,\x1b[0m"
                        );
                        println!(
                            "  \x1b[33m  but proxy is not enabled. This causes 401 auth failures.\x1b[0m"
                        );
                        println!("  Fix:  lean-ctx proxy cleanup   (remove stale URL)");
                        println!("        lean-ctx proxy enable    (enable the proxy)");
                    }
                }
            }
            "enable" => {
                let force = rest.iter().any(|a| a == "--force");
                if let Err(e) =
                    crate::core::config::Config::update_global(|c| c.proxy_enabled = Some(true))
                {
                    tracing::warn!("could not persist proxy_enabled: {e}");
                }

                let port = crate::proxy_setup::default_port();
                crate::proxy_autostart::install(port, false);
                std::thread::sleep(std::time::Duration::from_millis(500));

                let home = dirs::home_dir().unwrap_or_default();
                crate::proxy_setup::install_proxy_env_unchecked(&home, port, false, force);
                println!(
                    "\x1b[32m✓\x1b[0m Proxy enabled on port {port}. LLM requests will be compressed."
                );
            }
            "disable" => {
                if let Err(e) =
                    crate::core::config::Config::update_global(|c| c.proxy_enabled = Some(false))
                {
                    tracing::warn!("could not persist proxy_enabled: {e}");
                }

                crate::proxy_autostart::uninstall(false);
                let home = dirs::home_dir().unwrap_or_default();
                crate::proxy_setup::uninstall_proxy_env(&home, false);

                println!("\x1b[32m✓\x1b[0m Proxy disabled. Original endpoint restored.");
                println!("  Re-enable anytime: lean-ctx proxy enable");
            }
            "cleanup" => {
                let home = dirs::home_dir().unwrap_or_default();
                let removed = crate::proxy_setup::cleanup_stale_proxy_env(&home);
                if removed > 0 {
                    println!("\x1b[32m✓\x1b[0m Cleaned up {removed} stale proxy URL(s).");
                    println!("  Restart your AI tool for changes to take effect.");
                } else {
                    println!("  No stale proxy URLs found. Nothing to clean up.");
                }
            }
            _ => {
                println!(
                    "Usage: lean-ctx proxy <start|stop|restart|status|enable|disable|cleanup> [--port=4444]"
                );
            }
        }
    }
    #[cfg(not(feature = "http-server"))]
    {
        eprintln!("lean-ctx proxy is not available in this build");
        std::process::exit(1);
    }
}

/// True when the args ask for help anywhere (`--help`/`-h`/`help`).
/// Subcommand handlers must check this BEFORE executing: `lean-ctx daemon
/// enable --help` must print help, not install the service (GH #393).
pub(super) fn wants_help(args: &[String]) -> bool {
    args.iter()
        .any(|a| a == "--help" || a == "-h" || a == "help")
}

fn daemon_help() {
    println!("Usage: lean-ctx daemon <start|stop|restart|status|enable|disable>");
    println!();
    println!("Commands:");
    println!("  start     Start the daemon in the background");
    println!("  stop      Stop the running daemon");
    println!("  restart   Stop the daemon, then start it again");
    println!("  status    Show daemon status, PID, autostart state and service file");
    println!("  enable    Install + start the autostart service (systemd user unit / LaunchAgent)");
    println!("  disable   Stop + remove the autostart service");
    if let (Some(name), Some(path)) = (
        crate::daemon_autostart::service_name(),
        crate::daemon_autostart::service_file_path(),
    ) {
        println!();
        println!("Autostart service:");
        println!("  Name:         {name}");
        println!("  Service file: {}", path.display());
    }
}

pub(super) fn cmd_daemon(rest: &[String]) {
    // `--help` anywhere must never execute the verb (GH #393).
    if wants_help(rest) {
        daemon_help();
        return;
    }
    let sub = rest.first().map_or("status", std::string::String::as_str);
    match sub {
        "enable" => {
            crate::daemon_autostart::install(false);
            println!(
                "\x1b[32m✓\x1b[0m Daemon autostart enabled. Will start on login and restart if stopped."
            );
        }
        "disable" => {
            crate::daemon_autostart::uninstall(false);
            println!("\x1b[32m✓\x1b[0m Daemon autostart disabled.");
        }
        "start" => {
            if let Err(e) = crate::daemon::start_daemon(&rest[1..]) {
                eprintln!("Error: {e}");
                std::process::exit(1);
            }
        }
        "stop" => {
            crate::daemon_autostart::stop();
            match crate::daemon::stop_daemon() {
                Ok(()) => println!("Daemon stopped."),
                Err(e) => eprintln!("Error: {e}"),
            }
        }
        "restart" => {
            // Stop both the supervised service and a manually started daemon,
            // then start through the same channel that was active before.
            crate::daemon_autostart::stop();
            if let Err(e) = crate::daemon::stop_daemon() {
                println!("  (stop: {e})");
            }
            if crate::daemon_autostart::is_installed() {
                crate::daemon_autostart::start();
                println!("\x1b[32m✓\x1b[0m Daemon restarted via autostart service.");
            } else {
                match crate::daemon::start_daemon(&rest[1..]) {
                    Err(e) => {
                        eprintln!("Error: {e}");
                        std::process::exit(1);
                    }
                    _ => {
                        println!("\x1b[32m✓\x1b[0m Daemon restarted.");
                    }
                }
            }
        }
        "status" => {
            println!("lean-ctx daemon:");
            if crate::daemon::is_daemon_running() {
                let pid = crate::daemon::read_daemon_pid().unwrap_or(0);
                println!("  Status:    running (PID {pid})");
            } else {
                println!("  Status:    not running");
            }
            let installed = crate::daemon_autostart::is_installed();
            println!(
                "  Autostart: {}",
                if installed {
                    "enabled"
                } else {
                    "not installed (run: lean-ctx daemon enable)"
                }
            );
            if installed
                && let (Some(name), Some(path)) = (
                    crate::daemon_autostart::service_name(),
                    crate::daemon_autostart::service_file_path(),
                )
            {
                println!("  Service:   {name}");
                println!("  File:      {}", path.display());
            }
            if !crate::daemon::is_daemon_running() {
                println!();
                println!("  Start:     lean-ctx daemon start");
                if !installed {
                    println!("  Autostart: lean-ctx daemon enable");
                }
            }
        }
        _ => daemon_help(),
    }
}

pub(super) fn cmd_serve(rest: &[String]) {
    #[cfg(feature = "http-server")]
    {
        let mut cfg = crate::http_server::HttpServerConfig::default();
        let mut daemon_mode = false;
        let mut stop_mode = false;
        let mut status_mode = false;
        let mut foreground_daemon = false;
        let mut multi_roots: Vec<(String, Option<String>)> = Vec::new();
        let mut rrf_k: Option<f64> = None;
        let mut i = 0;
        while i < rest.len() {
            match rest[i].as_str() {
                "--daemon" | "-d" => daemon_mode = true,
                "--stop" => stop_mode = true,
                "--status" => status_mode = true,
                "--_foreground-daemon" => foreground_daemon = true,
                "--host" | "-H" => {
                    i += 1;
                    if i < rest.len() {
                        cfg.host.clone_from(&rest[i]);
                    }
                }
                arg if arg.starts_with("--host=") => {
                    cfg.host = arg["--host=".len()..].to_string();
                }
                "--port" | "-p" => {
                    i += 1;
                    if i < rest.len()
                        && let Ok(p) = rest[i].parse::<u16>()
                    {
                        cfg.port = p;
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
                    cfg.project_root = std::path::PathBuf::from(&arg["--project-root=".len()..]);
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
                "--root" => {
                    i += 1;
                    if i < rest.len() {
                        multi_roots.push((rest[i].clone(), None));
                    }
                }
                arg if arg.starts_with("--root=") => {
                    let val = arg["--root=".len()..].to_string();
                    if let Some((path, alias)) = val.split_once(':') {
                        multi_roots.push((path.to_string(), Some(alias.to_string())));
                    } else {
                        multi_roots.push((val, None));
                    }
                }
                "--rrf-k" => {
                    i += 1;
                    if i < rest.len() {
                        rrf_k = rest[i].parse::<f64>().ok();
                    }
                }
                arg if arg.starts_with("--rrf-k=") => {
                    rrf_k = arg["--rrf-k=".len()..].parse::<f64>().ok();
                }
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
                    if i < rest.len()
                        && let Ok(n) = rest[i].parse::<usize>()
                    {
                        cfg.max_body_bytes = n;
                    }
                }
                arg if arg.starts_with("--max-body-bytes=") => {
                    if let Ok(n) = arg["--max-body-bytes=".len()..].parse::<usize>() {
                        cfg.max_body_bytes = n;
                    }
                }
                "--max-concurrency" => {
                    i += 1;
                    if i < rest.len()
                        && let Ok(n) = rest[i].parse::<usize>()
                    {
                        cfg.max_concurrency = n;
                    }
                }
                arg if arg.starts_with("--max-concurrency=") => {
                    if let Ok(n) = arg["--max-concurrency=".len()..].parse::<usize>() {
                        cfg.max_concurrency = n;
                    }
                }
                "--max-rps" => {
                    i += 1;
                    if i < rest.len()
                        && let Ok(n) = rest[i].parse::<u32>()
                    {
                        cfg.max_rps = n;
                    }
                }
                arg if arg.starts_with("--max-rps=") => {
                    if let Ok(n) = arg["--max-rps=".len()..].parse::<u32>() {
                        cfg.max_rps = n;
                    }
                }
                "--rate-burst" => {
                    i += 1;
                    if i < rest.len()
                        && let Ok(n) = rest[i].parse::<u32>()
                    {
                        cfg.rate_burst = n;
                    }
                }
                arg if arg.starts_with("--rate-burst=") => {
                    if let Ok(n) = arg["--rate-burst=".len()..].parse::<u32>() {
                        cfg.rate_burst = n;
                    }
                }
                "--request-timeout-ms" => {
                    i += 1;
                    if i < rest.len()
                        && let Ok(n) = rest[i].parse::<u64>()
                    {
                        cfg.request_timeout_ms = n;
                    }
                }
                arg if arg.starts_with("--request-timeout-ms=") => {
                    if let Ok(n) = arg["--request-timeout-ms=".len()..].parse::<u64>() {
                        cfg.request_timeout_ms = n;
                    }
                }
                "--help" | "-h" => {
                    eprintln!(
                        "Usage: lean-ctx serve [--host H] [--port N] [--project-root DIR] [--daemon] [--stop] [--status]\\n\\
                         \\n\\
                         Options:\\n\\
                           --daemon, -d          Start as background daemon (UDS)\\n\\
                           --stop                Stop running daemon\\n\\
                           --status              Show daemon status\\n\\
                           --host, -H            Bind host (default: 127.0.0.1)\\n\\
                           --port, -p            Bind port (default: 8080)\\n\\
                           --project-root        Resolve relative paths against this root (default: cwd)\\n\\
                           --root PATH[:ALIAS]   Add a repo root for multi-repo mode (repeatable)\\n\\
                           --rrf-k N             RRF fusion parameter (default: 60.0)\\n\\
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

        if !multi_roots.is_empty() {
            if let Err(e) = crate::core::multi_repo::init_with_roots(&multi_roots, rrf_k) {
                eprintln!("Multi-repo init error: {e}");
                std::process::exit(1);
            }
            eprintln!("Multi-repo mode: {} roots configured", multi_roots.len());
        }

        if stop_mode {
            crate::daemon_autostart::stop();
            if let Err(e) = crate::daemon::stop_daemon() {
                eprintln!("Error: {e}");
                std::process::exit(1);
            }
            return;
        }

        if status_mode {
            println!("{}", crate::daemon::daemon_status());
            return;
        }

        if daemon_mode {
            if let Err(e) = crate::daemon::start_daemon(rest) {
                eprintln!("Error: {e}");
                std::process::exit(1);
            }
            return;
        }

        if foreground_daemon {
            if let Err(e) = crate::daemon::init_foreground_daemon() {
                eprintln!("Error writing PID file: {e}");
                std::process::exit(1);
            }
            let addr = crate::daemon::daemon_addr();
            if let Err(e) = super::run_async(crate::http_server::serve_ipc(cfg.clone(), addr)) {
                tracing::error!("Daemon server error: {e}");
                crate::daemon::cleanup_daemon_files();
                std::process::exit(1);
            }
            crate::daemon::cleanup_daemon_files();
            return;
        }

        if cfg.auth_token.is_none()
            && let Ok(v) = std::env::var("LEAN_CTX_HTTP_TOKEN")
            && !v.trim().is_empty()
        {
            cfg.auth_token = Some(v);
        }

        if let Err(e) = super::run_async(crate::http_server::serve(cfg)) {
            tracing::error!("HTTP server error: {e}");
            std::process::exit(1);
        }
    }
    #[cfg(not(feature = "http-server"))]
    {
        eprintln!("lean-ctx serve is not available in this build");
        std::process::exit(1);
    }
}

/// Reads a `--data-source <id>` / `--data-source=<id>` flag, defaulting to "jira".
fn data_source_flag(args: &[String]) -> String {
    args.iter()
        .enumerate()
        .find_map(|(i, a)| {
            if let Some(v) = a.strip_prefix("--data-source=") {
                return Some(v.to_string());
            }
            if a == "--data-source" {
                return args.get(i + 1).cloned();
            }
            None
        })
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "jira".to_string())
}

fn provider_usage() {
    eprintln!(
        "Usage: lean-ctx provider <command>\n\n\
         Commands:\n  \
         auth jira [--data-source <id>]     Connect a Jira Cloud site via OAuth 2.0 (3LO)\n  \
         logout jira [--data-source <id>]   Remove stored Jira OAuth credentials\n  \
         list                               List connected Jira OAuth data sources\n\n\
         Jira OAuth requires your own Atlassian app credentials in the environment:\n  \
         JIRA_OAUTH_CLIENT_ID, JIRA_OAUTH_CLIENT_SECRET\n  \
         (optional) JIRA_OAUTH_SCOPES — default: \"read:jira-work read:jira-user offline_access\"\n\n\
         Register a free app at https://developer.atlassian.com/console/myapps/"
    );
}

pub(super) fn cmd_provider(rest: &[String]) {
    use crate::core::providers::jira_oauth;

    let sub = rest.first().map_or("help", std::string::String::as_str);
    match sub {
        "auth" | "login" | "connect" => {
            let target = rest.get(1).map_or("", std::string::String::as_str);
            if !target.eq_ignore_ascii_case("jira") {
                eprintln!("Only 'jira' is supported for OAuth today.\n");
                provider_usage();
                std::process::exit(1);
            }
            let args: &[String] = if rest.len() > 2 { &rest[2..] } else { &[] };
            let data_source = data_source_flag(args);
            match jira_oauth::run_auth_flow(&data_source) {
                Ok(()) => {}
                Err(e) => {
                    eprintln!("\x1b[31m✗\x1b[0m Jira OAuth failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        "logout" | "disconnect" => {
            let target = rest.get(1).map_or("", std::string::String::as_str);
            if !target.eq_ignore_ascii_case("jira") {
                provider_usage();
                std::process::exit(1);
            }
            let args: &[String] = if rest.len() > 2 { &rest[2..] } else { &[] };
            let data_source = data_source_flag(args);
            match jira_oauth::remove_credential(&data_source) {
                Ok(true) => {
                    println!(
                        "\x1b[32m✓\x1b[0m Removed Jira OAuth credentials for '{data_source}'."
                    );
                }
                Ok(false) => {
                    println!("No stored Jira OAuth credentials for '{data_source}'.");
                }
                Err(e) => {
                    eprintln!("\x1b[31m✗\x1b[0m {e}");
                    std::process::exit(1);
                }
            }
        }
        "list" | "ls" | "status" => {
            let conns = jira_oauth::list_connections();
            if conns.is_empty() {
                println!("No Jira OAuth data sources connected. Run: lean-ctx provider auth jira");
            } else {
                println!("Connected Jira OAuth data sources:");
                for c in conns {
                    println!("  • {c}");
                }
            }
        }
        _ => provider_usage(),
    }
}

#[cfg(test)]
mod tests {
    use super::wants_help;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    // GH #393: `daemon enable --help` executed instead of showing help.
    // The guard must catch help flags at any position, for any verb.
    #[test]
    fn help_flag_detected_after_verb() {
        assert!(wants_help(&args(&["enable", "--help"])));
        assert!(wants_help(&args(&["disable", "-h"])));
        assert!(wants_help(&args(&["restart", "--help"])));
        assert!(wants_help(&args(&["help"])));
        assert!(wants_help(&args(&["--help"])));
    }

    #[test]
    fn normal_verbs_do_not_trigger_help() {
        assert!(!wants_help(&args(&["enable"])));
        assert!(!wants_help(&args(&["status"])));
        assert!(!wants_help(&args(&[])));
        // Values that merely contain "help" as a substring must not match.
        assert!(!wants_help(&args(&["--helper"])));
    }
}
