use std::sync::Arc;
use subtle::ConstantTimeEq;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

pub(crate) const DEFAULT_PORT: u16 = 3333;
const DEFAULT_HOST: &str = "127.0.0.1";

pub mod base_path;
pub(crate) mod cockpit;
pub(crate) mod display_snapshots;
pub(crate) mod launch;
pub(crate) mod project_binding;
pub mod routes;
pub(crate) mod vscode_open;

/// Start the dashboard server — the single local entry point that hosts the
/// Cockpit — or open the one already running. `view` is the Cockpit view to
/// open (a hash route such as `savings`); `None` opens its start page.
pub async fn start(
    port: Option<u16>,
    host: Option<String>,
    base_path: Option<String>,
    auth_token: Option<String>,
    open_mode: Option<String>,
    auth_enabled: Option<bool>,
    view: Option<String>,
) {
    // Live model prices (#1179): the measured-spend card prices with the
    // cached provider list — loaded from disk, kept fresh in the background.
    crate::core::gain::live_pricing::spawn_background_refresh();

    // How to reveal the URL once the server is up: --open= flag > env > browser.
    let open = resolve_open_mode(open_mode.as_deref());
    let requested_port = port.or_else(requested_port_from_env);
    let mut port = requested_port.unwrap_or(DEFAULT_PORT);

    let host = host.unwrap_or_else(|| {
        std::env::var("LEAN_CTX_HOST")
            .ok()
            .unwrap_or_else(|| DEFAULT_HOST.to_string())
    });

    // Reverse-proxy subpath (e.g. `/dashboard`). Normalized to "" or "/prefix".
    // Shared across connections behind an Arc; "" means "no subpath" (#355).
    let base_path = Arc::new(
        base_path
            .or_else(|| std::env::var("LEAN_CTX_DASHBOARD_BASE_PATH").ok())
            .map(|b| base_path::normalize(&b))
            .unwrap_or_default(),
    );

    let is_local = host == "127.0.0.1" || host == "localhost" || host == "::1";

    // Whether the dashboard requires a Bearer token. Precedence:
    // `--no-auth`/`--auth=` flag > LEAN_CTX_DASHBOARD_AUTH env > `dashboard_auth`
    // config > default `true`. When disabled, no token is generated and the
    // sensitive endpoints (`/api/*`, `/metrics`) are guarded by request-header
    // checks (Sec-Fetch-Site / Origin / Host allowlist) instead — see
    // `no_auth_request_ok`.
    let auth_required = resolve_auth_enabled(auth_enabled);

    // Resolve any *requested* fixed token (flag > LEAN_CTX_HTTP_TOKEN) up-front;
    // `None` means "generate a random one". Done before the already-running check
    // so we can warn when the requested token won't match a live instance (#377).
    let (requested_token, token_src) = resolve_requested_token(auth_token.as_deref());

    // Avoid accidental multiple dashboard instances (common source of "it
    // hangs"): reuse this data dir's healthy instance, replace its hung or
    // outdated background one, and move past a port someone else holds.
    if is_local {
        match launch::plan_now(requested_port) {
            (
                launch::Plan::Reuse {
                    port,
                    other_version,
                },
                record,
            ) => {
                let base = record
                    .filter(|r| r.port == port)
                    .map_or_else(|| base_path.to_string(), |r| r.base_path);
                println!("\n  LeanCTX Cockpit already running → http://{host}:{port}{base}");
                if let Some(version) = other_version {
                    eprintln!(
                        "  \x1b[33m⚠\x1b[0m The running server is version {version} (this is {}). \
                         Restart it to update: stop it, then run lean-ctx cockpit again.",
                        env!("CARGO_PKG_VERSION")
                    );
                }
                if let Some(req) = requested_token.as_deref()
                    && load_saved_token().as_deref() != Some(req)
                {
                    eprintln!(
                        "  \x1b[33m⚠\x1b[0m The running instance uses a different token — your {token_src} \
                         will be rejected. Stop it (Ctrl+C) and restart to apply the new token."
                    );
                }
                println!(
                    "  Tip: use Ctrl+C in its terminal (or lean-ctx cockpit stop) to stop it.\n"
                );
                open_dashboard_url(
                    &launch_url(port, &base, load_saved_token().as_deref(), view.as_deref()),
                    open,
                );
                return;
            }
            (launch::Plan::Replace { port: taken, .. }, Some(record)) => {
                println!(
                    "  Restarting this data dir's background Cockpit server (PID {}).",
                    record.pid
                );
                if !launch::stop_instance(&record) {
                    eprintln!(
                        "  Could not stop the previous Cockpit server (PID {}); stop it, then retry.",
                        record.pid
                    );
                    std::process::exit(1);
                }
                port = taken;
            }
            (launch::Plan::Replace { port: taken, .. }, None) => port = taken,
            (
                launch::Plan::Start {
                    port: free,
                    skipped,
                },
                _,
            ) => {
                if let Some(skipped) = skipped {
                    println!(
                        "  Port {skipped} is taken (another program or LeanCTX data dir) — using {free}."
                    );
                }
                port = free;
            }
            (launch::Plan::PortTaken { port, by }, _) => {
                eprintln!("{}", launch::port_taken_message(port, &by));
                std::process::exit(1);
            }
        }
    }
    let addr = format!("{host}:{port}");

    // Host values accepted in no-auth mode (anti-DNS-rebinding allowlist). Built
    // once and shared with every connection.
    let allowed_hosts = Arc::new(build_allowed_hosts(&host, port));

    // Auth defaults on (even on loopback) to prevent cross-origin reads of /api/*
    // from a malicious website (CORS is not a reliable boundary for localhost
    // services). When explicitly disabled, run token-less: cross-origin/CSRF and
    // DNS-rebinding attacks are blocked by `no_auth_request_ok` instead.
    let token = if auth_required {
        // #1281: reuse the persisted token across restarts. Rotating on every
        // start silently 401'd any open dashboard tab — the Live view polls
        // /api/* and just stayed empty. Precedence: --auth-token > env >
        // saved dashboard.token (0600) > freshly generated.
        let t = requested_token
            .or_else(|| load_saved_token().filter(|s| !s.is_empty()))
            .unwrap_or_else(generate_token);
        Some(Arc::new(t))
    } else {
        if requested_token.is_some() {
            eprintln!(
                "  \x1b[33m⚠\x1b[0m Ignoring the pinned token ({token_src}) — auth is disabled."
            );
        }
        None
    };

    // Bind BEFORE persisting the token: two racing `lean-ctx dashboard` starts
    // both used to write their fresh token, the bind loser exited — leaving a
    // token on disk that the surviving server never accepted. Every later
    // "already running" browser open (and any tool reading dashboard.token)
    // then got 401s. Binding first makes the loser exit without touching the
    // file, so dashboard.token always belongs to the live listener.
    let listener = match TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!(
                "Failed to bind to {addr}: {e} — choose another port: lean-ctx cockpit --port <N>"
            );
            std::process::exit(1);
        }
    };
    // Who serves this data dir's Cockpit now (no secret in it).
    let record = launch::LaunchRecord::of_this_process(&host, port, &base_path);
    launch::write_record(&record);

    if let Some(t) = token.as_ref() {
        save_token(t);
        let masked = if t.len() > 12 {
            format!(
                "{}…{}",
                &t[..t.floor_char_boundary(8)],
                &t[t.ceil_char_boundary(t.len().saturating_sub(4))..]
            )
        } else {
            t.to_string()
        };
        let src = if token_src.is_empty() {
            String::new()
        } else {
            format!(" (from {token_src})")
        };
        if is_local {
            println!("  Auth: enabled (local){src}");
            println!("  Browser URL:  http://localhost:{port}{base_path}/?token={t}");
        } else {
            eprintln!(
                "  \x1b[33m⚠\x1b[0m Binding to {host} — authentication enabled.\n  \
                 Bearer token{src}: \x1b[1;32m{masked}\x1b[0m\n  \
                 Browser URL:  http://<your-ip>:{port}{base_path}/?token={t}"
            );
        }
    } else if is_local {
        // No-auth on loopback: header-based CSRF protection is the boundary.
        println!(
            "  Auth: \x1b[1;33mDISABLED\x1b[0m (no-auth) — CSRF protected via Sec-Fetch-Site/Origin/Host"
        );
        println!("  Browser URL:  http://localhost:{port}{base_path}/");
    } else {
        // No-auth + non-loopback bind (e.g. Docker --host=0.0.0.0). Browser
        // cross-origin/CSRF is still blocked, but non-browser clients that can
        // reach the address have unauthenticated access — warn loudly.
        eprintln!(
            "  \x1b[33m⚠\x1b[0m Auth \x1b[1;31mDISABLED\x1b[0m and binding to {host} (not loopback).\n  \
             Browser cross-origin/CSRF stays blocked (Sec-Fetch-Site/Origin/Host),\n  \
             but ANY non-browser client that can reach {host}:{port} has full access.\n  \
             Docker: publish only to the host loopback → -p 127.0.0.1:{port}:{port}\n  \
             Add reachable hostnames via LEAN_CTX_DASHBOARD_ALLOWED_HOSTS=host:port,…\n  \
             Browser URL:  http://<your-ip>:{port}{base_path}/"
        );
    }

    let stats_path = crate::core::data_dir::lean_ctx_data_dir().map_or_else(
        |_| "~/.lean-ctx/stats.json".to_string(),
        |d| d.join("stats.json").display().to_string(),
    );

    if host == "0.0.0.0" {
        println!("\n  LeanCTX Cockpit → http://0.0.0.0:{port} (all interfaces)");
        println!("  Local access:  http://localhost:{port}");
    } else {
        println!("\n  LeanCTX Cockpit → http://{host}:{port}");
    }
    println!("  Stats file: {stats_path}");
    println!("  Press Ctrl+C to stop");
    println!(
        "  \x1b[2mLocal Cockpit: context observations and evidence remain on this machine.\x1b[0m\n"
    );

    // The slow reads are computed once now, so the first visit is instant.
    warm_display_snapshots(token.clone());

    if is_local {
        open_dashboard_url(
            &launch_url(
                port,
                &base_path,
                token.as_deref().map(String::as_str),
                view.as_deref(),
            ),
            open,
        );
    }
    if crate::shell::is_container() && is_local {
        println!("  Tip (Docker): bind 0.0.0.0 + publish port:");
        println!("    lean-ctx cockpit --host=0.0.0.0 --port={port}");
        println!("    docker run ... -p {port}:{port} ...");
        println!();
    }

    if crate::core::datadog_push::spawn_if_enabled() {
        println!(
            "  Datadog push: enabled (agentless, every LEAN_CTX_DATADOG_INTERVAL_SECS or 60s)"
        );
    }

    // The Cockpit is started on first use for this dashboard's port, token
    // and project, and stopped with the dashboard.
    let project_root = routes::helpers::dashboard_project_binding()
        .0
        .map(std::path::PathBuf::from)
        .unwrap_or_default();
    let cockpit = cockpit::runtime_resolver(port, token.as_deref().cloned(), project_root);
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                if let Ok((stream, _)) = accepted {
                    let token_ref = token.clone();
                    let base_ref = base_path.clone();
                    let allowed_ref = allowed_hosts.clone();
                    tokio::spawn(handle_request_with(
                        stream,
                        token_ref,
                        base_ref,
                        allowed_ref,
                        cockpit.clone(),
                    ));
                }
            }
            _ = tokio::signal::ctrl_c() => {
                let _ = tokio::task::spawn_blocking(cockpit::retire).await;
                launch::remove_record_of(record.pid);
                std::process::exit(130);
            }
            () = terminate_requested() => {
                let _ = tokio::task::spawn_blocking(cockpit::retire).await;
                launch::remove_record_of(record.pid);
                std::process::exit(0);
            }
        }
    }
}

