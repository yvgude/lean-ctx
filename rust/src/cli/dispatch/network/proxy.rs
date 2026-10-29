//! `lean-ctx proxy …` — proxy lifecycle, upstream stats, Codex ChatGPT opt-in.

#[allow(clippy::wildcard_imports)]
#[cfg(feature = "http-server")]
use super::*;

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

#[cfg(feature = "http-server")]
fn print_compression_by_upstream(v: &serde_json::Value) {
    let Some(per_upstream) = v.get("per_upstream").and_then(|u| u.as_object()) else {
        return;
    };
    println!("  Compression by upstream:");
    for (label, key) in [
        ("Anthropic", "anthropic"),
        ("OpenAI", "openai"),
        ("ChatGPT", "chatgpt"),
        ("Gemini", "gemini"),
        ("Grok", "grok"),
        ("CommandCode", "commandcode"),
    ] {
        let Some(row) = per_upstream.get(key).and_then(|x| x.as_object()) else {
            continue;
        };
        let requests = row
            .get("requests_total")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let saved = row
            .get("tokens_saved")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let ratio = row
            .get("compression_ratio_pct")
            .and_then(|x| x.as_str())
            .unwrap_or("0.0");
        println!("    {label:<10} {ratio:>5}% saved, {saved} tok, {requests} req");
    }
}

/// Prints the provider-verified savings line (#701) when counterfactual
/// metering has covered at least one request. Both sides of the pair were
/// counted by Anthropic on the same request — receipts, not the bytes/4
/// estimate above. Silent when the opt-in feature is off or no probe has
/// answered yet (`verified_savings` is `null`).
#[cfg(feature = "http-server")]
fn print_verified_savings(v: &serde_json::Value) {
    let Some(vs) = v.get("verified_savings").filter(|x| x.is_object()) else {
        return;
    };
    let saved = vs
        .get("verified_saved_tokens")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);
    let requests = vs
        .get("requests")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let counterfactual = vs
        .get("counterfactual_input_tokens")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    #[allow(clippy::cast_precision_loss)]
    let pct = if counterfactual > 0 {
        saved as f64 * 100.0 / counterfactual as f64
    } else {
        0.0
    };
    println!(
        "  Verified:    {saved} tok saved across {requests} probed req ({pct:.1}% — provider-counted, #701)"
    );
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
        ("ChatGPT", "chatgpt", ProxyProvider::ChatGpt, &disk.chatgpt),
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
    // Registry providers (enterprise#7): Grok dual-rail and other
    // `[[proxy.providers]]` entries only appear under `/providers/{id}/…`.
    if let Some(providers) = v.get("providers").and_then(|p| p.as_array()) {
        for p in providers {
            let id = p.get("id").and_then(|x| x.as_str()).unwrap_or("?");
            let base = p.get("base_url").and_then(|x| x.as_str()).unwrap_or("?");
            let label = if id.eq_ignore_ascii_case("grok-chat")
                || id.eq_ignore_ascii_case("xai")
                || id.eq_ignore_ascii_case("grok")
            {
                "Grok"
            } else if id.eq_ignore_ascii_case("commandcode")
                || id.eq_ignore_ascii_case("command-code")
            {
                "CommandCode"
            } else {
                id
            };
            // Prefer a stable display name; still show registry id when it
            // differs (e.g. grok-chat vs xai).
            let canonical_id = match label {
                "Grok" => Some("grok"),
                "CommandCode" => Some("commandcode"),
                _ => None,
            };
            if let Some(canon) = canonical_id
                && !id.eq_ignore_ascii_case(canon)
            {
                println!("    {label:<10} {base}  [{id}]");
            } else {
                println!("    {label:<10} {base}");
            }
        }
    }
    for note in notes {
        println!();
        println!("{note}");
    }

    // #590: a configured custom HTTPS upstream that is not permitted is silently
    // ignored — the managed proxy can't see the shell's env opt-in, so it serves
    // the provider default. Point the operator at the *persistent* config flag,
    // which does reach the managed proxy. Suppressed once the opt-in is active
    // (env or config), since the drift notes above then cover any remaining gap.
    if cfg.proxy.has_custom_host_upstream() && !cfg.proxy.allows_custom_upstream() {
        println!();
        println!(
            "  \x1b[33m⚠ A custom upstream host is configured but not permitted, so the proxy\x1b[0m"
        );
        println!(
            "  \x1b[33m  serves the provider default. Allow it (reaches the managed proxy):\x1b[0m"
        );
        println!("  \x1b[33m    lean-ctx config set proxy.allow_custom_upstream true\x1b[0m");
        println!("  \x1b[33m    lean-ctx proxy restart\x1b[0m");
    }
}

