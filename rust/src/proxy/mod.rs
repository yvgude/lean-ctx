pub mod anthropic;
pub mod cache_safety;
pub mod ccr;
pub mod chatgpt;
pub mod chatgpt_cookies;
pub mod cold_prefix;
pub mod compress;
pub mod compress_api;
pub mod cost;
pub mod effort;
pub mod forward;
pub mod google;
pub mod history_prune;
pub mod holdout;
pub mod introspect;
pub mod metrics;
pub mod openai;
pub mod openai_responses;
pub mod openai_responses_ws;
pub mod output_savings;
pub mod prose;
pub mod prose_ranker;
pub mod tool_kind;
pub mod usage;
pub mod usage_meter;
pub mod verbosity;

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::core::config::Upstreams;

use axum::{
    Router,
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
    routing::{any, get, post},
};

#[derive(Clone)]
pub struct ProxyState {
    pub client: reqwest::Client,
    pub port: u16,
    pub stats: Arc<ProxyStats>,
    pub introspect: Arc<introspect::IntrospectState>,
    /// Live provider upstreams, refreshed from config.toml without a proxy
    /// restart (#449). Read per request via [`ProxyState::openai_upstream`] etc.
    pub upstreams: tokio::sync::watch::Receiver<Arc<Upstreams>>,
}

impl ProxyState {
    /// Consistent snapshot of all upstreams for the current request/response.
    pub fn upstream_snapshot(&self) -> Arc<Upstreams> {
        self.upstreams.borrow().clone()
    }

    /// Current Anthropic upstream (live).
    pub fn anthropic_upstream(&self) -> String {
        self.upstreams.borrow().anthropic.clone()
    }

    /// Current OpenAI upstream (live).
    pub fn openai_upstream(&self) -> String {
        self.upstreams.borrow().openai.clone()
    }

    /// Current ChatGPT upstream (live).
    pub fn chatgpt_upstream(&self) -> String {
        self.upstreams.borrow().chatgpt.clone()
    }

    /// Current Gemini upstream (live).
    pub fn gemini_upstream(&self) -> String {
        self.upstreams.borrow().gemini.clone()
    }
}

pub struct ProxyStats {
    pub requests_total: AtomicU64,
    pub requests_compressed: AtomicU64,
    pub tokens_saved: AtomicU64,
    pub bytes_original: AtomicU64,
    pub bytes_compressed: AtomicU64,
}

impl Default for ProxyStats {
    fn default() -> Self {
        Self {
            requests_total: AtomicU64::new(0),
            requests_compressed: AtomicU64::new(0),
            tokens_saved: AtomicU64::new(0),
            bytes_original: AtomicU64::new(0),
            bytes_compressed: AtomicU64::new(0),
        }
    }
}

impl ProxyStats {
    pub fn record_request(&self, original: usize, compressed: usize) {
        self.requests_total.fetch_add(1, Ordering::Relaxed);
        self.bytes_original
            .fetch_add(original as u64, Ordering::Relaxed);
        let effective_compressed = compressed.min(original);
        self.bytes_compressed
            .fetch_add(effective_compressed as u64, Ordering::Relaxed);
        if compressed < original {
            self.requests_compressed.fetch_add(1, Ordering::Relaxed);
        }
        let saved_tokens = (original.saturating_sub(effective_compressed) / 4) as u64;
        self.tokens_saved.fetch_add(saved_tokens, Ordering::Relaxed);
    }

    pub fn compression_ratio(&self) -> f64 {
        let original = self.bytes_original.load(Ordering::Relaxed);
        if original == 0 {
            return 0.0;
        }
        let compressed = self.bytes_compressed.load(Ordering::Relaxed);
        (1.0 - compressed as f64 / original as f64) * 100.0
    }
}

#[cfg(test)]
mod stats_tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[test]
    fn compression_ratio_includes_uncompressed_requests() {
        let stats = ProxyStats::default();

        stats.record_request(1_000, 500);
        stats.record_request(1_000, 1_000);

        assert_eq!(stats.requests_total.load(Ordering::Relaxed), 2);
        assert_eq!(stats.requests_compressed.load(Ordering::Relaxed), 1);
        assert_eq!(stats.tokens_saved.load(Ordering::Relaxed), 125);
        assert_eq!(stats.compression_ratio(), 25.0);
    }

    #[test]
    fn expanded_requests_count_as_zero_savings() {
        let stats = ProxyStats::default();

        stats.record_request(1_000, 1_500);

        assert_eq!(stats.requests_total.load(Ordering::Relaxed), 1);
        assert_eq!(stats.requests_compressed.load(Ordering::Relaxed), 0);
        assert_eq!(stats.tokens_saved.load(Ordering::Relaxed), 0);
        assert_eq!(stats.compression_ratio(), 0.0);
    }
}

