use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use axum::{
    extract::Json,
    extract::Query,
    extract::State,
    http::{header, Request, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};
use serde::Deserialize;
use serde_json::Value;
use tokio::time::{Duration, Instant};

use crate::engine::ContextEngine;
use crate::tools::LeanCtxServer;

#[cfg(feature = "team-server")]
pub mod team;

#[derive(Clone, Debug)]
pub struct HttpServerConfig {
    pub host: String,
    pub port: u16,
    pub project_root: PathBuf,
    pub auth_token: Option<String>,
    pub stateful_mode: bool,
    pub json_response: bool,
    pub disable_host_check: bool,
    pub allowed_hosts: Vec<String>,
    pub max_body_bytes: usize,
    pub max_concurrency: usize,
    pub max_rps: u32,
    pub rate_burst: u32,
    pub request_timeout_ms: u64,
}

impl Default for HttpServerConfig {
    fn default() -> Self {
        let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self {
            host: "127.0.0.1".to_string(),
            port: 8080,
            project_root,
            auth_token: None,
            stateful_mode: false,
            json_response: true,
            disable_host_check: false,
            allowed_hosts: Vec::new(),
            max_body_bytes: 2 * 1024 * 1024,
            max_concurrency: 32,
            max_rps: 50,
            rate_burst: 100,
            request_timeout_ms: 30_000,
        }
    }
}

impl HttpServerConfig {
    pub fn validate(&self) -> Result<()> {
        let host = self.host.trim().to_lowercase();
        let is_loopback = host == "127.0.0.1" || host == "localhost" || host == "::1";
        if !is_loopback && self.auth_token.as_deref().unwrap_or("").is_empty() {
            return Err(anyhow!(
                "Refusing to bind to host='{host}' without auth. Provide --auth-token (or bind to 127.0.0.1)."
            ));
        }
        Ok(())
    }

    fn mcp_http_config(&self) -> StreamableHttpServerConfig {
        let mut cfg = StreamableHttpServerConfig::default()
            .with_stateful_mode(self.stateful_mode)
            .with_json_response(self.json_response);

        if self.disable_host_check {
            cfg = cfg.disable_allowed_hosts();
            return cfg;
        }

        if !self.allowed_hosts.is_empty() {
            cfg = cfg.with_allowed_hosts(self.allowed_hosts.clone());
            return cfg;
        }

        // Keep rmcp's secure loopback defaults; also allow the configured host (if it's loopback).
        let host = self.host.trim();
        if host == "127.0.0.1" || host == "localhost" || host == "::1" {
            cfg.allowed_hosts.push(host.to_string());
        }

        cfg
    }
}

#[derive(Clone)]
struct AppState {
    token: Option<String>,
    concurrency: Arc<tokio::sync::Semaphore>,
    rate: Arc<RateLimiter>,
    engine: Arc<ContextEngine>,
    timeout: Duration,
}

#[derive(Debug)]
struct RateLimiter {
    max_rps: f64,
    burst: f64,
    state: tokio::sync::Mutex<RateState>,
}

#[derive(Debug, Clone, Copy)]
struct RateState {
    tokens: f64,
    last: Instant,
}

impl RateLimiter {
    fn new(max_rps: u32, burst: u32) -> Self {
        let now = Instant::now();
        Self {
            max_rps: (max_rps.max(1)) as f64,
            burst: (burst.max(1)) as f64,
            state: tokio::sync::Mutex::new(RateState {
                tokens: (burst.max(1)) as f64,
                last: now,
            }),
        }
    }

    async fn allow(&self) -> bool {
        let mut s = self.state.lock().await;
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(s.last);
        let refill = elapsed.as_secs_f64() * self.max_rps;
        s.tokens = (s.tokens + refill).min(self.burst);
        s.last = now;
        if s.tokens >= 1.0 {
            s.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

async fn auth_middleware(
    State(state): State<AppState>,
    req: Request<axum::body::Body>,
    next: Next,
) -> Response {
    if state.token.is_none() {
        return next.run(req).await;
    }

    if req.uri().path() == "/health" {
        return next.run(req).await;
    }

    let expected = state.token.as_deref().unwrap_or("");
    let Some(h) = req.headers().get(header::AUTHORIZATION) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Ok(s) = h.to_str() else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Some(token) = s
        .strip_prefix("Bearer ")
        .or_else(|| s.strip_prefix("bearer "))
    else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if !constant_time_eq(token.as_bytes(), expected.as_bytes()) {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    next.run(req).await
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

async fn rate_limit_middleware(
    State(state): State<AppState>,
    req: Request<axum::body::Body>,
    next: Next,
) -> Response {
    if req.uri().path() == "/health" {
        return next.run(req).await;
    }
    if !state.rate.allow().await {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    next.run(req).await
}

async fn concurrency_middleware(
    State(state): State<AppState>,
    req: Request<axum::body::Body>,
    next: Next,
) -> Response {
    if req.uri().path() == "/health" {
        return next.run(req).await;
    }
    let Ok(permit) = state.concurrency.clone().try_acquire_owned() else {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    };
    let resp = next.run(req).await;
    drop(permit);
    resp
}

async fn health() -> impl IntoResponse {
    (StatusCode::OK, "ok\n")
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToolCallBody {
    name: String,
    #[serde(default)]
    arguments: Option<Value>,
}

async fn v1_manifest(State(state): State<AppState>) -> impl IntoResponse {
    let v = state.engine.manifest();
    (StatusCode::OK, Json(v))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToolsQuery {
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
}

async fn v1_tools(State(state): State<AppState>, Query(q): Query<ToolsQuery>) -> impl IntoResponse {
    let v = state.engine.manifest();
    let tools = v
        .get("tools")
        .and_then(|t| t.get("granular"))
        .cloned()
        .unwrap_or(Value::Array(vec![]));

    let all = tools.as_array().cloned().unwrap_or_default();
    let total = all.len();
    let offset = q.offset.unwrap_or(0).min(total);
    let limit = q.limit.unwrap_or(200).min(500);
    let page = all.into_iter().skip(offset).take(limit).collect::<Vec<_>>();

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "tools": page,
            "total": total,
            "offset": offset,
            "limit": limit,
        })),
    )
}

async fn v1_tool_call(
    State(state): State<AppState>,
    Json(body): Json<ToolCallBody>,
) -> impl IntoResponse {
    match tokio::time::timeout(
        state.timeout,
        state.engine.call_tool_value(&body.name, body.arguments),
    )
    .await
    {
        Ok(Ok(v)) => (StatusCode::OK, Json(serde_json::json!({ "result": v }))).into_response(),
        Ok(Err(e)) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
        Err(_) => (
            StatusCode::GATEWAY_TIMEOUT,
            Json(serde_json::json!({ "error": "request_timeout" })),
        )
            .into_response(),
    }
}

pub async fn serve(cfg: HttpServerConfig) -> Result<()> {
    cfg.validate()?;

    let addr: SocketAddr = format!("{}:{}", cfg.host, cfg.port)
        .parse()
        .context("invalid host/port")?;

    let project_root = cfg.project_root.to_string_lossy().to_string();
    let base = LeanCtxServer::new_with_project_root(Some(&project_root));
    let engine = Arc::new(ContextEngine::from_server(base.clone()));

    let service_factory = move || Ok(base.clone());
    let mcp_http = StreamableHttpService::new(
        service_factory,
        Arc::new(
            rmcp::transport::streamable_http_server::session::local::LocalSessionManager::default(),
        ),
        cfg.mcp_http_config(),
    );

    let state = AppState {
        token: cfg.auth_token.clone().filter(|t| !t.is_empty()),
        concurrency: Arc::new(tokio::sync::Semaphore::new(cfg.max_concurrency.max(1))),
        rate: Arc::new(RateLimiter::new(cfg.max_rps, cfg.rate_burst)),
        engine,
        timeout: Duration::from_millis(cfg.request_timeout_ms.max(1)),
    };

    let app = Router::new()
        .route("/health", get(health))
        .route("/v1/manifest", get(v1_manifest))
        .route("/v1/tools", get(v1_tools))
        .route("/v1/tools/call", axum::routing::post(v1_tool_call))
        .fallback_service(mcp_http)
        .layer(axum::extract::DefaultBodyLimit::max(cfg.max_body_bytes))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            rate_limit_middleware,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            concurrency_middleware,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind {addr}"))?;

    tracing::info!(
        "lean-ctx Streamable HTTP server listening on http://{addr} (project_root={})",
        cfg.project_root.display()
    );

    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .context("http server")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};
    use serde_json::json;
    use tower::ServiceExt;

    #[tokio::test]
    async fn auth_token_blocks_requests_without_bearer_header() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root_str = dir.path().to_string_lossy().to_string();
        let base = LeanCtxServer::new_with_project_root(Some(&root_str));
        let service_factory = move || Ok(base.clone());
        let cfg = StreamableHttpServerConfig::default()
            .with_stateful_mode(false)
            .with_json_response(true);

        let mcp_http = StreamableHttpService::new(
            service_factory,
            Arc::new(
                rmcp::transport::streamable_http_server::session::local::LocalSessionManager::default(),
            ),
            cfg,
        );

        let state = AppState {
            token: Some("secret".to_string()),
            concurrency: Arc::new(tokio::sync::Semaphore::new(4)),
            rate: Arc::new(RateLimiter::new(50, 100)),
            engine: Arc::new(ContextEngine::from_server(
                LeanCtxServer::new_with_project_root(Some(&root_str)),
            )),
            timeout: Duration::from_millis(30_000),
        };

        let app = Router::new()
            .fallback_service(mcp_http)
            .layer(middleware::from_fn_with_state(
                state.clone(),
                auth_middleware,
            ))
            .with_state(state);

        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list",
            "params": {}
        })
        .to_string();

        let req = Request::builder()
            .method("POST")
            .uri("/")
            .header("Host", "localhost")
            .header("Accept", "application/json, text/event-stream")
            .header("Content-Type", "application/json")
            .body(Body::from(body))
            .expect("request");

        let resp = app.clone().oneshot(req).await.expect("resp");
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn rate_limit_returns_429_when_exhausted() {
        let state = AppState {
            token: None,
            concurrency: Arc::new(tokio::sync::Semaphore::new(16)),
            rate: Arc::new(RateLimiter::new(1, 1)),
            engine: Arc::new(ContextEngine::new()),
            timeout: Duration::from_millis(30_000),
        };

        let app = Router::new()
            .route("/limited", get(|| async { (StatusCode::OK, "ok\n") }))
            .layer(middleware::from_fn_with_state(
                state.clone(),
                rate_limit_middleware,
            ))
            .with_state(state);

        let req1 = Request::builder()
            .method("GET")
            .uri("/limited")
            .header("Host", "localhost")
            .body(Body::empty())
            .expect("req1");
        let resp1 = app.clone().oneshot(req1).await.expect("resp1");
        assert_eq!(resp1.status(), StatusCode::OK);

        let req2 = Request::builder()
            .method("GET")
            .uri("/limited")
            .header("Host", "localhost")
            .body(Body::empty())
            .expect("req2");
        let resp2 = app.clone().oneshot(req2).await.expect("resp2");
        assert_eq!(resp2.status(), StatusCode::TOO_MANY_REQUESTS);
    }
}
