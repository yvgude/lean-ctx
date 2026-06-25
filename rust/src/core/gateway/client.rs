//! Downstream MCP client (#210).
//!
//! A *real* MCP client built on the official `rmcp` SDK — no bespoke JSON-RPC.
//! Each operation opens a fresh connection (performs the `initialize`
//! handshake), does its work, and shuts the connection down. This keeps the
//! gateway stateless and robust (no stale child processes / sessions); the
//! expensive part — listing the full catalog — is amortized by the TTL cache in
//! [`super::catalog`].

use std::time::Duration;

use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult, Tool};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::{StreamableHttpClientTransport, TokioChildProcess};
use serde_json::{Map, Value};

use super::config::ResolvedTransport;

/// A connected downstream MCP client session. Transport-erased: stdio, HTTP,
/// and (in tests) in-process duplex all collapse to this one type.
pub type ClientService = RunningService<RoleClient, ()>;

/// Open a connection to a downstream MCP server (runs the MCP `initialize`
/// handshake). The whole connect is bounded by `timeout`.
pub async fn open(
    transport: &ResolvedTransport,
    timeout: Duration,
) -> Result<ClientService, String> {
    let connect = async {
        match transport {
            ResolvedTransport::Stdio {
                command,
                args,
                env,
                binary_sha256,
                capabilities,
            } => {
                // Binary-hash pin (#403, P3): if the addon pinned its binary's
                // sha256, verify the file on PATH before doing anything else, so
                // a swapped executable is refused (fail-closed). No-op when
                // unpinned.
                crate::core::addons::binhash::verify_binary(command, binary_sha256)?;
                // Per-addon OS sandbox (#865, P1): declared capabilities drive
                // the profile (network/filesystem); absent caps fall back to the
                // legacy `addons.sandbox` mode. May wrap with sandbox-exec /
                // bwrap, or refuse to spawn (strict / enforce_capabilities).
                let (spawn_cmd, spawn_args) =
                    crate::core::addons::sandbox::apply_for(command, args, capabilities.as_ref())?;
                let mut cmd = tokio::process::Command::new(&spawn_cmd);
                cmd.args(&spawn_args);
                // Secure-by-default environment (P1): a capability-declaring
                // addon gets a scrubbed env (base allowlist + declared names);
                // legacy addons inherit the host env unchanged.
                crate::core::addons::env_scrub::apply_env(&mut cmd, env, capabilities.as_ref());
                let child = TokioChildProcess::new(cmd)
                    .map_err(|e| format!("spawn `{command}` failed: {e}"))?;
                ().serve(child)
                    .await
                    .map_err(|e| format!("MCP handshake failed (stdio): {e}"))
            }
            ResolvedTransport::Http { url, headers } => {
                let mut cfg = StreamableHttpClientTransportConfig::with_uri(url.clone());
                if !headers.is_empty() {
                    let mut custom = std::collections::HashMap::new();
                    for (k, v) in headers {
                        let name = http::HeaderName::from_bytes(k.as_bytes())
                            .map_err(|e| format!("invalid header name `{k}`: {e}"))?;
                        let val = http::HeaderValue::from_str(v)
                            .map_err(|e| format!("invalid header value for `{k}`: {e}"))?;
                        custom.insert(name, val);
                    }
                    cfg = cfg.custom_headers(custom);
                }
                let t = StreamableHttpClientTransport::from_config(cfg);
                ().serve(t)
                    .await
                    .map_err(|e| format!("MCP handshake failed (http): {e}"))
            }
        }
    };
    tokio::time::timeout(timeout, connect)
        .await
        .map_err(|_| "downstream connect timed out".to_string())?
}

/// List tools on an already-connected session (bounded by `timeout`).
pub async fn list_tools_on(
    service: &ClientService,
    timeout: Duration,
) -> Result<Vec<Tool>, String> {
    tokio::time::timeout(timeout, service.list_all_tools())
        .await
        .map_err(|_| "downstream tools/list timed out".to_string())
        .and_then(|r| r.map_err(|e| format!("downstream tools/list failed: {e}")))
}

/// Call a tool on an already-connected session (bounded by `timeout`).
pub async fn call_tool_on(
    service: &ClientService,
    tool: &str,
    arguments: Map<String, Value>,
    timeout: Duration,
) -> Result<CallToolResult, String> {
    let param = CallToolRequestParams::new(tool.to_string()).with_arguments(arguments);
    tokio::time::timeout(timeout, service.call_tool(param))
        .await
        .map_err(|_| "downstream tools/call timed out".to_string())
        .and_then(|r| r.map_err(|e| format!("downstream tools/call failed: {e}")))
}

/// List a downstream server's tools (connect → `tools/list` → disconnect).
pub async fn fetch_tools(
    transport: &ResolvedTransport,
    timeout: Duration,
) -> Result<Vec<Tool>, String> {
    let service = open(transport, timeout).await?;
    let listed = list_tools_on(&service, timeout).await;
    let _ = service.cancel().await;
    listed
}

/// Proxy a single tool call to a downstream server (connect → `tools/call` →
/// disconnect).
pub async fn proxy_call(
    transport: &ResolvedTransport,
    tool: &str,
    arguments: Map<String, Value>,
    timeout: Duration,
) -> Result<CallToolResult, String> {
    let service = open(transport, timeout).await?;
    let called = call_tool_on(&service, tool, arguments, timeout).await;
    let _ = service.cancel().await;
    called
}

/// Flatten a downstream [`CallToolResult`] into plain text. Text blocks are
/// concatenated; non-text blocks (images/resources) are summarized so the proxy
/// never returns binary blobs into the model context.
#[must_use]
pub fn result_to_text(result: &CallToolResult) -> String {
    let mut parts: Vec<String> = Vec::new();
    for c in &result.content {
        if let Some(t) = c.as_text() {
            parts.push(t.text.clone());
        } else if c.as_image().is_some() {
            parts.push("[image content omitted by gateway]".to_string());
        } else {
            parts.push("[non-text content omitted by gateway]".to_string());
        }
    }
    parts.join("\n")
}