/// Bridges the shell's `LEAN_CTX_ALLOW_CUSTOM_UPSTREAM` opt-in into `config.toml`
/// so the managed (LaunchAgent / systemd) proxy — which only reads `config.toml`,
/// never the shell env — honors a configured custom upstream (#590).
///
/// No-op unless all three hold: the env opt-in is present, a custom-host upstream
/// is actually configured, and `[proxy] allow_custom_upstream` is not already
/// `true` (idempotent). Returns true when it persisted the flag.
#[cfg(feature = "http-server")]
fn bridge_custom_upstream_optin() -> bool {
    if std::env::var("LEAN_CTX_ALLOW_CUSTOM_UPSTREAM").is_err() {
        return false;
    }
    let cfg = crate::core::config::Config::load();
    if cfg.proxy.allow_custom_upstream == Some(true) || !cfg.proxy.has_custom_host_upstream() {
        return false;
    }
    match crate::core::config::Config::update_global(|c| {
        c.proxy.allow_custom_upstream = Some(true);
    }) {
        Ok(_) => {
            println!(
                "  \x1b[32m✓\x1b[0m Custom upstream opt-in persisted: [proxy] allow_custom_upstream = true"
            );
            println!(
                "  \x1b[2m  (so the managed proxy honors your custom upstream — the env var never reaches it, #590)\x1b[0m"
            );
            true
        }
        Err(e) => {
            tracing::warn!("could not persist allow_custom_upstream: {e}");
            false
        }
    }
}

/// Pure decision for [`bridge_codex_chatgpt_optin`]: persist the env opt-in only
/// when it is present in the shell and `[proxy] codex_chatgpt_proxy` has not
/// already enabled it (idempotent).
#[cfg(feature = "http-server")]
pub(super) fn should_persist_codex_chatgpt_optin(env_present: bool, current: Option<bool>) -> bool {
    env_present && current != Some(true)
}

/// Bridges the shell's `LEAN_CTX_CODEX_CHATGPT_PROXY` opt-in into `config.toml` so
/// the managed proxy and every env-less setup pass (the LaunchAgent / systemd
/// proxy, the lean-ctx daemon, editor integrations, `lean-ctx setup`) honor it —
/// none of them inherit the shell env (#449 / #590). Without this the foreground
/// `proxy enable` writes Codex's local `chatgpt_base_url`, but the next env-less
/// `install_proxy_env` pass sees the opt-in as `false` and strips it straight back
/// to native, so a ChatGPT subscription never actually routes through the proxy
/// (#603 / #616).
///
/// No-op unless the env opt-in is present and the config flag is not already
/// `true` (idempotent). Returns true when it persisted the flag.
#[cfg(feature = "http-server")]
fn bridge_codex_chatgpt_optin() -> bool {
    let env_present = std::env::var("LEAN_CTX_CODEX_CHATGPT_PROXY").is_ok();
    let current = crate::core::config::Config::load()
        .proxy
        .codex_chatgpt_proxy;
    if !should_persist_codex_chatgpt_optin(env_present, current) {
        return false;
    }
    match crate::core::config::Config::update_global(|c| {
        c.proxy.codex_chatgpt_proxy = Some(true);
    }) {
        Ok(_) => {
            println!(
                "  \x1b[32m✓\x1b[0m Codex ChatGPT proxy opt-in persisted: [proxy] codex_chatgpt_proxy = true"
            );
            println!(
                "  \x1b[2m  (so the managed proxy and every env-less setup pass route Codex through it — the shell env never reaches them, #603/#616)\x1b[0m"
            );
            true
        }
        Err(e) => {
            tracing::warn!("could not persist codex_chatgpt_proxy: {e}");
            false
        }
    }
}

