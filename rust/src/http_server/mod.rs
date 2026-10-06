//! HTTP server combining Streamable-HTTP MCP transport, REST APIs, and
//! optional local HTTP surfaces.
//!
//! # Pillar mapping
//!
//! - **Engine:** HTTP MCP transport, Context OS event bus (SSE), A2A handoffs,
//!   agent registry, capabilities/manifest/openapi endpoints.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
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
use rmcp::transport::StreamableHttpService;
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::broadcast;
use tokio::time::{Duration, Instant};

use crate::core::a2a::relay::RelayPeerTableV1;
use crate::core::context_os::ContextOsMetrics;
use crate::engine::ContextEngine;
use crate::tools::LeanCtxServer;

mod config;
mod discovery;
mod handlers;
mod relay_rate;
mod relay_replay;
mod relay_replay_async;
#[cfg(test)]
mod relay_tls_tests;
mod remote_replay;
mod scoped_delivery;
mod task_control;
#[cfg(test)]
mod task_control_tests;
#[allow(clippy::wildcard_imports)]
use handlers::*;

pub mod kernel_api;

pub use config::HttpServerConfig;
use config::sanitize_id;
pub use relay_rate::{RelayQuota, RelayQuotaConfig};

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

type RequestConcurrencyLease = Arc<tokio::sync::OwnedSemaphorePermit>;

#[derive(Clone)]
struct AppState {
    token: Option<String>,
    a2a_signing_key: Option<String>,
    a2a_recipient_id: Option<String>,
    a2a_tenant_id: Option<String>,
    a2a_project_id: Option<String>,
    a2a_peers: Arc<RelayPeerTableV1>,
    relay_quotas: Option<Arc<tokio::sync::Mutex<relay_rate::RelayQuotaState>>>,
    /// Configured Ed25519 peer trust and capability grants. Sender-provided
    /// capability claims are never authority; only this policy is.
    a2a_task_authority: Arc<crate::core::a2a::task::TaskAuthorityConfigV1>,
    concurrency: Arc<tokio::sync::Semaphore>,
    rate: Arc<RateLimiter>,
    remote_replays: Arc<remote_replay::RemoteReplayGuard>,
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

    // Relay deliveries authenticate against their selected peer channel; the
    // general server bearer must not become a second singleton authority.
    if req.uri().path() == "/a2a/deliver" && !state.a2a_peers.peers.is_empty() {
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
    mut req: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let Ok(permit) = state.concurrency.clone().try_acquire_owned() else {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    };
    let permit = Arc::new(permit);
    req.extensions_mut().insert(permit.clone());
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
    tokio::task::spawn_blocking(move || {
        crate::core::index_orchestrator::ensure_all_background(&root);
        if !extra.is_empty() {
            crate::core::index_orchestrator::ensure_extra_roots_background(&root, &extra);
        }
    });
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
    /// Agent identity used for directed-event visibility. Without an identity,
    /// SSE exposes broadcast events only.
    #[serde(default)]
    agent_id: Option<String>,
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

/// `GET /v1/openapi.json` — OpenAPI 3.0 document for the public `/v1` surface,
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

    let kind_filter: Option<Vec<String>> = q.kind.as_deref().map(|k| {
        k.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    });
    let agent_id = q
        .agent_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string);
    let event_filter = crate::core::context_os::TopicFilter {
        kinds: kind_filter.as_ref().map(|kinds| {
            kinds
                .iter()
                .map(|kind| crate::core::context_os::ContextEventKindV1::parse(kind))
                .collect()
        }),
        agent_id: agent_id.clone(),
        include_directed: agent_id.is_some(),
        ..Default::default()
    };

    let rt = crate::core::context_os::runtime();
    let replay = rt.bus.read(&ws, &ch, since, limit);
    let replay: Vec<_> = replay
        .into_iter()
        .filter(|event| event_filter.matches(event))
        .collect();

