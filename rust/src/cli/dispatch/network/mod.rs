use crate::{dashboard, tui};

#[cfg(feature = "gateway-server")]
mod gateway;
#[cfg(feature = "gateway-server")]
pub(crate) use gateway::*;
mod provider;
pub(crate) use provider::*;
mod proxy;
pub(crate) use proxy::*;
mod team;
pub(crate) use team::*;
#[cfg(test)]
mod tests;

/// Open-mode when a `--vscode` / `--open=vscode` hand-off cannot produce a
/// native editor tab (extension missing, or not inside an editor). Invariant
/// (#424/#587): an explicit vscode intent NEVER falls back to the external
/// browser — it shows the URL + how to open the dashboard inside the editor
/// instead. Only `--no-open` downgrades it to a silent "none".
fn vscode_fallback_open_mode(no_open: bool) -> &'static str {
    if no_open { "none" } else { "vscode" }
}

pub(super) fn cmd_dashboard(rest: &[String]) {
    if rest.iter().any(|a| a == "--help" || a == "-h") {
        println!(
            "Usage: lean-ctx dashboard [--port=N] [--host=H] [--base-path=PREFIX] [--auth-token=TOKEN] [--no-auth] [--project=PATH] [--vscode] [--export]"
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
        println!(
            "  lean-ctx dashboard --no-auth                Run without a Bearer token (alias --auth=false)."
        );
        println!(
            "                                              Cross-origin/CSRF + DNS-rebinding stay blocked via"
        );
        println!(
            "                                              Sec-Fetch-Site/Origin/Host checks. Best on loopback;"
        );
        println!(
            "                                              for Docker publish to 127.0.0.1 (-p 127.0.0.1:PORT:PORT)."
        );
        println!("  lean-ctx dashboard --export        Export HTML report (replaces visualize)");
        println!(
            "  lean-ctx dashboard --open=none      Start without launching a browser (also --no-open)"
        );
        println!(
            "  lean-ctx dashboard --vscode         Open as a native editor tab (VS Code/Cursor/VSCodium/Windsurf) via the lean-ctx extension"
        );
        println!(
            "  lean-ctx dashboard --open=vscode    Alias for --vscode (falls back to printing how to open it inside the editor — never the external browser)"
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
        println!(
            "  LEAN_CTX_DASHBOARD_AUTH=true|false  Require the Bearer token (default true). false = no-auth mode (overridden by --no-auth/--auth=). Also settable via `lean-ctx config set dashboard_auth`."
        );
        println!(
            "  LEAN_CTX_DASHBOARD_ALLOWED_HOSTS=host:port,…  Extra Host header values accepted in no-auth mode (loopback + bound host are always allowed)."
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
    // `--no-auth` / `--auth=<bool>`: run the dashboard without a Bearer token.
    // No-auth is not unprotected — cross-origin/CSRF and DNS-rebinding are blocked
    // by request-header checks (Sec-Fetch-Site/Origin/Host allowlist). Precedence:
    // this flag > LEAN_CTX_DASHBOARD_AUTH env > `dashboard_auth` config > true.
    // `None` = "not given on the CLI" so the env/config decides.
    let auth_enabled = if rest.iter().any(|a| a == "--no-auth") {
        Some(false)
    } else {
        rest.iter()
            .find_map(|p| p.strip_prefix("--auth="))
            .and_then(|v| match v.trim().to_ascii_lowercase().as_str() {
                "true" | "1" | "yes" | "on" => Some(true),
                "false" | "0" | "no" | "off" => Some(false),
                _ => None,
            })
    };
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
    // `--vscode` / `--open=vscode` (and LEAN_CTX_DASHBOARD_OPEN=vscode): open the
    // dashboard as a native editor tab by handing off to the lean-ctx extension's
    // URI handler. On a successful hand-off the extension owns the server, so we
    // return without binding one here. Otherwise we fall back to the `vscode`
    // guidance mode — print the URL + how to open it inside the editor — and
    // NEVER the external browser (#424/#587). It stays "never a silent no-op"
    // (#875) because the URL and actionable steps are always printed.
    let want_vscode = rest.iter().any(|a| a == "--vscode")
        || matches!(open_mode.as_deref(), Some("vscode" | "code" | "editor"))
        || (open_mode.is_none()
            && std::env::var("LEAN_CTX_DASHBOARD_OPEN").is_ok_and(|v| {
                matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "vscode" | "code" | "editor"
                )
            }));
    let open_mode = if want_vscode {
        use crate::dashboard::vscode_open::{EditorOpen, open_in_editor};
        let fallback = vscode_fallback_open_mode(rest.iter().any(|a| a == "--no-open"));
        match open_in_editor() {
            EditorOpen::Handed(label) => {
                println!("\x1b[32m✓\x1b[0m Opening the lean-ctx dashboard in {label}…");
                println!(
                    "  \x1b[2mIt opens as a native editor tab via the lean-ctx extension.\x1b[0m"
                );
                return;
            }
            EditorOpen::NeedsExtension(label) => {
                eprintln!(
                    "  \x1b[33m⚠\x1b[0m {label} detected, but the lean-ctx extension isn't \
                     installed — showing how to open the dashboard inside {label} instead."
                );
                eprintln!(
                    "  \x1b[2mInstall \"lean-ctx\" from the {label} Extensions view for a one-step native tab.\x1b[0m"
                );
                Some(fallback.to_string())
            }
            EditorOpen::NoEditor => Some(fallback.to_string()),
        }
    } else {
        open_mode
    };
    // GH #450: pin the XDG layout before serving, exactly like the daemon/server
    // start paths do. Without this the dashboard was the only writer that could
    // land config.toml in a divergent (unpinned/legacy) dir while the runtime
    // read another — so a saved quick-setting silently "reset" on the next read.
    crate::core::layout_pin::heal();
    super::spawn_proxy_if_needed();
    super::run_async(dashboard::start(
        port,
        host,
        base_path,
        auth_token,
        open_mode,
        auth_enabled,
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