/// `--port`/`LEAN_CTX_PORT` when given: a port the user chose, which a start
/// never swaps silently for another one.
fn requested_port_from_env() -> Option<u16> {
    std::env::var("LEAN_CTX_PORT")
        .ok()
        .and_then(|p| p.trim().parse().ok())
}

/// Resolves when the process is asked to terminate (`lean-ctx cockpit stop`
/// sends SIGTERM to the recorded PID). Never resolves where there is no
/// such signal.
async fn terminate_requested() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        if let Ok(mut term) = signal(SignalKind::terminate()) {
            term.recv().await;
            return;
        }
    }
    std::future::pending::<()>().await;
}

/// The browser URL that opens the Cockpit: the token travels in the query
/// (the Cockpit stores it and removes it from the address bar), the view as
/// the hash route the Cockpit navigates to.
pub(crate) fn launch_url(
    port: u16,
    base_path: &str,
    token: Option<&str>,
    view: Option<&str>,
) -> String {
    let mut url = format!("http://localhost:{port}{base_path}/");
    if let Some(token) = token.filter(|t| !t.is_empty()) {
        url.push_str("?token=");
        url.push_str(token);
    }
    if let Some(view) = view.filter(|v| !v.is_empty()) {
        url.push('#');
        url.push_str(view);
    }
    url
}