    let rx = if let Some(sub) = rt.bus.subscribe_filtered(&ws, &ch, event_filter.clone()) {
        crate::core::context_os::SubscriptionKind::Filtered(sub)
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
            event_filter,
            ws.clone(),
            ch.clone(),
            since,
            redaction,
            bus,
            metrics,
        ),
        |(mut pending, mut rx, event_filter, ws, ch, mut last_id, redaction, bus, metrics)| async move {
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
                    (
                        pending,
                        rx,
                        event_filter,
                        ws,
                        ch,
                        last_id,
                        redaction,
                        bus,
                        metrics,
                    ),
                ));
            }

            loop {
                match rx.recv().await {
                    Ok(mut ev) if ev.id > last_id => {
                        last_id = ev.id;
                        if !event_filter.matches(&ev) {
                            continue;
                        }
                        redact_event_payload(&mut ev, redaction);
                        let data = serde_json::to_string(&ev).unwrap_or_else(|_| "{}".to_string());
                        let evt = SseEvent::default()
                            .id(ev.id.to_string())
                            .event(ev.kind)
                            .data(data);
                        return Some((
                            Ok(evt),
                            (
                                pending,
                                rx,
                                event_filter,
                                ws,
                                ch,
                                last_id,
                                redaction,
                                bus,
                                metrics,
                            ),
                        ));
                    }
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Closed) => return None,
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        let missed = bus.read(&ws, &ch, last_id, skipped as usize);
                        metrics.record_events_replayed(missed.len() as u64);
                        for ev in missed {
                            last_id = last_id.max(ev.id);
                            if event_filter.matches(&ev) {
                                pending.push_back(ev);
                            }
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

#[derive(Deserialize)]
struct ContextSummaryQuery {
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

async fn v1_context_summary(
    State(state): State<AppState>,
    Query(q): Query<ContextSummaryQuery>,
) -> impl IntoResponse {
    let ws = sanitize_id(&q.workspace_id.unwrap_or_else(|| "default".to_string()));
    let limit = q.limit.unwrap_or(100).min(1000);
    let rt = crate::core::context_os::runtime();
    let events = rt.bus.read(&ws, "default", 0, limit);
    let mut counts_by_kind = serde_json::Map::new();
    for ev in &events {
        let counter = counts_by_kind
            .entry(ev.kind.clone())
            .or_insert(serde_json::Value::from(0u64));
        if let Some(n) = counter.as_u64() {
            *counter = serde_json::Value::from(n + 1);
        }
    }
    Json(serde_json::json!({
        "workspaceId": ws,
        "channelId": "default",
        "projectRoot": state.project_root,
        "totalEvents": events.len(),
        "eventCountsByKind": counts_by_kind,
        "limit": limit,
    }))
}

#[derive(Deserialize)]
struct EventsSearchQuery {
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

async fn v1_events_search(Query(q): Query<EventsSearchQuery>) -> impl IntoResponse {
    let query = q.q.unwrap_or_default();
    let limit = q.limit.unwrap_or(50).min(500);
    let rt = crate::core::context_os::runtime();
    let all_events = rt.bus.read("default", "default", 0, limit * 10);
    let results: Vec<serde_json::Value> = all_events
        .into_iter()
        .filter(|ev| {
            let payload = serde_json::to_string(ev).unwrap_or_default();
            query.is_empty() || payload.contains(&query)
        })
        .take(limit)
        .map(|ev| serde_json::to_value(&ev).unwrap_or_default())
        .collect();
    Json(serde_json::json!({
        "query": query,
        "results": results,
        "count": results.len(),
    }))
}

#[derive(Deserialize)]
struct EventLineageQuery {
    #[serde(default)]
    id: Option<i64>,
    #[serde(default)]
    depth: Option<usize>,
}

async fn v1_events_lineage(Query(q): Query<EventLineageQuery>) -> impl IntoResponse {
    let event_id = q.id.unwrap_or(0);
    let depth = q.depth.unwrap_or(10).min(100);
    let rt = crate::core::context_os::runtime();
    let events = rt.bus.read("default", "default", 0, depth);
    let chain: Vec<serde_json::Value> = events
        .into_iter()
        .filter(|ev| ev.id >= event_id)
        .take(depth)
        .map(|ev| serde_json::to_value(&ev).unwrap_or_default())
        .collect();
    Json(serde_json::json!({
        "eventId": event_id,
        "depth": depth,
        "chain": chain,
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

async fn a2a_jsonrpc(State(state): State<AppState>, Json(body): Json<Value>) -> impl IntoResponse {
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
    let resp = match crate::core::a2a::task::TaskStore::scoped_path(&state.project_root) {
        Ok(path) => crate::core::a2a::a2a_compat::handle_a2a_jsonrpc_at_path(&req, &path),
        Err(error) => {
            tracing::warn!("A2A project scope error: {error}");
            crate::core::a2a::a2a_compat::JsonRpcResponse::server_error(
                req.id.clone(),
                "task storage unavailable",
            )
        }
    };
    let json = serde_json::to_value(resp).unwrap_or_default();
    (StatusCode::OK, Json(json))
}

async fn v1_a2a_agent_card(State(state): State<AppState>) -> impl IntoResponse {
    let card =
        crate::core::a2a::agent_card::build_agent_card(&state.project_root, state.token.is_some());
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        Json(card),
    )
}

async fn v1_agents_register(State(state): State<AppState>, Json(body): Json<Value>) -> Response {
    let agent_type = body
        .get("agent_type")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let role = body.get("role").and_then(|v| v.as_str());
    let project_root = body
        .get("project_root")
        .and_then(|v| v.as_str())
        .unwrap_or(&state.project_root);

    match crate::core::agents::AgentRegistry::mutate_locked(|registry| {
        registry.register(agent_type, role, project_root, None)
    }) {
        Ok((_, agent_id)) => (
            StatusCode::CREATED,
            Json(serde_json::json!({
                "agent_id": agent_id,
                "status": "registered"
            })),
        )
            .into_response(),
        Err(error) => {
            tracing::warn!(%error, "lean-ctx: agent registration failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error": "agent registry unavailable"})),
            )
                .into_response()
        }
    }
}

async fn v1_agents_heartbeat(Json(body): Json<Value>) -> Response {
    let agent_id = body.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
    match crate::core::agents::AgentRegistry::mutate_locked(|registry| {
        registry.update_heartbeat(agent_id)
    })
    .and_then(|(_, result)| result)
    {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"status": "ok"}))).into_response(),
        Err(error) => {
            tracing::warn!(%error, agent_id, "lean-ctx: rejected agent heartbeat");
            let (status, message) = if error.starts_with("agent presence '") {
                (StatusCode::NOT_FOUND, "unknown agent id")
            } else {
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "agent registry unavailable",
                )
            };
            (
                status,
                Json(serde_json::json!({
                    "error": message,
                    "status": "rejected"
                })),
            )
                .into_response()
        }
    }
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

