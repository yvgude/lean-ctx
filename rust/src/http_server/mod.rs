use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use axum::{
    Router,
    extract::Json,
    extract::Query,
    extract::State,
    http::{Request, StatusCode, header},
    middleware::{self, Next},
    response::sse::{Event as SseEvent, KeepAlive, Sse},
    response::{IntoResponse, Response},
    routing::get,
};
use futures::Stream;
use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::broadcast;
use tokio::time::{Duration, Instant};

use crate::core::context_os::ContextOsMetrics;
use crate::engine::ContextEngine;
use crate::tools::LeanCtxServer;

pub mod context_views;
pub mod roi_webhook;
pub mod savings_ingest;
pub mod savings_summary;
pub mod team;
pub mod team_billing;

/// Wrapper stream that calls `record_sse_disconnect` on drop.
use std::pin::Pin;

pub(crate) struct SseDisconnectGuard<I> {
    pub(crate) inner: Pin<Box<dyn Stream<Item = I> + Send>>,
    pub(crate) metrics: Arc<ContextOsMetrics>,
}

impl<I> Stream for SseDisconnectGuard<I> {
    type Item = I;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(cx)
    }
}

impl<I> Drop for SseDisconnectGuard<I> {
    fn drop(&mut self) {
        self.metrics.record_sse_disconnect();
    }
}

const MAX_ID_LEN: usize = 64;

fn sanitize_id(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "default".to_string();
    }
    let cleaned: String = trimmed
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_' || *c == '.')
        .take(MAX_ID_LEN)
        .collect();
    if cleaned.is_empty() {
        "default".to_string()
    } else {
        cleaned
    }
}

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

    #[must_use]
    pub fn effective_auth_token(&self) -> Option<String> {
        if let Some(ref token) = self.auth_token
            && !token.is_empty()
        {
            return Some(token.clone());
        }
        let host = self.host.trim().to_lowercase();
        let is_loopback = host == "127.0.0.1" || host == "localhost" || host == "::1";
        if is_loopback {
            let auto_token = crate::core::session_token::generate_token();
            eprintln!(
                "[lean-ctx] Auto-generated auth token for loopback: {auto_token}\n\
                 Pass as Bearer token or set --auth-token explicitly."
            );
            Some(auto_token)
        } else {
            None
        }
    }

    fn mcp_http_config(&self) -> StreamableHttpServerConfig {
        let mut cfg = StreamableHttpServerConfig::default()
            .with_stateful_mode(self.stateful_mode)
            .with_json_response(self.json_response);

        if self.disable_host_check {
            tracing::warn!(
                "⚠ --disable-host-check is active: DNS rebinding protection is OFF. \
                 Do NOT use this in production or on non-loopback interfaces."
            );
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
    project_root: String,
    timeout: Duration,
    server: LeanCtxServer,
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
            max_rps: f64::from(max_rps.max(1)),
            burst: f64::from(burst.max(1)),
            state: tokio::sync::Mutex::new(RateState {
                tokens: f64::from(burst.max(1)),
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
        return json_error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "missing Authorization header",
        );
    };
    let Ok(s) = h.to_str() else {
        return json_error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "malformed Authorization header",
        );
    };
    let Some(token) = s
        .strip_prefix("Bearer ")
        .or_else(|| s.strip_prefix("bearer "))
    else {
        return json_error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "Authorization must use the Bearer scheme",
        );
    };
    if !constant_time_eq(token.as_bytes(), expected.as_bytes()) {
        return json_error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "invalid bearer token",
        );
    }

    next.run(req).await
}

/// Structured REST error envelope: `{ "error": <human message>, "error_code": <stable code> }`.
///
/// `error_code` is the stable, machine-readable string SDKs switch on; `error` carries the
/// human-facing message. Used for every REST (non-A2A) error so clients branch on a code
/// instead of parsing prose. The A2A JSON-RPC surface keeps its own `-32xxx` envelope.
pub(crate) fn json_error(status: StatusCode, error_code: &str, message: &str) -> Response {
    (
        status,
        Json(serde_json::json!({ "error": message, "error_code": error_code })),
    )
        .into_response()
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    if a.len() != b.len() {
        return false;
    }
    bool::from(a.ct_eq(b))
}

