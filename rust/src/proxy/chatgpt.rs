use axum::{
    body::Body,
    extract::State,
    http::{HeaderName, Request, StatusCode},
    response::Response,
};

use super::admitted::{self, AdmittedBody};
use super::{ProxyState, forward, openai_responses};
use crate::core::context_admission::egress::{EgressTarget, TrustedDestinationBinding};

/// Codex subscription model turns hit ChatGPT's Responses-compatible rail:
/// `/backend-api/codex/responses`. Forward through the same compressor/metering
/// path as OpenAI Responses, but target `https://chatgpt.com`.
pub async fn codex_responses_handler(
    State(state): State<ProxyState>,
    mut req: Request<Body>,
) -> Result<Response, StatusCode> {
    // Drop the "responses-lite" marker before forwarding. Codex requests the
    // reduced lite transport on its HTTP path (`supports_websockets = false`),
    // but chatgpt.com rejects newer subscription models there
    // ("This model is not supported when using X-OpenAI-Internal-Codex-Responses-Lite",
    // seen with gpt-5.5). Stripping it makes chatgpt.com serve the full Responses
    // rail every model supports; Codex parses the full stream identically
    // (verified single- + multi-turn `previous_response_id` continuation). #623
    req.headers_mut().remove(CODEX_RESPONSES_LITE_HEADER);
    let upstream = state.chatgpt_upstream();
    forward::forward_request(
        State(state),
        req,
        &upstream,
        "/backend-api/codex/responses",
        openai_responses::compress_request_body,
        "ChatGPT",
        &[],
    )
    .await
}

/// Codex's HTTP fallback marks the reduced "responses-lite" transport with this
/// header. chatgpt.com gates newer subscription models behind the full rail, so
/// the Codex ChatGPT handler strips it (see [`codex_responses_handler`]).
const CODEX_RESPONSES_LITE_HEADER: &str = "x-openai-internal-codex-responses-lite";

/// ChatGPT's Codex rail rejects WS-only continuation fields such as
/// `previous_response_id`; ask Codex to retry through the HTTP/SSE path.
pub async fn codex_responses_ws_handler(
    State(_state): State<ProxyState>,
    _headers: axum::http::HeaderMap,
    _ws: axum::extract::ws::WebSocketUpgrade,
) -> Response {
    chatgpt_responses_ws_fallback_response()
}

fn chatgpt_responses_ws_fallback_response() -> Response {
    Response::builder()
        .status(StatusCode::UPGRADE_REQUIRED)
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"error":{"type":"unsupported_transport","message":"ChatGPT codex responses use HTTP/SSE; retry without WebSocket."}}"#,
        ))
        .expect("static response is valid")
}

/// Provider label of the ChatGPT backend rail in receipts and refusals.
pub(super) const BACKEND_PROVIDER: &str = "ChatGPT";

