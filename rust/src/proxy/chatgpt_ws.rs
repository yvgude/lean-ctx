//! WebSocket passthrough for ChatGPT's `/backend-api` rail (#597).
//!
//! When the Codex ChatGPT subscription opt-in is enabled, Codex's
//! `chatgpt_base_url` points at the proxy, so *every* ChatGPT backend call —
//! including Codex Desktop's **remote-control pairing**, which opens a
//! WebSocket to chatgpt.com — flows through the proxy. The HTTP/SSE
//! [`super::chatgpt::backend_api_handler`] cannot carry that: it strips the
//! `Upgrade`/`Connection` headers and never speaks the WS protocol, so pairing
//! never completed and remote control stayed broken.
//!
//! This module makes the proxy a transparent WebSocket tunnel for those calls:
//! it accepts the client upgrade, opens an upstream `wss://chatgpt.com` socket
//! (replaying the client's auth + the shared Cloudflare clearance), and relays
//! every frame verbatim in both directions. The model-turn rail
//! (`/backend-api/codex/responses`) keeps its own dedicated handlers and is
//! never reached here.

use axum::body::Body;
use axum::extract::FromRequestParts;
use axum::extract::ws::{
    CloseFrame as AxumCloseFrame, Message as AxumMessage, WebSocket, WebSocketUpgrade,
};
use axum::http::{
    HeaderMap, HeaderName, HeaderValue, Request, StatusCode, header, uri::PathAndQuery,
};
use axum::response::{IntoResponse, Response};
use futures::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message as TMessage;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::CloseFrame as TCloseFrame;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use super::ProxyState;

/// True when `headers` describe a WebSocket upgrade (`Connection: Upgrade` +
/// `Upgrade: websocket`, both case-insensitive). Lets the `/backend-api`
/// handler branch to the tunnel without consuming the request body.
pub(super) fn is_websocket_upgrade(headers: &HeaderMap) -> bool {
    let connection_upgrade = headers
        .get(header::CONNECTION)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(',')
                .any(|t| t.trim().eq_ignore_ascii_case("upgrade"))
        });
    let upgrade_websocket = headers
        .get(header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"));
    connection_upgrade && upgrade_websocket
}

/// Handshake headers tungstenite regenerates itself, plus the body framing
/// headers a WS upgrade never carries. Everything else (auth, cookies,
/// user-agent, the `x-openai-*`/`x-codex-*` identity set, subprotocol) is
/// forwarded so chatgpt.com sees the same request Codex would have sent direct.
fn is_handshake_header(name: &HeaderName) -> bool {
    matches!(
        name.as_str().to_ascii_lowercase().as_str(),
        "host"
            | "connection"
            | "upgrade"
            | "content-length"
            | "content-type"
            | "sec-websocket-key"
            | "sec-websocket-version"
            | "sec-websocket-accept"
            | "sec-websocket-extensions"
    )
}

fn capture_forward_headers(headers: &HeaderMap) -> Vec<(HeaderName, HeaderValue)> {
    headers
        .iter()
        .filter(|(name, _)| !is_handshake_header(name))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect()
}

/// `https://host` → `wss://host{path}`, `http://host` → `ws://host{path}`.
fn to_ws_url(upstream: &str, path: &str) -> Option<String> {
    let base = upstream.trim_end_matches('/');
    let ws_base = if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        return None;
    };
    Some(format!("{ws_base}{path}"))
}

/// Merge the proxy's shared Cloudflare clearance into the upstream `Cookie`
/// header so chatgpt.com does not bounce the handshake.
fn merge_cookie(headers: &mut HeaderMap, cf_cookie: &str) {
    let merged = match headers.get(header::COOKIE).and_then(|v| v.to_str().ok()) {
        Some(existing) if !existing.trim().is_empty() => format!("{existing}; {cf_cookie}"),
        _ => cf_cookie.to_string(),
    };
    if let Ok(value) = HeaderValue::from_str(&merged) {
        headers.insert(header::COOKIE, value);
    }
}