/// TCP connect timeout (seconds). Configurable via `LEAN_CTX_PROXY_CONNECT_TIMEOUT_SECS`.
fn connect_timeout_secs() -> u64 {
    std::env::var("LEAN_CTX_PROXY_CONNECT_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|s| *s > 0)
        .unwrap_or(15)
}

/// Idle read timeout (seconds) between bytes from upstream. Generous by default
/// so long extended-thinking phases (which still emit SSE keepalives) are never
/// cut, while a truly dead connection eventually fails. Configurable via
/// `LEAN_CTX_PROXY_READ_TIMEOUT_SECS`.
fn read_idle_timeout_secs() -> u64 {
    std::env::var("LEAN_CTX_PROXY_READ_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|s| *s > 0)
        .unwrap_or(300)
}

/// How often (seconds) a running proxy re-reads config.toml for upstream
/// changes. `LEAN_CTX_PROXY_RELOAD_SECS` overrides; default 5s.
fn upstream_reload_secs() -> u64 {
    std::env::var("LEAN_CTX_PROXY_RELOAD_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|s| *s > 0)
        .unwrap_or(5)
}

/// Background task: re-resolves the provider upstreams from config.toml on an
/// interval and publishes any change to the live request handlers (#449). Ends
/// once every receiver (the proxy itself) has been dropped.
///
/// `Config::load()` already keeps an internal content-hash cache, so re-reading
/// an unchanged `config.toml` skips the TOML parse + merge and costs only a small
/// file read; combined with the relaxed default interval (#453) the idle steady
/// state is negligible without needing a separate stat pre-check.
fn spawn_upstream_refresh(tx: tokio::sync::watch::Sender<Arc<Upstreams>>, initial: Upstreams) {
    let interval = std::time::Duration::from_secs(upstream_reload_secs());
    tokio::spawn(async move {
        let mut last = initial;
        loop {
            tokio::time::sleep(interval).await;
            let next = crate::core::config::Config::load()
                .proxy
                .refresh_upstreams(&last);
            if next != last {
                log_upstream_change(&last, &next);
                last = next.clone();
                if tx.send(Arc::new(next)).is_err() {
                    break;
                }
            }
        }
    });
}

/// One stdout line per changed provider, matching the startup banner style so a
/// running proxy's log shows when (and to what) an upstream switched.
fn log_upstream_change(old: &Upstreams, new: &Upstreams) {
    if old.anthropic != new.anthropic {
        println!("  ↻ Anthropic upstream → {}", new.anthropic);
    }
    if old.openai != new.openai {
        println!("  ↻ OpenAI upstream → {}", new.openai);
    }
    if old.chatgpt != new.chatgpt {
        println!("  ↻ ChatGPT upstream → {}", new.chatgpt);
    }
    if old.gemini != new.gemini {
        println!("  ↻ Gemini upstream → {}", new.gemini);
    }
}

pub async fn start_proxy(port: u16) -> anyhow::Result<()> {
    let token = crate::core::session_token::resolve_proxy_token("LEAN_CTX_PROXY_TOKEN");
    start_proxy_with_token(port, Some(token)).await
}

/// Security invariant: the proxy NEVER runs unauthenticated. `None` does not
/// mean "no auth" — it means "resolve the session token for me". Provider
/// routes additionally accept provider API keys (see `proxy_auth_guard`), so
/// IDE clients keep working without any setup.
fn effective_auth_token(auth_token: Option<String>) -> String {
    auth_token
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| crate::core::session_token::resolve_proxy_token("LEAN_CTX_PROXY_TOKEN"))
}

pub async fn start_proxy_with_token(port: u16, auth_token: Option<String>) -> anyhow::Result<()> {
    use crate::core::config::{Config, is_local_proxy_url};

    let auth_token = effective_auth_token(auth_token);

    // A single total timeout aborts long streaming generations (e.g. Opus doing
    // a big refactor) mid-response. Use a connect timeout plus a read (idle)
    // timeout instead: a genuinely hung upstream still fails, but a slow-but-
    // alive stream is never cut off. Both are configurable for edge networks.
    let client = chatgpt_cookies::with_chatgpt_cloudflare_cookie_store(reqwest::Client::builder())
        .connect_timeout(std::time::Duration::from_secs(connect_timeout_secs()))
        .read_timeout(std::time::Duration::from_secs(read_idle_timeout_secs()))
        .build()?;

    // Seed the measured-spend meter from disk so a proxy restart never zeroes
    // the user's cumulative real provider bill.
    usage_meter::resume_from_disk();
    // Seed the cold-prefix baselines too so a long idle gap that straddles a
    // proxy restart is still detected and the repack can fire (#499).
    cold_prefix::resume_from_disk();

    let cfg = Config::load();
    // Read once at startup — avoids a Config::load() on every proxied request.
    let require_token = cfg.proxy_require_token;
    let initial = cfg.proxy.resolve_all();

    // The proxy reads its upstreams live from a watch channel: a background task
    // re-resolves them from config.toml on an interval and publishes any change,
    // so `lean-ctx config set proxy.*_upstream` (or any config.toml edit) takes
    // effect on the running proxy within seconds, without a restart (#449).
    let (upstream_tx, upstream_rx) = tokio::sync::watch::channel(Arc::new(initial.clone()));
    spawn_upstream_refresh(upstream_tx, initial.clone());

    let Upstreams {
        anthropic: anthropic_upstream,
        openai: openai_upstream,
        chatgpt: chatgpt_upstream,
        gemini: gemini_upstream,
    } = initial;

    let state = ProxyState {
        client,
        port,
        stats: Arc::new(ProxyStats::default()),
        introspect: Arc::new(introspect::IntrospectState::default()),
        upstreams: upstream_rx,
    };

    let mut app = Router::new()
        .route("/health", get(health))
        .route("/status", get(status_handler))
        .route("/v1/messages", any(anthropic::handler))
        .route("/v1/messages/{*rest}", any(anthropic::handler))
        .route("/v1/chat/completions", any(openai::handler))
        // POST → HTTP/SSE forwarder; GET → Codex/OpenAI WebSocket bridge (#440).
        .route(
            "/v1/responses",
            post(openai_responses::handler).get(openai_responses::ws_handler),
        )
        .route("/v1/responses/{*rest}", any(openai_responses::handler))
        // Bare provider endpoints (no `/v1` prefix). Clients whose base URL points
        // at the proxy root — notably OpenCode via `@ai-sdk/openai`, whose
        // Responses-API requests hit `/responses` — dispatch here. The
        // `normalize_provider_path` layer rewrites the URI to its canonical
        // `/v1/...` form before the handler forwards upstream (#353).
        .route("/messages", any(anthropic::handler))
        .route("/messages/{*rest}", any(anthropic::handler))
        .route("/chat/completions", any(openai::handler))
        .route(
            "/responses",
            post(openai_responses::handler).get(openai_responses::ws_handler),
        )
        .route("/responses/{*rest}", any(openai_responses::handler))
        .route(
            "/backend-api/codex/responses",
            post(chatgpt::codex_responses_handler).get(chatgpt::codex_responses_ws_handler),
        )
        .route(
            "/backend-api/codex/responses/{*rest}",
            any(chatgpt::codex_responses_handler),
        )
        // Non-model ChatGPT backend calls (including codex_apps MCP) are not
        // prompt JSON. Keep them as credential-preserving passthrough traffic.
        .route("/backend-api", any(chatgpt::backend_api_handler))
        .route("/backend-api/{*rest}", any(chatgpt::backend_api_handler))
        .route("/v1/references/{id}", get(v1_resolve_reference))
        // Drop-in `compress(messages, model)` contract (#739): deterministic
        // messages-in / messages-out compression for SDK clients.
        .route("/v1/compress", post(compress_api::handler))
        .fallback(fallback_router)
        .layer(axum::middleware::from_fn(host_guard))
        .with_state(state);

    {
        let expected = auth_token.clone();
        app = app.layer(axum::middleware::from_fn(move |req, next| {
            let expected = expected.clone();
            proxy_auth_guard(req, next, expected, require_token)
        }));
    }

    // Outermost layer (runs first): normalize bare provider endpoints to their
    // canonical `/v1/...` form so auth, routing and upstream forwarding all agree,
    // regardless of whether the client's base URL includes `/v1` (#353).
    app = app.layer(axum::middleware::from_fn(normalize_provider_path));

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    println!("lean-ctx proxy listening on http://{addr} (token auth enabled)");
    println!("  Anthropic: POST /v1/messages → {anthropic_upstream}");
    println!("  OpenAI:    POST /v1/chat/completions → {openai_upstream}");
    println!(
        "  OpenAI:    POST /v1/responses → {openai_upstream}  (bare /responses also accepted)"
    );
    println!("  ChatGPT:   POST /backend-api/codex/responses → {chatgpt_upstream}");
    println!("  ChatGPT:   any  /backend-api/* → {chatgpt_upstream}");
    println!("  Gemini:    POST /v1beta/models/... → {gemini_upstream}");
    println!("  Compress:  POST /v1/compress (deterministic messages-in/out, local)");
    // Codex defaults to a WebSocket Responses transport (ws://…/responses). The
    // proxy now bridges it to the HTTP/SSE upstream (#440), so Codex works as a
    // drop-in without a `supports_websockets = false` workaround.
    println!(
        "  Codex:     WS  ws://{addr}/responses → bridged to {openai_upstream} (HTTP/SSE, #440)"
    );
    if openai_upstream.starts_with("http://") && !is_local_proxy_url(&openai_upstream) {
        println!(
            "  ⚠ OpenAI upstream is plaintext HTTP to a non-loopback host \
             (allow_insecure_http_upstream) — use only on a trusted local network"
        );
    }

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    println!("lean-ctx proxy shut down cleanly.");
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();

    #[cfg(unix)]
    {
        // Fall back to Ctrl-C only if the SIGTERM handler cannot be installed,
        // rather than panicking the proxy on startup.
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sigterm) => {
                tokio::select! {
                    _ = ctrl_c => {},
                    _ = sigterm.recv() => {},
                }
            }
            Err(e) => {
                tracing::warn!("lean-ctx proxy: SIGTERM handler unavailable ({e}); Ctrl-C only");
                ctrl_c.await.ok();
            }
        }
    }

    #[cfg(not(unix))]
    {
        ctrl_c.await.ok();
    }

    println!("lean-ctx proxy: received shutdown signal, draining…");
}