/// Compute the persisted display snapshots of the dashboard's own project
/// in the background at server start.
fn warm_display_snapshots(token: Option<Arc<String>>) {
    std::thread::spawn(move || {
        let partition = project_binding::partition_of(None);
        let store = display_snapshots::DisplaySnapshots::global();
        for route in display_snapshots::DisplaySnapshots::warmed_routes() {
            let token = token.clone();
            let compute: display_snapshots::Compute = Arc::new(move || {
                routes::route_response(route, "", None, token.as_ref(), true, "GET", "")
            });
            store.warm(route, &partition, compute);
        }
    });
}

/// Name of the env var that pins the dashboard Bearer token (#377).
const HTTP_TOKEN_ENV: &str = "LEAN_CTX_HTTP_TOKEN";
/// Read-only token accepted **only** for `GET /metrics` (GL #401) so
/// monitoring agents never hold the full dashboard credential.
const SCRAPE_TOKEN_ENV: &str = "LEAN_CTX_SCRAPE_TOKEN";
/// Toggles dashboard Bearer-token auth. `false`/`0`/`no`/`off` disable it.
const DASHBOARD_AUTH_ENV: &str = "LEAN_CTX_DASHBOARD_AUTH";
/// Extra `Host` header values accepted in no-auth mode (CSV, e.g.
/// `box.local:3333,10.0.0.5:3333`). Extends the loopback/bound-host allowlist
/// for Docker/reverse-proxy setups reached via a custom hostname.
const ALLOWED_HOSTS_ENV: &str = "LEAN_CTX_DASHBOARD_ALLOWED_HOSTS";