/// Accept the client WebSocket and tunnel it to chatgpt.com's `/backend-api`.
/// Returns an error response only when the request is not a valid upgrade; the
/// actual relay runs after the 101 on the upgraded connection.
pub(super) async fn passthrough(state: ProxyState, req: Request<Body>) -> Response {
    let (mut parts, _body) = req.into_parts();

    let path = parts
        .uri
        .path_and_query()
        .map_or("/backend-api", PathAndQuery::as_str)
        .to_string();
    let Some(ws_url) = to_ws_url(&state.chatgpt_upstream(), &path) else {
        return (StatusCode::BAD_GATEWAY, "invalid ChatGPT upstream").into_response();
    };

    let forwarded = capture_forward_headers(&parts.headers);
    let cf_cookie = state.chatgpt_cookie_header();

    let ws = match WebSocketUpgrade::from_request_parts(&mut parts, &state).await {
        Ok(ws) => ws,
        Err(rejection) => return rejection.into_response(),
    };

    ws.on_upgrade(move |client| async move {
        if let Err(err) = tunnel(client, ws_url, forwarded, cf_cookie).await {
            tracing::warn!("lean-ctx proxy: ChatGPT WebSocket passthrough failed: {err}");
        }
    })
}

async fn tunnel(
    client: WebSocket,
    ws_url: String,
    forwarded: Vec<(HeaderName, HeaderValue)>,
    cf_cookie: Option<String>,
) -> Result<(), tokio_tungstenite::tungstenite::Error> {
    let mut request = ws_url.into_client_request()?;
    {
        let headers = request.headers_mut();
        for (name, value) in forwarded {
            headers.insert(name, value);
        }
        if let Some(cf) = cf_cookie {
            merge_cookie(headers, &cf);
        }
    }

    let (upstream, _response) = tokio_tungstenite::connect_async(request).await?;
    relay(client, upstream).await;
    Ok(())
}

async fn relay(client: WebSocket, upstream: WebSocketStream<MaybeTlsStream<TcpStream>>) {
    let (mut client_tx, mut client_rx) = client.split();
    let (mut upstream_tx, mut upstream_rx) = upstream.split();

    let client_to_upstream = async {
        while let Some(Ok(msg)) = client_rx.next().await {
            let closing = matches!(msg, AxumMessage::Close(_));
            if upstream_tx.send(axum_to_tungstenite(msg)).await.is_err() {
                break;
            }
            if closing {
                break;
            }
        }
    };

    let upstream_to_client = async {
        while let Some(Ok(msg)) = upstream_rx.next().await {
            let Some(msg) = tungstenite_to_axum(msg) else {
                continue;
            };
            let closing = matches!(msg, AxumMessage::Close(_));
            if client_tx.send(msg).await.is_err() {
                break;
            }
            if closing {
                break;
            }
        }
    };

    // Either side closing tears down the other: dropping the unfinished future
    // releases its socket half, which closes the connection.
    tokio::select! {
        () = client_to_upstream => {},
        () = upstream_to_client => {},
    }
}

fn axum_to_tungstenite(msg: AxumMessage) -> TMessage {
    match msg {
        AxumMessage::Text(text) => TMessage::Text(text.as_str().into()),
        AxumMessage::Binary(data) => TMessage::Binary(data),
        AxumMessage::Ping(data) => TMessage::Ping(data),
        AxumMessage::Pong(data) => TMessage::Pong(data),
        AxumMessage::Close(None) => TMessage::Close(None),
        AxumMessage::Close(Some(frame)) => TMessage::Close(Some(TCloseFrame {
            code: frame.code.into(),
            reason: frame.reason.as_str().into(),
        })),
    }
}