async fn rate_limit_middleware(
    State(state): State<AppState>,
    req: Request<axum::body::Body>,
    next: Next,
) -> Response {
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

async fn v1_shutdown() -> impl IntoResponse {
    tokio::spawn(async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        std::process::exit(0);
    });
    (StatusCode::OK, "shutting down\n")
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IndexEnsureBody {
    root: String,
    #[serde(default)]
    extra_roots: Vec<String>,
}

/// Daemon-side index delegation (#460). A thin-client session POSTs the repo it
/// needs warmed and the daemon — the single long-lived indexer — builds it once
/// in the background (deduped per root). Every other session for the same root
/// then load-shares the on-disk result via the `graph-idx`/`bm25-idx`
/// cross-process locks instead of running its own scan, so N concurrent sessions
/// cost ~one index pass machine-wide instead of N. Returns immediately; the
/// build runs in the orchestrator's own worker thread.
async fn v1_index_ensure(Json(body): Json<IndexEnsureBody>) -> impl IntoResponse {
    if body.root.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "root is required\n");
    }
    let root = body.root;
    let extra = body.extra_roots;
    // Indexes are SQLite-backed — no explicit build trigger needed.
    let _ = (root, extra);
    (StatusCode::OK, "{\"status\":\"ok\"}\n")
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToolCallBody {
    name: String,
    #[serde(default)]
    arguments: Option<Value>,
    #[serde(default)]
    _workspace_id: Option<String>,
    #[serde(default)]
    _channel_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EventsQuery {
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    channel_id: Option<String>,
    #[serde(default)]
    since: Option<i64>,
    #[serde(default)]
    limit: Option<usize>,
    /// Comma-separated event kind filter (e.g. `tool_call,session_start`).
    /// When set, only matching events are delivered via SSE.
    #[serde(default)]
    kind: Option<String>,
}

async fn v1_manifest(State(state): State<AppState>) -> impl IntoResponse {
    let _ = state;
    let v = crate::core::mcp_manifest::manifest_value();
    (StatusCode::OK, Json(v))
}

/// `GET /v1/capabilities` — discovery document describing what this instance
/// supports (presets, tools, read modes, features, extensions, contract
/// versions). See `docs/contracts/capabilities-contract-v1.md`.
async fn v1_capabilities(State(state): State<AppState>) -> impl IntoResponse {
    let _ = state;
    (
        StatusCode::OK,
        Json(crate::core::server_capabilities::capabilities_value()),
    )
}

/// `GET /v1/openapi.json` — `OpenAPI` 3.0 document for the public `/v1` surface,
/// generated from the in-code endpoint inventory (`core::openapi`).
async fn v1_openapi(State(state): State<AppState>) -> impl IntoResponse {
    let _ = state;
    (StatusCode::OK, Json(crate::core::openapi::openapi_value()))
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
    let _ = state;
    let v = crate::core::mcp_manifest::manifest_value();
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
    let engine = ContextEngine::from_server(state.server.clone());
    match tokio::time::timeout(
        state.timeout,
        engine.call_tool_value(&body.name, body.arguments),
    )
    .await
    {
        Ok(Ok(v)) => (StatusCode::OK, Json(serde_json::json!({ "result": v }))).into_response(),
        Ok(Err(e)) => {
            tracing::warn!("tool call error: {e}");
            json_error(
                StatusCode::BAD_REQUEST,
                "tool_error",
                "tool execution failed",
            )
        }
        Err(_) => json_error(
            StatusCode::GATEWAY_TIMEOUT,
            "request_timeout",
            "tool call timed out",
        ),
    }
}

async fn v1_events(
    State(state): State<AppState>,
    Query(q): Query<EventsQuery>,
) -> Sse<impl Stream<Item = Result<SseEvent, std::convert::Infallible>>> {
    use crate::core::context_os::{ContextEventV1, RedactionLevel, redact_event_payload};

    let ws = sanitize_id(&q.workspace_id.unwrap_or_else(|| "default".to_string()));
    let ch = sanitize_id(&q.channel_id.unwrap_or_else(|| "default".to_string()));
    let _ = &state.project_root;
    let since = q.since.unwrap_or(0);
    let limit = q.limit.unwrap_or(200).min(1000);
    let redaction = RedactionLevel::RefsOnly;

    let kind_filter: Option<Vec<String>> = q
        .kind
        .as_deref()
        .map(|k| k.split(',').map(|s| s.trim().to_string()).collect());

    let rt = crate::core::context_os::runtime();
    let replay = rt.bus.read(&ws, &ch, since, limit);

    let replay = if let Some(ref kinds) = kind_filter {
        replay
            .into_iter()
            .filter(|ev| kinds.contains(&ev.kind))
            .collect()
    } else {
        replay
    };

    let rx = if let Some(ref kinds) = kind_filter {
        let kind_refs: Vec<&str> = kinds.iter().map(String::as_str).collect();
        let filter = crate::core::context_os::TopicFilter::kinds(&kind_refs);
        if let Some(sub) = rt.bus.subscribe_filtered(&ws, &ch, filter) {
            crate::core::context_os::SubscriptionKind::Filtered(sub)
        } else {
            tracing::warn!("SSE subscriber limit reached for {ws}/{ch}");
            let (_, rx) = broadcast::channel::<ContextEventV1>(1);
            crate::core::context_os::SubscriptionKind::Unfiltered(rx)
        }
    } else if let Some(sub) = rt.bus.subscribe(&ws, &ch) {
        crate::core::context_os::SubscriptionKind::Unfiltered(sub)
    } else {
        tracing::warn!("SSE subscriber limit reached for {ws}/{ch}");
        let (_, rx) = broadcast::channel::<ContextEventV1>(1);
        crate::core::context_os::SubscriptionKind::Unfiltered(rx)
    };

    rt.metrics.record_sse_connect();
    rt.metrics.record_events_replayed(replay.len() as u64);
    rt.metrics.record_workspace_active(&ws);

    let bus = rt.bus.clone();
    let metrics = rt.metrics.clone();
    let pending: std::collections::VecDeque<ContextEventV1> = replay.into();

    let stream = futures::stream::unfold(
        (
            pending,
            rx,
            ws.clone(),
            ch.clone(),
            since,
            redaction,
            bus,
            metrics,
        ),
        |(mut pending, mut rx, ws, ch, mut last_id, redaction, bus, metrics)| async move {
            if let Some(mut ev) = pending.pop_front() {
                last_id = ev.id;
                redact_event_payload(&mut ev, redaction);
                let data = serde_json::to_string(&ev).unwrap_or_else(|_| "{}".to_string());
                let evt = SseEvent::default()
                    .id(ev.id.to_string())
                    .event(ev.kind)
                    .data(data);
                return Some((
                    Ok(evt),
                    (pending, rx, ws, ch, last_id, redaction, bus, metrics),
                ));
            }

            loop {
                match rx.recv().await {
                    Ok(mut ev) if ev.id > last_id => {
                        last_id = ev.id;
                        redact_event_payload(&mut ev, redaction);
                        let data = serde_json::to_string(&ev).unwrap_or_else(|_| "{}".to_string());
                        let evt = SseEvent::default()
                            .id(ev.id.to_string())
                            .event(ev.kind)
                            .data(data);
                        return Some((
                            Ok(evt),
                            (pending, rx, ws, ch, last_id, redaction, bus, metrics),
                        ));
                    }
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Closed) => return None,
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        let missed = bus.read(&ws, &ch, last_id, skipped as usize);
                        metrics.record_events_replayed(missed.len() as u64);
                        for ev in missed {
                            last_id = last_id.max(ev.id);
                            pending.push_back(ev);
                        }
                    }
                }
            }
        },
    );

    let metrics_ref = rt.metrics.clone();
    let guarded = SseDisconnectGuard {
        inner: Box::pin(stream),
        metrics: metrics_ref,
    };

    Sse::new(guarded).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}