/// Action selected by `proxy codex-chatgpt <arg>`. A bare/no-arg call reports
/// status (read-only), never silently mutating state.
#[cfg(feature = "http-server")]
#[derive(Debug, PartialEq, Eq)]
pub(super) enum CodexChatgptAction {
    On,
    Off,
    Status,
    Unknown,
}

/// Pure arg → action mapping for `proxy codex-chatgpt`. `on/enable/true` and
/// `off/disable/false` are accepted as synonyms; `status` or no arg reports state.
#[cfg(feature = "http-server")]
pub(super) fn parse_codex_chatgpt_action(arg: Option<&str>) -> CodexChatgptAction {
    match arg {
        Some("on" | "enable" | "true") => CodexChatgptAction::On,
        Some("off" | "disable" | "false") => CodexChatgptAction::Off,
        Some("status") | None => CodexChatgptAction::Status,
        Some(_) => CodexChatgptAction::Unknown,
    }
}

/// `lean-ctx proxy codex-chatgpt on|off`: the durable, env-free switch for routing
/// a Codex **ChatGPT-subscription** login through the proxy (#603/#616). It writes
/// the opt-in straight to `config.toml` — the single source of truth the env-less
/// managed proxy and every later setup pass read — then re-applies ONLY the Codex
/// env so Codex's `chatgpt_base_url` is written (on) or stripped (off) right away.
/// This is what fixes the trap where exporting `LEAN_CTX_CODEX_CHATGPT_PROXY` in a
/// shell never reached the process that actually rewrote the Codex config.
#[cfg(feature = "http-server")]
fn codex_chatgpt_set(on: bool, port: u16) {
    if let Err(e) =
        crate::core::config::Config::update_global(|c| c.proxy.codex_chatgpt_proxy = Some(on))
    {
        println!("\x1b[31m✗\x1b[0m Could not persist [proxy] codex_chatgpt_proxy: {e}");
        return;
    }
    // #1775: the opt-in genuinely is persisted, so report exactly that and
    // nothing more. The old green line claimed routing was "enabled" before the
    // installer had even run, so a write skipped against an unreachable proxy
    // still read as success — the user saw a checkmark for something that had
    // not happened.
    let state = if on { "true" } else { "false" };
    println!("\x1b[32m✓\x1b[0m Opt-in saved: [proxy] codex_chatgpt_proxy = \x1b[1m{state}\x1b[0m");

    // Apply now: writes (on) or strips (off) Codex's proxy entries.
    let home = dirs::home_dir().unwrap_or_default();
    match crate::proxy_setup::install_codex_env(&home, port, false) {
        crate::proxy_setup::CodexEnvOutcome::Written => {
            let effect = if on {
                "routed through lean-ctx"
            } else {
                "back to native"
            };
            println!("\x1b[32m✓\x1b[0m Codex config updated — Codex is now {effect}.");
        }
        crate::proxy_setup::CodexEnvOutcome::AlreadyConfigured => {
            println!("\x1b[32m✓\x1b[0m Codex config was already in the requested state.");
        }
        crate::proxy_setup::CodexEnvOutcome::LeftNative => {
            println!("\x1b[32m✓\x1b[0m Codex left native — nothing to write.");
        }
        crate::proxy_setup::CodexEnvOutcome::SkippedProxyDown => {
            println!();
            println!(
                "  \x1b[33m⚠ Codex config NOT written\x1b[0m — nothing is serving port {port}."
            );
            println!("    The opt-in is saved, but Codex is not routed yet. Start the proxy,");
            println!("    then run this command again — both steps are needed:");
            println!("      lean-ctx proxy start --port={port}");
            println!(
                "      lean-ctx proxy codex-chatgpt {}",
                if on { "on" } else { "off" }
            );
        }
        crate::proxy_setup::CodexEnvOutcome::NoConfigDir => {
            println!();
            println!("  \x1b[33m⚠ Codex config NOT written\x1b[0m — no Codex config directory.");
            println!("    Run `codex login` once to create it, then run this command again.");
        }
    }
}