async fn v1_agents_deregister(Json(body): Json<Value>) -> Response {
    let agent_id = body.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
    match crate::core::agents::AgentRegistry::mutate_locked(|registry| {
        registry.set_status(
            agent_id,
            crate::core::agents::AgentStatus::Finished,
            Some("deregistered via API"),
        )
    })
    .and_then(|(_, result)| result)
    {
        Ok(()) => (
            StatusCode::OK,
            Json(serde_json::json!({"status": "deregistered"})),
        )
            .into_response(),
        Err(error) => {
            tracing::warn!(%error, agent_id, "lean-ctx: rejected agent deregistration");
            let (status, message) = if error.starts_with("agent presence '") {
                (StatusCode::NOT_FOUND, "unknown agent id")
            } else {
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "agent registry unavailable",
                )
            };
            (
                status,
                Json(serde_json::json!({
                    "error": message,
                    "status": "rejected"
                })),
            )
                .into_response()
        }
    }
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

fn build_app_router(
    cfg: &HttpServerConfig,
    receipt_authority: Option<Arc<crate::core::execution_ledger::host::HostReceiptAuthority>>,
) -> Router {
    build_app_router_with_auth(cfg, true, receipt_authority)
}

#[cfg(test)]
pub(crate) fn build_delivery_test_router(cfg: &HttpServerConfig) -> Router {
    build_app_router_with_auth(cfg, false, None)
}