#[derive(Debug, Deserialize)]
struct AuditEventsQuery {
    #[serde(default = "default_audit_limit")]
    limit: usize,
}

fn default_audit_limit() -> usize {
    100
}

async fn v1_audit_events(Query(q): Query<AuditEventsQuery>) -> impl IntoResponse {
    let capped = q.limit.min(1000);
    let boundary_events = crate::core::memory_boundary::load_audit_events(capped);
    let trail_events = crate::core::audit_trail::load_recent(capped);

    Json(serde_json::json!({
        "cross_project_events": boundary_events,
        "audit_trail": trail_events,
    }))
}

async fn v1_metrics(State(_state): State<AppState>) -> impl IntoResponse {
    let rt = crate::core::context_os::runtime();
    let snap = rt.metrics.snapshot();
    (
        StatusCode::OK,
        Json(serde_json::to_value(snap).unwrap_or_default()),
    )
}

const MAX_HANDOFF_PAYLOAD_BYTES: usize = 1_000_000;
const MAX_HANDOFF_FILES: usize = 50;

async fn v1_a2a_handoff(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let envelope = match crate::core::a2a_transport::parse_envelope(
        &serde_json::to_string(&body).unwrap_or_default(),
    ) {
        Ok(env) => env,
        Err(e) => {
            tracing::warn!("a2a handoff parse error: {e}");
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "invalid_envelope"})),
            );
        }
    };

    if envelope.payload_json.len() > MAX_HANDOFF_PAYLOAD_BYTES {
        tracing::warn!(
            "a2a handoff payload too large: {} bytes (limit {MAX_HANDOFF_PAYLOAD_BYTES})",
            envelope.payload_json.len()
        );
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(serde_json::json!({"error": "payload_too_large"})),
        );
    }

    let rt = crate::core::context_os::runtime();
    rt.bus.append(
        &state.project_root,
        "a2a",
        &crate::core::context_os::ContextEventKindV1::SessionMutated,
        Some(&envelope.sender.agent_id),
        serde_json::json!({
            "type": "handoff_received",
            "content_type": format!("{:?}", envelope.content_type),
            "sender": envelope.sender.agent_id,
            "payload_size": envelope.payload_json.len(),
        }),
    );

    match envelope.content_type {
        crate::core::a2a_transport::TransportContentType::ContextPackage => {
            let dir = std::path::Path::new(&state.project_root)
                .join(".lean-ctx")
                .join("handoffs")
                .join("packages");
            let _ = std::fs::create_dir_all(&dir);
            evict_oldest_files(&dir, MAX_HANDOFF_FILES);
            let out = dir.join(format!(
                "ctx-{}.{}",
                chrono::Utc::now().format("%Y%m%d_%H%M%S"),
                crate::core::contracts::PACKAGE_EXTENSION
            ));
            if let Err(e) = std::fs::write(&out, &envelope.payload_json) {
                tracing::error!("a2a handoff write failed: {e}");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": "write_failed"})),
                );
            }
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "status": "received",
                    "content_type": "context_package",
                })),
            )
        }
        crate::core::a2a_transport::TransportContentType::HandoffBundle => {
            // Signature enforcement at the network boundary (GL #465): a
            // payload that is not a parseable bundle, or whose signature
            // material does not verify, is rejected fail-closed before it
            // ever touches disk. Legacy unsigned bundles are stored with the
            // status surfaced so the importer can warn.
            let bundle =
                match crate::core::handoff_transfer_bundle::parse_bundle_v1(&envelope.payload_json)
                {
                    Ok(b) => b,
                    Err(e) => {
                        tracing::warn!("a2a handoff rejected: not a valid bundle: {e}");
                        return (
                            StatusCode::BAD_REQUEST,
                            Json(serde_json::json!({"error": "invalid_bundle"})),
                        );
                    }
                };
            let signature =
                match crate::core::handoff_transfer_bundle::check_bundle_signature(&bundle) {
                    crate::core::handoff_transfer_bundle::BundleSignatureStatus::Invalid(
                        reason,
                    ) => {
                        tracing::warn!("a2a handoff rejected: signature invalid: {reason}");
                        crate::core::audit_trail::record(
                            crate::core::audit_trail::AuditEntryData {
                                agent_id: envelope.sender.agent_id.clone(),
                                tool: "http:/v1/a2a/handoff".to_string(),
                                action: Some("import_signature_invalid".to_string()),
                                input_hash: String::new(),
                                output_tokens: 0,
                                role: crate::core::roles::active_role_name(),
                                event_type:
                                    crate::core::audit_trail::AuditEventType::SecurityViolation,
                            },
                        );
                        return (
                            StatusCode::BAD_REQUEST,
                            Json(serde_json::json!({"error": "invalid_signature"})),
                        );
                    }
                    crate::core::handoff_transfer_bundle::BundleSignatureStatus::Verified(
                        signer,
                    ) => {
                        serde_json::json!({"status": "verified", "signer": signer})
                    }
                    crate::core::handoff_transfer_bundle::BundleSignatureStatus::Unsigned => {
                        serde_json::json!({"status": "unsigned"})
                    }
                };

            let dir = std::path::Path::new(&state.project_root)
                .join(".lean-ctx")
                .join("handoffs");
            let _ = std::fs::create_dir_all(&dir);
            evict_oldest_files(&dir, MAX_HANDOFF_FILES);
            let out = dir.join(format!(
                "received-{}.json",
                chrono::Utc::now().format("%Y%m%d_%H%M%S")
            ));
            if let Err(e) = std::fs::write(&out, &envelope.payload_json) {
                tracing::error!("a2a handoff write failed: {e}");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": "write_failed"})),
                );
            }
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "status": "received",
                    "content_type": "handoff_bundle",
                    "signature": signature,
                })),
            )
        }
        _ => (
            StatusCode::OK,
            Json(serde_json::json!({
                "status": "received",
                "content_type": format!("{:?}", envelope.content_type),
            })),
        ),
    }
}

