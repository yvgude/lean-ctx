use std::sync::Arc;
use subtle::ConstantTimeEq;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const DEFAULT_PORT: u16 = 3333;
const DEFAULT_HOST: &str = "127.0.0.1";
const COCKPIT_INDEX_HTML: &str = include_str!("static/index.html");
const COCKPIT_STYLE_CSS: &str = include_str!("static/style.css");
const COCKPIT_LIB_API_JS: &str = include_str!("static/lib/api.js");
const COCKPIT_LIB_FORMAT_JS: &str = include_str!("static/lib/format.js");
const COCKPIT_LIB_ROUTER_JS: &str = include_str!("static/lib/router.js");
const COCKPIT_LIB_CHARTS_JS: &str = include_str!("static/lib/charts.js");
const COCKPIT_LIB_SHARED_JS: &str = include_str!("static/lib/shared.js");
const COCKPIT_LIB_DOCTOR_JS: &str = include_str!("static/lib/doctor.js");
const COCKPIT_COMPONENT_NAV_JS: &str = include_str!("static/components/cockpit-nav.js");
const COCKPIT_COMPONENT_CONTEXT_JS: &str = include_str!("static/components/cockpit-context.js");
const COCKPIT_COMPONENT_OVERVIEW_JS: &str = include_str!("static/components/cockpit-overview.js");
const COCKPIT_COMPONENT_LIVE_JS: &str = include_str!("static/components/cockpit-live.js");
const COCKPIT_COMPONENT_KNOWLEDGE_JS: &str = include_str!("static/components/cockpit-knowledge.js");
const COCKPIT_COMPONENT_AGENTS_JS: &str = include_str!("static/components/cockpit-agents.js");
const COCKPIT_COMPONENT_MEMORY_JS: &str = include_str!("static/components/cockpit-memory.js");
const COCKPIT_COMPONENT_SEARCH_JS: &str = include_str!("static/components/cockpit-search.js");
const COCKPIT_COMPONENT_COMPRESSION_JS: &str =
    include_str!("static/components/cockpit-compression.js");
const COCKPIT_COMPONENT_TOUR_JS: &str = include_str!("static/components/cockpit-tour.js");
const COCKPIT_COMPONENT_GRAPH_JS: &str = include_str!("static/components/cockpit-graph.js");
const COCKPIT_COMPONENT_ARCHITECTURE_JS: &str =
    include_str!("static/components/cockpit-architecture.js");
const COCKPIT_COMPONENT_EXPLORER_JS: &str = include_str!("static/components/cockpit-explorer.js");
const COCKPIT_COMPONENT_HEALTH_JS: &str = include_str!("static/components/cockpit-health.js");
const COCKPIT_COMPONENT_REMAINING_JS: &str = include_str!("static/components/cockpit-remaining.js");
const COCKPIT_COMPONENT_COMMANDER_JS: &str = include_str!("static/components/cockpit-commander.js");
const COCKPIT_COMPONENT_PALETTE_JS: &str = include_str!("static/components/cockpit-palette.js");
const COCKPIT_COMPONENT_ROI_JS: &str = include_str!("static/components/cockpit-roi.js");
const COCKPIT_COMPONENT_LEADERBOARD_JS: &str =
    include_str!("static/components/cockpit-leaderboard.js");
const COCKPIT_COMPONENT_AREA_TABS_JS: &str = include_str!("static/components/cockpit-area-tabs.js");
const COCKPIT_COMPONENT_PROTECTION_JS: &str =
    include_str!("static/components/cockpit-protection.js");
const COCKPIT_COMPONENT_SETTINGS_JS: &str = include_str!("static/components/cockpit-settings.js");

// Vendored third-party libraries — embedded so the dashboard works fully offline
// (no external CDN). Served as text via the standard route pipeline.
const COCKPIT_VENDOR_CHART_JS: &str = include_str!("static/vendor/chart.umd.min.js");
const COCKPIT_VENDOR_D3_JS: &str = include_str!("static/vendor/d3.min.js");
const COCKPIT_FONTS_CSS: &str = include_str!("static/fonts/fonts.css");
const COCKPIT_FAVICON_SVG: &str = include_str!("static/favicon.svg");

