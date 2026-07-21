//! End-to-end test for the MCP Tool-Catalog Gateway client (#210).
//!
//! Two layers, no mocks:
//! 1. In-process: a *real* rmcp MCP server over a Tokio duplex pipe, exercising
//!    the MCP protocol (`initialize` → `tools/list` → `tools/call`) for
//!    determinism.
//! 2. Real stdio (#1077): spawns an actual child process (a Node.js MCP fixture)
//!    through the gateway's production `open`/`fetch_tools`/`proxy_call` path —
//!    the spawn + handshake + transport the in-process test cannot cover — and
//!    asserts the [`pool`] reuses one session across calls (#1078). Skips
//!    cleanly when `node` is unavailable.

use std::collections::BTreeMap;
use std::future::Future;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use rmcp::model::{
    CallToolRequestParams, CallToolResult, ContentBlock, ListToolsResult, PaginatedRequestParams,
    ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt};
use serde_json::json;
use serial_test::serial;

use lean_ctx::core::mcp_catalog::{ResolvedTransport, client, pool};

/// Minimal downstream MCP server exposing two tools: `echo` and `add`.
struct EchoServer;

impl ServerHandler for EchoServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, ErrorData>> {
        let echo = Tool::new(
            "echo",
            "Echo back the provided text",
            Arc::new(
                json!({
                    "type": "object",
                    "properties": { "text": { "type": "string" } },
                    "required": ["text"]
                })
                .as_object()
                .unwrap()
                .clone(),
            ),
        );
        let add = Tool::new(
            "add",
            "Add two integers a and b and return the sum",
            Arc::new(
                json!({
                    "type": "object",
                    "properties": {
                        "a": { "type": "integer" },
                        "b": { "type": "integer" }
                    },
                    "required": ["a", "b"]
                })
                .as_object()
                .unwrap()
                .clone(),
            ),
        );
        std::future::ready(Ok(ListToolsResult {
            tools: vec![echo, add],
            ..Default::default()
        }))
    }

    fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResult, ErrorData>> {
        let args = request.arguments.unwrap_or_default();
        std::future::ready(match request.name.as_ref() {
            "echo" => {
                let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("");
                Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                    "echo:{text}"
                ))]))
            }
            "add" => {
                let a = args
                    .get("a")
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or(0);
                let b = args
                    .get("b")
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or(0);
                Ok(CallToolResult::success(vec![ContentBlock::text(
                    (a + b).to_string(),
                )]))
            }
            other => Err(ErrorData::invalid_params(
                format!("unknown tool: {other}"),
                None,
            )),
        })
    }
}