async fn health() -> impl IntoResponse {
    let body = serde_json::json!({
        "status": "ok",
        "pid": std::process::id(),
    });
    (StatusCode::OK, axum::Json(body))
}

async fn v1_resolve_reference(
    axum::extract::Path(id): axum::extract::Path<String>,
) -> impl IntoResponse {
    match crate::server::reference_store::resolve(&id) {
        Some(content) => (StatusCode::OK, content),
        None => (
            StatusCode::NOT_FOUND,
            "Reference expired or not found".to_string(),
        ),
    }
}

async fn status_handler(State(state): State<ProxyState>) -> impl IntoResponse {
    use std::sync::atomic::Ordering::Relaxed;
    let s = &state.stats;
    let i = &state.introspect;

    let last_breakdown = i
        .last_breakdown
        .lock()
        .ok()
        .and_then(|guard| guard.as_ref().map(|b| serde_json::to_value(b).ok()))
        .flatten();

    let spend = usage_meter::snapshot();
    let spend_total: f64 = spend.iter().map(|m| m.cost_usd).sum();

    // Live upstreams the proxy is forwarding to right now (#449). This is the
    // single source of truth for "where is my traffic actually going" — it
    // reflects config.toml hot-reloads and any start-time env override.
    let up = state.upstream_snapshot();

    // Resolve the effort level fresh so /status reflects config.toml hot-reloads
    // and env overrides, matching the upstream snapshot above (#834).
    let active_effort = crate::core::config::Config::load().proxy.resolved_effort();

    let body = serde_json::json!({
        "status": "running",
        "port": state.port,
        "upstreams": {
            "anthropic": up.anthropic.clone(),
            "openai": up.openai.clone(),
            "chatgpt": up.chatgpt.clone(),
            "gemini": up.gemini.clone(),
        },
        "requests_total": s.requests_total.load(Relaxed),
        "requests_compressed": s.requests_compressed.load(Relaxed),
        "tokens_saved": s.tokens_saved.load(Relaxed),
        "tokens_saved_estimated": true,
        "bytes_original": s.bytes_original.load(Relaxed),
        "bytes_compressed": s.bytes_compressed.load(Relaxed),
        "compression_ratio_pct": format!("{:.1}", s.compression_ratio()),
        "cache_safety": cache_safety::snapshot(),
        "effort": effort::snapshot(active_effort),
        "per_model": cost::snapshot(),
        "spend": {
            "source": "measured",
            "total_usd": spend_total,
            "per_model": spend,
            "note": "Actual provider bill: real model + billed tokens (incl. cache reads/writes & reasoning) read from upstream responses for proxy-routed clients."
        },
        "note": "Savings are request-side (tokens removed before forwarding); they do not subtract any re-reads the agent performs. Token figures are estimates; USD uses the shared model price table.",
        "introspect": {
            "total_requests_analyzed": i.total_requests.load(Relaxed),
            "total_system_prompt_tokens": i.total_system_prompt_tokens.load(Relaxed),
            "last_breakdown": last_breakdown,
        }
    });
    (StatusCode::OK, axum::Json(body))
}