fn evict_oldest_files(dir: &std::path::Path, max_files: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(std::time::SystemTime, std::path::PathBuf)> = entries
        .filter_map(|e| {
            let e = e.ok()?;
            let meta = e.metadata().ok()?;
            if meta.is_file() {
                Some((meta.modified().unwrap_or(std::time::UNIX_EPOCH), e.path()))
            } else {
                None
            }
        })
        .collect();

    if files.len() < max_files {
        return;
    }
    files.sort_by_key(|(mtime, _)| *mtime);
    let to_remove = files.len().saturating_sub(max_files.saturating_sub(1));
    for (_, path) in files.into_iter().take(to_remove) {
        let _ = std::fs::remove_file(path);
    }
}

async fn a2a_jsonrpc(Json(body): Json<Value>) -> impl IntoResponse {
    let req: crate::core::a2a::a2a_compat::JsonRpcRequest = match serde_json::from_value(body) {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!("a2a JSON-RPC parse error: {e}");
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": {"code": -32700, "message": "invalid request"}
                })),
            );
        }
    };
    let resp = crate::core::a2a::a2a_compat::handle_a2a_jsonrpc(&req);
    let json = serde_json::to_value(resp).unwrap_or_default();
    (StatusCode::OK, Json(json))
}

async fn v1_a2a_agent_card(State(state): State<AppState>) -> impl IntoResponse {
    let card = crate::core::a2a::agent_card::build_agent_card(&state.project_root);
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        Json(card),
    )
}