/// Parse a human boolean (`true/false/1/0/yes/no/on/off`, case-insensitive).
fn parse_human_bool(s: &str) -> Option<bool> {
    match s.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// Resolve whether the dashboard requires Bearer-token auth. Precedence:
/// `--no-auth`/`--auth=` flag > `LEAN_CTX_DASHBOARD_AUTH` env > `dashboard_auth`
/// config > default `true`.
fn resolve_auth_enabled(flag: Option<bool>) -> bool {
    if let Some(v) = flag {
        return v;
    }
    if let Ok(raw) = std::env::var(DASHBOARD_AUTH_ENV)
        && let Some(v) = parse_human_bool(&raw)
    {
        return v;
    }
    crate::core::config::Config::load().dashboard_auth
}

/// Build the `Host` header allowlist for no-auth mode (anti-DNS-rebinding).
/// Always includes the loopback aliases for `port` (localhost is the intended
/// audience), the actual bound `host:port`, and any `LEAN_CTX_DASHBOARD_ALLOWED_HOSTS`
/// entries. Bare-host forms (no port) are added too so non-browser clients that
/// omit the port aren't rejected. `0.0.0.0` is never added — browsers don't send
/// `Host: 0.0.0.0`; operators expose reachable names via the env allowlist.
fn build_allowed_hosts(host: &str, port: u16) -> Vec<String> {
    let mut allowed: Vec<String> = Vec::new();
    let mut push = |h: String| {
        if !h.is_empty() && !allowed.iter().any(|e| e.eq_ignore_ascii_case(&h)) {
            allowed.push(h);
        }
    };
    for base in ["127.0.0.1", "localhost", "[::1]", "::1"] {
        push(base.to_string());
        push(format!("{base}:{port}"));
    }
    if host != "0.0.0.0" && host != "::" {
        push(host.to_string());
        push(format!("{host}:{port}"));
    }
    if let Ok(raw) = std::env::var(ALLOWED_HOSTS_ENV) {
        for entry in raw.split(',') {
            push(entry.trim().to_string());
        }
    }
    allowed
}

/// Is the request `Host` header in the allowlist (case-insensitive)?
fn host_allowed(host: &str, allowed: &[String]) -> bool {
    allowed.iter().any(|a| a.eq_ignore_ascii_case(host))
}

/// True when the `Host` header's hostname is a loopback literal
/// (`localhost`, `127.0.0.0/8`, or `::1`), **regardless of port**.
///
/// A loopback `Host` is never a DNS-rebinding vector: the browser only sends one
/// when the user navigated to a loopback URL directly (an attacker can't make
/// their own hostname resolve to — and report a `Host` of — `127.0.0.1`). So in
/// no-auth mode we accept loopback on any port, not just the bound one. This is
/// what makes a port-remapped Docker publish work out of the box — e.g. the
/// container binds `0.0.0.0:3333`, Docker publishes it as `-p 60000:3333`, and
/// the host browser reaches `http://127.0.0.1:60000`, so the `Host` header is
/// `127.0.0.1:60000` (the *published* port), which the bind-port allowlist
/// (`127.0.0.1:3333`) would otherwise reject. Cross-origin/CSRF stays blocked by
/// the `Sec-Fetch-Site`/`Origin` checks in `no_auth_request_ok`.
fn host_is_loopback(host: &str) -> bool {
    // Extract the hostname, dropping any `:port`. IPv6 literals are bracketed
    // (`[::1]` / `[::1]:port`); a bare IPv6 (`::1`) can't carry a port.
    let hostname = if let Some(rest) = host.strip_prefix('[') {
        match rest.split_once(']') {
            Some((inner, _)) => inner,
            None => return false,
        }
    } else if host.matches(':').count() == 1 {
        host.rsplit_once(':').map_or(host, |(h, _)| h)
    } else {
        // No colon (bare host[:no-port]) or multiple colons (unbracketed IPv6).
        host
    };
    if hostname.eq_ignore_ascii_case("localhost") {
        return true;
    }
    if let Ok(v4) = hostname.parse::<std::net::Ipv4Addr>() {
        return v4.is_loopback();
    }
    if let Ok(v6) = hostname.parse::<std::net::Ipv6Addr>() {
        return v6.is_loopback();
    }
    false
}

/// Token-free request gate for no-auth dashboards. Applied to the same sensitive
/// endpoints the Bearer token guards (`/api/*`, `/metrics`). Blocks browser
/// cross-origin/CSRF and DNS-rebinding without a credential:
///  * `Sec-Fetch-Site`, when sent, must be `same-origin` or `none` (the header is
///    set by the browser and cannot be forged by page JS).
///  * `Host` must be in `allowed_hosts` (missing `Host` is rejected).
///  * `Origin`, when sent and not `null`, must be same-origin as `Host`.
///
/// Non-browser clients (curl, Prometheus) omit `Sec-Fetch-Site`/`Origin` and pass
/// those checks — they only need to target an allowlisted `Host`.
fn no_auth_request_ok(header_section: &str, allowed_hosts: &[String]) -> bool {
    if let Some(sfs) = header_line_value(header_section, "Sec-Fetch-Site") {
        let sfs = sfs.trim();
        if !sfs.is_empty()
            && !sfs.eq_ignore_ascii_case("same-origin")
            && !sfs.eq_ignore_ascii_case("none")
        {
            return false;
        }
    }
    let Some(host) = header_line_value(header_section, "Host") else {
        return false;
    };
    // Accept the explicit allowlist (loopback aliases for the bound port, the
    // bound host, and any LEAN_CTX_DASHBOARD_ALLOWED_HOSTS entries) OR any
    // loopback host on any port. The latter covers port-remapped Docker
    // publishes (e.g. `-p 60000:3333` reached via `127.0.0.1:60000`) without a
    // manual allowlist entry — loopback hosts are not a rebinding vector.
    if !host_allowed(host, allowed_hosts) && !host_is_loopback(host) {
        return false;
    }
    if let Some(origin) = header_line_value(header_section, "Origin")
        && !origin.is_empty()
        && !origin.eq_ignore_ascii_case("null")
        && !origin_matches_dashboard_host(origin, host)
    {
        return false;
    }
    true
}

/// Resolve the dashboard Bearer token.
///
/// Honors `LEAN_CTX_HTTP_TOKEN` (#377): when set to a non-empty value it is used
/// verbatim so reverse-proxy / container deployments keep a stable token across
/// restarts and redeploys (nginx can inject a fixed `Authorization: Bearer …`).
/// When unset or empty, a fresh random token is generated (no behavior change).
///
/// Resolve a *requested* fixed token with precedence `--auth-token` flag >
/// `LEAN_CTX_HTTP_TOKEN` (#377). The flag wins so it survives container/service
/// environments that strip or fail to inherit the env var. Returns the trimmed,
/// non-empty token and a human label of its source; `None` means "no fixed token
/// requested → caller generates a random one".
fn resolve_requested_token(flag: Option<&str>) -> (Option<String>, &'static str) {
    if let Some(t) = flag.map(str::trim).filter(|s| !s.is_empty()) {
        return (Some(t.to_string()), "--auth-token");
    }
    if let Ok(raw) = std::env::var(HTTP_TOKEN_ENV) {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return (Some(trimmed.to_string()), HTTP_TOKEN_ENV);
        }
    }
    (None, "")
}

fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    if getrandom::fill(&mut bytes).is_err() {
        tracing::warn!("CSPRNG unavailable — falling back to time-based token");
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = ((ts >> (i % 16 * 8)) & 0xFF) as u8;
        }
    }
    format!("lctx_{}", hex_lower(&bytes))
}

fn save_token(token: &str) {
    if let Ok(dir) = crate::core::paths::state_dir() {
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("dashboard.token");
        #[cfg(unix)]
        {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let Ok(mut f) = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&path)
            else {
                return;
            };
            let _ = f.write_all(token.as_bytes());
        }
        #[cfg(not(unix))]
        {
            let _ = std::fs::write(&path, token);
        }
    }
}

pub(crate) fn load_saved_token() -> Option<String> {
    let dir = crate::core::paths::state_dir().ok()?;
    let path = dir.join("dashboard.token");
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
}

/// Adds `nonce="..."` to all inline `<script>` tags (those without a `src=` attribute).
/// External scripts (`<script src="...">`) are left untouched.
pub fn add_nonce_to_inline_scripts(html: &str, nonce: &str) -> String {
    let mut result = String::with_capacity(html.len() + 128);
    let mut remaining = html;
    while let Some(pos) = remaining.find("<script") {
        result.push_str(&remaining[..pos]);
        let tag_start = &remaining[pos..];
        let tag_end = tag_start.find('>').unwrap_or(tag_start.len());
        let tag = &tag_start[..=tag_end];
        if tag.contains("src=") || tag.contains("nonce=") {
            result.push_str(tag);
        } else {
            result.push_str(&tag.replacen("<script", &format!("<script nonce=\"{nonce}\""), 1));
        }
        remaining = &tag_start[tag_end + 1..];
    }
    result.push_str(remaining);
    result
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

/// How `lean-ctx dashboard` reveals the URL after the server is up (#424).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum DashboardOpen {
    /// Launch the system default browser (historical default).
    Browser,
    /// Don't auto-launch anything — just print the URL. For users who run the
    /// dashboard inside an editor / reverse proxy and don't want a new window.
    None,
    /// Suppress the external browser and print the steps to open the URL in
    /// VS Code's built-in browser. VS Code exposes no stable CLI flag to open
    /// its Simple/Integrated Browser, so we guide rather than fake it.
    Vscode,
}

/// Resolve the open mode from (in precedence order) the `--open=` flag, the
/// `LEAN_CTX_DASHBOARD_OPEN` env var, else the `browser` default.
pub(crate) fn resolve_open_mode(flag: Option<&str>) -> DashboardOpen {
    let raw = flag
        .map(str::to_string)
        .or_else(|| std::env::var("LEAN_CTX_DASHBOARD_OPEN").ok())
        .unwrap_or_default();
    match raw.trim().to_ascii_lowercase().as_str() {
        "none" | "off" | "false" | "no" => DashboardOpen::None,
        "vscode" | "code" | "editor" => DashboardOpen::Vscode,
        _ => DashboardOpen::Browser,
    }
}

/// Reveal `url` to the user according to `mode`.
pub(crate) fn open_dashboard_url(url: &str, mode: DashboardOpen) {
    match mode {
        DashboardOpen::Browser => open_browser(url),
        DashboardOpen::None => {}
        DashboardOpen::Vscode => {
            // Prefer the extension's native webview tab (#466 item 3): with the
            // lean-ctx VS Code extension installed, one command opens the
            // dashboard as a real editor tab — no URL copy/paste. Keep the
            // Simple Browser path as the no-extension fallback.
            println!(
                "  \x1b[2mNative tab: run ⇧⌘P → \"lean-ctx: Open Cockpit\" (needs the lean-ctx VS Code extension)\x1b[0m"
            );
            println!(
                "  \x1b[2mNo extension? ⇧⌘P → \"Simple Browser: Show\" → paste the URL above\x1b[0m"
            );
        }
    }
}

fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(url).spawn();
    }

    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("xdg-open")
            .arg(url)
            .stderr(std::process::Stdio::null())
            .spawn();
    }

    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", url])
            .spawn();
    }
}

/// Probes `http://{host}:{port}/api/version` (auth-aware) and returns true only
/// when it answers `200` with the lean-ctx dashboard's own version JSON. Single
/// source of truth for "is *our* dashboard already live on this port": used both
/// when opening the browser (avoids spawning a second instance) and by `doctor`'s
/// port check, so port 3333 held by our own dashboard reads as healthy instead of
/// a false conflict (#644). The body check is what tells our dashboard apart from
/// an unrelated service that merely answers 200 on the same port.
pub(crate) fn dashboard_responding(host: &str, port: u16) -> bool {
    // Field markers mirror `version_check::version_info_json`; see
    // `launch::classify_response`. `localhost` and IPv6 hosts now probe the
    // address they name instead of falling back to 127.0.0.1.
    matches!(
        launch::probe(host, port, load_saved_token().as_deref()),
        launch::Health::Healthy { .. }
    )
}