/// ChatGPT backend calls outside the model rail are not model JSON and must not be
/// compressed or cost-metered. They are credential-preserving passthroughs —
/// of headers. Their bodies are context leaving this machine like any other:
/// each one is admitted by the gateway, bound to the configured ChatGPT
/// upstream in its receipt, and refused with the typed reason when it may not
/// leave. An admitted body is forwarded byte for byte.
///
/// A WebSocket upgrade (Codex Desktop remote-control pairing, #597) is tunnelled
/// to chatgpt.com by [`super::chatgpt_ws`], frame by admitted frame; plain
/// HTTP/SSE falls through to the forward below.
pub async fn backend_api_handler(
    State(state): State<ProxyState>,
    req: Request<Body>,
) -> Result<Response, StatusCode> {
    if super::chatgpt_ws::is_websocket_upgrade(req.headers()) {
        return Ok(super::chatgpt_ws::passthrough(state, req).await);
    }

    let (parts, body) = req.into_parts();
    let body_bytes = axum::body::to_bytes(body, forward::max_body_bytes())
        .await
        .map_err(|_| StatusCode::PAYLOAD_TOO_LARGE)?;
    let upstream = state.chatgpt_upstream();
    let path = parts
        .uri
        .path_and_query()
        .map_or("/backend-api", axum::http::uri::PathAndQuery::as_str);
    let url = format!("{upstream}{path}");

    // The destination is the configured upstream; no caller header selects it.
    let target = EgressTarget {
        provider: BACKEND_PROVIDER,
        model: None,
        upstream_base: &upstream,
    };
    // The path, query and relayed header values leave as surely as the body
    // (AP10-C2).
    if let Err(refusal) = admitted::admit_request_target(
        path,
        parts
            .headers
            .iter()
            .filter(|(key, _)| is_backend_passthrough_request_header(key)),
        &target,
        TrustedDestinationBinding::default(),
        forward::sealed_principal(&parts).as_deref(),
    )
    .await
    {
        return Ok(refusal.response(BACKEND_PROVIDER));
    }
    let admission = admitted::admit_passthrough(
        &parts,
        &body_bytes,
        forward::max_body_bytes(),
        &target,
        TrustedDestinationBinding::default(),
    );
    // The relayed header set is bound with the body; the relay rule itself
    // (everything but hop-by-hop and this proxy's own namespace) is kept.
    let head = admitted::RequestHead::new(
        parts.method.clone(),
        parts
            .headers
            .iter()
            .filter(|(key, _)| is_backend_passthrough_request_header(key))
            .map(|(key, value)| (key.clone(), value.clone())),
    );
    let admitted_body = match admission {
        Ok((outcome, bytes)) => {
            AdmittedBody::bind(
                outcome,
                bytes,
                &upstream,
                &url,
                head,
                body_bytes.len(),
                None,
            )
            .await
        }
        Err(refusal) => Err(refusal),
    };
    let admitted_body = match admitted_body {
        Ok(admitted_body) => admitted_body,
        Err(refusal) => return Ok(refusal.response(BACKEND_PROVIDER)),
    };

    let attempt = admitted_body
        .send(&state.client, 1, |request| request)
        .await;
    let attempt = match attempt {
        Ok(attempt) => attempt,
        Err(refusal) => return Ok(refusal.response(BACKEND_PROVIDER)),
    };
    let response = attempt.response.map_err(|e| {
        tracing::error!(
            "lean-ctx proxy: ChatGPT backend upstream error: {}",
            admitted::transport_failure(&e)
        );
        StatusCode::BAD_GATEWAY
    })?;

    let status = StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::OK);
    let headers = response.headers().clone();
    let is_stream = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.contains("text/event-stream"));

    let mut out = Response::builder().status(status);
    for (key, value) in &headers {
        if is_backend_passthrough_response_header(key) {
            out = out.header(key, value);
        }
    }

    if is_stream {
        return out
            .body(Body::from_stream(super::sse_keepalive::keepalive_stream(
                Box::pin(response.bytes_stream()),
            )))
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR);
    }

    let bytes = response
        .bytes()
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;

    out.body(Body::from(bytes))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

fn is_backend_passthrough_request_header(name: &HeaderName) -> bool {
    let lower = name.as_str().to_ascii_lowercase();
    // `x-leanctx-*` is this proxy's own namespace (project path, agent and
    // session ids, the presenter capability). This relay passes everything it
    // does not name, so the whole prefix is named: a header added to the
    // namespace later is never relayed either.
    !lower.starts_with(LEANCTX_INTERNAL_HEADER_PREFIX)
        && !matches!(
            lower.as_str(),
            "host"
                | "connection"
                | "content-length"
                | "transfer-encoding"
                | "upgrade"
                | "keep-alive"
                | "proxy-authenticate"
                | "proxy-authorization"
                | "te"
                | "trailer"
                | "accept-encoding"
        )
}

/// Prefix of the headers that are addressed to this proxy and never leave it.
pub(super) const LEANCTX_INTERNAL_HEADER_PREFIX: &str = "x-leanctx-";

