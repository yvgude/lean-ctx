//! End-to-end tests for the `/mcp/{server}` reverse proxy (GL#100).
//!
//! Real HTTP both ways: a live axum upstream speaking MCP Streamable HTTP
//! (JSON + SSE responses) behind a live proxy router — no handler internals
//! are stubbed. What these prove:
//!
//! - byte-verbatim relay for JSON and SSE bodies,
//! - credential isolation (caller key never reaches the upstream; the
//!   gateway-held `auth_env` credential does),
//! - registry misses answer 404 in JSON-RPC error shape,
//! - GET listen streams pass through.

use axum::Router;
use axum::extract::Request;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use tokio::net::TcpListener;

use crate::core::config::ResolvedMcpServer;
use crate::proxy::ProxyState;

/// A real MCP-shaped upstream: POST answers per JSON-RPC method (tools/call →
/// JSON frame, tools/list → SSE stream), GET answers an SSE listen stream.
/// It reflects the auth headers it *received* into the response payload so
/// tests can assert credential isolation over the wire.
async fn upstream_handler(req: Request) -> Response {
    let auth_seen = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let api_key_seen = req.headers().contains_key("x-api-key");

    if req.method() == axum::http::Method::GET {
        return (
            [(header::CONTENT_TYPE, "text/event-stream")],
            "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/tools/list_changed\"}\n\n",
        )
            .into_response();
    }

    let body = axum::body::to_bytes(req.into_body(), 1024 * 1024)
        .await
        .unwrap_or_default();
    let frame: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
    let method = frame["method"].as_str().unwrap_or("");
    let id = frame["id"].clone();

    if method == "tools/list" {
        // SSE response carrying the tool catalog (Streamable HTTP shape).
        let result = serde_json::json!({
            "jsonrpc": "2.0", "id": id,
            "result": { "tools": [
                { "name": "get_issue", "inputSchema": { "type": "object" } }
            ]}
        });
        (
            [(header::CONTENT_TYPE, "text/event-stream")],
            format!("event: message\ndata: {result}\n\n"),
        )
            .into_response()
    } else {
        let result = serde_json::json!({
            "jsonrpc": "2.0", "id": id,
            "result": {
                "content": [{ "type": "text", "text": "issue #42 body" }],
                "_authSeenByUpstream": auth_seen,
                "_apiKeySeenByUpstream": api_key_seen,
            }
        });
        (
            [(header::CONTENT_TYPE, "application/json")],
            result.to_string(),
        )
            .into_response()
    }
}

/// Boots upstream + proxy on ephemeral loopback ports and returns the proxy
/// base URL for `/mcp/{id}` calls.
async fn spawn_gateway(registry: Vec<ResolvedMcpServer>) -> String {
    let upstream_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream_listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            upstream_listener,
            Router::new().fallback(any(upstream_handler)),
        )
        .await
        .unwrap();
    });

    // Point every registry entry at the live upstream.
    let servers: Vec<ResolvedMcpServer> = registry
        .into_iter()
        .map(|mut s| {
            s.url = format!("http://{upstream_addr}/mcp");
            s
        })
        .collect();

    let state = ProxyState::for_tests(servers);
    let app = Router::new()
        .route("/mcp/{server}", any(super::proxy::handler))
        .with_state(state);
    let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(proxy_listener, app).await.unwrap();
    });
    format!("http://{proxy_addr}")
}

fn registry_entry(id: &str, auth_env: Option<&str>) -> ResolvedMcpServer {
    ResolvedMcpServer {
        id: id.into(),
        url: String::new(), // filled by spawn_gateway
        auth_env: auth_env.map(str::to_string),
    }
}