const MAX_HTTP_MESSAGE: usize = 2 * 1024 * 1024;

fn header_line_value<'a>(header_section: &'a str, name: &str) -> Option<&'a str> {
    for line in header_section.lines() {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        if k.trim().eq_ignore_ascii_case(name) {
            return Some(v.trim());
        }
    }
    None
}

/// Loopback dashboards often use `localhost` vs `127.0.0.1` interchangeably in `Origin`.
fn host_loopback_aliases(host: &str) -> Vec<String> {
    let mut v = vec![host.to_string()];
    if let Some(port) = host.strip_prefix("127.0.0.1:") {
        v.push(format!("localhost:{port}"));
    }
    if let Some(port) = host.strip_prefix("localhost:") {
        v.push(format!("127.0.0.1:{port}"));
    }
    if let Some(port) = host.strip_prefix("[::1]:") {
        v.push(format!("127.0.0.1:{port}"));
        v.push(format!("localhost:{port}"));
    }
    v
}

fn origin_matches_dashboard_host(origin: &str, host: &str) -> bool {
    let origin = origin.trim_end_matches('/');
    for h in host_loopback_aliases(host) {
        if origin.eq_ignore_ascii_case(&format!("http://{h}"))
            || origin.eq_ignore_ascii_case(&format!("https://{h}"))
        {
            return true;
        }
    }
    false
}

/// Defense-in-depth for browser POSTs: reject cross-site `Origin` on mutating `/api/*` calls.
/// Non-browser clients (no `Origin`) remain allowed when Bearer auth succeeds.
fn csrf_origin_ok(header_section: &str, method: &str, path: &str) -> bool {
    let uc = method.to_ascii_uppercase();
    if !matches!(uc.as_str(), "POST" | "PUT" | "PATCH" | "DELETE") {
        return true;
    }
    if !path.starts_with("/api/") {
        return true;
    }
    let Some(origin) = header_line_value(header_section, "Origin") else {
        return true;
    };
    if origin.is_empty() || origin.eq_ignore_ascii_case("null") {
        return true;
    }
    let Some(host) = header_line_value(header_section, "Host") else {
        return false;
    };
    origin_matches_dashboard_host(origin, host)
}

fn find_headers_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn parse_content_length_header(header_section: &[u8]) -> Option<usize> {
    let text = String::from_utf8_lossy(header_section);
    for line in text.lines() {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        if k.trim().eq_ignore_ascii_case("content-length") {
            return v.trim().parse::<usize>().ok();
        }
    }
    Some(0)
}

async fn read_http_message(stream: &mut tokio::net::TcpStream) -> Option<Vec<u8>> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 8192];
    loop {
        if let Some(end) = find_headers_end(&buf) {
            let cl = parse_content_length_header(&buf[..end])?;
            let total = end + 4 + cl;
            if total > MAX_HTTP_MESSAGE {
                return None;
            }
            if buf.len() >= total {
                buf.truncate(total);
                return Some(buf);
            }
        } else if buf.len() > 65_536 {
            return None;
        }

        let n = stream.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.len() > MAX_HTTP_MESSAGE {
            return None;
        }
    }
}

#[cfg(test)]
async fn handle_request(
    stream: tokio::net::TcpStream,
    token: Option<Arc<String>>,
    base_path: Arc<String>,
    allowed_hosts: Arc<Vec<String>>,
) {
    handle_request_with(
        stream,
        token,
        base_path,
        allowed_hosts,
        Arc::new(|| Err(cockpit::CockpitUnavailable::NotInstalled)),
    )
    .await;
}

/// Write a complete response with a JSON error body.
async fn write_json_error(stream: &mut tokio::net::TcpStream, status: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
}