#[allow(clippy::result_large_err)]
async fn proxy_auth_guard(
    req: axum::extract::Request,
    next: axum::middleware::Next,
    expected_token: String,
    require_token: bool,
) -> Result<Response, Response> {
    let path = req.uri().path();
    if path == "/health" {
        return Ok(next.run(req).await);
    }

    if let Some(auth) = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        && let Some(token) = auth.strip_prefix("Bearer ")
        && constant_time_eq(token.as_bytes(), expected_token.as_bytes())
    {
        return Ok(next.run(req).await);
    }

    // Accept provider API keys on provider routes (loopback-only, host_guard runs first).
    // AI tools like Claude Code send x-api-key, not Bearer tokens. Since the proxy
    // only binds to 127.0.0.1, the presence of a valid API key header is sufficient
    // to authenticate the request as coming from a local AI tool. Disabled when
    // `proxy_require_token` is set — strict hosts then require the Bearer token.
    if provider_key_fallback_allowed(
        require_token,
        has_provider_api_key(&req),
        is_provider_route(path),
    ) {
        return Ok(next.run(req).await);
    }

    let cfg = crate::core::config::Config::load();
    let hint = match cfg.proxy_enabled {
        Some(true) => {
            "lean-ctx proxy requires authentication. Use a Bearer token (LEAN_CTX_PROXY_TOKEN) or configure your AI tool's API key."
        }
        Some(false) => "lean-ctx proxy is disabled but still running. Run: lean-ctx proxy cleanup",
        None => {
            "lean-ctx proxy is not configured. Your AI tool's ANTHROPIC_BASE_URL may be pointing here by mistake. Fix: lean-ctx proxy cleanup  OR  lean-ctx proxy enable"
        }
    };

    let body = serde_json::json!({
        "type": "error",
        "error": {
            "type": "authentication_error",
            "message": format!("401 Unauthorized — {hint}")
        }
    });

    Err((StatusCode::UNAUTHORIZED, axum::Json(body)).into_response())
}

