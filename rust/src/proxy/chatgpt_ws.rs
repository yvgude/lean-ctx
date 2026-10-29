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
use super::admitted::{self, GatewayRefusal, Payload, TextAdmission};
use super::chatgpt::BACKEND_PROVIDER;
use crate::core::context_admission::egress::{EgressTarget, TrustedDestinationBinding};

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
    let lower = name.as_str().to_ascii_lowercase();
    // Not handshake headers, but never forwarded either: `x-leanctx-*` is
    // this proxy's own namespace (project path, ids, presenter capability),
    // and this tunnel passes everything it does not name — so the whole
    // prefix is named, not a list that can go stale.
    lower.starts_with(super::chatgpt::LEANCTX_INTERNAL_HEADER_PREFIX)
        || matches!(
            lower.as_str(),
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
    } else {
        let rest = base.strip_prefix("http://")?;
        format!("ws://{rest}")
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
    let upstream_base = state.chatgpt_upstream();
    let Some(ws_url) = to_ws_url(&upstream_base, &path) else {
        return (StatusCode::BAD_GATEWAY, "invalid ChatGPT upstream").into_response();
    };
    // The caller as the upgrade presented it: every frame is admitted for it.
    let principal = super::forward::sealed_principal(&parts);
    // The handshake's relayed headers, final before anything is decided:
    // the channel's receipt binds them (AP10-C2).
    let mut handshake = HeaderMap::new();
    for (name, value) in capture_forward_headers(&parts.headers) {
        handshake.insert(name, value);
    }
    if let Some(cf_cookie) = state.chatgpt_cookie_header() {
        merge_cookie(&mut handshake, &cf_cookie);
    }
    // The tunnel is an egress channel to the configured upstream. Before the
    // upgrade is accepted or anything connects, the path, query and relayed
    // header values the handshake carries are admitted (AP10-C2) — first, so
    // a refused target leaves no prepared channel receipt behind — and then
    // the placement rules, in the receipt that binds the handshake.
    let target = tunnel_target(&upstream_base);
    let head = admitted::RequestHead::new(
        axum::http::Method::GET,
        handshake
            .iter()
            .map(|(name, value)| (name.clone(), value.clone())),
    );
    let decided = async {
        admitted::admit_request_target(
            &path,
            &handshake,
            &target,
            TrustedDestinationBinding::default(),
            principal.as_deref(),
        )
        .await?;
        admitted::admit_channel(
            &target,
            TrustedDestinationBinding::default(),
            principal.as_deref(),
            &ws_url,
            &head,
        )
        .await
    };
    if let Err(refusal) = decided.await {
        return refusal.response(BACKEND_PROVIDER);
    }

    let ws = match WebSocketUpgrade::from_request_parts(&mut parts, &state).await {
        Ok(ws) => ws,
        Err(rejection) => return rejection.into_response(),
    };

    ws.on_upgrade(move |client| async move {
        let tunnelled = tunnel(client, ws_url, &upstream_base, principal, handshake);
        if let Err(err) = tunnelled.await {
            tracing::warn!(
                "lean-ctx proxy: ChatGPT WebSocket passthrough failed: {}",
                handshake_failure(&err)
            );
        }
    })
}

/// The kind of a failed upstream handshake, for logs (AP10-R4). A
/// tungstenite error can print the request URL with its query, or the
/// upstream's HTTP response; the log line names the kind only.
fn handshake_failure(error: &tokio_tungstenite::tungstenite::Error) -> &'static str {
    use tokio_tungstenite::tungstenite::Error;
    match error {
        Error::Io(_) => "connection failure",
        Error::Url(_) => "invalid upstream url",
        Error::Http(response) if response.status().is_client_error() => "upstream refused (4xx)",
        Error::Http(_) => "upstream refused",
        Error::HttpFormat(_) => "invalid handshake request",
        Error::ConnectionClosed | Error::AlreadyClosed => "connection closed",
        _ => "protocol failure",
    }
}

fn tunnel_target(upstream_base: &str) -> EgressTarget<'_> {
    EgressTarget {
        provider: BACKEND_PROVIDER,
        model: None,
        upstream_base,
    }
}

/// A client frame the gateway admitted for the tunnel's upstream, with the
/// receipt it is bound to (`None`: a control frame, which carries no
/// context). The relay can only send these.
struct AdmittedFrame(TMessage, Option<admitted::BoundDelivery>);

