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

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::{HeaderMap, HeaderName, HeaderValue};
use axum::response::Response;
use futures::StreamExt;
use serde_json::{Value, json};

use super::ProxyState;

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

async fn run_turn(
    socket: &mut WebSocket,
    state: &ProxyState,
    upstream: &str,
    path: &str,
    fwd_headers: &[(HeaderName, HeaderValue)],
    text: &str,
) -> ControlFlow<()> {
    let Some(mut doc) = build_upstream_body(text) else {
        return send_error(
            socket,
            400,
            "invalid_request_error",
            "Expected a JSON `response.create` event",
        )
        .await;
    };

    let original_size = text.len();
    // Same two-stage path as the HTTP handler: cache-aware prune of the frozen
    // OLD region, then compress the recent outputs.
    let mut modified = super::openai_responses::prune_responses_input(&mut doc);
    modified |= super::openai_responses::compress_responses_input(&mut doc);
    let payload = serde_json::to_vec(&doc).unwrap_or_default();
    let compressed_size = if modified {
        payload.len()
    } else {
        original_size
    };
    state.stats.record_request(original_size, compressed_size);

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
            return send_error(
                socket,
                502,
                "upstream_error",
                "Failed to reach the OpenAI Responses upstream",
            )
            .await;
        }
    };

    let status = resp.status();
    if !status.is_success() {
        let detail = resp.text().await.unwrap_or_default();
        return relay_upstream_error(socket, status.as_u16(), &detail).await;
    }

    stream_sse_to_ws(socket, resp).await
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

async fn stream_sse_to_ws(socket: &mut WebSocket, resp: reqwest::Response) -> ControlFlow<()> {
    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    // Observe the relayed SSE for the real model + billed tokens (Responses
    // reports them in the `response.completed` event). Recorded only on a clean
    // end-of-stream, so an interrupted/aborted turn never books partial spend.
    let mut scanner = super::usage::Scanner::new(super::usage::Provider::OpenAi, None);
    while let Some(chunk) = stream.next().await {
        let Ok(chunk) = chunk else {
            return send_error(
                socket,
                502,
                "upstream_error",
                "OpenAI Responses stream interrupted",
            )
            .await;
        };
        scanner.feed(&chunk);
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
                return ControlFlow::Break(());
            }
        }
    }
    // A final event may arrive without a trailing newline.
    if let Some(payload) = sse_data_payload(&buf) {
        let _ = socket.send(Message::Text(payload.into())).await;
    }
    if let Some(usage) = scanner.finalize() {
        super::usage_meter::record(&usage);
    }
    ControlFlow::Continue(())
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
async fn send_error(
    socket: &mut WebSocket,
    status: u16,
    code: &str,
    message: &str,
) -> ControlFlow<()> {
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
async fn relay_upstream_error(
    socket: &mut WebSocket,
    status: u16,
    detail: &str,
) -> ControlFlow<()> {
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