fn has_provider_api_key(req: &axum::extract::Request) -> bool {
    let headers = req.headers();
    // Provider-specific key headers: Anthropic `x-api-key`, Google
    // `x-goog-api-key`, Azure `api-key`. Any non-empty value authenticates.
    for key in ["x-api-key", "x-goog-api-key", "api-key"] {
        if headers
            .get(key)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| !v.trim().is_empty())
        {
            return true;
        }
    }
    // OpenAI-style `Authorization` auth. Accept ANY non-empty credential, not
    // just `Bearer sk-`/`gsk_`: OpenAI-*compatible* providers driven through
    // OpenCode/Codex (Azure, OpenRouter, Groq, vLLM/Ollama gateways, project &
    // service-account keys) issue keys that don't carry those prefixes. The proxy
    // binds to loopback only and never injects upstream credentials — it forwards
    // this header verbatim, so an invalid key is rejected by the real upstream,
    // never silently honoured. Gating provider routes on key *shape* only ever
    // produced false 401s for those clients (#362).
    if let Some(auth) = headers.get("authorization").and_then(|v| v.to_str().ok()) {
        let auth = auth.trim();
        let credential = auth
            .strip_prefix("Bearer ")
            .or_else(|| auth.strip_prefix("bearer "))
            .unwrap_or(auth)
            .trim();
        // Reject an empty value or a bare scheme keyword carrying no token.
        return !credential.is_empty() && !credential.eq_ignore_ascii_case("bearer");
    }
    false
}

fn is_provider_route(path: &str) -> bool {
    path.starts_with("/v1/")
        || path.starts_with("/v1beta/")
        || path.starts_with("/chat/completions")
        || path.starts_with("/responses")
        || path.starts_with("/messages")
        || path.starts_with("/backend-api")
}

/// Decides whether a request authenticates via a provider API key alone, without
/// the lean-ctx Bearer token. True only in the default, loopback-friendly mode
/// where a local AI tool's own provider key is accepted on a provider route. When
/// `require_token` is set (strict, shared-host mode) the fallback is disabled and
/// the Bearer token becomes mandatory. Pure, so the policy is unit-testable
/// without axum middleware plumbing.
fn provider_key_fallback_allowed(
    require_token: bool,
    has_provider_key: bool,
    is_provider_route: bool,
) -> bool {
    !require_token && has_provider_key && is_provider_route
}

/// Maps a bare provider endpoint to its canonical `/v1/...` form, preserving any
/// sub-path. Returns `None` when the path is already canonical or not a known
/// provider endpoint.
///
/// Some OpenAI-compatible clients treat the configured base URL as the API root
/// and append the bare endpoint, so they send `POST /responses` or
/// `/chat/completions` instead of `/v1/responses` — notably OpenCode via
/// `@ai-sdk/openai`, whose Responses-API requests land on `/responses`. The proxy
/// and every upstream only know the `/v1/...` paths, so an un-prefixed request
/// would 401 (not a provider route) and then 404 (no handler). (#353)
fn canonical_provider_path(path: &str) -> Option<String> {
    // Inverse case of the bare-endpoint rewrite below: the advertised
    // OPENAI_BASE_URL includes `/v1` (#366), so a client that treats the base URL
    // as an origin and appends `/v1/...` itself produces `/v1/v1/...`.
    if let Some(rest) = path.strip_prefix("/v1/v1/") {
        return Some(format!("/v1/{rest}"));
    }
    const BARE_TO_CANONICAL: &[(&str, &str, &str)] = &[
        ("/responses", "/v1/responses", "/responses/"),
        (
            "/chat/completions",
            "/v1/chat/completions",
            "/chat/completions/",
        ),
        ("/messages", "/v1/messages", "/messages/"),
    ];
    for (bare, canonical, bare_with_slash) in BARE_TO_CANONICAL {
        if path == *bare {
            return Some((*canonical).to_string());
        }
        if let Some(rest) = path.strip_prefix(bare_with_slash) {
            return Some(format!("{canonical}/{rest}"));
        }
    }
    None
}

/// Returns the canonicalized URI for a bare provider endpoint (query preserved),
/// or `None` when no rewrite is needed. Pure, so the rewrite is unit-testable
/// without constructing axum middleware plumbing.
fn normalized_provider_uri(uri: &axum::http::Uri) -> Option<axum::http::Uri> {
    let canonical = canonical_provider_path(uri.path())?;
    let new_path_and_query = match uri.query() {
        Some(q) => format!("{canonical}?{q}"),
        None => canonical,
    };
    new_path_and_query.parse::<axum::http::Uri>().ok()
}