// Self-hosted variable fonts (binary woff2). Served via a dedicated binary
// branch in `handle_request` so the bytes are never corrupted by the
// String-based route pipeline.
const FONT_INTER_WOFF2: &[u8] = include_bytes!("static/fonts/inter-variable.woff2");
const FONT_JETBRAINS_WOFF2: &[u8] = include_bytes!("static/fonts/jetbrains-mono-variable.woff2");
const FONT_SPACE_GROTESK_WOFF2: &[u8] = include_bytes!("static/fonts/space-grotesk-variable.woff2");

/// Maps a request path to an embedded binary font asset.
fn match_font_asset(path: &str) -> Option<&'static [u8]> {
    match path {
        "/static/fonts/inter-variable.woff2" => Some(FONT_INTER_WOFF2),
        "/static/fonts/jetbrains-mono-variable.woff2" => Some(FONT_JETBRAINS_WOFF2),
        "/static/fonts/space-grotesk-variable.woff2" => Some(FONT_SPACE_GROTESK_WOFF2),
        _ => None,
    }
}

pub mod base_path;
pub mod routes;

pub async fn start(
    port: Option<u16>,
    host: Option<String>,
    base_path: Option<String>,
    auth_token: Option<String>,
    open_mode: Option<String>,
) {
    // How to reveal the URL once the server is up: --open= flag > env > browser.
    let open = resolve_open_mode(open_mode.as_deref());
    let port = port.unwrap_or_else(|| {
        std::env::var("LEAN_CTX_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(DEFAULT_PORT)
    });

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

    let addr = format!("{host}:{port}");
    let is_local = host == "127.0.0.1" || host == "localhost" || host == "::1";

    // Resolve any *requested* fixed token (flag > LEAN_CTX_HTTP_TOKEN) up-front;
    // `None` means "generate a random one". Done before the already-running check
    // so we can warn when the requested token won't match a live instance (#377).
    let (requested_token, token_src) = resolve_requested_token(auth_token.as_deref());

    // Avoid accidental multiple dashboard instances (common source of "it hangs").
    // Only safe to auto-detect for local dashboards without auth.
    if is_local && dashboard_responding(&host, port) {
        println!("\n  lean-ctx dashboard already running → http://{host}:{port}{base_path}");
        if let Some(req) = requested_token.as_deref()
            && load_saved_token().as_deref() != Some(req)
        {
            eprintln!(
                "  \x1b[33m⚠\x1b[0m The running instance uses a different token — your {token_src} \
                     will be rejected. Stop it (Ctrl+C) and restart to apply the new token."
            );
        }
        println!("  Tip: use Ctrl+C in the existing terminal to stop it.\n");
        if let Some(t) = load_saved_token() {
            open_dashboard_url(
                &format!("http://localhost:{port}{base_path}/?token={t}"),
                open,
            );
        } else {
            open_dashboard_url(&format!("http://localhost:{port}{base_path}/"), open);
        }
        return;
    }

    // Always enable auth (even on loopback) to prevent cross-origin reads of /api/*
    // from a malicious website (CORS is not a reliable boundary for localhost services).
    let t = requested_token.unwrap_or_else(generate_token);
    let token = Some(Arc::new(t));

    // Bind BEFORE persisting the token: two racing `lean-ctx dashboard` starts
    // both used to write their fresh token, the bind loser exited — leaving a
    // token on disk that the surviving server never accepted. Every later
    // "already running" browser open (and any tool reading dashboard.token)
    // then got 401s. Binding first makes the loser exit without touching the
    // file, so dashboard.token always belongs to the live listener.
    let listener = match TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("Failed to bind to {addr}: {e}");
            std::process::exit(1);
        }
    };

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
    }

    let stats_path = crate::core::data_dir::lean_ctx_data_dir().map_or_else(
        |_| "~/.lean-ctx/stats.json".to_string(),
        |d| d.join("stats.json").display().to_string(),
    );

    if host == "0.0.0.0" {
        println!("\n  lean-ctx dashboard → http://0.0.0.0:{port} (all interfaces)");
        println!("  Local access:  http://localhost:{port}");
    } else {
        println!("\n  lean-ctx dashboard → http://{host}:{port}");
    }
    println!("  Stats file: {stats_path}");
    println!("  Press Ctrl+C to stop");
    println!(
        "  \x1b[2m💡 Join the public leaderboard at https://leanctx.com/metrics: lean-ctx gain --publish --leaderboard\x1b[0m\n"
    );

    if is_local {
        if let Some(t) = token.as_ref() {
            open_dashboard_url(
                &format!("http://localhost:{port}{base_path}/?token={t}"),
                open,
            );
        } else {
            open_dashboard_url(&format!("http://localhost:{port}{base_path}/"), open);
        }
    }
    if crate::shell::is_container() && is_local {
        println!("  Tip (Docker): bind 0.0.0.0 + publish port:");
        println!("    lean-ctx dashboard --host=0.0.0.0 --port={port}");
        println!("    docker run ... -p {port}:{port} ...");
        println!();
    }

    if crate::core::datadog_push::spawn_if_enabled() {
        println!(
            "  Datadog push: enabled (agentless, every LEAN_CTX_DATADOG_INTERVAL_SECS or 60s)"
        );
    }

    loop {
        if let Ok((stream, _)) = listener.accept().await {
            let token_ref = token.clone();
            let base_ref = base_path.clone();
            tokio::spawn(handle_request(stream, token_ref, base_ref));
        }
    }
}

