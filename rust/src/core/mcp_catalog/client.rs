//! Downstream MCP client (#210).
//!
//! A *real* MCP client built on the official `rmcp` SDK — no bespoke JSON-RPC.
//! [`open`] performs the MCP `initialize` handshake; sessions are then kept alive
//! and reused by the [`super::pool`] (#1078), so list/call operations no longer
//! pay spawn+handshake latency on every invocation. The pool only ever hands out
//! a *live* session (it sweeps closed ones at acquire), so requests never reach a
//! dead pipe. Listing is idempotent, so a mid-flight transport failure is evicted
//! and reopened once; a *call* is never auto-retried (a downstream tool may be
//! non-idempotent — re-issuing could double-execute a side effect), only evicted.
//! The catalog-listing cost is additionally amortized by the TTL cache in
//! [`super::catalog`].

use std::collections::{BTreeMap, HashMap};
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
                ..
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
            ResolvedTransport::Http {
                url,
                headers,
                secret_fingerprints,
            } => {
                let mut cfg = StreamableHttpClientTransportConfig::with_uri(url.clone());
                if !headers.is_empty() {
                    cfg = cfg.custom_headers(http_headers(headers, secret_fingerprints)?);
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

fn http_headers(
    headers: &BTreeMap<String, String>,
    secret_fingerprints: &BTreeMap<String, String>,
) -> Result<HashMap<http::HeaderName, http::HeaderValue>, String> {
    headers
        .iter()
        .map(|(name, value)| {
            let header_name = http::HeaderName::from_bytes(name.as_bytes())
                .map_err(|error| format!("invalid header name `{name}`: {error}"))?;
            let mut header_value = http::HeaderValue::from_str(value)
                .map_err(|error| format!("invalid header value for `{name}`: {error}"))?;
            if secret_fingerprints
                .keys()
                .any(|secret| secret.eq_ignore_ascii_case(name))
            {
                header_value.set_sensitive(true);
            }
            Ok((header_name, header_value))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memento_headers_are_marked_sensitive() {
        let headers = BTreeMap::from([
            ("Accept".into(), "application/json".into()),
            ("Authorization".into(), "Bearer private-token".into()),
        ]);
        let secrets = BTreeMap::from([("authorization".into(), "fingerprint".into())]);
        let converted = http_headers(&headers, &secrets).expect("valid headers");

        assert!(!converted[&http::header::ACCEPT].is_sensitive());
        assert!(converted[&http::header::AUTHORIZATION].is_sensitive());
    }
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

/// List a downstream server's tools over a pooled session (`tools/list`).
/// [`super::pool::acquire`] guarantees a live session, so the only failure left
/// is a rare mid-flight transport death; because listing is idempotent we evict
/// the suspect session and reopen once. A timeout is surfaced (not retried).
pub async fn fetch_tools(
    transport: &ResolvedTransport,
    timeout: Duration,
) -> Result<Vec<Tool>, String> {
    let key = super::pool::key(transport);
    let service = super::pool::acquire(transport, timeout).await?;
    match list_tools_on(&service, timeout).await {
        Ok(tools) => Ok(tools),
        Err(e) => {
            super::pool::evict(key);
            if is_broken_connection(&e) {
                let service = super::pool::acquire(transport, timeout).await?;
                list_tools_on(&service, timeout).await
            } else {
                Err(e)
            }
        }
    }
}

/// Proxy a single tool call to a downstream server over a pooled session
/// (`tools/call`). [`super::pool::acquire`] only returns a live session, so the
/// request is never sent into a dead pipe. A failed call is **never** retried:
/// a downstream tool may be non-idempotent, so re-issuing could double-execute a
/// side effect. On any failure we evict the (now-suspect) session — the next
/// call reopens cleanly — and surface the error for the caller to decide.
pub async fn proxy_call(
    transport: &ResolvedTransport,
    tool: &str,
    arguments: Map<String, Value>,
    timeout: Duration,
) -> Result<CallToolResult, String> {
    let key = super::pool::key(transport);
    let service = super::pool::acquire(transport, timeout).await?;
    let result = call_tool_on(&service, tool, arguments, timeout).await;
    if result.is_err() {
        super::pool::evict(key);
    }
    result
}

/// Whether `err` indicates the pooled connection is broken (so a fresh reopen +
/// retry of an *idempotent* op is safe), as opposed to a timeout (the request
/// may still be running) or a higher-level failure. Errors here are produced by
/// this module, so the match is on our own stable strings.
fn is_broken_connection(err: &str) -> bool {
    !err.contains("timed out")
}

/// Flatten a downstream [`CallToolResult`] into plain text. Text blocks are
/// concatenated; non-text blocks (images/resources) are summarized so the proxy
/// never returns binary blobs into the model context.
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