/// Connect a gateway client to an in-process `EchoServer` and return the
/// running client session.
async fn connect_to_echo_server() -> client::ClientService {
    let (server_transport, client_transport) = tokio::io::duplex(8192);
    tokio::spawn(async move {
        if let Ok(server) = EchoServer.serve(server_transport).await {
            let _ = server.waiting().await;
        }
    });
    ().serve(client_transport)
        .await
        .expect("client initialize handshake")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gateway_client_lists_downstream_tools() {
    let service = connect_to_echo_server().await;
    let timeout = Duration::from_secs(5);

    let tools = client::list_tools_on(&service, timeout)
        .await
        .expect("list tools");
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();

    assert!(names.contains(&"echo"), "expected echo tool, got {names:?}");
    assert!(names.contains(&"add"), "expected add tool, got {names:?}");

    let _ = service.cancel().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gateway_client_proxies_a_call() {
    let service = connect_to_echo_server().await;
    let timeout = Duration::from_secs(5);

    let mut args = serde_json::Map::new();
    args.insert("a".into(), json!(2));
    args.insert("b".into(), json!(3));
    let result = client::call_tool_on(&service, "add", args, timeout)
        .await
        .expect("call add");
    assert_eq!(client::result_to_text(&result).trim(), "5");

    let mut echo_args = serde_json::Map::new();
    echo_args.insert("text".into(), json!("hello"));
    let echoed = client::call_tool_on(&service, "echo", echo_args, timeout)
        .await
        .expect("call echo");
    assert_eq!(client::result_to_text(&echoed).trim(), "echo:hello");

    let _ = service.cancel().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gateway_client_reports_unknown_tool_error() {
    let service = connect_to_echo_server().await;
    let timeout = Duration::from_secs(5);

    // The downstream returns a protocol error for an unknown tool; our client
    // surfaces it as an Err rather than panicking.
    let res =
        client::call_tool_on(&service, "does_not_exist", serde_json::Map::new(), timeout).await;
    assert!(res.is_err(), "unknown tool should error, got {res:?}");

    let _ = service.cancel().await;
}

/// Whether a `node` runtime is on PATH, so the real-stdio test can skip cleanly
/// in environments without it (the test asserts nothing when Node is absent).
fn node_available() -> bool {
    Command::new("node")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Resolved transport that spawns the Node.js MCP fixture over real stdio.
fn fixture_transport() -> ResolvedTransport {
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/mcp_stdio_echo.mjs"
    );
    ResolvedTransport::Stdio {
        command: "node".into(),
        args: vec![fixture.to_string()],
        env: BTreeMap::new(),
        binary_sha256: String::new(),
        capabilities: None,
    }
}

/// #1077 + #1078: drive the *real* stdio spawn path end to end — spawn a child
/// process, run the MCP handshake, `tools/list`, then `tools/call` — and confirm
/// the session pool reuses one live child across both operations instead of
/// respawning per call.
// Serialized: both real-stdio tests drive the *global* session `pool` against
// the *same* fixture wiring (same key) using `clear`/`len`, so the default
// concurrent harness would let one test's reset corrupt the other's session.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial(gateway_stdio_pool)]
async fn gateway_spawns_real_stdio_server_and_reuses_one_pooled_session() {
    if !node_available() {
        eprintln!("skipping gateway stdio E2E: `node` is not available on PATH");
        return;
    }

    pool::clear();
    let transport = fixture_transport();
    let timeout = if cfg!(windows) {
        Duration::from_secs(30)
    } else {
        Duration::from_secs(15)
    };

    // Real spawn → initialize → tools/list.
    let tools = client::fetch_tools(&transport, timeout)
        .await
        .expect("fetch_tools over real stdio");
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    assert!(names.contains(&"echo"), "expected echo tool, got {names:?}");

    // Real proxy_call over the same pooled session.
    let mut args = serde_json::Map::new();
    args.insert("text".into(), json!("hi"));
    let result = client::proxy_call(&transport, "echo", args, timeout)
        .await
        .expect("proxy_call over real stdio");
    assert_eq!(client::result_to_text(&result).trim(), "echo:hi");

    // The list + the call shared a single pooled child (no per-call respawn).
    assert_eq!(
        pool::len(),
        1,
        "expected exactly one pooled session reused across list + call"
    );

    // Tear down: closes the child's stdin so the fixture exits.
    pool::clear();
}

/// #1078: when a pooled child dies mid-call, the call surfaces an error *once*
/// (no blind retry — a tool may be non-idempotent), the broken session is
/// evicted, and the very next call transparently reopens a fresh child. Drives
/// the real stdio path with a fault-injecting `boom` tool that exits without
/// replying.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial(gateway_stdio_pool)]
async fn gateway_pool_evicts_a_dead_session_and_reopens_on_next_call() {
    if !node_available() {
        eprintln!("skipping gateway stdio self-heal E2E: `node` is not available on PATH");
        return;
    }

    pool::clear();
    let transport = fixture_transport();
    let timeout = if cfg!(windows) {
        Duration::from_secs(30)
    } else {
        Duration::from_secs(15)
    };

    // Warm one pooled session.
    let _ = client::fetch_tools(&transport, timeout)
        .await
        .expect("warm the pool");
    assert_eq!(pool::len(), 1, "one warmed session");

    // `boom` kills the child without answering: the call must fail (not hang,
    // not loop) and the dead session must be evicted rather than reused.
    let boom = client::proxy_call(&transport, "boom", serde_json::Map::new(), timeout).await;
    assert!(
        boom.is_err(),
        "a call to a dying child must surface an error"
    );
    assert_eq!(
        pool::len(),
        0,
        "the broken session is evicted, not retained"
    );

    // The next call reopens a fresh child and succeeds — self-healing.
    let mut args = serde_json::Map::new();
    args.insert("text".into(), json!("back"));
    let ok = client::proxy_call(&transport, "echo", args, timeout)
        .await
        .expect("next call reopens a fresh session");
    assert_eq!(client::result_to_text(&ok).trim(), "echo:back");
    assert_eq!(pool::len(), 1, "exactly one fresh session after reopen");

    pool::clear();
}