/// Name of the env var that pins the dashboard Bearer token (#377).
const HTTP_TOKEN_ENV: &str = "LEAN_CTX_HTTP_TOKEN";
/// Read-only token accepted **only** for `GET /metrics` (GL #401) so
/// monitoring agents never hold the full dashboard credential.
const SCRAPE_TOKEN_ENV: &str = "LEAN_CTX_SCRAPE_TOKEN";

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

fn load_saved_token() -> Option<String> {
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
enum DashboardOpen {
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
fn resolve_open_mode(flag: Option<&str>) -> DashboardOpen {
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
fn open_dashboard_url(url: &str, mode: DashboardOpen) {
    match mode {
        DashboardOpen::Browser => open_browser(url),
        DashboardOpen::None => {}
        DashboardOpen::Vscode => {
            // Prefer the extension's native webview tab (#466 item 3): with the
            // lean-ctx VS Code extension installed, one command opens the
            // dashboard as a real editor tab — no URL copy/paste. Keep the
            // Simple Browser path as the no-extension fallback.
            println!(
                "  \x1b[2mNative tab: run ⇧⌘P → \"lean-ctx: Open Web Dashboard\" (needs the lean-ctx VS Code extension)\x1b[0m"
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

fn dashboard_responding(host: &str, port: u16) -> bool {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::time::Duration;

    let addr = format!("{host}:{port}");
    let Ok(mut s) = TcpStream::connect_timeout(
        &addr
            .parse()
            .unwrap_or_else(|_| std::net::SocketAddr::from(([127, 0, 0, 1], port))),
        Duration::from_millis(150),
    ) else {
        return false;
    };
    let _ = s.set_read_timeout(Some(Duration::from_millis(150)));
    let _ = s.set_write_timeout(Some(Duration::from_millis(150)));

    let auth_header = load_saved_token()
        .map(|t| format!("Authorization: Bearer {t}\r\n"))
        .unwrap_or_default();

    let req = format!(
        "GET /api/version HTTP/1.1\r\nHost: localhost\r\n{auth_header}Connection: close\r\n\r\n"
    );
    if s.write_all(req.as_bytes()).is_err() {
        return false;
    }
    let mut buf = [0u8; 256];
    let Ok(n) = s.read(&mut buf) else {
        return false;
    };
    let head = String::from_utf8_lossy(&buf[..n]);
    head.starts_with("HTTP/1.1 200") || head.starts_with("HTTP/1.0 200")
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

async fn handle_request(
    mut stream: tokio::net::TcpStream,
    token: Option<Arc<String>>,
    base_path: Arc<String>,
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

    // Binary font assets are public (like CSS/JS) and bypass the String-based
    // route pipeline so their bytes stay intact.
    if let Some(bytes) = match_font_asset(&path) {
        let header = format!(
            "HTTP/1.1 200 OK\r\n\
             Content-Type: font/woff2\r\n\
             Content-Length: {}\r\n\
             Cache-Control: public, max-age=31536000, immutable\r\n\
             X-Content-Type-Options: nosniff\r\n\
             Connection: close\r\n\
             \r\n",
            bytes.len()
        );
        let _ = stream.write_all(header.as_bytes()).await;
        let _ = stream.write_all(bytes).await;
        return;
    }

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
    }

    // Route handlers are synchronous and a few (graph/index builds) do seconds
    // of disk work. Running them inline on an async worker thread lets one slow
    // endpoint starve the small worker pool, so a trivial GET like
    // `/api/settings` can wait minutes behind it (#431, Windows few-core). Run
    // them on the blocking pool instead: the async workers stay free to serve
    // light endpoints promptly. `spawn_blocking` also captures panics (returns
    // a `JoinError`), so the previous `catch_unwind` is no longer needed.
    let route_started = std::time::Instant::now();
    let route_label = path.clone();
    let compute = tokio::task::spawn_blocking(move || {
        routes::route_response(
            &path,
            &query_str,
            query_token.as_ref(),
            token.as_ref(),
            is_loopback,
            &method,
            &body_str,
        )
    })
    .await;
    let (status, content_type, mut body) = match compute {
        Ok(v) => v,
        // The blocking task panicked or was cancelled — surface a 500 rather
        // than dropping the connection.
        Err(_) => (
            "500 Internal Server Error",
            "application/json",
            r#"{"error":"dashboard route panicked"}"#.to_string(),
        ),
    };
    // Observability: a slow light endpoint is exactly the #431 symptom, so make
    // any handler that crosses 1s visible in the logs for future diagnosis.
    let route_elapsed = route_started.elapsed();
    if route_elapsed >= std::time::Duration::from_secs(1) {
        tracing::warn!(
            target: "lean_ctx::dashboard",
            "slow dashboard route {route_label} took {} ms",
            route_elapsed.as_millis()
        );
    }

    // Under a reverse-proxy subpath, rewrite root-absolute asset/API URLs in the
    // served HTML/CSS/JS so the browser requests them under the prefix (#355).
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
mod tests {
    use super::routes::helpers::normalize_dashboard_demo_path;
    use super::*;
    use tempfile::tempdir;

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn check_auth_with_valid_bearer() {
        let req = "GET /api/stats HTTP/1.1\r\nAuthorization: Bearer lctx_abc123\r\n\r\n";
        assert!(check_auth(req, "lctx_abc123"));
    }

    #[test]
    fn check_auth_with_invalid_bearer() {
        let req = "GET /api/stats HTTP/1.1\r\nAuthorization: Bearer wrong_token\r\n\r\n";
        assert!(!check_auth(req, "lctx_abc123"));
    }

    #[test]
    fn open_mode_flag_parses_all_variants() {
        // Explicit flag wins and never consults the environment (#424).
        assert_eq!(resolve_open_mode(Some("none")), DashboardOpen::None);
        assert_eq!(resolve_open_mode(Some("off")), DashboardOpen::None);
        assert_eq!(resolve_open_mode(Some("no")), DashboardOpen::None);
        assert_eq!(resolve_open_mode(Some("vscode")), DashboardOpen::Vscode);
        assert_eq!(resolve_open_mode(Some("code")), DashboardOpen::Vscode);
        assert_eq!(resolve_open_mode(Some("editor")), DashboardOpen::Vscode);
        assert_eq!(resolve_open_mode(Some("VSCode")), DashboardOpen::Vscode);
        assert_eq!(resolve_open_mode(Some("browser")), DashboardOpen::Browser);
        // Unknown values fall back to the historical default rather than erroring.
        assert_eq!(resolve_open_mode(Some("wat")), DashboardOpen::Browser);
    }

    #[test]
    fn open_mode_env_is_used_when_no_flag() {
        let _guard = ENV_LOCK.lock().unwrap();
        crate::test_env::set_var("LEAN_CTX_DASHBOARD_OPEN", "none");
        assert_eq!(resolve_open_mode(None), DashboardOpen::None);
        crate::test_env::set_var("LEAN_CTX_DASHBOARD_OPEN", "vscode");
        assert_eq!(resolve_open_mode(None), DashboardOpen::Vscode);
        // Flag still overrides the env var.
        assert_eq!(resolve_open_mode(Some("browser")), DashboardOpen::Browser);
        crate::test_env::remove_var("LEAN_CTX_DASHBOARD_OPEN");
        assert_eq!(resolve_open_mode(None), DashboardOpen::Browser);
    }

    #[test]
    fn check_auth_missing_header() {
        let req = "GET /api/stats HTTP/1.1\r\nHost: localhost\r\n\r\n";
        assert!(!check_auth(req, "lctx_abc123"));
    }

    #[test]
    fn check_auth_lowercase_bearer() {
        let req = "GET /api/stats HTTP/1.1\r\nauthorization: bearer lctx_abc123\r\n\r\n";
        assert!(check_auth(req, "lctx_abc123"));
    }

    #[test]
    fn query_token_parsing() {
        let raw_path = "/index.html?token=lctx_abc123&other=val";
        let idx = raw_path.find('?').unwrap();
        let qs = &raw_path[idx + 1..];
        let tok = qs.split('&').find_map(|pair| pair.strip_prefix("token="));
        assert_eq!(tok, Some("lctx_abc123"));
    }

    #[test]
    fn api_path_detection() {
        assert!("/api/stats".starts_with("/api/"));
        assert!("/api/version".starts_with("/api/"));
        assert!(!"/".starts_with("/api/"));
        assert!(!"/index.html".starts_with("/api/"));
        assert!(!"/favicon.ico".starts_with("/api/"));
    }

    #[test]
    fn normalize_dashboard_demo_path_strips_rooted_relative_windows_path() {
        let normalized = normalize_dashboard_demo_path(r"\backend\list_tables.js");
        assert_eq!(
            normalized,
            format!("backend{}list_tables.js", std::path::MAIN_SEPARATOR)
        );
    }

    #[test]
    fn normalize_dashboard_demo_path_preserves_absolute_windows_path() {
        let input = r"C:\repo\backend\list_tables.js";
        assert_eq!(normalize_dashboard_demo_path(input), input);
    }

    #[test]
    fn normalize_dashboard_demo_path_preserves_unc_path() {
        let input = r"\\server\share\backend\list_tables.js";
        assert_eq!(normalize_dashboard_demo_path(input), input);
    }

    #[test]
    fn normalize_dashboard_demo_path_strips_dot_slash_prefix() {
        assert_eq!(
            normalize_dashboard_demo_path("./src/main.rs"),
            "src/main.rs"
        );
        assert_eq!(
            normalize_dashboard_demo_path(r".\src\main.rs"),
            format!("src{}main.rs", std::path::MAIN_SEPARATOR)
        );
    }

    #[test]
    fn api_profile_returns_json() {
        let (_status, _ct, body) =
            routes::route_response("/api/profile", "", None, None, false, "GET", "");
        let v: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");
        assert!(v.get("active_name").is_some(), "missing active_name");
        assert!(
            v.pointer("/profile/profile/name")
                .and_then(|n| n.as_str())
                .is_some(),
            "missing profile.profile.name"
        );
        assert!(v.get("available").and_then(|a| a.as_array()).is_some());
    }

    #[test]
    fn api_billing_badge_returns_cosmetic_shape() {
        let (status, ct, body) =
            routes::route_response("/api/billing-badge", "", None, None, false, "GET", "");
        assert_eq!(status, "200 OK");
        assert_eq!(ct, "application/json");
        let v: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");
        assert!(v.get("plan").and_then(|p| p.as_str()).is_some());
        assert!(
            v.get("supporter")
                .and_then(serde_json::Value::as_bool)
                .is_some()
        );
        assert!(
            matches!(
                v.get("source").and_then(|s| s.as_str()),
                Some("live" | "cached" | "expired" | "none")
            ),
            "unexpected source: {body}"
        );
    }

    #[test]
    fn api_episodes_returns_json() {
        let (_status, _ct, body) =
            routes::route_response("/api/episodes", "", None, None, false, "GET", "");
        let v: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");
        assert!(v.get("project_hash").is_some());
        assert!(v.get("stats").is_some());
        assert!(v.get("recent").and_then(|a| a.as_array()).is_some());
    }

    #[test]
    fn api_procedures_returns_json() {
        let (_status, _ct, body) =
            routes::route_response("/api/procedures", "", None, None, false, "GET", "");
        let v: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");
        assert!(v.get("project_hash").is_some());
        assert!(v.get("procedures").and_then(|a| a.as_array()).is_some());
        assert!(v.get("suggestions").and_then(|a| a.as_array()).is_some());
    }

    #[test]
    fn api_compression_demo_heals_moved_file_paths() {
        let _g = ENV_LOCK.lock().expect("env lock");
        let td = tempdir().expect("tempdir");
        let root = td.path();
        std::fs::create_dir_all(root.join("src").join("moved")).expect("mkdir");
        std::fs::write(
            root.join("src").join("moved").join("foo.rs"),
            "pub fn foo() { println!(\"hi\"); }\n",
        )
        .expect("write foo.rs");

        let root_s = root.to_string_lossy().to_string();
        crate::test_env::set_var("LEAN_CTX_DASHBOARD_PROJECT", &root_s);

        let (_status, _ct, body) = routes::route_response(
            "/api/compression-demo",
            "path=src/foo.rs",
            None,
            None,
            false,
            "GET",
            "",
        );
        let v: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");
        assert!(v.get("error").is_none(), "unexpected error: {body}");
        assert_eq!(
            v.get("resolved_from").and_then(|x| x.as_str()),
            Some("src/moved/foo.rs")
        );

        crate::test_env::remove_var("LEAN_CTX_DASHBOARD_PROJECT");
        if let Some(dir) = crate::core::graph_index::ProjectIndex::index_dir(&root_s) {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn resolve_token_uses_env_var_verbatim() {
        let _g = ENV_LOCK.lock().expect("env lock");
        crate::test_env::set_var(HTTP_TOKEN_ENV, "lctx_mystatic");
        let (token, src) = resolve_requested_token(None);
        crate::test_env::remove_var(HTTP_TOKEN_ENV);
        assert_eq!(
            src, HTTP_TOKEN_ENV,
            "token should be reported as env-sourced"
        );
        assert_eq!(token.as_deref(), Some("lctx_mystatic"));
    }

    #[test]
    fn resolve_token_trims_env_var() {
        let _g = ENV_LOCK.lock().expect("env lock");
        crate::test_env::set_var(HTTP_TOKEN_ENV, "  lctx_padded  ");
        let (token, src) = resolve_requested_token(None);
        crate::test_env::remove_var(HTTP_TOKEN_ENV);
        assert_eq!(src, HTTP_TOKEN_ENV);
        assert_eq!(token.as_deref(), Some("lctx_padded"));
    }

    #[test]
    fn resolve_token_falls_back_to_random_when_unset() {
        let _g = ENV_LOCK.lock().expect("env lock");
        crate::test_env::remove_var(HTTP_TOKEN_ENV);
        let (token, src) = resolve_requested_token(None);
        assert!(token.is_none(), "unset env requests no fixed token");
        assert!(src.is_empty());
        // The production fallback in `start()` generates a random token.
        let generated = token.unwrap_or_else(generate_token);
        assert!(
            generated.starts_with("lctx_"),
            "generated token prefix, got {generated}"
        );
        assert!(
            generated.len() > 12,
            "generated token should be 32-byte hex"
        );
    }

    #[test]
    fn resolve_token_ignores_empty_env() {
        let _g = ENV_LOCK.lock().expect("env lock");
        crate::test_env::set_var(HTTP_TOKEN_ENV, "   ");
        let (token, src) = resolve_requested_token(None);
        crate::test_env::remove_var(HTTP_TOKEN_ENV);
        assert!(
            token.is_none(),
            "whitespace-only env requests no fixed token"
        );
        assert!(src.is_empty());
    }

    #[test]
    fn resolve_token_flag_overrides_env() {
        // #377: --auth-token must win over LEAN_CTX_HTTP_TOKEN so it survives
        // environments that strip/fail to inherit the env var.
        let _g = ENV_LOCK.lock().expect("env lock");
        crate::test_env::set_var(HTTP_TOKEN_ENV, "lctx_fromenv");
        let (token, src) = resolve_requested_token(Some("lctx_fromflag"));
        crate::test_env::remove_var(HTTP_TOKEN_ENV);
        assert_eq!(src, "--auth-token");
        assert_eq!(token.as_deref(), Some("lctx_fromflag"));
    }

    #[test]
    fn resolve_token_uses_flag_when_env_unset() {
        let _g = ENV_LOCK.lock().expect("env lock");
        crate::test_env::remove_var(HTTP_TOKEN_ENV);
        let (token, src) = resolve_requested_token(Some("  lctx_flag_padded  "));
        assert_eq!(src, "--auth-token");
        assert_eq!(token.as_deref(), Some("lctx_flag_padded"));
    }

    #[test]
    fn resolve_token_empty_flag_falls_back_to_env() {
        let _g = ENV_LOCK.lock().expect("env lock");
        crate::test_env::set_var(HTTP_TOKEN_ENV, "lctx_fromenv");
        let (token, src) = resolve_requested_token(Some("   "));
        crate::test_env::remove_var(HTTP_TOKEN_ENV);
        assert_eq!(src, HTTP_TOKEN_ENV);
        assert_eq!(token.as_deref(), Some("lctx_fromenv"));
    }
}