/// `lean-ctx proxy codex-chatgpt status` (also the bare/no-arg form): report the
/// resolved opt-in, its source (config vs env), whether the Codex config actually
/// carries the proxy rail, and whether the proxy is reachable — so a user can see
/// at a glance why Codex is or isn't routed.
#[cfg(feature = "http-server")]
fn codex_chatgpt_status(port: u16) {
    let cfg = crate::core::config::Config::load();
    let effective = cfg.proxy.codex_chatgpt_proxy_enabled();
    println!("Codex ChatGPT proxy routing:");
    println!(
        "  Effective: {}",
        if effective {
            "\x1b[32mon\x1b[0m"
        } else {
            "off"
        }
    );
    match cfg.proxy.codex_chatgpt_proxy {
        Some(true) => println!("  Config:    [proxy] codex_chatgpt_proxy = true"),
        Some(false) => println!("  Config:    [proxy] codex_chatgpt_proxy = false"),
        None => println!("  Config:    (unset → default off)"),
    }
    if let Ok(v) = std::env::var("LEAN_CTX_CODEX_CHATGPT_PROXY") {
        println!("  Env:       LEAN_CTX_CODEX_CHATGPT_PROXY={v} (forces on for this process)");
    }

    let home = dirs::home_dir().unwrap_or_default();
    let codex_cfg = crate::core::home::resolve_codex_dir()
        .unwrap_or_else(|| home.join(".codex"))
        .join("config.toml");
    let routed = std::fs::read_to_string(&codex_cfg)
        .is_ok_and(|c| c.contains("leanctx-chatgpt") || c.contains("chatgpt_base_url"));
    println!(
        "  Codex cfg: {}",
        if routed {
            "model_provider → leanctx-chatgpt (routed)"
        } else {
            "native (no proxy entry)"
        }
    );
    println!(
        "  Proxy:     {}",
        if crate::proxy_setup::is_proxy_reachable(port) {
            "running"
        } else {
            "not running"
        }
    );
    if !effective {
        println!();
        println!("  Enable: lean-ctx proxy codex-chatgpt on");
    }
}

