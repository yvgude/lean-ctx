//! End-to-end test for the MCP Tool-Catalog Gateway client (#210).
//!
//! Spins up a *real* rmcp MCP server in-process (over a Tokio duplex pipe),
//! connects lean-ctx's gateway client to it, and exercises the real MCP
//! protocol: `initialize` handshake → `tools/list` → `tools/call`. No mocks —
//! this is the same `rmcp` server/client machinery used in production, just
//! wired over an in-memory transport for determinism.

use std::sync::Arc;
use std::time::Duration;

use rmcp::model::{
    CallToolRequestParams, CallToolResult, Content, ListToolsResult, PaginatedRequestParams,
    ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt};
use serde_json::json;

use lean_ctx::core::gateway::client;

/// Minimal downstream MCP server exposing two tools: `echo` and `add`.
struct EchoServer;

impl ServerHandler for EchoServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
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
        Ok(ListToolsResult {
            tools: vec![echo, add],
            ..Default::default()
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let args = request.arguments.unwrap_or_default();
        match request.name.as_ref() {
            "echo" => {
                let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("");
                Ok(CallToolResult::success(vec![Content::text(format!(
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
                Ok(CallToolResult::success(vec![Content::text(
                    (a + b).to_string(),
                )]))
            }
            other => Err(ErrorData::invalid_params(
                format!("unknown tool: {other}"),
                None,
            )),
        }
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
