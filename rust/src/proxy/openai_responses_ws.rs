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
use super::admitted::{self, AdmittedBody, GatewayRefusal, Payload};
use crate::core::context_admission::egress::{EgressTarget, TrustedDestinationBinding};
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
    // A presenter capability shown on the upgrade is request state of every
    // turn of this connection. It is not in `fwd` (an allowlist) and so is
    // never sent upstream.
    let presenter = super::request_project::shown_presenter(headers);
    ws.on_upgrade(move |socket| {
        super::request_project::as_presenter(presenter, bridge(socket, state, upstream, path, fwd))
    })
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
    /// Project key of the folder the turn's request named, for the spend split.
    project_key: Option<String>,
}

fn record_ws_ledger(ledger: &Mutex<WsLedger>, stats: &super::ProxyStats) -> StageDisposition {
    let (result, project_key) = {
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
        (result, ledger.project_key.clone())
    };
    stats.record_provider_request("OpenAI", result.original_size, result.compressed_size);
    if let Some(usage) = result.usage.as_ref() {
        let mut usage = usage.clone();
        if project_key.is_some() {
            usage
                .wire
                .get_or_insert_with(|| {
                    Box::new(super::usage::WireContext {
                        provider: "OpenAI".to_string(),
                        ..Default::default()
                    })
                })
                .project_key = project_key;
        }
        super::usage_meter::record(&usage);
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
    // The host's own tool list of this turn, read on the frame as the host
    // sent it, before the gateway's decision or any pass changes it: the
    // only thing a recovery stub of this turn may name (see below).
    let handles = super::sticky_tools::HandleUse::for_request(Some(&doc), true);
    // G01: the gateway decides on the caller's turn before pruning,
    // compression or any derived store reads it, as on the HTTP rail.
    let model = doc.get("model").and_then(Value::as_str).map(str::to_owned);
    let target = EgressTarget {
        provider: "OpenAI",
        model: model.as_deref(),
        upstream_base: upstream,
    };
    let binding = TrustedDestinationBinding::default();
    // The caller as the upgrade presented it (its credential is relayed on
    // every turn), as the HTTP rail tells callers apart (AP10-C6).
    let presented: HeaderMap = fwd_headers.iter().cloned().collect();
    let principal = super::forward::principal_of(&presented, None);
    // The relayed upgrade header values leave with every turn (AP10-C2).
    if let Err(refusal) =
        admitted::admit_request_target(path, &presented, &target, binding, principal.as_deref())
            .await
    {
        let flow = send_gateway_refusal(socket, &refusal).await;
        return refused_turn(flow, original_size);
    }
    let mut caller =
        match admitted::admit_caller(Payload::Document(&doc), &target, binding, principal) {
            Ok(caller) => caller,
            Err(refusal) => {
                let flow = send_gateway_refusal(socket, &refusal).await;
                return refused_turn(flow, original_size);
            }
        };
    if caller.is_refused() {
        let refusal = admitted::record_refusal(caller.into_admission(), original_size, None).await;
        let flow = send_gateway_refusal(socket, &refusal).await;
        return refused_turn(flow, original_size);
    }
    if let Some(admitted_doc) = caller.take_rewritten() {
        doc = admitted_doc;
    }
    // Same two-stage path as the HTTP handler: cache-aware prune of the frozen
    // OLD region, then compress the recent outputs — of the admitted turn.
    // Both passes run observed, as on the HTTP rail: under protection a block
    // is shortened only with a recovery copy bound to its lineage (the same
    // gate inside both passes), and what stays whole for want of one is
    // counted by code. A turn has no response header to carry the codes; they
    // go into its receipt and the log line below.
    //
    // The turn is one pass. A block that leaves with a recovery stub names
    // the Engine's expand tool, and the proxy declares no tool: only a turn
    // whose own `tools` list names that tool, and whose settings let the
    // model call it, gets a stub — under the name the host registered. A
    // turn that relies on tools of an earlier turn of the connection does
    // not list them here: it hands out no stub and keeps its blocks whole.
    let mut issued = super::compress::gate::Issued::default();
    let (((pruned, compressed), mut kept_whole), _handles) =
        super::sticky_tools::within(handles, || {
            super::compress::gate::collecting(&mut issued, || {
                super::compress::observing(super::compress::Reader::Model, || {
                    super::sticky_tools::begin(super::sticky_tools::Shape::Responses, &doc);
                    let pruned = super::openai_responses::prune_responses_input(&mut doc);
                    let compressed = super::openai_responses::compress_responses_input(&mut doc);
                    (pruned, compressed)
                })
            })
        });
    // G5: final egress control on the turn that leaves; the receipt binds the
    // exact payload and upstream URL the dispatch below is limited to. A stub
    // of this turn that the decision would alter or withhold is taken back:
    // the block leaves as its original, admitted as such, and is counted.
    let (egress, readmitted) = match admitted::admit_final_with(
        Payload::Document(&doc),
        &target,
        binding,
        caller,
        !(pruned || compressed),
        |admitted| issued.restore_unadmitted(&doc, admitted, &mut kept_whole),
    ) {
        Ok(admission) => admission,
        Err(refusal) => {
            let flow = send_gateway_refusal(socket, &refusal).await;
            return refused_turn(flow, original_size);
        }
    };
    if let Some(admitted_doc) = readmitted {
        doc = admitted_doc;
    }
    if !kept_whole.is_empty() {
        // Codes and counts only: no id, path, digest or content.
        tracing::info!(
            "lean-ctx proxy: Responses WS blocks kept whole under protection, \
             no recovery copy ({})",
            kept_whole.summary()
        );
    }
    let project_key = super::project_attribution::project_key(&doc);
    let payload = serde_json::to_vec(&doc).expect("optimized JSON document serializes");
    let compressed_size = payload.len();
    let result = |termination, usage| TurnResult {
        termination,
        usage,
        original_size,
        compressed_size,
    };
    let url = format!("{upstream}{path}");
    let egress = egress.note_kept_whole(&kept_whole);
    let head = admitted::RequestHead::new(
        reqwest::Method::POST,
        [
            (
                axum::http::header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            ),
            (
                axum::http::header::ACCEPT,
                HeaderValue::from_static("text/event-stream"),
            ),
        ]
        .into_iter()
        .chain(fwd_headers.iter().cloned()),
    );
    let admitted_turn = match AdmittedBody::bind(
        egress,
        payload,
        upstream,
        &url,
        head,
        original_size,
        None,
    )
    .await
    {
        Ok(admitted_turn) => admitted_turn,
        Err(refusal) => {
            let flow = send_gateway_refusal(socket, &refusal).await;
            return refused_turn(flow, original_size);
        }
    };
    {
        let mut ledger = ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ledger.result = Some(result(TurnTermination::ClientDisconnected, None));
        ledger.project_key = project_key;
    }

    // This WebSocket-facing adapter dispatches an HTTP POST upstream. The
    // attempt is recorded before its SSE stream is consumed or relayed, and
    // it is only made while the turn's authority is still the current one.
    let attempt = admitted_turn
        .send(&state.client, 1, |request| request)
        .await;
    let response = match attempt {
        Ok(attempt) => attempt.response,
        Err(refusal) => {
            let flow = send_gateway_refusal(socket, &refusal).await;
            return refused_turn(flow, original_size);
        }
    };
    let resp = match response {
        Ok(resp) => resp,
        Err(e) => {
            tracing::error!(
                "lean-ctx proxy: OpenAI Responses WS upstream error: {}",
                admitted::transport_failure(&e)
            );
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

    // Sealed items of this turn's response are bound to the turn's context.
    let mut sealed = admitted_turn.sealed_recorder();
    let stream = resp.bytes_stream().map(move |chunk| {
        if let Ok(bytes) = &chunk {
            sealed.feed(bytes);
        }
        chunk
    });
    let (termination, usage) = stream_sse_to_ws(socket, stream, Some(ledger)).await;
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

/// Sends the gateway's typed refusal as a Responses-protocol `error` event:
/// the HTTP-equivalent status, the stable `context_gateway` error type and the
/// machine-readable reason codes. Content-free; the socket stays open.
async fn send_gateway_refusal<S>(socket: &mut S, refusal: &GatewayRefusal) -> ControlFlow<()>
where
    S: Sink<Message> + Send + Unpin,
    S::Error: Send,
{
    let detail = refusal.detail();
    let event = json!({
        "type": "error",
        "status": refusal.status().as_u16(),
        "error": {
            "type": detail["refusal"],
            "code": detail["reason_codes"][0],
            "reason_codes": detail["reason_codes"],
            "message": refusal.message(),
        },
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

/// The result of a turn the gateway did not forward.
fn refused_turn(flow: ControlFlow<()>, original_size: usize) -> TurnResult {
    TurnResult {
        termination: if flow.is_break() {
            TurnTermination::ClientDisconnected
        } else {
            TurnTermination::UpstreamRejected
        },
        usage: None,
        original_size,
        compressed_size: original_size,
    }
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
#[path = "openai_responses_ws_tests.rs"]
mod tests;