/// Rewrites the request URI in place when it targets a bare provider endpoint, so
/// downstream auth (`is_provider_route`), routing and upstream forwarding all see
/// the canonical `/v1/...` path. (#353)
async fn normalize_provider_path(
    mut req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    if let Some(uri) = normalized_provider_uri(req.uri()) {
        *req.uri_mut() = uri;
    }
    next.run(req).await
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    if a.len() != b.len() {
        return false;
    }
    bool::from(a.ct_eq(b))
}

async fn host_guard(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Result<Response, StatusCode> {
    if let Some(host) = req.headers().get("host").and_then(|v| v.to_str().ok()) {
        let h = host.split(':').next().unwrap_or(host);
        if matches!(h, "127.0.0.1" | "localhost" | "[::1]") {
            return Ok(next.run(req).await);
        }
    }
    Err(StatusCode::FORBIDDEN)
}

async fn fallback_router(State(state): State<ProxyState>, req: Request<Body>) -> Response {
    let path = req.uri().path().to_string();

    if path.starts_with("/v1beta/models/") || path.starts_with("/v1/models/") {
        match google::handler(State(state), req).await {
            Ok(resp) => resp,
            Err(status) => Response::builder()
                .status(status)
                .body(Body::from("proxy error"))
                .expect("BUG: building error response with valid status should never fail"),
        }
    } else {
        let method = req.method().to_string();
        eprintln!("lean-ctx proxy: unmatched {method} {path}");
        Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::from(format!(
                "lean-ctx proxy: no handler for {method} {path}"
            )))
            .expect("BUG: building 404 response should never fail")
    }
}

#[cfg(test)]
mod auth_tests {
    use super::*;

    // P0-4 (#416): the proxy must never run unauthenticated — `None` means
    // "resolve the session token", not "no auth".
    #[test]
    fn effective_auth_token_never_yields_empty() {
        let _env = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().unwrap();
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", tmp.path());

        assert_eq!(effective_auth_token(Some("tok".into())), "tok");
        let auto = effective_auth_token(None);
        assert!(!auto.trim().is_empty(), "None must auto-resolve a token");
        let blank = effective_auth_token(Some("   ".into()));
        assert!(!blank.trim().is_empty(), "blank tokens must be replaced");

        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
    }

    #[test]
    fn is_provider_route_v1() {
        assert!(is_provider_route("/v1/chat/completions"));
        assert!(is_provider_route("/v1/messages"));
        assert!(is_provider_route("/v1/completions"));
    }

    #[test]
    fn is_provider_route_anthropic_subpaths() {
        assert!(is_provider_route("/v1/messages/count_tokens"));
        assert!(is_provider_route("/v1/messages/batches"));
        assert!(is_provider_route("/v1/messages/batches/batch_123"));
    }

    #[test]
    fn is_provider_route_v1beta() {
        assert!(is_provider_route("/v1beta/models"));
    }

    #[test]
    fn is_provider_route_chat() {
        assert!(is_provider_route("/chat/completions"));
    }

    #[test]
    fn is_provider_route_chatgpt_backend_api() {
        assert!(is_provider_route("/backend-api/codex/responses"));
        assert!(is_provider_route("/backend-api/codex/responses/resp_123"));
        assert!(is_provider_route("/backend-api/wham/session"));
        assert!(is_provider_route("/backend-api/ps/mcp"));
        assert!(is_provider_route("/backend-api/codex_apps"));
        assert!(is_provider_route("/backend-api/codex_apps/mcp"));
        assert!(is_provider_route("/backend-api/mcp/codex_apps"));
        assert!(is_provider_route("/backend-api/apps/codex_apps/mcp"));
    }

    #[test]
    fn is_provider_route_rejects_non_provider() {
        assert!(!is_provider_route("/health"));
        assert!(!is_provider_route("/api/v2/test"));
        assert!(!is_provider_route("/"));
    }

    fn build_request(headers: &[(&str, &str)], path: &str) -> axum::extract::Request {
        let mut builder = axum::http::Request::builder().uri(path);
        for (k, v) in headers {
            builder = builder.header(*k, *v);
        }
        builder.body(axum::body::Body::empty()).unwrap()
    }

    #[test]
    fn has_provider_api_key_x_api_key() {
        let req = build_request(&[("x-api-key", "sk-ant-abc123")], "/v1/messages");
        assert!(has_provider_api_key(&req));
    }

    #[test]
    fn has_provider_api_key_x_goog() {
        let req = build_request(&[("x-goog-api-key", "AIzaSyAbc")], "/v1beta/models");
        assert!(has_provider_api_key(&req));
    }

    #[test]
    fn has_provider_api_key_azure() {
        let req = build_request(&[("api-key", "deadbeef")], "/v1/completions");
        assert!(has_provider_api_key(&req));
    }

    #[test]
    fn has_provider_api_key_bearer_sk() {
        let req = build_request(
            &[("authorization", "Bearer sk-proj-abc123")],
            "/v1/chat/completions",
        );
        assert!(has_provider_api_key(&req));
    }