fn tungstenite_to_axum(msg: TMessage) -> Option<AxumMessage> {
    match msg {
        TMessage::Text(text) => Some(AxumMessage::Text(text.as_str().into())),
        TMessage::Binary(data) => Some(AxumMessage::Binary(data)),
        TMessage::Ping(data) => Some(AxumMessage::Ping(data)),
        TMessage::Pong(data) => Some(AxumMessage::Pong(data)),
        TMessage::Close(None) => Some(AxumMessage::Close(None)),
        TMessage::Close(Some(frame)) => Some(AxumMessage::Close(Some(AxumCloseFrame {
            code: frame.code.into(),
            reason: frame.reason.as_str().into(),
        }))),
        // Raw frames never surface from a high-level `next()` read.
        TMessage::Frame(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_websocket_upgrade() {
        let mut headers = HeaderMap::new();
        headers.insert(header::CONNECTION, HeaderValue::from_static("Upgrade"));
        headers.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
        assert!(is_websocket_upgrade(&headers));
    }

    #[test]
    fn detects_websocket_upgrade_case_and_list_insensitive() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONNECTION,
            HeaderValue::from_static("keep-alive, Upgrade"),
        );
        headers.insert(header::UPGRADE, HeaderValue::from_static("WebSocket"));
        assert!(is_websocket_upgrade(&headers));
    }

    #[test]
    fn plain_request_is_not_an_upgrade() {
        let mut headers = HeaderMap::new();
        headers.insert(header::CONNECTION, HeaderValue::from_static("keep-alive"));
        assert!(!is_websocket_upgrade(&headers));

        let empty = HeaderMap::new();
        assert!(!is_websocket_upgrade(&empty));
    }

    #[test]
    fn to_ws_url_rewrites_scheme_and_keeps_path() {
        assert_eq!(
            to_ws_url("https://chatgpt.com", "/backend-api/wham/connect?x=1"),
            Some("wss://chatgpt.com/backend-api/wham/connect?x=1".to_string())
        );
        assert_eq!(
            to_ws_url("http://127.0.0.1:4444/", "/backend-api/ws"),
            Some("ws://127.0.0.1:4444/backend-api/ws".to_string())
        );
        assert_eq!(to_ws_url("ftp://nope", "/x"), None);
    }

    #[test]
    fn handshake_headers_are_dropped_but_auth_is_forwarded() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("127.0.0.1:4444"));
        headers.insert(header::CONNECTION, HeaderValue::from_static("Upgrade"));
        headers.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
        headers.insert(
            "sec-websocket-key",
            HeaderValue::from_static("dGhlIHNhbXBsZQ=="),
        );
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer chatgpt-token"),
        );
        headers.insert("x-codex-installation-id", HeaderValue::from_static("abc"));

        let forwarded = capture_forward_headers(&headers);
        let names: Vec<String> = forwarded
            .iter()
            .map(|(n, _)| n.as_str().to_string())
            .collect();

        assert!(names.contains(&"authorization".to_string()));
        assert!(names.contains(&"x-codex-installation-id".to_string()));
        assert!(!names.iter().any(|n| n == "host"));
        assert!(!names.iter().any(|n| n == "connection"));
        assert!(!names.iter().any(|n| n == "upgrade"));
        assert!(!names.iter().any(|n| n == "sec-websocket-key"));
    }

    #[test]
    fn merge_cookie_appends_to_existing() {
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, HeaderValue::from_static("session=abc"));
        merge_cookie(&mut headers, "cf_clearance=xyz");
        assert_eq!(
            headers.get(header::COOKIE).unwrap().to_str().unwrap(),
            "session=abc; cf_clearance=xyz"
        );
    }

    #[test]
    fn merge_cookie_sets_when_absent() {
        let mut headers = HeaderMap::new();
        merge_cookie(&mut headers, "cf_clearance=xyz");
        assert_eq!(
            headers.get(header::COOKIE).unwrap().to_str().unwrap(),
            "cf_clearance=xyz"
        );
    }

    #[test]
    fn message_conversion_round_trips() {
        let original = AxumMessage::Text("ping".into());
        let back = tungstenite_to_axum(axum_to_tungstenite(original)).unwrap();
        assert!(matches!(back, AxumMessage::Text(t) if t.as_str() == "ping"));

        let binary = AxumMessage::Binary(vec![1, 2, 3].into());
        let back = tungstenite_to_axum(axum_to_tungstenite(binary)).unwrap();
        assert!(matches!(back, AxumMessage::Binary(b) if b.as_ref() == [1, 2, 3]));
    }

    /// End-to-end: a WebSocket client → proxy `/backend-api` → upstream echo
    /// server. Proves the handshake is tunnelled and frames relay both ways,
    /// which is exactly what Codex Desktop remote-control pairing needs (#597).
    #[tokio::test]
    async fn tunnels_websocket_through_backend_api_to_upstream() {
        use std::sync::Arc;
        use std::time::Duration;

        use axum::Router;
        use axum::routing::any;
        use tokio::net::TcpListener;
        use tokio_tungstenite::tungstenite::Message;

        // Upstream echo WS server, addressed exactly like chatgpt.com would be.
        let upstream_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_addr = upstream_listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((stream, _)) = upstream_listener.accept().await {
                tokio::spawn(async move {
                    let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                    while let Some(Ok(msg)) = ws.next().await {
                        match msg {
                            Message::Text(_) | Message::Binary(_) => {
                                if ws.send(msg).await.is_err() {
                                    break;
                                }
                            }
                            Message::Close(_) => break,
                            _ => {}
                        }
                    }
                });
            }
        });

        // Proxy app: just the `/backend-api` rail, pointed at the echo upstream.
        let (_tx, rx) = tokio::sync::watch::channel(Arc::new(crate::core::config::Upstreams {
            anthropic: "https://api.anthropic.com".into(),
            openai: "https://api.openai.com".into(),
            chatgpt: format!("http://{upstream_addr}"),
            gemini: "https://generativelanguage.googleapis.com".into(),
            providers: Vec::new(),
        }));
        let state = ProxyState {
            client: reqwest::Client::new(),
            port: 0,
            stats: Arc::new(crate::proxy::ProxyStats::default()),
            introspect: Arc::new(crate::proxy::introspect::IntrospectState::default()),
            upstreams: rx,
            chatgpt_cookies: crate::proxy::chatgpt_cookies::shared_chatgpt_cloudflare_cookie_store(
            ),
            mcp_servers: Arc::new(Vec::new()),
        };
        let app = Router::new()
            .route(
                "/backend-api/{*rest}",
                any(crate::proxy::chatgpt::backend_api_handler),
            )
            .with_state(state);
        let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(proxy_listener, app).await.unwrap();
        });

        // Client connects to the proxy and round-trips a frame each way.
        let url = format!("ws://{proxy_addr}/backend-api/wham/connect");
        let (mut client, _resp) = tokio::time::timeout(
            Duration::from_secs(3),
            tokio_tungstenite::connect_async(url),
        )
        .await
        .expect("handshake must complete")
        .expect("proxy must tunnel the upgrade to the upstream");

        client
            .send(Message::Text("remote-control".into()))
            .await
            .unwrap();
        let echoed = tokio::time::timeout(Duration::from_secs(3), client.next())
            .await
            .expect("echo must arrive")
            .expect("stream open")
            .expect("valid frame");
        assert_eq!(echoed, Message::Text("remote-control".into()));

        let binary = Message::Binary(vec![9, 8, 7].into());
        client.send(binary.clone()).await.unwrap();
        let echoed = tokio::time::timeout(Duration::from_secs(3), client.next())
            .await
            .expect("binary echo must arrive")
            .expect("stream open")
            .expect("valid frame");
        assert_eq!(echoed, binary);
    }
}
