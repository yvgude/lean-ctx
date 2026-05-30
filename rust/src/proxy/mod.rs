pub mod anthropic;
pub mod compress;
pub mod forward;
pub mod google;
pub mod history_prune;
pub mod introspect;
pub mod metrics;
pub mod openai;

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
    routing::{any, get},
    Router,
};

#[derive(Clone)]
pub struct ProxyState {
    pub client: reqwest::Client,
    pub port: u16,
    pub stats: Arc<ProxyStats>,
    pub introspect: Arc<introspect::IntrospectState>,
    pub anthropic_upstream: String,
    pub openai_upstream: String,
    pub gemini_upstream: String,
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
    pub fn record_request(&self) {
        self.requests_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_compression(&self, original: usize, compressed: usize) {
        self.requests_compressed.fetch_add(1, Ordering::Relaxed);
        self.bytes_original
            .fetch_add(original as u64, Ordering::Relaxed);
        self.bytes_compressed
            .fetch_add(compressed as u64, Ordering::Relaxed);
        let saved_tokens = (original.saturating_sub(compressed) / 4) as u64;
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

pub async fn start_proxy(port: u16) -> anyhow::Result<()> {
    let token = crate::core::session_token::resolve_proxy_token("LEAN_CTX_PROXY_TOKEN");
    start_proxy_with_token(port, Some(token)).await
}

pub async fn start_proxy_with_token(port: u16, auth_token: Option<String>) -> anyhow::Result<()> {
    use crate::core::config::{Config, ProxyProvider};

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_mins(2))
        .build()?;

    let cfg = Config::load();
    let anthropic_upstream = cfg.proxy.resolve_upstream(ProxyProvider::Anthropic);
    let openai_upstream = cfg.proxy.resolve_upstream(ProxyProvider::OpenAi);
    let gemini_upstream = cfg.proxy.resolve_upstream(ProxyProvider::Gemini);

    let state = ProxyState {
        client,
        port,
        stats: Arc::new(ProxyStats::default()),
        introspect: Arc::new(introspect::IntrospectState::default()),
        anthropic_upstream: anthropic_upstream.clone(),
        openai_upstream: openai_upstream.clone(),
        gemini_upstream: gemini_upstream.clone(),
    };

    let mut app = Router::new()
        .route("/health", get(health))
        .route("/status", get(status_handler))
        .route("/v1/messages", any(anthropic::handler))
        .route("/v1/messages/count_tokens", any(anthropic::handler))
        .route("/v1/chat/completions", any(openai::handler))
        .route("/v1/references/{id}", get(v1_resolve_reference))
        .fallback(fallback_router)
        .layer(axum::middleware::from_fn(host_guard))
        .with_state(state);

    if let Some(ref token) = auth_token {
        let expected = token.clone();
        app = app.layer(axum::middleware::from_fn(move |req, next| {
            let expected = expected.clone();
            proxy_auth_guard(req, next, expected)
        }));
    }

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    if auth_token.is_some() {
        println!("lean-ctx proxy listening on http://{addr} (token auth enabled)");
    } else {
        println!("lean-ctx proxy listening on http://{addr} (no auth — set LEAN_CTX_PROXY_TOKEN to enable)");
    }
    println!("  Anthropic: POST /v1/messages → {anthropic_upstream}");
    println!("  OpenAI:    POST /v1/chat/completions → {openai_upstream}");
    println!("  Gemini:    POST /v1beta/models/... → {gemini_upstream}");

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

    let body = serde_json::json!({
        "status": "running",
        "port": state.port,
        "requests_total": s.requests_total.load(Relaxed),
        "requests_compressed": s.requests_compressed.load(Relaxed),
        "tokens_saved": s.tokens_saved.load(Relaxed),
        "bytes_original": s.bytes_original.load(Relaxed),
        "bytes_compressed": s.bytes_compressed.load(Relaxed),
        "compression_ratio_pct": format!("{:.1}", s.compression_ratio()),
        "introspect": {
            "total_requests_analyzed": i.total_requests.load(Relaxed),
            "total_system_prompt_tokens": i.total_system_prompt_tokens.load(Relaxed),
            "last_breakdown": last_breakdown,
        }
    });
    (StatusCode::OK, axum::Json(body))
}

async fn proxy_auth_guard(
    req: axum::extract::Request,
    next: axum::middleware::Next,
    expected_token: String,
) -> Result<Response, Response> {
    let path = req.uri().path();
    if path == "/health" {
        return Ok(next.run(req).await);
    }

    // Accept Bearer token (lean-ctx session token)
    if let Some(auth) = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
    {
        if let Some(token) = auth.strip_prefix("Bearer ") {
            if constant_time_eq(token.as_bytes(), expected_token.as_bytes()) {
                return Ok(next.run(req).await);
            }
        }
    }

    // Accept provider API keys on provider routes (loopback-only, host_guard runs first).
    // AI tools like Claude Code send x-api-key, not Bearer tokens. Since the proxy
    // only binds to 127.0.0.1, the presence of a valid API key header is sufficient
    // to authenticate the request as coming from a local AI tool.
    if has_provider_api_key(&req) && is_provider_route(path) {
        return Ok(next.run(req).await);
    }

    let cfg = crate::core::config::Config::load();
    let hint = match cfg.proxy_enabled {
        Some(true) => "lean-ctx proxy requires authentication. Use a Bearer token (LEAN_CTX_PROXY_TOKEN) or configure your AI tool's API key.",
        Some(false) => "lean-ctx proxy is disabled but still running. Run: lean-ctx proxy cleanup",
        None => "lean-ctx proxy is not configured. Your AI tool's ANTHROPIC_BASE_URL may be pointing here by mistake. Fix: lean-ctx proxy cleanup  OR  lean-ctx proxy enable",
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
    for key in ["x-api-key", "x-goog-api-key", "api-key"] {
        if headers
            .get(key)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| !v.trim().is_empty())
        {
            return true;
        }
    }
    if let Some(auth) = headers.get("authorization").and_then(|v| v.to_str().ok()) {
        if auth.starts_with("Bearer sk-") || auth.starts_with("Bearer gsk_") {
            return true;
        }
    }
    false
}

fn is_provider_route(path: &str) -> bool {
    path.starts_with("/v1/")
        || path.starts_with("/v1beta/")
        || path.starts_with("/chat/completions")
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

    #[test]
    fn is_provider_route_v1() {
        assert!(is_provider_route("/v1/chat/completions"));
        assert!(is_provider_route("/v1/messages"));
        assert!(is_provider_route("/v1/completions"));
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
    fn has_provider_api_key_regular_bearer_rejected() {
        let req = build_request(
            &[("authorization", "Bearer some-session-token")],
            "/v1/chat",
        );
        assert!(
            !has_provider_api_key(&req),
            "non-sk/gsk Bearer should not count as provider key"
        );
    }
}