    #[test]
    fn has_provider_api_key_empty_rejected() {
        let req = build_request(&[("x-api-key", "  ")], "/v1/messages");
        assert!(!has_provider_api_key(&req));
    }

    #[test]
    fn has_provider_api_key_no_headers() {
        let req = build_request(&[], "/v1/messages");
        assert!(!has_provider_api_key(&req));
    }

    #[test]
    fn has_provider_api_key_accepts_non_sk_bearer() {
        // #362: OpenAI-*compatible* providers (Azure, OpenRouter, Groq, vLLM/
        // Ollama gateways, project/service keys) issue keys without the sk-/gsk_
        // prefix. OpenCode (@ai-sdk/openai) forwards them as `Bearer <key>`; they
        // must authenticate on a loopback provider route. The upstream validates
        // the real key — the proxy never injects one.
        for key in [
            "Bearer or-v1-9f8e7d6c", // OpenRouter
            "Bearer gsk_live_1234",  // (still works)
            "Bearer abc.def.ghi",    // gateway/service token
            "Bearer 0123456789",     // opaque
        ] {
            let req = build_request(&[("authorization", key)], "/v1/responses");
            assert!(
                has_provider_api_key(&req),
                "non-sk Bearer must count as a provider credential: {key}"
            );
        }
    }

    #[test]
    fn has_provider_api_key_empty_bearer_rejected() {
        // A blank credential — or a bare scheme word with no token (some HTTP
        // stacks trim trailing whitespace down to just "Bearer") — is not auth.
        for bad in ["Bearer    ", "", "Bearer", "bearer", "   "] {
            let req = build_request(&[("authorization", bad)], "/responses");
            assert!(
                !has_provider_api_key(&req),
                "blank/scheme-only Authorization must not authenticate: {bad:?}"
            );
        }
    }

    // --- #334: opt-in strict proxy auth (proxy_require_token) ---

    #[test]
    fn provider_key_fallback_allowed_in_default_mode() {
        // Default (require_token = false): a provider key on a provider route is
        // sufficient. This is what lets a local AI tool authenticate with its own
        // key and no lean-ctx Bearer token (the loopback-friendly behavior).
        assert!(provider_key_fallback_allowed(false, true, true));
    }

    #[test]
    fn provider_key_fallback_denied_in_strict_mode() {
        // Strict (require_token = true, e.g. shared/multi-user host): the
        // provider-key fallback is disabled, so even a valid provider key on a
        // provider route is not enough — the Bearer token becomes mandatory.
        assert!(!provider_key_fallback_allowed(true, true, true));
    }

    #[test]
    fn provider_key_fallback_requires_key_and_provider_route() {
        // The fallback never fires without a provider key, nor off a provider
        // route — regardless of mode.
        assert!(!provider_key_fallback_allowed(false, false, true));
        assert!(!provider_key_fallback_allowed(false, true, false));
        assert!(!provider_key_fallback_allowed(true, false, true));
    }

    #[test]
    fn proxy_require_token_defaults_off() {
        // The strict mode must be opt-in: a fresh config keeps the loopback
        // behavior so existing local setups (Claude Code, OpenCode, Codex) keep
        // working without a token.
        assert!(!crate::core::config::Config::default().proxy_require_token);
    }

    // --- #353: bare provider endpoints (OpenCode / @ai-sdk/openai) ---

    #[test]
    fn is_provider_route_bare_responses_and_messages() {
        // Clients that point their base URL at the proxy root (no `/v1`) send the
        // bare endpoint; auth must still recognise it as a provider route.
        assert!(is_provider_route("/responses"));
        assert!(is_provider_route("/responses/resp_123/input_items"));
        assert!(is_provider_route("/messages"));
    }

    #[test]
    fn canonical_provider_path_rewrites_bare_endpoints() {
        assert_eq!(
            canonical_provider_path("/responses").as_deref(),
            Some("/v1/responses")
        );
        assert_eq!(
            canonical_provider_path("/chat/completions").as_deref(),
            Some("/v1/chat/completions")
        );
        assert_eq!(
            canonical_provider_path("/messages").as_deref(),
            Some("/v1/messages")
        );
    }

    #[test]
    fn canonical_provider_path_preserves_subpaths() {
        assert_eq!(
            canonical_provider_path("/responses/resp_abc/cancel").as_deref(),
            Some("/v1/responses/resp_abc/cancel")
        );
        assert_eq!(
            canonical_provider_path("/messages/batches/batch_1").as_deref(),
            Some("/v1/messages/batches/batch_1")
        );
    }

    #[test]
    fn canonical_provider_path_ignores_already_canonical_and_unknown() {
        // Already canonical → no rewrite (avoids `/v1/v1/...`).
        assert_eq!(canonical_provider_path("/v1/responses"), None);
        assert_eq!(canonical_provider_path("/v1/chat/completions"), None);
        // Unrelated paths are untouched.
        assert_eq!(canonical_provider_path("/health"), None);
        assert_eq!(canonical_provider_path("/responsesx"), None);
        assert_eq!(canonical_provider_path("/"), None);
    }