fn build_app_router_with_auth(
    cfg: &HttpServerConfig,
    require_auth: bool,
    receipt_authority: Option<Arc<crate::core::execution_ledger::host::HostReceiptAuthority>>,
) -> Router {
    let project_root = cfg.project_root.to_string_lossy().to_string();
    let service_project_root = project_root.clone();
    // REST and Streamable HTTP share the same startup authority snapshot.
    let server_factory = move || {
        let mut server =
            LeanCtxServer::new_shared_with_context(&service_project_root, "default", "default");
        server
            .native_receipt_authority
            .clone_from(&receipt_authority);
        server
    };
    let rest_server = server_factory();
    let service_factory = move || -> Result<LeanCtxServer, std::io::Error> { Ok(server_factory()) };
    let mcp_http = StreamableHttpService::new(
        service_factory,
        Arc::new(
            rmcp::transport::streamable_http_server::session::local::LocalSessionManager::default(),
        ),
        cfg.mcp_http_config(),
    );

    let state = AppState {
        token: if require_auth {
            cfg.effective_auth_token()
        } else {
            None
        },
        a2a_signing_key: cfg.a2a_signing_key.clone(),
        a2a_recipient_id: cfg.a2a_recipient_id.clone(),
        a2a_tenant_id: cfg.a2a_tenant_id.clone(),
        a2a_project_id: cfg.a2a_project_id.clone(),
        a2a_peers: Arc::new(cfg.a2a_peers.clone()),
        relay_quotas: match cfg.relay_quota_state() {
            Ok(state) => state.map(|state| Arc::new(tokio::sync::Mutex::new(state))),
            Err(error) => {
                tracing::error!("relay quota configuration rejected: {error}");
                None
            }
        },
        a2a_task_authority: Arc::new(cfg.a2a_task_authority.clone()),
        concurrency: Arc::new(tokio::sync::Semaphore::new(cfg.max_concurrency.max(1))),
        rate: Arc::new(RateLimiter::new(cfg.max_rps, cfg.rate_burst)),
        remote_replays: Arc::new(remote_replay::RemoteReplayGuard::new(
            &project_root,
            handlers::REMOTE_REPLAY_RETENTION_SECONDS,
        )),
        project_root,
        timeout: Duration::from_millis(cfg.request_timeout_ms.max(1)),
        server: rest_server,
    };

    let ocla = match (
        state.token.as_ref(),
        state.a2a_tenant_id.as_deref(),
        state.a2a_project_id.as_deref(),
    ) {
        (Some(_), Some(tenant_id), Some(project_id)) => {
            match crate::core::a2a::dlq::DlqScope::new(tenant_id, project_id) {
                Ok(scope) => crate::core::ocla::wire_api::ocla_router_with_dlq(scope),
                Err(_) => crate::core::ocla::wire_api::ocla_router(),
            }
        }
        _ => crate::core::ocla::wire_api::ocla_router(),
    };

    Router::new()
        .route("/health", get(health))
        .route(
            "/ocla/v1/delivery/scoped",
            axum::routing::post(scoped_delivery::execute),
        )
        .route("/v1/shutdown", axum::routing::post(v1_shutdown))
        .route("/v1/index/ensure", axum::routing::post(v1_index_ensure))
        .route("/v1/manifest", get(v1_manifest))
        .route("/v1/capabilities", get(v1_capabilities))
        .route("/v1/openapi.json", get(v1_openapi))
        .route("/v1/cache/stats", get(v1_cache_stats))
        .route("/v1/tools", get(v1_tools))
        .route("/v1/tools/call", axum::routing::post(v1_tool_call))
        .route("/v1/events", get(v1_events))
        .route("/v1/metrics", get(v1_metrics))
        .route("/v1/context/summary", get(v1_context_summary))
        .route("/v1/events/search", get(v1_events_search))
        .route("/v1/events/lineage", get(v1_events_lineage))
        .route("/v1/audit/events", get(v1_audit_events))
        .route("/v1/a2a/handoff", axum::routing::post(v1_a2a_handoff))
        .route("/a2a/deliver", axum::routing::post(handlers::a2a_deliver))
        .route("/v1/a2a/agent-card", get(v1_a2a_agent_card))
        .route("/.well-known/agent.json", get(v1_a2a_agent_card))
        .route(
            "/.well-known/mcp-server.json",
            get(discovery::mcp_server_card),
        )
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
        .route("/v1/kernel/dashboard", get(kernel_api::dashboard))
        .route("/v1/kernel/etpao", get(kernel_api::etpao))
        .route("/v1/kernel/config", get(kernel_api::get_config))
        .route(
            "/v1/kernel/config",
            axum::routing::post(kernel_api::set_config),
        )
        .route("/v1/kernel/evidence", get(kernel_api::evidence))
        .route("/v1/kernel/health", get(kernel_api::health))
        .route("/v1/kernel/report", get(kernel_api::report))
        .route(
            "/v1/kernel/reset",
            axum::routing::post(kernel_api::reset_state),
        )
        .merge(ocla.with_state(()))
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
    let receipt_authority = crate::server::native_receipts::load_configured_host_authority()
        .map_err(anyhow::Error::msg)?;

    // Surface any path-jail relaxation inherited from the launch env or config,
    // so a loosened boundary is never silent (GH security audit, finding 3).
    crate::core::pathjail::warn_if_relaxed();

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
    if !warm_root.is_empty() {
        crate::core::index_orchestrator::ensure_all_background(&warm_root);
    }

    let addr: SocketAddr = format!("{}:{}", cfg.host, cfg.port)
        .parse()
        .context("invalid host/port")?;

    let app = build_app_router(&cfg, receipt_authority);

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
pub(crate) fn fire_session_end() {}

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
    let receipt_authority = crate::server::native_receipts::load_configured_host_authority()
        .map_err(anyhow::Error::msg)?;

    crate::core::savings_autopush::spawn_if_enabled();
    crate::cloud_sync::spawn_daemon_telemetry();

    match addr {
        #[cfg(unix)]
        crate::ipc::DaemonAddr::Unix(ref path) => {
            let app = build_app_router_with_auth(&cfg, false, receipt_authority);
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
            let app = build_app_router_with_auth(&cfg, false, receipt_authority);
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
mod cancellation_tests;

#[cfg(test)]
mod native_receipt_tests;

#[cfg(test)]
#[path = "main_http_tests.rs"]
mod tests;