async fn mcp_server_card() -> impl IntoResponse {
    let card = serde_json::json!({
        "name": "lean-ctx",
        "version": env!("CARGO_PKG_VERSION"),
        "description": "Context Infrastructure Layer — compression, caching, governance for AI agents",
        "capabilities": {
            "tools": true,
            "resources": false,
            "prompts": false,
            "sampling": false
        },
        "tool_categories": [
            {"name": "file_operations", "tools": ["ctx_read", "ctx_search", "ctx_tree", "ctx_edit"], "avg_token_cost": 150},
            {"name": "session_management", "tools": ["ctx_session", "ctx_compress", "ctx_dedup", "ctx_preload"], "avg_token_cost": 80},
            {"name": "intelligence", "tools": ["ctx_knowledge", "ctx_semantic_search", "ctx_graph", "ctx_overview"], "avg_token_cost": 200},
            {"name": "agent_ops", "tools": ["ctx_agent", "ctx_handoff", "ctx_task", "ctx_share"], "avg_token_cost": 120}
        ],
        "features": {
            "compression": "deterministic AST-based, 40-70% token reduction",
            "caching": "session-scoped with zstd, re-reads ~13 tokens",
            "audit_trail": "SHA-256 chained JSONL",
            "rbac": "5 built-in roles with capability-based access",
            "sandboxing": "Level 0 (subprocess) + Level 1 (OS-level)",
            "secret_detection": "8 regex patterns + custom"
        },
        "security": {
            "path_jail": true,
            "rate_limiting": true,
            "budget_tracking": true,
            "signed_handoffs": true,
            "timing_safe_auth": true
        }
    });
    Json(card)
}

async fn v1_agents_register(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let agent_type = body
        .get("agent_type")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let role = body.get("role").and_then(|v| v.as_str());
    let project_root = body
        .get("project_root")
        .and_then(|v| v.as_str())
        .unwrap_or(&state.project_root);

    let mut registry = crate::core::agents::AgentRegistry::load_or_create();
    let agent_id = registry.register(agent_type, role, project_root);
    let _ = registry.save();

    Json(serde_json::json!({
        "agent_id": agent_id,
        "status": "registered"
    }))
}

async fn v1_agents_heartbeat(Json(body): Json<Value>) -> impl IntoResponse {
    let agent_id = body.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
    let mut registry = crate::core::agents::AgentRegistry::load_or_create();
    registry.update_heartbeat(agent_id);
    let _ = registry.save();
    Json(serde_json::json!({"status": "ok"}))
}

async fn v1_agents_list() -> impl IntoResponse {
    let registry = crate::core::agents::AgentRegistry::load_or_create();
    let active = registry.list_active(None);
    Json(serde_json::json!({
        "agents": active.iter().map(|a| serde_json::json!({
            "agent_id": a.agent_id,
            "agent_type": a.agent_type,
            "role": a.role,
            "status": a.status.to_string(),
            "last_active": a.last_active.to_rfc3339(),
        })).collect::<Vec<_>>()
    }))
}

async fn v1_agents_deregister(Json(body): Json<Value>) -> impl IntoResponse {
    let agent_id = body.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
    let mut registry = crate::core::agents::AgentRegistry::load_or_create();
    registry.set_status(
        agent_id,
        crate::core::agents::AgentStatus::Finished,
        Some("deregistered via API"),
    );
    let _ = registry.save();
    Json(serde_json::json!({"status": "deregistered"}))
}

async fn v1_agents_events_sse()
-> Sse<impl Stream<Item = Result<SseEvent, std::convert::Infallible>>> {
    let stream = futures::stream::unfold(0usize, |last_count| async move {
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            let registry = crate::core::agents::AgentRegistry::load_or_create();
            let active = registry.list_active(None);
            let count = active.len();
            if count != last_count {
                let data = serde_json::json!({
                    "type": "agents_changed",
                    "active_count": count,
                    "agents": active.iter().map(|a| &a.agent_id).collect::<Vec<_>>(),
                });
                return Some((
                    Ok::<_, std::convert::Infallible>(SseEvent::default().data(data.to_string())),
                    count,
                ));
            }
        }
    });

    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}