async fn handle_request_with(
    mut stream: tokio::net::TcpStream,
    token: Option<Arc<String>>,
    base_path: Arc<String>,
    allowed_hosts: Arc<Vec<String>>,
    cockpit: cockpit::CockpitResolver,
) {
    let is_loopback = stream.peer_addr().is_ok_and(|a| a.ip().is_loopback());

    let Some(buf) = read_http_message(&mut stream).await else {
        return;
    };
    let Some(header_end) = find_headers_end(&buf) else {
        return;
    };
    let header_text = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let body_start = header_end + 4;
    let Some(content_len) = parse_content_length_header(&buf[..header_end]) else {
        return;
    };
    if buf.len() < body_start + content_len {
        return;
    }
    let body_str = std::str::from_utf8(&buf[body_start..body_start + content_len])
        .unwrap_or("")
        .to_string();

    let first = header_text.lines().next().unwrap_or("");
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or("GET").to_string();
    let raw_path = parts.next().unwrap_or("/").to_string();

    let (path, query_token) = if let Some(idx) = raw_path.find('?') {
        let p = &raw_path[..idx];
        let qs = &raw_path[idx + 1..];
        let tok = qs
            .split('&')
            .find_map(|pair| pair.strip_prefix("token="))
            .map(std::string::ToString::to_string);
        (p.to_string(), tok)
    } else {
        (raw_path.clone(), None)
    };

    let query_str = raw_path
        .find('?')
        .map_or(String::new(), |i| raw_path[i + 1..].to_string());

    // Strip the reverse-proxy subpath prefix (if any) so all downstream matching
    // (fonts, auth, routing) works on root-relative paths whether or not the
    // proxy already stripped it (#355).
    let path = base_path::strip(&path, base_path.as_str()).to_string();

    let is_api = path.starts_with("/api/");
    let requires_auth = is_api || path == "/metrics";

    if let Some(ref expected) = token {
        let mut has_header_auth = check_auth(&header_text, expected);

        // Read-only scrape token (GL #401): lets a Prometheus/Datadog agent
        // scrape `/metrics` without holding the full dashboard token. Valid
        // for the metrics endpoint only — every other API stays gated on the
        // dashboard token.
        if !has_header_auth
            && path == "/metrics"
            && let Ok(scrape) = std::env::var(SCRAPE_TOKEN_ENV)
        {
            let scrape = scrape.trim();
            if !scrape.is_empty() && check_auth(&header_text, scrape) {
                has_header_auth = true;
            }
        }

        if requires_auth && !has_header_auth {
            let body = r#"{"error":"unauthorized"}"#;
            let response = format!(
                "HTTP/1.1 401 Unauthorized\r\n\
                 Content-Type: application/json\r\n\
                 Content-Length: {}\r\n\
                 WWW-Authenticate: Bearer\r\n\
                 Connection: close\r\n\
                 \r\n\
                 {body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
            return;
        }

        if !csrf_origin_ok(&header_text, method.as_str(), path.as_str()) {
            let body = r#"{"error":"forbidden"}"#;
            let response = format!(
                "HTTP/1.1 403 Forbidden\r\n\
                 Content-Type: application/json\r\n\
                 Content-Length: {}\r\n\
                 Connection: close\r\n\
                 \r\n\
                 {body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
            return;
        }
    } else if requires_auth && !no_auth_request_ok(&header_text, &allowed_hosts) {
        // No-auth mode: the Bearer token is gone, so cross-origin/CSRF and
        // DNS-rebinding are blocked by request-header validation instead.
        let body = r#"{"error":"forbidden"}"#;
        let response = format!(
            "HTTP/1.1 403 Forbidden\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\
             \r\n\
             {body}",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes()).await;
        return;
    }

    // The Cockpit answers `/`, its static assets and `/api/gateway/*`; the
    // engine every other `/api/*` route and `/metrics`. A request that came
    // from the Cockpit itself is never sent back to it.
    if cockpit::serves(&path) && header_line_value(&header_text, cockpit::COCKPIT_HEADER).is_none()
    {
        let resolve = cockpit.clone();
        let target = tokio::task::spawn_blocking(move || resolve())
            .await
            .unwrap_or(Err(cockpit::CockpitUnavailable::StartFailed));
        let is_read = method.eq_ignore_ascii_case("GET") || method.eq_ignore_ascii_case("HEAD");
        match target {
            Ok(target) if is_api || is_read => {
                let forwarded = if query_str.is_empty() {
                    path.clone()
                } else {
                    format!("{path}?{query_str}")
                };
                let body = &buf[body_start..body_start + content_len];
                match cockpit::forward(&target, &method, &forwarded, &header_text, body).await {
                    Some(response) => {
                        let _ = stream.write_all(&response).await;
                    }
                    None => {
                        write_json_error(
                            &mut stream,
                            "502 Bad Gateway",
                            r#"{"error":"cockpit unavailable"}"#,
                        )
                        .await;
                    }
                }
                return;
            }
            Ok(_) => {
                write_json_error(
                    &mut stream,
                    "405 Method Not Allowed",
                    r#"{"error":"method not allowed"}"#,
                )
                .await;
                return;
            }
            // Without a Cockpit the engine keeps answering `/api/gateway/*`.
            Err(_) if is_api => {}
            Err(reason) => {
                let (status, content_type, body) = match path.as_str() {
                    // A source build is complete as it is: it has no Cockpit.
                    "/" | "/index.html" | "/cockpit" | "/cockpit/" => (
                        if reason == cockpit::CockpitUnavailable::SourceBuild {
                            "200 OK"
                        } else {
                            "503 Service Unavailable"
                        },
                        "text/html; charset=utf-8",
                        cockpit::unavailable_page(reason),
                    ),
                    "/favicon.ico" => ("204 No Content", "text/plain", String::new()),
                    _ => ("404 Not Found", "text/plain", "Not Found".to_string()),
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\n\
                     Content-Type: {content_type}\r\n\
                     Content-Length: {}\r\n\
                     Cache-Control: no-store\r\n\
                     X-Content-Type-Options: nosniff\r\n\
                     X-Frame-Options: DENY\r\n\
                     Referrer-Policy: no-referrer\r\n\
                     Content-Security-Policy: default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; frame-ancestors 'none'\r\n\
                     Connection: close\r\n\
                     \r\n\
                     {body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
                return;
            }
        }
    }

    // `X-LeanCTX-Project`: a read for a named project. A path that names no
    // project this dashboard may show is refused without saying why.
    let route = project_binding::canonical_route(&path).to_string();
    let project = match header_line_value(&header_text, cockpit::PROJECT_HEADER)
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        None => None,
        Some(requested) => {
            let requested = requested.to_string();
            let authorised = tokio::task::spawn_blocking(move || {
                project_binding::authorise(&requested, &project_binding::project_pool())
            })
            .await
            .ok()
            .flatten();
            if authorised.is_none() {
                write_json_error(&mut stream, "403 Forbidden", r#"{"error":"forbidden"}"#).await;
                return;
            }
            authorised
        }
    };
    let project = project.filter(|_| project_binding::is_project_read(&route, &method));
    // Display snapshots answer plain reads only; a query string selects a
    // different answer than the snapshot holds.
    let snapshot_read = method.eq_ignore_ascii_case("GET")
        && display_snapshots::is_display_route(&route)
        && query_str
            .split('&')
            .all(|pair| pair.is_empty() || pair.starts_with("token="));

    // Route handlers are synchronous and a few (graph/index builds) do seconds
    // of disk work. Running them inline on an async worker thread lets one slow
    // endpoint starve the small worker pool, so a trivial GET like
    // `/api/settings` can wait minutes behind it (#431, Windows few-core). Run
    // them on the blocking pool instead: the async workers stay free to serve
    // light endpoints promptly. `spawn_blocking` also captures panics (returns
    // a `JoinError`), so the previous `catch_unwind` is no longer needed.
    let route_started = std::time::Instant::now();
    let route_label = route.clone();
    let compute = tokio::task::spawn_blocking(move || {
        let run: display_snapshots::Compute = {
            let project = project.clone();
            let route = route.clone();
            Arc::new(move || {
                project_binding::in_request_project(project.as_deref(), || {
                    routes::route_response(
                        &route,
                        &query_str,
                        query_token.as_ref(),
                        token.as_ref(),
                        is_loopback,
                        &method,
                        &body_str,
                    )
                })
            })
        };
        if snapshot_read {
            let partition = project_binding::partition_of(project.as_deref());
            display_snapshots::DisplaySnapshots::global().serve(&route, &partition, &run)
        } else {
            (run(), None)
        }
    })
    .await;
    let ((status, content_type, mut body), observation) = match compute {
        Ok(v) => v,
        // The blocking task panicked or was cancelled — surface a 500 rather
        // than dropping the connection.
        Err(_) => (
            (
                "500 Internal Server Error",
                "application/json",
                r#"{"error":"dashboard route panicked"}"#.to_string(),
            ),
            None,
        ),
    };
    // Observability: a slow light endpoint is exactly the #431 symptom, so make
    // any handler that crosses 1s visible in the logs for future diagnosis.
    let route_elapsed = route_started.elapsed();
    if route_elapsed >= std::time::Duration::from_secs(3) {
        tracing::warn!(
            target: "lean_ctx::dashboard",
            "slow dashboard route {route_label} took {} ms",
            route_elapsed.as_millis()
        );
    }

    // Under a reverse-proxy subpath, rewrite root-absolute asset/API URLs in
    // any HTML/CSS/JS the engine serves so the browser requests them under
    // the prefix (#355).
    if !base_path.is_empty()
        && (content_type.contains("text/html")
            || content_type.contains("text/css")
            || content_type.contains("javascript"))
    {
        body = base_path::rewrite_asset_urls(&body, base_path.as_str());
    }

    let cache_header = if content_type.starts_with("application/json") {
        "Cache-Control: no-cache, no-store, must-revalidate\r\nPragma: no-cache\r\n"
    } else if content_type.starts_with("application/javascript")
        || content_type.starts_with("text/css")
    {
        "Cache-Control: no-cache, must-revalidate\r\n"
    } else {
        ""
    };
    // Display data says when it was observed and whether it is a snapshot.
    let snapshot_headers = observation
        .as_ref()
        .map(display_snapshots::Observation::headers)
        .unwrap_or_default();

    let nonce = {
        let mut nb = [0u8; 16];
        if getrandom::fill(&mut nb).is_err() {
            nb.iter_mut().enumerate().for_each(|(i, b)| {
                *b = (std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .subsec_nanos()
                    .wrapping_add(i as u32)) as u8;
            });
        }
        hex_lower(&nb)
    };
    if content_type.contains("text/html") {
        body = add_nonce_to_inline_scripts(&body, &nonce);
    }
    let security_headers = format!(
        "X-Content-Type-Options: nosniff\r\n\
         X-Frame-Options: DENY\r\n\
         Referrer-Policy: no-referrer\r\n\
         Content-Security-Policy: default-src 'self'; script-src 'self' 'nonce-{nonce}'; style-src 'self' 'unsafe-inline'; font-src 'self'; img-src 'self' data:; connect-src 'self'\r\n"
    );

    let response = format!(
        "HTTP/1.1 {status}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         {cache_header}\
         {snapshot_headers}\
         {security_headers}\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len()
    );

    let _ = stream.write_all(response.as_bytes()).await;
}

fn check_auth(request: &str, expected_token: &str) -> bool {
    for line in request.lines() {
        let lower = line.to_lowercase();
        if lower.starts_with("authorization:") {
            let value = line["authorization:".len()..].trim();
            if let Some(token) = value
                .strip_prefix("Bearer ")
                .or_else(|| value.strip_prefix("bearer "))
            {
                return constant_time_eq(token.trim().as_bytes(), expected_token.as_bytes());
            }
        }
    }
    false
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    bool::from(a.ct_eq(b))
}

#[cfg(test)]
mod tests;
