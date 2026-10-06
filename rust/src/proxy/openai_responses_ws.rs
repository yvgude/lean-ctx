//! WebSocket bridge for the OpenAI/Codex Responses transport (#440).
//!
//! Codex CLI defaults to a persistent WebSocket connection to `/responses`
//! (gated by `supports_websockets`): it sends one `response.create` event per
//! turn and receives the Responses streaming events back as WebSocket messages
//! (protocol: <https://developers.openai.com/api/docs/guides/websocket-mode>).
//!
//! The proxy speaks that protocol on the Codex-facing side and bridges each turn
//! to the configured HTTP/SSE upstream (e.g. `codex-lb` or the OpenAI Responses
//! endpoint): it converts the `response.create` event into a streaming
//! `POST /v1/responses`, applies lean-ctx's tool-output compression, and relays
//! every upstream SSE `data:` event verbatim as a WebSocket text frame. This
//! makes the proxy a drop-in for Codex without forcing `supports_websockets =
//! false` on the client.
//!
//! Boundaries of an HTTP-backed bridge (documented, not hidden):
//! - Continuation relies on the upstream's own `previous_response_id` semantics
//!   (works with `store=true`). The native WS connection-local cache that keeps
//!   `store=false`/ZDR continuations fast is an upstream-only optimization and is
//!   not reconstructed here; the client still works because it falls back to the
//!   persisted chain.
//! - `generate:false` warmup frames have no HTTP equivalent, so they are
//!   forwarded as normal turns.

use std::ops::ControlFlow;
use std::sync::{Arc, Mutex};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::{HeaderMap, HeaderName, HeaderValue};
use axum::response::Response;
use futures::{Sink, SinkExt, Stream, StreamExt};
use serde_json::{Value, json};

use super::ProxyState;
use crate::core::execution_lifecycle::{
    CancellationCompletion, CancellationHandler, CompletionObservation, ExecutionDriver,
    ExecutionLifecycle, LifecycleRunError, ProductEntitlements, ProxyEconomicsObservation,
    RuntimeContext, StageDisposition, TaskContext, ToolRequest, ToolSurface,
};

/// Request headers copied from the WS upgrade onto each upstream turn. Mirrors
/// the subset of the HTTP path's allowlist that an OpenAI Responses call needs
/// (`forward::ALLOWED_REQUEST_HEADERS`); the proxy forwards them verbatim and
/// never injects upstream credentials of its own.
const FORWARDED_UPGRADE_HEADERS: &[&str] = &[
    "authorization",
    "x-api-key",
    "chatgpt-account-id",
    "x-openai-fedramp",
    "x-openai-internal-codex-residency",
    "x-openai-internal-codex-responses-lite",
    "x-openai-product-sku",
    "oai-product-sku",
    "x-oai-attestation",
    "x-client-request-id",
    "x-codex-beta-features",
    "x-codex-installation-id",
    "x-codex-parent-thread-id",
    "x-openai-subagent",
    "x-codex-turn-state",
    "x-codex-turn-metadata",
    "x-codex-window-id",
    "x-openai-memgen-request",
    "x-responsesapi-include-timing-metrics",
    "openai-organization",
    "openai-project",
    "openai-beta",
    "originator",
    "user-agent",
];

/// Upgrades a Responses WebSocket and bridges it to the HTTP/SSE upstream.
pub fn upgrade(state: ProxyState, ws: WebSocketUpgrade, headers: &HeaderMap) -> Response {
    let upstream = state.openai_upstream();
    upgrade_to(state, ws, headers, upstream, "/v1/responses")
}

/// Upgrades a Responses WebSocket and bridges it to a selected HTTP/SSE target.
pub fn upgrade_to(
    state: ProxyState,
    ws: WebSocketUpgrade,
    headers: &HeaderMap,
    upstream: String,
    path: &'static str,
) -> Response {
    let fwd = capture_forward_headers(headers);
    ws.on_upgrade(move |socket| bridge(socket, state, upstream, path, fwd))
}

fn capture_forward_headers(headers: &HeaderMap) -> Vec<(HeaderName, HeaderValue)> {
    FORWARDED_UPGRADE_HEADERS
        .iter()
        .filter_map(|name| {
            let value = headers.get(*name)?;
            let header_name = HeaderName::from_bytes(name.as_bytes()).ok()?;
            Some((header_name, value.clone()))
        })
        .collect()
}