impl AdmittedFrame {
    /// Hand the frame to the upstream socket: the authority it was admitted
    /// under is proved immediately before the send (AP10-R1), and the send is
    /// observed against the frame's receipt after it (AP10-C7). `Ok(false)`:
    /// the socket is gone.
    async fn send<S>(self, upstream_tx: &mut S) -> Result<bool, GatewayRefusal>
    where
        S: futures::Sink<TMessage> + Unpin,
    {
        let Self(message, bound) = self;
        if let Some(bound) = &bound {
            bound.verify_current()?;
        }
        let sent = upstream_tx.send(message).await.is_ok();
        if let Some(bound) = &bound {
            bound.observe_frame(sent).await?;
        }
        Ok(sent)
    }
}

/// Admit one client frame before it leaves. Text is admitted as one object
/// and forwarded unchanged or masked — one frame in, one frame out. A binary
/// frame cannot be inspected: it is an opaque payload, forwarded marked as
/// such in developer mode and refused in governed and sovereign mode.
/// Control frames carry no context. Each admitted frame is bound to its own
/// receipt under the authority current at that moment.
async fn admit_frame(
    msg: AxumMessage,
    upstream_base: &str,
    principal: Option<&str>,
) -> Result<AdmittedFrame, GatewayRefusal> {
    let target = tunnel_target(upstream_base);
    let binding = TrustedDestinationBinding::default();
    match msg {
        AxumMessage::Text(text) if !text.is_empty() => {
            let TextAdmission {
                admission,
                rewritten,
            } = admitted::admit_text(text.as_str(), &target, binding, principal)?;
            let delivered = rewritten.unwrap_or_else(|| text.as_str().to_owned());
            let bound =
                admitted::bind_delivery(admission, delivered.as_bytes(), text.len()).await?;
            Ok(AdmittedFrame(
                TMessage::Text(delivered.as_str().into()),
                Some(bound),
            ))
        }
        AxumMessage::Binary(data) if !data.is_empty() => {
            let admission =
                admitted::admit_as(Payload::Opaque(data.len()), &target, binding, principal)?;
            let bound = admitted::bind_delivery(admission, &data, data.len()).await?;
            Ok(AdmittedFrame(TMessage::Binary(data), Some(bound)))
        }
        other => Ok(AdmittedFrame(axum_to_tungstenite(other), None)),
    }
}

async fn tunnel(
    client: WebSocket,
    ws_url: String,
    upstream_base: &str,
    principal: Option<String>,
    handshake: HeaderMap,
) -> Result<(), tokio_tungstenite::tungstenite::Error> {
    let mut request = ws_url.into_client_request()?;
    // Exactly the bound header set, beside what the protocol generates.
    request.headers_mut().extend(handshake);

    let (upstream, _response) = tokio_tungstenite::connect_async(request).await?;
    relay(client, upstream, upstream_base, principal.as_deref()).await;
    Ok(())
}

/// RFC 6455 close code for a message that violates the endpoint's policy.
const CLOSE_POLICY_VIOLATION: u16 = 1008;