fn build_app_router(cfg: &HttpServerConfig) -> Router {
    let project_root = cfg.project_root.to_string_lossy().to_string();
    let service_project_root = project_root.clone();
    let service_factory = move || -> Result<LeanCtxServer, std::io::Error> {
        Ok(LeanCtxServer::new_shared_with_context(
            &service_project_root,
            "default",
            "default",
        ))
    };
    let mcp_http = StreamableHttpService::new(
        service_factory,
        Arc::new(
            rmcp::transport::streamable_http_server::session::local::LocalSessionManager::default(),
        ),
        cfg.mcp_http_config(),
    );

    let rest_server = LeanCtxServer::new_shared_with_context(&project_root, "default", "default");

    let state = AppState {
        token: cfg.effective_auth_token(),
        concurrency: Arc::new(tokio::sync::Semaphore::new(cfg.max_concurrency.max(1))),
        rate: Arc::new(RateLimiter::new(cfg.max_rps, cfg.rate_burst)),
        project_root,
        timeout: Duration::from_millis(cfg.request_timeout_ms.max(1)),
        server: rest_server,
    };

    Router::new()
        .route("/health", get(health))
        .route("/v1/shutdown", axum::routing::post(v1_shutdown))
        .route("/v1/index/ensure", axum::routing::post(v1_index_ensure))
        .route("/v1/manifest", get(v1_manifest))
        .route("/v1/capabilities", get(v1_capabilities))
        .route("/v1/openapi.json", get(v1_openapi))
        .route("/v1/tools", get(v1_tools))
        .route("/v1/tools/call", axum::routing::post(v1_tool_call))
        .route("/v1/events", get(v1_events))
        .route(
            "/v1/context/summary",
            get(context_views::v1_context_summary),
        )
        .route("/v1/events/search", get(context_views::v1_events_search))
        .route("/v1/events/lineage", get(context_views::v1_event_lineage))
        .route("/v1/metrics", get(v1_metrics))
        .route("/v1/audit/events", get(v1_audit_events))
        .route("/v1/a2a/handoff", axum::routing::post(v1_a2a_handoff))
        .route("/v1/a2a/agent-card", get(v1_a2a_agent_card))
        .route("/.well-known/agent.json", get(v1_a2a_agent_card))
        .route("/.well-known/mcp-server.json", get(mcp_server_card))
        .route("/a2a", axum::routing::post(a2a_jsonrpc))
        .route(
            "/v1/agents/register",
            axum::routing::post(v1_agents_register),
        )
        .route(
            "/v1/agents/heartbeat",
            axum::routing::post(v1_agents_heartbeat),
        )
        .route("/v1/agents/list", get(v1_agents_list))
        .route(
            "/v1/agents/deregister",
            axum::routing::post(v1_agents_deregister),
        )
        .route("/v1/agents/events", get(v1_agents_events_sse))
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
        .with_state(state)
}

pub async fn serve(cfg: HttpServerConfig) -> Result<()> {
    crate::core::protocol::set_mcp_context(true);
    cfg.validate()?;

    // Surface any path-jail relaxation inherited from the launch env or config,
    // so a loosened boundary is never silent (GH security audit, finding 3).
    crate::core::pathjail::warn_if_relaxed();

    crate::core::plugins::PluginManager::init();
    crate::core::savings_autopush::spawn_if_enabled();

    // Pre-warm the project indices in the background for this long-lived HTTP
    // server. The stdio path deliberately stays lazy — short-lived respawns must
    // not each pay a full graph + BM25 scan (#453) — but `serve` is a single,
    // persistent process: one background build gives the first heavy/search tool
    // call a warm index instead of racing a cold scan of a large project root
    // against the per-request timeout (the SDK-conformance regression, GL #395).
    // The build is deduped per root and idle CPU settles flat once it completes
    // (the memory guard backs off), so #453 idle hygiene is preserved.
    let warm_root = cfg.project_root.to_string_lossy().to_string();
    let _ = warm_root;
    // Indexes are SQLite-backed; no warm-up required.

    let addr: SocketAddr = format!("{}:{}", cfg.host, cfg.port)
        .parse()
        .context("invalid host/port")?;

    let app = build_app_router(&cfg);

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

    fire_session_end();
    Ok(())
}

/// Fire the `on_session_end` plugin hook synchronously (best-effort, bounded by
/// each plugin's own timeout) so listeners run before the process exits. A
/// no-op unless a plugin declares the hook.
pub(crate) fn fire_session_end() {
    if crate::core::plugins::PluginManager::has_listener("on_session_end") {
        let _ = crate::core::plugins::PluginManager::fire_hook(
            &crate::core::plugins::executor::HookPoint::OnSessionEnd,
        );
    }
}

#[cfg(windows)]
impl axum::serve::Listener for crate::ipc::NamedPipeListener {
    type Io = tokio::net::windows::named_pipe::NamedPipeServer;
    type Addr = String;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            match self.accept_pipe().await {
                Ok(pipe) => return (pipe, self.name().to_string()),
                Err(e) => {
                    tracing::error!("named pipe accept error: {e}");
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            }
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        Ok(self.name().to_string())
    }
}