async fn bridge(
    mut socket: WebSocket,
    state: ProxyState,
    upstream: String,
    path: &'static str,
    fwd_headers: Vec<(HeaderName, HeaderValue)>,
) {
    let session_id = uuid::Uuid::new_v4().to_string();
    // The Responses WS protocol runs turns sequentially: one in-flight response
    // per connection. We mirror that — each `response.create` is fully streamed
    // back before the next inbound frame is read.
    while let Some(msg) = socket.recv().await {
        let Ok(msg) = msg else { break };
        match msg {
            Message::Text(text) => {
                if run_turn(
                    &mut socket,
                    &state,
                    &upstream,
                    path,
                    &fwd_headers,
                    text.as_str(),
                    &session_id,
                )
                .await
                .is_break()
                {
                    break;
                }
            }
            Message::Ping(payload) => {
                if socket.send(Message::Pong(payload)).await.is_err() {
                    break;
                }
            }
            Message::Close(_) => break,
            // Binary / Pong are never part of the Responses transport.
            Message::Pong(_) | Message::Binary(_) => {}
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TurnTermination {
    CleanEof,
    UpstreamUnavailable,
    UpstreamRejected,
    StreamInterrupted,
    ClientDisconnected,
}

#[derive(Debug, Clone)]
struct TurnResult {
    termination: TurnTermination,
    usage: Option<super::usage::RealUsage>,
    original_size: usize,
    compressed_size: usize,
}

impl TurnResult {
    fn flow(&self) -> ControlFlow<()> {
        if self.termination == TurnTermination::ClientDisconnected {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    }
}

struct WsTurnDriver<'a, S> {
    socket: &'a mut S,
    state: &'a ProxyState,
    upstream: &'a str,
    path: &'a str,
    headers: &'a [(HeaderName, HeaderValue)],
    document: Option<Value>,
    model: String,
    ledger: Arc<Mutex<WsLedger>>,
}

#[derive(Default)]
struct WsLedger {
    result: Option<TurnResult>,
    recorded: bool,
}

fn record_ws_ledger(ledger: &Mutex<WsLedger>, stats: &super::ProxyStats) -> StageDisposition {
    let result = {
        let mut ledger = ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if ledger.recorded {
            return StageDisposition::Skipped("WS ledger already recorded");
        }
        let Some(result) = ledger.result.clone() else {
            return StageDisposition::Skipped("WS dispatch not started");
        };
        // Reserve before effects: cancellation/panic must not retry a partial write.
        ledger.recorded = true;
        result
    };
    stats.record_provider_request("OpenAI", result.original_size, result.compressed_size);
    if let Some(usage) = result.usage.as_ref() {
        super::usage_meter::record(usage);
    }
    StageDisposition::Applied
}

#[async_trait::async_trait]
impl<S> ExecutionDriver for WsTurnDriver<'_, S>
where
    S: Sink<Message> + Send + Unpin,
    S::Error: Send,
{
    type Primitive = TurnResult;
    type Processed = TurnResult;
    type Output = TurnResult;
    type Error = std::convert::Infallible;

    fn cancellation_handler(&self) -> Option<CancellationHandler> {
        let ledger = self.ledger.clone();
        let stats = self.state.stats.clone();
        let model = self.model.clone();
        Some(Box::new(move || {
            let snapshot = ledger
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .result
                .clone();
            CancellationCompletion {
                observation: turn_observation(snapshot.as_ref(), &model),
                ledger: record_ws_ledger(&ledger, &stats),
            }
        }))
    }

    async fn dispatch_primitive(
        &mut self,
        _context: &TaskContext,
    ) -> Result<(TurnResult, StageDisposition), Self::Error> {
        let result = forward_turn(
            self.socket,
            self.state,
            self.upstream,
            self.path,
            self.headers,
            self.document.take().expect("WS document consumed once"),
            &self.ledger,
        )
        .await;
        self.ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .result = Some(result.clone());
        Ok((result, StageDisposition::Applied))
    }

    async fn reversible_post_process(
        &mut self,
        _context: &TaskContext,
        result: TurnResult,
    ) -> Result<(TurnResult, StageDisposition), Self::Error> {
        Ok((
            result,
            StageDisposition::Skipped("WS relay preserves response bytes"),
        ))
    }

    async fn record_context_ir(
        &mut self,
        _context: &TaskContext,
        _result: &TurnResult,
    ) -> Result<StageDisposition, Self::Error> {
        Ok(StageDisposition::Skipped("WS Context IR unavailable"))
    }

    async fn record_ledger(
        &mut self,
        _context: &TaskContext,
        _result: &TurnResult,
    ) -> Result<StageDisposition, Self::Error> {
        Ok(record_ws_ledger(&self.ledger, &self.state.stats))
    }

    fn output_from_processed(&mut self, result: TurnResult) -> TurnResult {
        result
    }

    fn observe(
        &self,
        result: &Result<TurnResult, LifecycleRunError<Self::Error>>,
    ) -> CompletionObservation {
        turn_observation(result.as_ref().ok(), &self.model)
    }
}

fn turn_observation(result: Option<&TurnResult>, model: &str) -> CompletionObservation {
    let usage = result.and_then(|result| result.usage.as_ref());
    let mut observation = CompletionObservation::tool_result(
        usage.map_or(0, |usage| {
            usage
                .input_tokens
                .saturating_add(usage.cache_read_tokens)
                .saturating_add(usage.cache_write_tokens)
        }),
        usage.map_or(0, |usage| usage.output_tokens),
        usage.map_or(model, |usage| usage.model.as_str()),
        result.is_some_and(|result| result.termination == TurnTermination::CleanEof),
    );
    "OpenAI".clone_into(&mut observation.provider);
    // Transport failure is not a compiler failure or quality evidence.
    observation.outcome_signals.clear();
    observation.proxy_economics = result.map(|result| ProxyEconomicsObservation {
        // Same byte/4 estimate as ProxyStats; never presented as measured usage.
        tokens_pruned: result.original_size.saturating_sub(result.compressed_size) / 4,
        original_tokens: result.original_size / 4,
        task_class: "proxy_responses_ws".to_owned(),
    });
    observation
}

#[allow(clippy::too_many_arguments)]
async fn run_turn<S>(
    socket: &mut S,
    state: &ProxyState,
    upstream: &str,
    path: &str,
    fwd_headers: &[(HeaderName, HeaderValue)],
    text: &str,
    session_id: &str,
) -> ControlFlow<()>
where
    S: Sink<Message> + Send + Unpin,
    S::Error: Send,
{
    let Some(doc) = build_upstream_body(text) else {
        return send_error(
            socket,
            400,
            "invalid_request_error",
            "Expected a JSON `response.create` event",
        )
        .await;
    };

    let model = doc
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let result = ExecutionLifecycle::global()
        .run(
            ToolRequest {
                tool_name: "proxy_responses_ws".to_owned(),
                query: None,
                session_id: session_id.to_owned(),
                agent_id: "OpenAI".to_owned(),
                surface: ToolSurface::Proxy,
                // Equal payloads on a persistent socket are distinct turns.
                idempotency_key: None,
            },
            RuntimeContext {
                client_name: Some("OpenAI Responses WebSocket".to_owned()),
                project_root: None,
            },
            ProductEntitlements::default(),
            WsTurnDriver {
                socket,
                state,
                upstream,
                path,
                headers: fwd_headers,
                document: Some(doc),
                model,
                ledger: Arc::new(Mutex::new(WsLedger::default())),
            },
        )
        .await;
    match result {
        Ok(result) => result.flow(),
        Err(_) => ControlFlow::Break(()),
    }
}

#[allow(clippy::too_many_arguments)]
async fn forward_turn<S>(
    socket: &mut S,
    state: &ProxyState,
    upstream: &str,
    path: &str,
    fwd_headers: &[(HeaderName, HeaderValue)],
    mut doc: Value,
    ledger: &Mutex<WsLedger>,
) -> TurnResult
where
    S: Sink<Message> + Send + Unpin,
    S::Error: Send,
{
    // Compare the same HTTP representation before and after optimization;
    // removing WS fields or JSON whitespace is not context compression.
    let original_size = serde_json::to_vec(&doc)
        .expect("parsed JSON document serializes")
        .len();
    // Same two-stage path as the HTTP handler: cache-aware prune of the frozen
    // OLD region, then compress the recent outputs.
    super::openai_responses::prune_responses_input(&mut doc);
    super::openai_responses::compress_responses_input(&mut doc);
    // G6: final egress control on the turn that leaves, as on the HTTP rail.
    let model = doc.get("model").and_then(Value::as_str).map(str::to_owned);
    let egress = crate::core::context_admission::egress::admit_request(
        &doc,
        &crate::core::context_admission::egress::EgressTarget {
            provider: "OpenAI",
            model: model.as_deref(),
            upstream_base: upstream,
        },
    );
    let refusal = match &egress.body {
        crate::core::context_admission::egress::EgressBody::Rewritten(admitted) => {
            doc = admitted.clone();
            None
        }
        crate::core::context_admission::egress::EgressBody::Refused(refusal) => {
            Some(refusal.clone())
        }
        crate::core::context_admission::egress::EgressBody::Unchanged => None,
    };
    let payload = serde_json::to_vec(&doc).expect("optimized JSON document serializes");
    let compressed_size = payload.len();
    let result = |termination, usage| TurnResult {
        termination,
        usage,
        original_size,
        compressed_size,
    };
    if let Some(refusal) = refusal {
        crate::core::context_admission::egress::finish_off_runtime(
            egress,
            None,
            original_size,
            None,
        )
        .await;
        tracing::warn!("lean-ctx proxy: {refusal} (see `lean-ctx inspect --proxy`)");
        let flow = send_error(socket, 403, "context_gateway", &refusal).await;
        return result(
            if flow.is_break() {
                TurnTermination::ClientDisconnected
            } else {
                TurnTermination::UpstreamRejected
            },
            None,
        );
    }
    crate::core::context_admission::egress::finish_off_runtime(
        egress,
        Some(payload.clone()),
        original_size,
        None,
    )
    .await;
    ledger
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .result = Some(result(TurnTermination::ClientDisconnected, None));

    let url = format!("{upstream}{path}");
    let mut req = state
        .client
        .post(&url)
        .header("content-type", "application/json")
        .header("accept", "text/event-stream");
    for (name, value) in fwd_headers {
        req = req.header(name, value);
    }

    let resp = match req.body(payload).send().await {
        Ok(resp) => resp,
        Err(e) => {
            tracing::error!("lean-ctx proxy: OpenAI Responses WS upstream error: {e}");
            let flow = send_error(
                socket,
                502,
                "upstream_error",
                "Failed to reach the OpenAI Responses upstream",
            )
            .await;
            return result(
                if flow.is_break() {
                    TurnTermination::ClientDisconnected
                } else {
                    TurnTermination::UpstreamUnavailable
                },
                None,
            );
        }
    };

    let status = resp.status();
    if !status.is_success() {
        let detail = resp.text().await.unwrap_or_default();
        let flow = relay_upstream_error(socket, status.as_u16(), &detail).await;
        return result(
            if flow.is_break() {
                TurnTermination::ClientDisconnected
            } else {
                TurnTermination::UpstreamRejected
            },
            None,
        );
    }

    let (termination, usage) = stream_sse_to_ws(socket, resp.bytes_stream(), Some(ledger)).await;
    result(termination, usage)
}

/// Converts a client `response.create` WS event into an upstream Responses-API
/// request body: drops the WS-only fields (`type`, `generate`, `background`) and
/// forces `stream:true` so the upstream replies with SSE we can relay frame by
/// frame. Returns `None` unless the frame is a JSON object whose `type` is
/// `response.create`.
fn build_upstream_body(text: &str) -> Option<Value> {
    let mut value: Value = serde_json::from_str(text).ok()?;
    let obj = value.as_object_mut()?;
    if obj.get("type").and_then(Value::as_str) != Some("response.create") {
        return None;
    }
    obj.remove("type");
    obj.remove("generate");
    obj.remove("background");
    obj.insert("stream".to_string(), Value::Bool(true));
    Some(value)
}

async fn stream_sse_to_ws<S, E>(
    socket: &mut S,
    stream: impl Stream<Item = Result<axum::body::Bytes, E>> + Send,
    ledger: Option<&Mutex<WsLedger>>,
) -> (TurnTermination, Option<super::usage::RealUsage>)
where
    S: Sink<Message> + Send + Unpin,
    S::Error: Send,
    E: Send,
{
    futures::pin_mut!(stream);
    let mut buf: Vec<u8> = Vec::new();
    // Observe the relayed SSE for the real model + billed tokens (Responses
    // reports them in terminal response events). A later transport failure
    // cannot revoke an observed final measurement; partial usage stays unbooked.
    let mut scanner = Some(super::usage::Scanner::new(
        super::usage::Provider::OpenAi,
        None,
    ));
    let mut usage = None;
    while let Some(chunk) = stream.next().await {
        let Ok(chunk) = chunk else {
            let flow = send_error(
                socket,
                502,
                "upstream_error",
                "OpenAI Responses stream interrupted",
            )
            .await;
            return (
                if flow.is_break() {
                    TurnTermination::ClientDisconnected
                } else {
                    TurnTermination::StreamInterrupted
                },
                usage,
            );
        };
        if let Some(scanner) = scanner.as_mut() {
            scanner.feed(&chunk);
        }
        if scanner
            .as_ref()
            .is_some_and(super::usage::Scanner::has_terminal_usage)
        {
            usage = scanner
                .take()
                .and_then(super::usage::Scanner::finalize_terminal);
            retain_ws_usage(ledger, usage.as_ref());
        }
        buf.extend_from_slice(&chunk);
        while let Some(nl) = buf.iter().position(|&b| b == b'\n') {
            let mut line: Vec<u8> = buf.drain(..=nl).collect();
            line.pop(); // drop '\n'
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if let Some(payload) = sse_data_payload(&line)
                && socket.send(Message::Text(payload.into())).await.is_err()
            {
                return (TurnTermination::ClientDisconnected, usage);
            }
        }
    }
    // EOF allows the scanner to parse a final event without a trailing newline.
    let usage = usage.or_else(|| scanner.and_then(super::usage::Scanner::finalize));
    retain_ws_usage(ledger, usage.as_ref());
    if let Some(payload) = sse_data_payload(&buf)
        && socket.send(Message::Text(payload.into())).await.is_err()
    {
        return (TurnTermination::ClientDisconnected, usage);
    }
    (TurnTermination::CleanEof, usage)
}

fn retain_ws_usage(ledger: Option<&Mutex<WsLedger>>, usage: Option<&super::usage::RealUsage>) {
    if let (Some(ledger), Some(usage)) = (ledger, usage)
        && let Some(result) = ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .result
            .as_mut()
    {
        result.usage = Some(usage.clone());
    }
}

/// Extracts the JSON payload from an SSE `data:` line, returning `None` for SSE
/// metadata (`event:`, `id:`, comments, blank lines) and the `[DONE]` sentinel.
fn sse_data_payload(line: &[u8]) -> Option<String> {
    let line = std::str::from_utf8(line).ok()?;
    let data = line.strip_prefix("data:")?.trim();
    if data.is_empty() || data == "[DONE]" {
        return None;
    }
    Some(data.to_string())
}

/// Sends a Responses-protocol `error` event and keeps the socket open so the
/// client can retry or continue with another turn.
async fn send_error<S>(socket: &mut S, status: u16, code: &str, message: &str) -> ControlFlow<()>
where
    S: Sink<Message> + Send + Unpin,
    S::Error: Send,
{
    let event = json!({
        "type": "error",
        "status": status,
        "error": { "type": code, "message": message },
    });
    if socket
        .send(Message::Text(event.to_string().into()))
        .await
        .is_err()
    {
        return ControlFlow::Break(());
    }
    ControlFlow::Continue(())
}

/// Relays a non-2xx upstream response as a WS `error` event, preserving the
/// upstream's own `error` object when the body is JSON.
async fn relay_upstream_error<S>(socket: &mut S, status: u16, detail: &str) -> ControlFlow<()>
where
    S: Sink<Message> + Send + Unpin,
    S::Error: Send,
{
    let error_obj = serde_json::from_str::<Value>(detail)
        .ok()
        .and_then(|v| v.get("error").cloned())
        .unwrap_or_else(|| json!({ "type": "upstream_error", "message": detail }));
    let event = json!({ "type": "error", "status": status, "error": error_obj });
    if socket
        .send(Message::Text(event.to_string().into()))
        .await
        .is_err()
    {
        return ControlFlow::Break(());
    }
    ControlFlow::Continue(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Bytes;
    use futures::channel::mpsc;
    use std::sync::{Arc, Mutex, atomic::Ordering};

    fn capture_task(
        sink: mpsc::UnboundedSender<Message>,
        task: Arc<Mutex<Option<lean_ctx_protocol::TaskEnvelopeV1>>>,
    ) -> impl Sink<Message, Error = mpsc::SendError> + Send + Unpin {
        sink.with(move |message| {
            *task.lock().unwrap() = crate::core::task_spine::TaskSpine::current();
            futures::future::ready(Ok(message))
        })
    }

    const COMPLETED: &str = r#"{"type":"response.completed","response":{"model":"gpt-5.4","usage":{"input_tokens":120,"input_tokens_details":{"cached_tokens":20},"output_tokens":7}}}"#;

    fn test_state() -> ProxyState {
        let (_, upstreams) =
            tokio::sync::watch::channel(Arc::new(crate::core::config::Upstreams {
                anthropic: "http://127.0.0.1:1".to_owned(),
                openai: "http://127.0.0.1:1".to_owned(),
                chatgpt: "http://127.0.0.1:1".to_owned(),
                gemini: "http://127.0.0.1:1".to_owned(),
                providers: Vec::new(),
            }));
        ProxyState {
            client: reqwest::Client::builder().no_proxy().build().unwrap(),
            port: 0,
            stats: Arc::new(super::super::ProxyStats::default()),
            break_even: Arc::new(super::super::break_even::BreakEvenCalculator::new(1500)),
            introspect: Arc::new(super::super::introspect::IntrospectState::default()),
            ocla_cache: None,
            upstreams,
            chatgpt_cookies: super::super::chatgpt_cookies::shared_chatgpt_cloudflare_cookie_store(
            ),
            mcp_servers: Arc::new(Vec::new()),
            web_app_tracker: Arc::new(std::sync::Mutex::new(
                super::super::web_app::conversation_tracker::ConversationTracker::default(),
            )),
        }
    }

    async fn upstream(
        status: axum::http::StatusCode,
        body: String,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let app = axum::Router::new().route(
            "/v1/responses",
            axum::routing::post(move || {
                let body = body.clone();
                async move { (status, body) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{address}"), handle)
    }

    #[tokio::test]
    async fn relay_preserves_fragmented_frames_and_returns_measured_usage() {
        let (mut sink, mut messages) = mpsc::unbounded();
        let chunks: Vec<Result<Bytes, ()>> = vec![
            Ok(Bytes::from_static(
                b"event: response.created\r\ndata: {\"type\":\"response.",
            )),
            Ok(Bytes::from_static(b"created\"}\r\n\r\n")),
            Ok(Bytes::from(format!("data: {COMPLETED}"))),
        ];
        let (termination, usage) =
            stream_sse_to_ws(&mut sink, futures::stream::iter(chunks), None).await;
        assert_eq!(termination, TurnTermination::CleanEof);
        let usage = usage.unwrap();
        assert_eq!(
            (
                usage.input_tokens,
                usage.cache_read_tokens,
                usage.output_tokens
            ),
            (100, 20, 7)
        );
        assert_eq!(
            messages.next().await.unwrap().into_text().unwrap(),
            r#"{"type":"response.created"}"#
        );
        assert_eq!(
            messages.next().await.unwrap().into_text().unwrap(),
            COMPLETED
        );
    }

    #[tokio::test]
    async fn interrupted_stream_keeps_socket_open_and_retains_terminal_usage() {
        let (mut sink, mut messages) = mpsc::unbounded();
        let chunks = vec![Ok(Bytes::from(format!("data: {COMPLETED}\n"))), Err(())];
        let (termination, usage) =
            stream_sse_to_ws(&mut sink, futures::stream::iter(chunks), None).await;
        assert_eq!(termination, TurnTermination::StreamInterrupted);
        assert_eq!(usage.unwrap().output_tokens, 7);
        assert_eq!(
            messages.next().await.unwrap().into_text().unwrap(),
            COMPLETED
        );
        let frame: Value =
            serde_json::from_str(&messages.next().await.unwrap().into_text().unwrap()).unwrap();
        assert_eq!(frame["status"], 502);
        assert_eq!(frame["error"]["type"], "upstream_error");
    }

    #[tokio::test]
    async fn disconnect_including_last_unterminated_frame_is_not_clean_eof() {
        for suffix in ["\n", ""] {
            let (mut sink, messages) = mpsc::unbounded::<Message>();
            drop(messages);
            let chunks: Vec<Result<Bytes, ()>> =
                vec![Ok(Bytes::from(format!("data: {COMPLETED}{suffix}")))];
            let (termination, usage) =
                stream_sse_to_ws(&mut sink, futures::stream::iter(chunks), None).await;
            assert_eq!(termination, TurnTermination::ClientDisconnected);
            assert_eq!(usage.unwrap().output_tokens, 7);
        }
    }

    #[tokio::test]
    async fn interrupted_nonterminal_usage_stays_unbooked() {
        let (mut sink, _messages) = mpsc::unbounded();
        let partial = COMPLETED.replace("response.completed", "response.in_progress");
        let chunks = vec![Ok(Bytes::from(format!("data: {partial}\n"))), Err(())];
        let (termination, usage) =
            stream_sse_to_ws(&mut sink, futures::stream::iter(chunks), None).await;
        assert_eq!(termination, TurnTermination::StreamInterrupted);
        assert!(usage.is_none());
    }

    #[tokio::test]
    async fn invalid_frame_does_not_start_a_lifecycle_or_record_a_request() {
        let state = test_state();
        let (mut sink, mut messages) = mpsc::unbounded();
        let before = crate::core::task_spine::TaskSpine::current().map(|task| task.task_id);
        let flow = run_turn(
            &mut sink,
            &state,
            "http://127.0.0.1:1",
            "/v1/responses",
            &[],
            "not json",
            "invalid-frame-test",
        )
        .await;
        assert!(flow.is_continue());
        assert_eq!(state.stats.requests_total.load(Ordering::Relaxed), 0);
        assert_eq!(
            crate::core::task_spine::TaskSpine::current().map(|task| task.task_id),
            before
        );
        let frame: Value =
            serde_json::from_str(&messages.next().await.unwrap().into_text().unwrap()).unwrap();
        assert_eq!(frame["status"], 400);
    }

    #[tokio::test]
    async fn identical_turns_each_dispatch_and_record_once_without_acceptance() {
        let _isolation = crate::core::data_dir::isolated_data_dir();
        let state = test_state();
        let (url, server) = upstream(
            axum::http::StatusCode::OK,
            "data: {\"type\":\"response.completed\"}\n".to_owned(),
        )
        .await;
        let (sink, mut messages) = mpsc::unbounded();
        let mut ids = Vec::new();
        let captured = Arc::new(Mutex::new(None));
        let mut sink = capture_task(sink, captured.clone());
        let outer = crate::core::task_spine::TaskSpine::current().map(|task| task.task_id);
        for count in 1..=2 {
            let flow = run_turn(
                &mut sink,
                &state,
                &url,
                "/v1/responses",
                &[],
                r#"{"type":"response.create","input":[]}"#,
                "same-session",
            )
            .await;
            assert!(flow.is_continue());
            assert_eq!(state.stats.requests_total.load(Ordering::Relaxed), count);
            assert_eq!(
                crate::core::task_spine::TaskSpine::current().map(|task| task.task_id),
                outer
            );
            let task = captured.lock().unwrap().clone().unwrap();
            let outcome = ExecutionLifecycle::global()
                .outcome_for(task.task_id.as_str())
                .unwrap();
            assert_eq!(
                outcome.accepted_outcome.accepted,
                lean_ctx_protocol::AcceptanceState::Unknown
            );
            assert!(outcome.assessment.is_none());
            ids.push(task.task_id);
            assert_eq!(
                messages.next().await.unwrap().into_text().unwrap(),
                r#"{"type":"response.completed"}"#
            );
        }
        assert_ne!(
            ids[0], ids[1],
            "identical payloads must not reuse a prior turn"
        );
        server.abort();
    }

    /// Bypass path "WebSocket": a credential in a Responses WebSocket turn is
    /// masked before the turn leaves, exactly as on the HTTP rail.
    #[tokio::test]
    async fn websocket_turn_reaches_the_upstream_without_the_credential() {
        let _isolation = crate::core::data_dir::isolated_data_dir();
        let state = test_state();
        let received = Arc::new(Mutex::new(String::new()));
        let seen = received.clone();
        let app = axum::Router::new().route(
            "/v1/responses",
            axum::routing::post(move |body: String| {
                *seen.lock().unwrap() = body;
                async { "data: {\"type\":\"response.completed\"}\n" }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let credential = format!("AKIA{}", "Q3EGRZ7WSRAILX4KEY");
        let frame = serde_json::json!({
            "type": "response.create",
            "model": "gpt-5.4",
            "input": [{"role": "user", "content": [
                {"type": "input_text", "text": format!("deploy fails with {credential}")}
            ]}],
        })
        .to_string();
        let (mut sink, _messages) = mpsc::unbounded();
        let flow = run_turn(
            &mut sink,
            &state,
            &url,
            "/v1/responses",
            &[],
            &frame,
            "ws-egress",
        )
        .await;
        assert!(flow.is_continue());
        let forwarded = received.lock().unwrap().clone();
        assert!(
            forwarded.contains("deploy fails with"),
            "the turn was forwarded"
        );
        assert!(
            !forwarded.contains(&credential),
            "the credential left on the WebSocket rail"
        );
        server.abort();
    }

    #[tokio::test]
    async fn upstream_rejection_preserves_error_and_defers_stats_to_ledger() {
        let state = test_state();
        let (url, server) = upstream(
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            r#"{"error":{"type":"rate_limit_error","message":"slow down"}}"#.to_owned(),
        )
        .await;
        let (mut sink, mut messages) = mpsc::unbounded();
        let result = forward_turn(
            &mut sink,
            &state,
            &url,
            "/v1/responses",
            &[],
            json!({"input":[],"stream":true}),
            &Mutex::new(WsLedger::default()),
        )
        .await;
        assert_eq!(result.termination, TurnTermination::UpstreamRejected);
        assert!(result.flow().is_continue());
        assert!(result.usage.is_none());
        assert_eq!(state.stats.requests_total.load(Ordering::Relaxed), 0);
        let frame: Value =
            serde_json::from_str(&messages.next().await.unwrap().into_text().unwrap()).unwrap();
        assert_eq!(frame["status"], 429);
        assert_eq!(frame["error"]["type"], "rate_limit_error");
        let driver = WsTurnDriver {
            socket: &mut sink,
            state: &state,
            upstream: &url,
            path: "/v1/responses",
            headers: &[],
            document: None,
            model: "gpt-5.4".to_owned(),
            ledger: Arc::new(Mutex::new(WsLedger::default())),
        };
        let observation = driver.observe(&Ok(result));
        assert!(!observation.success);
        assert!(observation.outcome_signals.is_empty());
        assert_eq!(observation.provider, "OpenAI");
        assert_eq!(observation.model, "gpt-5.4");
        server.abort();
    }

    #[tokio::test]
    async fn upstream_rejection_finishes_unknown_without_quality_evidence() {
        let _isolation = crate::core::data_dir::isolated_data_dir();
        let state = test_state();
        let (url, server) = upstream(
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            r#"{"error":{"type":"rate_limit_error"}}"#.to_owned(),
        )
        .await;
        let (sink, _messages) = mpsc::unbounded();
        let captured = Arc::new(Mutex::new(None));
        let mut sink = capture_task(sink, captured.clone());
        let flow = run_turn(
            &mut sink,
            &state,
            &url,
            "/v1/responses",
            &[],
            r#"{"type":"response.create","input":[]}"#,
            "rejected-transport",
        )
        .await;
        assert!(flow.is_continue());
        assert_eq!(state.stats.requests_total.load(Ordering::Relaxed), 1);
        let task = captured.lock().unwrap().clone().unwrap();
        let outcome = ExecutionLifecycle::global()
            .outcome_for(task.task_id.as_str())
            .unwrap();
        assert_eq!(
            outcome.accepted_outcome.accepted,
            lean_ctx_protocol::AcceptanceState::Unknown
        );
        assert!(outcome.assessment.is_none());
        server.abort();
    }

    #[tokio::test]
    async fn ws_conversion_and_whitespace_are_not_compression_savings() {
        let _isolation = crate::core::data_dir::isolated_data_dir();
        let state = test_state();
        let (url, server) = upstream(axum::http::StatusCode::OK, String::new()).await;
        let (mut sink, _messages) = mpsc::unbounded();
        let event =
            r#"{ "type": "response.create", "generate": true, "background": false, "input": [] }"#;
        let baseline = serde_json::to_vec(&build_upstream_body(event).unwrap())
            .unwrap()
            .len() as u64;
        assert_ne!(baseline, event.len() as u64);
        assert!(
            run_turn(
                &mut sink,
                &state,
                &url,
                "/v1/responses",
                &[],
                event,
                "wire-conversion"
            )
            .await
            .is_continue()
        );
        assert_eq!(state.stats.bytes_original.load(Ordering::Relaxed), baseline);
        assert_eq!(
            state.stats.bytes_compressed.load(Ordering::Relaxed),
            baseline
        );
        assert_eq!(state.stats.tokens_saved.load(Ordering::Relaxed), 0);
        assert_eq!(state.stats.requests_compressed.load(Ordering::Relaxed), 0);
        server.abort();
    }

    #[tokio::test]
    async fn measured_usage_is_booked_once_even_when_quality_is_unknown() {
        let _isolation = crate::core::data_dir::isolated_data_dir();
        let state = test_state();
        let model = "ws-phase5-measured-fixture";
        let before = super::super::usage_meter::snapshot()
            .into_iter()
            .find(|usage| usage.model == model)
            .map_or(0, |usage| usage.requests);
        let body = format!("data: {}\n", COMPLETED.replace("gpt-5.4", model));
        let (url, server) = upstream(axum::http::StatusCode::OK, body).await;
        let (sink, _messages) = mpsc::unbounded();
        let captured = Arc::new(Mutex::new(None));
        let mut sink = capture_task(sink, captured.clone());
        let flow = run_turn(
            &mut sink,
            &state,
            &url,
            "/v1/responses",
            &[],
            r#"{"type":"response.create","input":[]}"#,
            "measured-transport",
        )
        .await;
        assert!(flow.is_continue());
        let usage = super::super::usage_meter::snapshot()
            .into_iter()
            .find(|usage| usage.model == model)
            .unwrap();
        assert_eq!(usage.requests, before + 1);
        assert_eq!(
            (
                usage.input_tokens,
                usage.cache_read_tokens,
                usage.output_tokens
            ),
            (100, 20, 7)
        );
        assert_eq!(state.stats.requests_total.load(Ordering::Relaxed), 1);
        let task = captured.lock().unwrap().clone().unwrap();
        let outcome = ExecutionLifecycle::global()
            .outcome_for(task.task_id.as_str())
            .unwrap();
        assert_eq!(
            outcome.accepted_outcome.accepted,
            lean_ctx_protocol::AcceptanceState::Unknown
        );
        assert!(outcome.assessment.is_none());
        server.abort();
    }

    #[tokio::test]
    async fn terminal_usage_reaches_lifecycle_ledger_after_upstream_stream_error() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let _isolation = crate::core::data_dir::isolated_data_dir();
        let state = test_state();
        let model = "ws-terminal-error-lifecycle";
        let before = super::super::usage_meter::snapshot()
            .into_iter()
            .find(|usage| usage.model == model)
            .map_or(0, |usage| usage.requests);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let body = format!("data: {}\n", COMPLETED.replace("gpt-5.4", model));
        let (completion_seen, completion_received) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let mut bytes = [0; 1024];
                let count = socket.read(&mut bytes).await.unwrap();
                assert!(count > 0 && request.len() < 8192);
                request.extend_from_slice(&bytes[..count]);
            }
            let wire = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{}\r\n",
                body.len(),
                body
            );
            socket.write_all(wire.as_bytes()).await.unwrap();
            socket.flush().await.unwrap();
            completion_received.await.unwrap();
            // Missing terminating zero chunk causes a real upstream body error,
            // only after the WS sink has observed the complete usage event.
        });
        let (sink, mut messages) = mpsc::unbounded();
        let captured = Arc::new(Mutex::new(None));
        let sink = capture_task(sink, captured.clone());
        let mut completion_seen = Some(completion_seen);
        let mut sink = sink.with(move |message: Message| {
            if matches!(&message, Message::Text(text) if text.contains("response.completed"))
                && let Some(signal) = completion_seen.take()
            {
                signal.send(()).unwrap();
            }
            futures::future::ready(Ok::<_, mpsc::SendError>(message))
        });
        let flow = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            run_turn(
                &mut sink,
                &state,
                &url,
                "/v1/responses",
                &[],
                r#"{"type":"response.create","input":[]}"#,
                "terminal-error-lifecycle",
            ),
        )
        .await
        .unwrap();
        assert!(flow.is_continue());
        server.await.unwrap();
        let completed: Value =
            serde_json::from_str(&messages.next().await.unwrap().into_text().unwrap()).unwrap();
        assert_eq!(completed["type"], "response.completed");
        let error: Value =
            serde_json::from_str(&messages.next().await.unwrap().into_text().unwrap()).unwrap();
        assert_eq!(error["status"], 502);
        let measured = super::super::usage_meter::snapshot()
            .into_iter()
            .find(|usage| usage.model == model)
            .unwrap();
        assert_eq!(measured.requests, before + 1);
        assert_eq!(
            (
                measured.input_tokens,
                measured.cache_read_tokens,
                measured.output_tokens
            ),
            (100, 20, 7)
        );
        assert_eq!(state.stats.requests_total.load(Ordering::Relaxed), 1);
        let task = captured.lock().unwrap().clone().unwrap();
        let outcome = ExecutionLifecycle::global()
            .outcome_for(task.task_id.as_str())
            .unwrap();
        assert_eq!(
            outcome.accepted_outcome.accepted,
            lean_ctx_protocol::AcceptanceState::Unknown
        );
        assert!(outcome.assessment.is_none());
    }

    async fn cancelled_turn_usage(terminal: bool) {
        let _isolation = crate::core::data_dir::isolated_data_dir();
        let state = test_state();
        let model = if terminal {
            "ws-cancel-terminal-evidence"
        } else {
            "ws-cancel-partial-control"
        };
        let before = super::super::usage_meter::snapshot()
            .into_iter()
            .find(|usage| usage.model == model)
            .map_or(0, |usage| usage.requests);
        let event = if terminal {
            COMPLETED.replace("gpt-5.4", model)
        } else {
            json!({"type":"response.created","response":{
                "model":model,"usage":{"input_tokens":120,"output_tokens":0}
            }})
            .to_string()
        };
        let body = Bytes::from(format!("data: {event}\n\n"));
        let app = axum::Router::new().route(
            "/v1/responses",
            axum::routing::post(move || {
                let body = body.clone();
                async move {
                    axum::body::Body::from_stream(
                        futures::stream::once(
                            async move { Ok::<_, std::convert::Infallible>(body) },
                        )
                        .chain(futures::stream::pending()),
                    )
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let (sink, mut messages) = mpsc::unbounded();
        let captured = Arc::new(Mutex::new(None));
        let sink = capture_task(sink, captured.clone());
        let (seen, received) = tokio::sync::oneshot::channel();
        let mut seen = Some(seen);
        let mut sink = sink.with(move |message: Message| {
            if let Some(signal) = seen.take() {
                signal.send(()).unwrap();
            }
            futures::future::ready(Ok::<_, mpsc::SendError>(message))
        });
        let mut turn = Box::pin(run_turn(
            &mut sink,
            &state,
            &url,
            "/v1/responses",
            &[],
            r#"{"type":"response.create","input":[]}"#,
            model,
        ));
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::select! {
                result = &mut turn => panic!("turn completed before cancellation: {result:?}"),
                observed = received => observed.unwrap(),
            }
        })
        .await
        .unwrap();
        // Cancel the actual lifecycle future while upstream remains open, after
        // its scanner and WS sink have observed the event; do not fake an error.
        drop(turn);
        let message = tokio::time::timeout(std::time::Duration::from_secs(5), messages.next())
            .await
            .unwrap()
            .unwrap();
        let relayed: Value = serde_json::from_str(&message.into_text().unwrap()).unwrap();
        assert_eq!(
            relayed["type"],
            if terminal {
                "response.completed"
            } else {
                "response.created"
            }
        );
        server.abort();
        assert!(server.await.unwrap_err().is_cancelled());
        let task = captured.lock().unwrap().clone().unwrap();
        let outcome = ExecutionLifecycle::global()
            .outcome_for(task.task_id.as_str())
            .expect("canonical guard must finalize cancellation");
        let after = super::super::usage_meter::snapshot()
            .into_iter()
            .find(|usage| usage.model == model);
        let measured_tokens = after.as_ref().map(|usage| {
            (
                usage.input_tokens,
                usage.cache_read_tokens,
                usage.output_tokens,
            )
        });
        // Report quality and measurement together: an invalid rejection must
        // not hide the independently missing provider measurement.
        assert_eq!(
            (
                outcome.accepted_outcome.accepted,
                outcome.assessment.is_none(),
                after.map_or(0, |usage| usage.requests) - before,
                measured_tokens,
            ),
            (
                lean_ctx_protocol::AcceptanceState::Unknown,
                true,
                u64::from(terminal),
                terminal.then_some((100, 20, 7)),
            ),
        );
    }

    #[tokio::test]
    async fn cancelled_turn_retains_observed_terminal_usage() {
        cancelled_turn_usage(true).await;
    }

    #[tokio::test]
    async fn cancelled_turn_does_not_book_partial_usage() {
        cancelled_turn_usage(false).await;
    }

    #[test]
    fn build_upstream_body_strips_ws_fields_and_forces_stream() {
        let event = r#"{
            "type": "response.create",
            "model": "gpt-5.5",
            "generate": false,
            "background": true,
            "input": [{"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]}]
        }"#;
        let body = build_upstream_body(event).expect("valid response.create");
        let obj = body.as_object().unwrap();
        assert!(!obj.contains_key("type"), "WS event type must be stripped");
        assert!(
            !obj.contains_key("generate"),
            "warmup hint must be stripped"
        );
        assert!(
            !obj.contains_key("background"),
            "background must be stripped"
        );
        assert_eq!(obj.get("stream"), Some(&Value::Bool(true)));
        assert_eq!(obj.get("model").and_then(Value::as_str), Some("gpt-5.5"));
        assert!(obj.contains_key("input"), "input must be preserved");
    }

    #[test]
    fn build_upstream_body_preserves_previous_response_id() {
        let event = r#"{"type":"response.create","previous_response_id":"resp_123","input":[]}"#;
        let body = build_upstream_body(event).unwrap();
        assert_eq!(
            body.get("previous_response_id").and_then(Value::as_str),
            Some("resp_123"),
            "continuation chaining must survive the bridge"
        );
    }

    #[test]
    fn build_upstream_body_rejects_non_create_events() {
        assert!(build_upstream_body(r#"{"type":"response.cancel"}"#).is_none());
        assert!(build_upstream_body("not json").is_none());
        assert!(build_upstream_body("[]").is_none());
        assert!(build_upstream_body(r#"{"input":[]}"#).is_none());
    }

    #[test]
    fn sse_data_payload_extracts_event_json() {
        assert_eq!(
            sse_data_payload(b"data: {\"type\":\"response.created\"}"),
            Some("{\"type\":\"response.created\"}".to_string())
        );
        // No space after the colon is still valid SSE.
        assert_eq!(
            sse_data_payload(b"data:{\"a\":1}"),
            Some("{\"a\":1}".to_string())
        );
    }

    #[test]
    fn sse_data_payload_ignores_metadata_and_done() {
        assert!(sse_data_payload(b"event: response.created").is_none());
        assert!(sse_data_payload(b": keep-alive comment").is_none());
        assert!(sse_data_payload(b"id: 42").is_none());
        assert!(sse_data_payload(b"").is_none());
        assert!(sse_data_payload(b"data: [DONE]").is_none());
    }
}