fn is_backend_passthrough_response_header(name: &HeaderName) -> bool {
    let lower = name.as_str().to_ascii_lowercase();
    !matches!(
        lower.as_str(),
        "connection"
            | "content-length"
            | "transfer-encoding"
            | "upgrade"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;
    use crate::core::config::Upstreams;

    fn proxy_state(chatgpt_upstream: String) -> ProxyState {
        let (_tx, rx) = tokio::sync::watch::channel(Arc::new(Upstreams {
            anthropic: "https://api.anthropic.com".into(),
            openai: "https://api.openai.com".into(),
            chatgpt: chatgpt_upstream,
            gemini: "https://generativelanguage.googleapis.com".into(),
            providers: Vec::new(),
        }));
        ProxyState {
            client: reqwest::Client::new(),
            port: 0,
            stats: Arc::new(crate::proxy::ProxyStats::default()),
            break_even: Arc::new(crate::proxy::break_even::BreakEvenCalculator::new(1500)),
            introspect: Arc::new(crate::proxy::introspect::IntrospectState::default()),
            ocla_cache: None,
            upstreams: rx,
            chatgpt_cookies: crate::proxy::chatgpt_cookies::shared_chatgpt_cloudflare_cookie_store(
            ),
            mcp_servers: Arc::new(Vec::new()),
        }
    }

    #[test]
    fn codex_responses_ws_requests_trigger_http_fallback() {
        let response = chatgpt_responses_ws_fallback_response();
        assert_eq!(response.status(), StatusCode::UPGRADE_REQUIRED);
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::CONTENT_TYPE)
                .unwrap(),
            "application/json"
        );
    }

    async fn spawn_streaming_upstream() -> (String, tokio::sync::oneshot::Receiver<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = Vec::new();
            loop {
                let mut chunk = [0_u8; 1024];
                let n = socket.read(&mut chunk).await.unwrap();
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let _ = tx.send(String::from_utf8_lossy(&buf).into_owned());
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\n\
                      content-type: text/event-stream\r\n\
                      mcp-session-id: server-session\r\n\
                      cache-control: no-cache\r\n\
                      x-custom-backend-state: passthrough\r\n\
                      \r\n\
                      event: message\n\
                      data: {\"jsonrpc\":\"2.0\"}\n\n",
                )
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_secs(2)).await;
        });
        (format!("http://{addr}"), rx)
    }

    /// The backend rail passes headers through, not unexamined content: a
    /// protected value in the body — also in a member called `name` — is
    /// masked before the body reaches the upstream.
    #[tokio::test]
    async fn backend_api_body_is_admitted_before_it_is_forwarded() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let key = concat!("AK", "IAIOSFODNN7EXAMPLE");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut chunk = [0_u8; 1024];
            let (header_end, length) = loop {
                let read = socket.read(&mut chunk).await.unwrap();
                assert!(read > 0, "upstream closed before the request headers");
                request.extend_from_slice(&chunk[..read]);
                if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
                    let length = head
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length:"))
                        .map_or(0, |value| value.trim().parse::<usize>().unwrap());
                    break (end + 4, length);
                }
            };
            while request.len() < header_end + length {
                let read = socket.read(&mut chunk).await.unwrap();
                assert!(read > 0, "upstream closed before the request body");
                request.extend_from_slice(&chunk[..read]);
            }
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}",
                )
                .await
                .unwrap();
            String::from_utf8_lossy(&request[header_end..]).into_owned()
        });
        let body = serde_json::json!({
            "note": format!("deploy with {key} today"),
            "name": key,
        });
        let req = Request::builder()
            .method("POST")
            .uri("/backend-api/wham/tasks")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();

        let response = tokio::time::timeout(
            Duration::from_secs(5),
            backend_api_handler(State(proxy_state(upstream)), req),
        )
        .await
        .expect("bounded backend request")
        .expect("backend request should succeed");
        assert_eq!(response.status(), StatusCode::OK);

        let received = server.await.unwrap();
        assert!(
            !received.contains(key),
            "the upstream received the raw value: {received}"
        );
        assert!(received.contains("deploy with"), "{received}");
    }

    #[tokio::test]
    async fn backend_api_streams_mcp_sse_and_preserves_session_headers() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let (upstream, seen_request) = spawn_streaming_upstream().await;
        let state = proxy_state(upstream);
        let req = Request::builder()
            .method("POST")
            .uri("/backend-api/ps/mcp?transport=streamable")
            .header("Authorization", "Bearer codex-token")
            .header("Mcp-Session-Id", "client-session")
            .header("Last-Event-ID", "event-7")
            .header("X-OpenAI-Product-Sku", "codex")
            .header("X-OpenAI-Internal-Codex-Residency", "us")
            .header("Originator", "codex_cli_rs")
            .header("Accept", "application/json, text/event-stream")
            // The proxy's own namespace, under a name no list knows.
            .header("X-LeanCTX-Later-Id", "internal")
            .body(Body::empty())
            .unwrap();

        let response = tokio::time::timeout(
            Duration::from_millis(500),
            backend_api_handler(State(state), req),
        )
        .await
        .expect("SSE passthrough must return after upstream headers")
        .expect("backend request should succeed");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get("mcp-session-id").unwrap(),
            "server-session"
        );
        assert_eq!(
            response.headers().get("x-custom-backend-state").unwrap(),
            "passthrough"
        );

        let request = seen_request.await.unwrap().to_ascii_lowercase();
        assert!(request.contains("post /backend-api/ps/mcp?transport=streamable http/1.1"));
        assert!(request.contains("authorization: bearer codex-token"));
        assert!(request.contains("mcp-session-id: client-session"));
        assert!(request.contains("last-event-id: event-7"));
        assert!(request.contains("x-openai-product-sku: codex"));
        assert!(request.contains("x-openai-internal-codex-residency: us"));
        assert!(request.contains("originator: codex_cli_rs"));
        assert!(
            !request.contains(LEANCTX_INTERNAL_HEADER_PREFIX),
            "no x-leanctx-* header is relayed"
        );
    }
}