/// Serve the daemon over a platform-independent IPC channel (UDS on Unix,
/// Named Pipes on Windows).
pub async fn serve_ipc(cfg: HttpServerConfig, addr: crate::ipc::DaemonAddr) -> Result<()> {
    cfg.validate()?;

    crate::core::plugins::PluginManager::init();
    crate::core::savings_autopush::spawn_if_enabled();

    match addr {
        #[cfg(unix)]
        crate::ipc::DaemonAddr::Unix(ref path) => {
            let app = build_app_router(&cfg);
            let listener = crate::ipc::bind_listener(&addr)?;

            tracing::info!(
                "lean-ctx daemon listening on {} (project_root={})",
                path.display(),
                cfg.project_root.display()
            );

            axum::serve(listener, app.into_make_service())
                .with_graceful_shutdown(async move {
                    let _ = tokio::signal::ctrl_c().await;
                })
                .await
                .context("ipc server")?;
            Ok(())
        }
        #[cfg(windows)]
        crate::ipc::DaemonAddr::NamedPipe(ref name) => {
            let app = build_app_router(&cfg);
            let listener = crate::ipc::bind_listener(&addr)?;

            tracing::info!(
                "lean-ctx daemon listening on {} (project_root={})",
                name,
                cfg.project_root.display()
            );

            axum::serve(listener, app.into_make_service())
                .with_graceful_shutdown(async move {
                    let _ = tokio::signal::ctrl_c().await;
                })
                .await
                .context("ipc server")?;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use futures::StreamExt;
    use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};
    use serde_json::json;
    use tower::ServiceExt;

    async fn read_first_sse_message(body: Body) -> String {
        let mut stream = body.into_data_stream();
        let mut buf: Vec<u8> = Vec::new();
        for _ in 0..32 {
            let next = tokio::time::timeout(Duration::from_secs(2), stream.next()).await;
            let Ok(Some(Ok(bytes))) = next else {
                break;
            };
            buf.extend_from_slice(&bytes);
            if buf.windows(2).any(|w| w == b"\n\n") {
                break;
            }
        }
        String::from_utf8_lossy(&buf).to_string()
    }

    #[test]
    fn index_ensure_body_parses_root_and_optional_extra_roots() {
        // Wire contract for the #460 daemon delegation endpoint: camelCase
        // `extraRoots`, optional and defaulting to empty. daemon_client serializes
        // exactly this shape, so a drift here silently breaks delegation.
        let full: IndexEnsureBody =
            serde_json::from_str(r#"{"root":"/a","extraRoots":["/b","/c"]}"#).unwrap();
        assert_eq!(full.root, "/a");
        assert_eq!(full.extra_roots, vec!["/b".to_string(), "/c".to_string()]);

        let minimal: IndexEnsureBody = serde_json::from_str(r#"{"root":"/a"}"#).unwrap();
        assert_eq!(minimal.root, "/a");
        assert!(minimal.extra_roots.is_empty());
    }

    #[tokio::test]
    async fn auth_token_blocks_requests_without_bearer_header() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root_str = dir.path().to_string_lossy().to_string();
        let service_project_root = root_str.clone();
        let service_factory = move || -> Result<LeanCtxServer, std::io::Error> {
            Ok(LeanCtxServer::new_shared_with_context(
                &service_project_root,
                "default",
                "default",
            ))
        };
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
            project_root: root_str.clone(),
            timeout: Duration::from_secs(30),
            server: LeanCtxServer::new_shared_with_context(&root_str, "default", "default"),
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
    async fn mcp_service_factory_isolates_per_client_state() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root_str = dir.path().to_string_lossy().to_string();

        // Mirrors the serve() setup: service_factory must create a fresh server per MCP session.
        let service_project_root = root_str.clone();
        let service_factory = move || -> Result<LeanCtxServer, std::convert::Infallible> {
            Ok(LeanCtxServer::new_shared_with_context(
                &service_project_root,
                "default",
                "default",
            ))
        };

        let s1 = service_factory().expect("server 1");
        let s2 = service_factory().expect("server 2");

        // If the two servers accidentally share the same Arc-backed fields, these writes would
        // clobber each other. This test stays independent of rmcp's InitializeRequestParams API.
        *s1.client_name.write().await = "client-a".to_string();
        *s2.client_name.write().await = "client-b".to_string();

        let a = s1.client_name.read().await.clone();
        let b = s2.client_name.read().await.clone();
        assert_eq!(a, "client-a");
        assert_eq!(b, "client-b");
    }

    #[tokio::test]
    async fn rate_limit_returns_429_when_exhausted() {
        let state = AppState {
            token: None,
            concurrency: Arc::new(tokio::sync::Semaphore::new(16)),
            rate: Arc::new(RateLimiter::new(1, 1)),
            project_root: ".".to_string(),
            timeout: Duration::from_secs(30),
            server: LeanCtxServer::new_shared_with_context(".", "default", "default"),
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

    #[tokio::test]
    async fn audit_events_endpoint_returns_json() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root_str = dir.path().to_string_lossy().to_string();

        let state = AppState {
            token: None,
            concurrency: Arc::new(tokio::sync::Semaphore::new(16)),
            rate: Arc::new(RateLimiter::new(50, 100)),
            project_root: root_str.clone(),
            timeout: Duration::from_secs(30),
            server: LeanCtxServer::new_shared_with_context(&root_str, "default", "default"),
        };

        let app = Router::new()
            .route("/v1/audit/events", get(v1_audit_events))
            .with_state(state);

        let req = Request::builder()
            .method("GET")
            .uri("/v1/audit/events?limit=10")
            .header("Host", "localhost")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), 1_000_000)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json.get("cross_project_events").unwrap().is_array());
        assert!(json.get("audit_trail").unwrap().is_array());
    }

    #[tokio::test]
    async fn capabilities_endpoint_returns_contract() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root_str = dir.path().to_string_lossy().to_string();

        let state = AppState {
            token: None,
            concurrency: Arc::new(tokio::sync::Semaphore::new(16)),
            rate: Arc::new(RateLimiter::new(50, 100)),
            project_root: root_str.clone(),
            timeout: Duration::from_secs(30),
            server: LeanCtxServer::new_shared_with_context(&root_str, "default", "default"),
        };

        let app = Router::new()
            .route("/v1/capabilities", get(v1_capabilities))
            .with_state(state);

        let req = Request::builder()
            .method("GET")
            .uri("/v1/capabilities")
            .header("Host", "localhost")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), 1_000_000)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["contract_version"], json!(1));
        assert!(json["tools"]["total"].as_u64().unwrap() > 0);
        assert!(json["features"]["compression"].as_bool().unwrap());
        assert!(json["contracts"].is_object());
    }

    #[tokio::test]
    async fn openapi_endpoint_returns_spec() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root_str = dir.path().to_string_lossy().to_string();

        let state = AppState {
            token: None,
            concurrency: Arc::new(tokio::sync::Semaphore::new(16)),
            rate: Arc::new(RateLimiter::new(50, 100)),
            project_root: root_str.clone(),
            timeout: Duration::from_secs(30),
            server: LeanCtxServer::new_shared_with_context(&root_str, "default", "default"),
        };

        let app = Router::new()
            .route("/v1/openapi.json", get(v1_openapi))
            .with_state(state);

        let req = Request::builder()
            .method("GET")
            .uri("/v1/openapi.json")
            .header("Host", "localhost")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), 1_000_000)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["openapi"], json!("3.0.3"));
        assert!(json["paths"]["/v1/capabilities"]["get"].is_object());
        assert!(json["paths"]["/v1/openapi.json"]["get"].is_object());
    }

    #[tokio::test]
    async fn events_endpoint_replays_tool_call_event() {
        use crate::core::context_os::{self, ContextEventKindV1};

        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join(".git")).expect("git marker");
        std::fs::write(dir.path().join("a.txt"), "ok").expect("file");
        let root_str = dir.path().to_string_lossy().to_string();

        let state = AppState {
            token: None,
            concurrency: Arc::new(tokio::sync::Semaphore::new(16)),
            rate: Arc::new(RateLimiter::new(50, 100)),
            project_root: root_str.clone(),
            timeout: Duration::from_secs(30),
            server: LeanCtxServer::new_shared_with_context(&root_str, "default", "default"),
        };

        let app = Router::new()
            .route("/v1/events", get(v1_events))
            .with_state(state);

        // Directly append an event to the bus — no fire-and-forget timing dependency.
        let rt = context_os::runtime();
        let _ = rt.bus.append(
            "ws1",
            "ch1",
            &ContextEventKindV1::ToolCallRecorded,
            Some("test-agent"),
            json!({"tool": "ctx_session", "action": "status"}),
        );

        let req = Request::builder()
            .method("GET")
            .uri("/v1/events?workspaceId=ws1&channelId=ch1&since=0&limit=1")
            .header("Host", "localhost")
            .header("Accept", "text/event-stream")
            .body(Body::empty())
            .expect("req");
        let resp = app.clone().oneshot(req).await.expect("events");
        assert_eq!(resp.status(), StatusCode::OK);

        let msg = read_first_sse_message(resp.into_body()).await;
        assert!(msg.contains("event: tool_call_recorded"), "msg={msg:?}");
        assert!(msg.contains("\"ws1\""), "msg={msg:?}");
        assert!(msg.contains("\"ch1\""), "msg={msg:?}");
    }
}