pub(crate) fn cmd_proxy(rest: &[String]) {
    #[cfg(feature = "http-server")]
    {
        // `--help` anywhere must never execute the verb (GH #393).
        if wants_help(rest) {
            println!(
                "Usage: lean-ctx proxy <start|stop|restart|status|enable|disable|cleanup|token|codex-chatgpt> [--port=4444]"
            );
            println!();
            println!("Commands:");
            println!(
                "  start     Run the compression proxy (foreground; -d/--detach for background; --autostart installs a service)"
            );
            println!("  stop      Stop the proxy on the given port");
            println!(
                "  restart   Restart the managed proxy (re-reads config.toml; drops env overrides)"
            );
            println!("  status    Show proxy config, process, live upstreams and stats");
            println!("  enable    Enable the proxy: config flag, autostart service, env wiring");
            println!("  disable   Disable the proxy and restore the original endpoint");
            println!("  cleanup   Remove stale proxy URLs from AI tool configs");
            println!("  token     Print the current proxy Bearer token (for MCP/HTTP clients)");
            println!(
                "  codex-chatgpt <on|off|status>  Route a Codex ChatGPT-subscription login through the proxy"
            );
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
                    if !crate::proxy_autostart::install(port, false) {
                        std::process::exit(1);
                    }
                    return;
                }
                let detach = rest.iter().any(|a| a == "--detach" || a == "-d");
                if detach {
                    start_detached(port);
                    return;
                }
                if let Err(e) = crate::cli::dispatch::run_async(crate::proxy::start_proxy(port)) {
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
                    // #590: persist the shell's custom-upstream opt-in to config
                    // before the restart so the re-read picks up the custom host.
                    bridge_custom_upstream_optin();
                    // #603/#616: likewise persist the Codex ChatGPT-subscription
                    // opt-in so the restarted service keeps routing Codex through
                    // the proxy (the service never sees the shell env var).
                    bridge_codex_chatgpt_optin();
                    // Managed service (LaunchAgent / systemd): a clean bootout +
                    // bootstrap restarts the proxy so it re-reads config.toml. It
                    // deliberately drops any `LEAN_CTX_*_UPSTREAM` env override
                    // (the service context has none), making config.toml the
                    // single source of truth for the long-lived proxy (#449).
                    crate::proxy_autostart::stop();
                    std::thread::sleep(std::time::Duration::from_millis(700));
                    if crate::proxy_autostart::start_on_port(port) {
                        println!("\x1b[32m✓\x1b[0m Proxy restarted (managed service).");
                        println!("  Verify active upstreams: lean-ctx proxy status");
                    } else {
                        eprintln!(
                            "\x1b[31m✗\x1b[0m Managed proxy restart failed; port {port} was not transferred."
                        );
                        std::process::exit(1);
                    }
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
                            println!("  Tokens saved: {} (estimated)", v["tokens_saved"]);
                            println!(
                                "  Compression: {}%",
                                v["compression_ratio_pct"].as_str().unwrap_or("0.0")
                            );
                            print_verified_savings(&v);
                            print_compression_by_upstream(&v);
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

                // #590: persist the shell's custom-upstream opt-in to config BEFORE
                // the managed proxy starts, so it reads the flag on startup (the
                // service never inherits the shell's env var).
                bridge_custom_upstream_optin();
                // #603/#616: same hazard for the Codex ChatGPT-subscription opt-in.
                // The env var only reaches this foreground process; persist it so
                // the managed proxy and every later env-less `install_proxy_env`
                // pass route Codex through the proxy instead of stripping its
                // `chatgpt_base_url` back to native.
                bridge_codex_chatgpt_optin();

                let port = crate::proxy_setup::default_port();
                // #1773: the managed service and the client wiring are separate
                // concerns, and coupling them stranded every Windows user.
                // `proxy_autostart::install` has backends for macOS (launchd) and
                // Linux (systemd) only, so on Windows it always returns false —
                // the old `exit(1)` fired here and `install_proxy_env_unchecked`
                // below was unreachable. The error then pointed at
                // `lean-ctx proxy start`, which wires no client either, so the
                // documented path could never produce a working setup.
                //
                // Wire first, report second. A manually started proxy is a
                // perfectly good outcome on a platform without autostart.
                let autostart_ok = crate::proxy_autostart::install(port, false);
                if autostart_ok {
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }

                let home = dirs::home_dir().unwrap_or_default();
                crate::proxy_setup::install_proxy_env_unchecked(&home, port, false, force);

                if autostart_ok {
                    println!(
                        "\x1b[32m✓\x1b[0m Proxy enabled on port {port}. LLM requests will be compressed."
                    );
                } else if crate::proxy_setup::is_proxy_reachable(port) {
                    println!(
                        "\x1b[32m✓\x1b[0m Clients wired to the proxy already serving port {port}."
                    );
                    println!(
                        "  This platform has no autostart backend, so the proxy does not survive a"
                    );
                    println!("  reboot — start it again with:  lean-ctx proxy start --port={port}");
                } else {
                    // Both `install_shell_exports` and `install_codex_env` refuse to
                    // write against an unreachable proxy, so nothing above wired
                    // anything. Say that plainly instead of claiming success.
                    eprintln!(
                        "\x1b[31m✗\x1b[0m Proxy enable: no autostart backend, and nothing is serving port {port}."
                    );
                    eprintln!(
                        "  Client wiring requires a reachable proxy, so no client config was written."
                    );
                    eprintln!("  Start the proxy, then re-run enable — both steps are needed:");
                    eprintln!("    lean-ctx proxy start --port={port}");
                    eprintln!("    lean-ctx proxy enable");
                    std::process::exit(1);
                }
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
            "codex-chatgpt" => {
                let port = parse_proxy_port(rest);
                // Skip the verb itself and any `--port=`/`-p=` flag to find the action.
                let action_arg = rest
                    .get(1..)
                    .unwrap_or_default()
                    .iter()
                    .find(|a| !a.starts_with("--port=") && !a.starts_with("-p="))
                    .map(std::string::String::as_str);
                match parse_codex_chatgpt_action(action_arg) {
                    CodexChatgptAction::On => codex_chatgpt_set(true, port),
                    CodexChatgptAction::Off => codex_chatgpt_set(false, port),
                    CodexChatgptAction::Status => codex_chatgpt_status(port),
                    CodexChatgptAction::Unknown => {
                        println!("Unknown argument '{}'.", action_arg.unwrap_or(""));
                        println!(
                            "Usage: lean-ctx proxy codex-chatgpt <on|off|status> [--port=4444]"
                        );
                    }
                }
            }
            "token" => {
                let token = crate::core::session_token::resolve_proxy_token("LEAN_CTX_PROXY_TOKEN");
                let quiet = rest.iter().any(|a| a == "--quiet" || a == "-q");
                if quiet {
                    print!("{token}");
                } else {
                    println!("{token}");
                }
            }
            _ => {
                println!(
                    "Usage: lean-ctx proxy <start|stop|restart|status|enable|disable|cleanup|token|codex-chatgpt> [--port=4444]"
                );
            }
        }
    }
    /// Fork a child process running `lean-ctx proxy start --port=N` with
    /// stdout/stderr redirected to a log file, then exit the parent immediately.
    /// The child's PID is written to the proxy PID file so `lean-ctx proxy stop`
    /// can find and kill it (#758).
    #[cfg(feature = "http-server")]
    fn start_detached(port: u16) {
        let exe = std::env::current_exe().unwrap_or_else(|_| "lean-ctx".into());
        let log_dir = crate::core::paths::state_dir()
            .unwrap_or_else(|_| std::env::temp_dir().join("lean-ctx"));
        let _ = std::fs::create_dir_all(&log_dir);
        let log_path = log_dir.join("proxy.log");
        let log_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .unwrap_or_else(|e| {
                eprintln!("Cannot open {}: {e}", log_path.display());
                std::process::exit(1);
            });
        let stderr_file = log_file.try_clone().unwrap_or_else(|e| {
            eprintln!("Cannot clone log fd: {e}");
            std::process::exit(1);
        });
        match std::process::Command::new(&exe)
            .args(["proxy", "start", &format!("--port={port}")])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::from(log_file))
            .stderr(std::process::Stdio::from(stderr_file))
            .spawn()
        {
            Ok(child) => {
                println!(
                    "\x1b[32m✓\x1b[0m Proxy started in background (PID {}, port {port})",
                    child.id()
                );
                println!("  Logs: {}", log_path.display());
                println!("  Stop: lean-ctx proxy stop --port={port}");
            }
            Err(e) => {
                eprintln!("Failed to start detached proxy: {e}");
                std::process::exit(1);
            }
        }
    }

    #[cfg(not(feature = "http-server"))]
    {
        let _ = rest;
        eprintln!("lean-ctx proxy is not available in this build");
        std::process::exit(1);
    }
}