    #[test]
    fn canonical_provider_path_collapses_double_v1_prefix() {
        // OPENAI_BASE_URL now advertises `/v1` (#366); a client treating it as an
        // origin and appending `/v1/...` itself produces a double prefix.
        assert_eq!(
            canonical_provider_path("/v1/v1/responses").as_deref(),
            Some("/v1/responses")
        );
        assert_eq!(
            canonical_provider_path("/v1/v1/chat/completions").as_deref(),
            Some("/v1/chat/completions")
        );
    }

    #[test]
    fn normalized_provider_uri_rewrites_path_and_preserves_query() {
        use axum::http::Uri;
        let uri: Uri = "/responses?stream=true".parse().unwrap();
        let rewritten = normalized_provider_uri(&uri).expect("bare /responses must rewrite");
        assert_eq!(rewritten.path(), "/v1/responses");
        assert_eq!(rewritten.query(), Some("stream=true"));
        assert_eq!(
            rewritten
                .path_and_query()
                .map(axum::http::uri::PathAndQuery::as_str),
            Some("/v1/responses?stream=true")
        );
    }

    #[test]
    fn normalized_provider_uri_noop_for_canonical() {
        use axum::http::Uri;
        let uri: Uri = "/v1/responses".parse().unwrap();
        assert!(normalized_provider_uri(&uri).is_none());
    }
}

#[cfg(test)]
mod upstream_tests {
    use super::*;

    fn upstreams_with_openai(openai: &str) -> Upstreams {
        Upstreams {
            anthropic: "https://api.anthropic.com".into(),
            openai: openai.into(),
            chatgpt: "https://chatgpt.com".into(),
            gemini: "https://generativelanguage.googleapis.com".into(),
        }
    }

    /// The #449 core wiring: provider handlers read the upstream per request from
    /// the watch channel, so a published change is served immediately, without
    /// rebuilding the `ProxyState`.
    #[tokio::test]
    async fn proxy_state_reads_upstream_live_from_watch() {
        let (tx, rx) =
            tokio::sync::watch::channel(Arc::new(upstreams_with_openai("https://old.example")));
        let state = ProxyState {
            client: reqwest::Client::new(),
            port: 0,
            stats: Arc::new(ProxyStats::default()),
            introspect: Arc::new(introspect::IntrospectState::default()),
            upstreams: rx,
        };
        assert_eq!(state.openai_upstream(), "https://old.example");

        tx.send(Arc::new(upstreams_with_openai("https://new.example")))
            .unwrap();
        assert_eq!(
            state.openai_upstream(),
            "https://new.example",
            "a live handler read must reflect the published change"
        );
        assert_eq!(state.upstream_snapshot().openai, "https://new.example");
    }

    /// End-to-end #449 repro (in-process, no network): a `config set`-style edit
    /// to config.toml is picked up by a *running* proxy's refresh task within the
    /// reload interval — without any restart. Before the fix this value stayed
    /// frozen at the start-time upstream forever.
    ///
    /// The process-global env lock is intentionally held across the polling
    /// `.await`s to keep `LEAN_CTX_*` isolated for the whole test; safe because
    /// each `#[tokio::test]` owns its current-thread runtime, so this std guard
    /// only makes *other* test threads wait — it can never deadlock this one.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn config_change_is_picked_up_live_without_restart() {
        use crate::core::config::Config;

        let _lock = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().unwrap();
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", tmp.path());
        // Isolate from a developer shell that exports the env override (#449),
        // and make the reload fast + deterministic.
        crate::test_env::remove_var("LEAN_CTX_OPENAI_UPSTREAM");
        crate::test_env::set_var("LEAN_CTX_PROXY_RELOAD_SECS", "1");

        // Start state: config.toml points OpenAI at a loopback upstream.
        Config::update_global(|c| {
            c.proxy.openai_upstream = Some("http://127.0.0.1:19101".into());
        })
        .unwrap();
        let initial = Config::load().proxy.resolve_all();
        assert_eq!(initial.openai, "http://127.0.0.1:19101");

        let (tx, rx) = tokio::sync::watch::channel(Arc::new(initial.clone()));
        spawn_upstream_refresh(tx, initial);

        // `lean-ctx config set proxy.openai_upstream …` (same safe write path).
        Config::update_global(|c| {
            c.proxy.openai_upstream = Some("http://127.0.0.1:19102".into());
        })
        .unwrap();

        // Poll the live value the handlers would read — no restart in between.
        let mut live = rx.borrow().openai.clone();
        for _ in 0..80 {
            if live == "http://127.0.0.1:19102" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            live = rx.borrow().openai.clone();
        }
        assert_eq!(
            live, "http://127.0.0.1:19102",
            "running proxy must serve the new config.toml upstream without a restart"
        );

        crate::test_env::remove_var("LEAN_CTX_PROXY_RELOAD_SECS");
        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
    }
}