async fn relay(
    client: WebSocket,
    upstream: WebSocketStream<MaybeTlsStream<TcpStream>>,
    upstream_base: &str,
    principal: Option<&str>,
) {
    let (mut client_tx, mut client_rx) = client.split();
    let (mut upstream_tx, mut upstream_rx) = upstream.split();

    // Ends with the refusal when the gateway did not admit a client frame;
    // that frame and everything after it stays on this machine.
    let client_to_upstream = async {
        while let Some(Ok(msg)) = client_rx.next().await {
            let closing = matches!(msg, AxumMessage::Close(_));
            let sent = match admit_frame(msg, upstream_base, principal).await {
                Ok(frame) => frame.send(&mut upstream_tx).await,
                Err(refusal) => Err(refusal),
            };
            match sent {
                Ok(true) => {}
                Ok(false) => break,
                Err(refusal) => return Some(refusal),
            }
            if closing {
                break;
            }
        }
        None
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
    let refusal = tokio::select! {
        refusal = client_to_upstream => refusal,
        () = upstream_to_client => None,
    };
    if let Some(refusal) = refusal {
        // Tell the client why the tunnel ends: a typed, content-free close.
        let _ = client_tx
            .send(AxumMessage::Close(Some(AxumCloseFrame {
                code: CLOSE_POLICY_VIOLATION,
                reason: refusal.close_reason().as_str().into(),
            })))
            .await;
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
        // The proxy's own namespace, in a spelling a client might send and
        // with a name no list in this file knows.
        for internal in ["X-LeanCTX-Project", "x-leanctx-agent-id", "x-leanctx-later"] {
            headers.insert(
                HeaderName::from_bytes(internal.as_bytes()).expect("header name"),
                HeaderValue::from_static("internal"),
            );
        }

        let forwarded = capture_forward_headers(&headers);
        assert!(
            !forwarded
                .iter()
                .any(|(name, _)| name.as_str().starts_with("x-leanctx-")),
            "no x-leanctx-* header leaves the proxy"
        );
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

    use std::time::Duration;
    use tokio_tungstenite::tungstenite::Message;

    /// A WebSocket client connected through the proxy's `/backend-api` rail
    /// to an upstream that echoes every text and binary frame it receives.
    async fn client_through_tunnel() -> WebSocketStream<MaybeTlsStream<TcpStream>> {
        use std::sync::Arc;

        use axum::Router;
        use axum::routing::any;
        use tokio::net::TcpListener;

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
            break_even: Arc::new(crate::proxy::break_even::BreakEvenCalculator::new(1500)),
            introspect: Arc::new(crate::proxy::introspect::IntrospectState::default()),
            ocla_cache: None,
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

        let url = format!("ws://{proxy_addr}/backend-api/wham/connect");
        tokio::time::timeout(
            Duration::from_secs(3),
            tokio_tungstenite::connect_async(url),
        )
        .await
        .expect("handshake must complete")
        .expect("proxy must tunnel the upgrade to the upstream")
        .0
    }

    async fn next_frame(client: &mut WebSocketStream<MaybeTlsStream<TcpStream>>) -> Message {
        tokio::time::timeout(Duration::from_secs(3), client.next())
            .await
            .expect("a frame must arrive")
            .expect("stream open")
            .expect("valid frame")
    }

    /// End-to-end: a WebSocket client → proxy `/backend-api` → upstream echo
    /// server. Proves the handshake is tunnelled and frames relay both ways,
    /// which is exactly what Codex Desktop remote-control pairing needs (#597).
    #[tokio::test]
    async fn tunnels_websocket_through_backend_api_to_upstream() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        // Client connects to the proxy and round-trips a frame each way.
        let mut client = client_through_tunnel().await;

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

    /// The tunnel is an egress carrier (P1): a text frame leaves masked, in
    /// one frame, and a frame the gateway cannot inspect ends the tunnel with
    /// a typed close in governed mode instead of reaching the upstream.
    #[tokio::test]
    async fn tunnelled_frames_are_admitted_before_they_leave() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let config = crate::core::paths::config_dir_read_only().expect("config directory");
        std::fs::create_dir_all(&config).expect("config directory");
        std::fs::write(
            config.join("config.toml"),
            "[context_gateway]\nmode = \"governed\"\n",
        )
        .expect("governed config");
        let key = concat!("AK", "IAIOSFODNN7EXAMPLE");
        let mut client = client_through_tunnel().await;

        client
            .send(Message::Text(format!("pair with {key} now").into()))
            .await
            .unwrap();
        let Message::Text(echoed) = next_frame(&mut client).await else {
            panic!("the upstream echoes one text frame per text frame");
        };
        assert!(
            !echoed.as_str().contains(key),
            "the upstream received the raw value: {echoed}"
        );
        assert!(echoed.as_str().contains("pair with"), "{echoed}");

        client
            .send(Message::Binary(vec![0xff, 0x00, 0x7f].into()))
            .await
            .unwrap();
        let Message::Close(Some(close)) = next_frame(&mut client).await else {
            panic!("an uninspectable frame must end the tunnel, not be echoed by the upstream");
        };
        assert_eq!(u16::from(close.code), CLOSE_POLICY_VIOLATION);
        assert!(
            close.reason.as_str().contains("egress.opaque_payload"),
            "the close names its reason: {}",
            close.reason
        );
    }

    fn governed(extra: &str) {
        let config = crate::core::paths::config_dir_read_only().expect("config directory");
        std::fs::create_dir_all(&config).expect("config directory");
        std::fs::write(
            config.join("config.toml"),
            format!("[context_gateway]\nmode = \"governed\"\n{extra}"),
        )
        .expect("governed config");
    }

    /// AP10-R2 (candidate, not reproduced): the tunnel admits with the
    /// default destination binding, which carries no account or region. A
    /// placement rule that names an account and region for this very
    /// provider must therefore refuse the channel and every frame, never
    /// pass them for lack of a binding to compare.
    #[tokio::test]
    async fn an_account_and_region_rule_refuses_the_unbound_tunnel() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let config = crate::core::paths::config_dir_read_only().expect("config directory");
        std::fs::create_dir_all(&config).expect("config directory");
        std::fs::write(
            config.join("config.toml"),
            "[[context_gateway.allowed_destinations]]\nprovider = \"chatgpt\"\n\
             locality = \"remote\"\norganization_managed = false\n\
             account_ref = \"production-account\"\nregion = \"eu-west-1\"\n",
        )
        .expect("destination allow-list config");
        let upstream = "https://chatgpt.com";
        let head = admitted::RequestHead::new(axum::http::Method::GET, []);
        let refused = admitted::admit_channel(
            &tunnel_target(upstream),
            TrustedDestinationBinding::default(),
            None,
            "wss://chatgpt.com/backend-api/wham/connect",
            &head,
        )
        .await
        .expect_err("no account or region is bound");
        assert!(
            refused
                .close_reason()
                .contains("destination.constraint_unsatisfied"),
            "{refused:?}"
        );
        let Err(refused) = admit_frame(AxumMessage::Text("pair".into()), upstream, None).await
        else {
            panic!("a frame left for an unbound destination");
        };
        assert!(
            refused
                .close_reason()
                .contains("destination.constraint_unsatisfied")
        );
    }

    /// AP10-R1: a frame was sealed under one policy and handed to the socket
    /// after an await, with no proof in between — the policy could change
    /// there and the frame still left. Sealed, then the policy changes, then
    /// the send: refused, and the socket received nothing.
    #[tokio::test]
    async fn a_frame_sealed_under_an_old_policy_is_not_sent() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        governed("");
        let frame = admit_frame(
            AxumMessage::Text("pairing frame".into()),
            "http://127.0.0.1:9",
            None,
        )
        .await
        .expect("admitted under the current policy");
        governed("secrets = \"block\"\n");
        let (mut upstream, mut received) = futures::channel::mpsc::unbounded::<TMessage>();
        let refusal = frame
            .send(&mut upstream)
            .await
            .expect_err("the authority is no longer current");
        assert!(
            refusal.close_reason().contains("policy.changed"),
            "{refusal:?}"
        );
        assert!(received.try_recv().is_err(), "nothing reached the socket");
    }

    /// AP10-C7: a tunnelled frame left without any transport observation.
    /// Now each send is observed against the frame's receipt; where the
    /// receipt is owed and the observation cannot be saved, the channel
    /// refuses to go on.
    #[tokio::test]
    async fn an_owed_frame_observation_that_cannot_be_saved_ends_the_channel() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        governed("");
        let observations = crate::core::data_dir::lean_ctx_data_dir()
            .expect("data dir")
            .join("gateway/egress/observations");
        std::fs::create_dir_all(observations.parent().expect("egress dir")).expect("egress dir");
        std::fs::write(&observations, b"obstructed").expect("obstruct the observation store");
        let (mut upstream, mut received) = futures::channel::mpsc::unbounded::<TMessage>();

        let frame = admit_frame(
            AxumMessage::Text("first".into()),
            "http://127.0.0.1:9",
            None,
        )
        .await
        .expect("admitted");
        let refusal = frame
            .send(&mut upstream)
            .await
            .expect_err("no evidence, no further frames");
        assert!(
            refusal
                .close_reason()
                .contains("receipt.observation_unavailable"),
            "{refusal:?}"
        );
        assert!(received.try_recv().is_ok(), "the frame itself had left");

        std::fs::remove_file(&observations).expect("clear the obstruction");
        let frame = admit_frame(
            AxumMessage::Text("second".into()),
            "http://127.0.0.1:9",
            None,
        )
        .await
        .expect("admitted");
        assert_eq!(frame.send(&mut upstream).await, Ok(true));
        assert!(observations.is_dir(), "the send was observed");
    }
}
