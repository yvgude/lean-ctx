use axum::{
    body::Body,
    extract::State,
    http::{HeaderName, Request, StatusCode},
    response::Response,
};

use super::{ProxyState, forward, openai_responses};

/// Codex subscription model turns hit ChatGPT's Responses-compatible rail:
/// `/backend-api/codex/responses`. Forward through the same compressor/metering
/// path as OpenAI Responses, but target `https://chatgpt.com`.
pub async fn codex_responses_handler(
    State(state): State<ProxyState>,
    req: Request<Body>,
) -> Result<Response, StatusCode> {
    let upstream = state.chatgpt_upstream();
    forward::forward_request(
        State(state),
        req,
        &upstream,
        "/backend-api/codex/responses",
        openai_responses::compress_request_body,
        "OpenAI",
        &[],
    )
    .await
}

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

/// ChatGPT backend calls outside the model rail are not model JSON and must not be
/// compressed or cost-metered. They are credential-preserving passthroughs.
pub async fn backend_api_handler(
    State(state): State<ProxyState>,
    req: Request<Body>,
) -> Result<Response, StatusCode> {
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

    let mut upstream_req = state.client.request(parts.method.clone(), &url);
    for (key, value) in &parts.headers {
        if is_backend_passthrough_request_header(key) {
            upstream_req = upstream_req.header(key.clone(), value.clone());
        }
    }

    let response = upstream_req
        .body(body_bytes.to_vec())
        .send()
        .await
        .map_err(|e| {
            tracing::error!("lean-ctx proxy: ChatGPT backend upstream error: {e}");
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
            .body(Body::from_stream(response.bytes_stream()))
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
    !matches!(
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
        }));
        ProxyState {
            client: reqwest::Client::new(),
            port: 0,
            stats: Arc::new(crate::proxy::ProxyStats::default()),
            introspect: Arc::new(crate::proxy::introspect::IntrospectState::default()),
            upstreams: rx,
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

    #[tokio::test]
    async fn backend_api_streams_mcp_sse_and_preserves_session_headers() {
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
    }
}