/// tools/call over JSON: body relays verbatim; the caller's gateway key is
/// stripped and the gateway-held upstream credential is injected — asserted
/// from what the upstream actually received, over real sockets.
///
/// The env lock is intentionally held across `.await`s to keep `LC_E2E_*`
/// isolated for the whole test — the documented pattern from
/// `proxy::upstream_tests` (each `#[tokio::test]` owns its runtime, so the
/// std guard can only make other test threads wait, never deadlock this one).
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn tools_call_roundtrip_isolates_credentials() {
    let _lock = crate::core::data_dir::test_env_lock();
    crate::test_env::set_var("LC_E2E_MCP_TOKEN", "upstream-secret");
    let base = spawn_gateway(vec![registry_entry("github", Some("LC_E2E_MCP_TOKEN"))]).await;

    let resp = reqwest::Client::new()
        .post(format!("{base}/mcp/github"))
        .header("authorization", "Bearer gk-alice-personal-key")
        .header("x-api-key", "sk-should-never-forward")
        .header("content-type", "application/json")
        .body(r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"get_issue","arguments":{"n":42}}}"#)
        .send()
        .await
        .expect("proxy reachable");
    crate::test_env::remove_var("LC_E2E_MCP_TOKEN");

    assert_eq!(resp.status(), 200);
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["id"], 7, "JSON-RPC frame must relay verbatim");
    assert_eq!(v["result"]["content"][0]["text"], "issue #42 body");
    assert_eq!(
        v["result"]["_authSeenByUpstream"], "Bearer upstream-secret",
        "gateway must inject the env credential upstream"
    );
    assert_eq!(
        v["result"]["_apiKeySeenByUpstream"], false,
        "caller x-api-key must never reach the tool server"
    );
}

/// tools/list over SSE: the streamed body arrives byte-verbatim through the
/// tee, and no Authorization header is invented when auth_env is absent.
#[tokio::test]
async fn tools_list_sse_stream_relays_verbatim() {
    let base = spawn_gateway(vec![registry_entry("docs", None)]).await;

    let resp = reqwest::Client::new()
        .post(format!("{base}/mcp/docs"))
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .body(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#)
        .send()
        .await
        .expect("proxy reachable");

    assert_eq!(resp.status(), 200);
    assert!(
        resp.headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| ct.starts_with("text/event-stream")),
        "content-type must pass through"
    );
    let body = resp.text().await.unwrap();
    assert!(
        body.starts_with("event: message\ndata: "),
        "SSE framing intact"
    );
    assert!(body.contains("\"get_issue\""), "tool catalog relayed");
    assert!(body.ends_with("\n\n"), "event terminator intact");
}

/// Unknown ids never leave the gateway: 404 in JSON-RPC error shape.
#[tokio::test]
async fn unknown_server_id_is_a_json_rpc_404() {
    let base = spawn_gateway(vec![registry_entry("github", None)]).await;

    let resp = reqwest::Client::new()
        .post(format!("{base}/mcp/not-registered"))
        .header("content-type", "application/json")
        .body(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#)
        .send()
        .await
        .expect("proxy reachable");

    assert_eq!(resp.status(), 404);
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["jsonrpc"], "2.0");
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not-registered"),
        "error names the unknown id"
    );
}

/// GET opens the server-push listen stream — pure passthrough.
#[tokio::test]
async fn get_listen_stream_passes_through() {
    let base = spawn_gateway(vec![registry_entry("github", None)]).await;

    let resp = reqwest::Client::new()
        .get(format!("{base}/mcp/github"))
        .header("accept", "text/event-stream")
        .send()
        .await
        .expect("proxy reachable");

    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(body.contains("notifications/tools/list_changed"));
}

/// A registered-but-down upstream answers 502 (fail-open contract: the
/// gateway explains, it never hangs).
#[tokio::test]
async fn dead_upstream_answers_502() {
    // Bind-then-drop to get a port that is guaranteed closed right now.
    let dead = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead_addr = dead.local_addr().unwrap();
    drop(dead);

    let state = ProxyState::for_tests(vec![ResolvedMcpServer {
        id: "down".into(),
        url: format!("http://{dead_addr}/mcp"),
        auth_env: None,
    }]);
    let app = Router::new()
        .route("/mcp/{server}", any(super::proxy::handler))
        .with_state(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/mcp/down"))
        .header("content-type", "application/json")
        .body(r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"x"}}"#)
        .send()
        .await
        .expect("proxy reachable");
    assert_eq!(resp.status(), 502);
    let v: serde_json::Value = resp.json().await.unwrap();
    assert!(v["error"]["message"].as_str().unwrap().contains("down"));
}
