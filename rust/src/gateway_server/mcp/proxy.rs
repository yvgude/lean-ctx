//! `/mcp/{server}` — the governed MCP reverse proxy (GL#100).
//!
//! MCP Streamable HTTP has one endpoint per server: the client POSTs JSON-RPC
//! frames (response arrives as `application/json` or as an SSE stream), GETs
//! an optional server-push listen stream, and DELETEs its session. The
//! gateway fronts each registered upstream under `/mcp/{id}`:
//!
//! - **Auth**: the proxy's Bearer guard runs first — org token or per-person
//!   gateway key, exactly like the LLM channel. `/mcp/*` is deliberately not
//!   a provider route, so the loopback provider-key fallback never applies.
//! - **Credential isolation**: the caller's `Authorization` (their gateway
//!   key) is always stripped; when the registry entry names an `auth_env`,
//!   the gateway injects `Authorization: Bearer <env value>` upstream. Tool
//!   credentials live in the gateway environment, never on laptops.
//! - **Observe, don't touch**: request and response bytes pass through
//!   verbatim (SSE responses are teed, never buffered-and-replayed). Only
//!   POST exchanges are metered — `tools/call` is the billable unit; the GET
//!   listen stream carries server-initiated traffic, not tool calls.
//! - **Fail-open**: analysis/metering failures log and pass traffic through.
//!
//! Registry changes (config.toml edits) take effect on gateway restart, the
//! same lifecycle as `gateway-keys.toml` (documented; live reload is an M4
//! concern once enforcement makes it safety-relevant).

use std::time::Instant;

use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use futures::StreamExt;

use crate::core::config::ResolvedMcpServer;
use crate::proxy::ProxyState;
use crate::proxy::gateway_identity::GatewayTags;

use super::frames::{self, ParsedRequest, RequestKind, ResponseInfo};
use super::metering::{self, MeteredExchange};
use super::store::McpEvent;

/// Identity fallbacks, mirroring the LLM channel's honest defaults
/// (`store::ANONYMOUS_PERSON`/`DEFAULT_PROJECT`): rows stay attributable in
/// solo/loopback mode; strict gateways make keys mandatory anyway.
const ANONYMOUS_PERSON: &str = "anonymous";
const DEFAULT_PROJECT: &str = "default";

/// Request-body ceiling for MCP POST frames. Tool *arguments* are small
/// compared to LLM prompts; 8 MiB is generous without inviting abuse.
const MAX_REQUEST_BODY: usize = 8 * 1024 * 1024;

/// How many response bytes the analyzer will hold to find the JSON-RPC
/// response frame. Beyond this the exchange is still passed through and
/// metered, with tokens approximated from the byte count (documented in
/// [`approx_tokens_from_bytes`]).
const MAX_ANALYSIS_BYTES: usize = 8 * 1024 * 1024;

/// The single entry point for every `/mcp/{server}` request. Mounted on the
/// main proxy router (feature-gated in `proxy::start_proxy`), so it shares
/// `ProxyState` — the upstream client and the registry snapshot.
pub async fn handler(
    State(state): State<ProxyState>,
    Path(server_id): Path<String>,
    req: Request<Body>,
) -> Response {
    let Some(server) = state
        .mcp_servers
        .iter()
        .find(|s| s.id == server_id)
        .cloned()
    else {
        return json_rpc_error(
            StatusCode::NOT_FOUND,
            &format!(
                "unknown MCP server '{server_id}' — register it under [[gateway_server.mcp_servers]]"
            ),
        );
    };

    let tags = req
        .extensions()
        .get::<GatewayTags>()
        .cloned()
        .unwrap_or_default();

    let method = req.method().clone();
    let (parts, body) = req.into_parts();

    // Upstream request: fixed registry URL (no path/query joining — the MCP
    // endpoint is a single URL; not forwarding caller paths is the SSRF-
    // narrowest possible surface), curated headers, gateway-held credential.
    let mut upstream_headers = forwarded_request_headers(&parts.headers);
    if let Err(resp) = inject_upstream_credential(&server, &mut upstream_headers) {
        return *resp;
    }

    match method {
        axum::http::Method::POST => {
            let Ok(body_bytes) = axum::body::to_bytes(body, MAX_REQUEST_BODY).await else {
                return json_rpc_error(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    &format!("MCP request body exceeds {MAX_REQUEST_BODY} bytes"),
                );
            };
            let parsed = frames::parse_request(&body_bytes);
            let started = Instant::now();
            let upstream = state
                .client
                .post(&server.url)
                .headers(upstream_headers)
                .body(body_bytes.to_vec())
                .send()
                .await;
            relay_post_response(upstream, &server, parsed, tags, started).await
        }
        // GET opens the server-push listen stream; DELETE ends the session.
        // Pure passthrough: no JSON-RPC exchange to meter here (tools/call
        // always travels over POST).
        axum::http::Method::GET | axum::http::Method::DELETE => {
            let builder = if method == axum::http::Method::GET {
                state.client.get(&server.url)
            } else {
                state.client.delete(&server.url)
            };
            match builder.headers(upstream_headers).send().await {
                Ok(upstream) => passthrough_response(upstream),
                Err(e) => upstream_unreachable(&server.id, &e),
            }
        }
        _ => json_rpc_error(
            StatusCode::METHOD_NOT_ALLOWED,
            "MCP Streamable HTTP uses POST, GET and DELETE",
        ),
    }
}

/// Relays a POST response, teeing bytes into the frame analyzer so the
/// exchange lands in `mcp_events` without delaying the client.
async fn relay_post_response(
    upstream: Result<reqwest::Response, reqwest::Error>,
    server: &ResolvedMcpServer,
    parsed: Option<ParsedRequest>,
    tags: GatewayTags,
    started: Instant,
) -> Response {
    let upstream = match upstream {
        Ok(u) => u,
        Err(e) => {
            record_exchange(
                server,
                parsed.as_ref(),
                &tags,
                "upstream_error",
                started.elapsed().as_millis(),
                None,
                0,
            );
            return upstream_unreachable(&server.id, &e);
        }
    };

    let status = upstream.status();
    let content_type = upstream
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let headers = forwarded_response_headers(upstream.headers());

    if metering::installed() && content_type.starts_with("text/event-stream") {
        // SSE: tee the stream — verbatim passthrough, analyzer on the side,
        // event recorded when the upstream closes the stream.
        let analyzer = SseAnalyzer::new(server.clone(), parsed, tags, started, u16_status(status));
        let teed = tee_sse(upstream.bytes_stream(), analyzer);
        return build_response(status, headers, Body::from_stream(teed));
    }

    // JSON (or metering off, or an error body): buffer, analyze, relay.
    // MCP JSON responses are single frames — buffering them is bounded by
    // the upstream's own response discipline; the SSE path above covers the
    // long-running case.
    match upstream.bytes().await {
        Ok(bytes) => {
            if metering::installed() {
                let info = parsed
                    .as_ref()
                    .and_then(|p| frames::analyze_response_json(&bytes, p.id.as_ref()));
                let status_label = exchange_status(u16_status(status), info.as_ref());
                record_exchange(
                    server,
                    parsed.as_ref(),
                    &tags,
                    status_label,
                    started.elapsed().as_millis(),
                    info,
                    bytes.len() as u64,
                );
            }
            build_response(status, headers, Body::from(bytes))
        }
        Err(e) => {
            record_exchange(
                server,
                parsed.as_ref(),
                &tags,
                "upstream_error",
                started.elapsed().as_millis(),
                None,
                0,
            );
            upstream_unreachable(&server.id, &e)
        }
    }
}

/// Streams an upstream response through untouched (GET listen / DELETE).
fn passthrough_response(upstream: reqwest::Response) -> Response {
    let status = upstream.status();
    let headers = forwarded_response_headers(upstream.headers());
    build_response(status, headers, Body::from_stream(upstream.bytes_stream()))
}

/// Observes teed SSE bytes and books the exchange when the stream ends.
struct SseAnalyzer {
    server: ResolvedMcpServer,
    parsed: Option<ParsedRequest>,
    tags: GatewayTags,
    started: Instant,
    http_status: u16,
    /// Raw bytes held for frame analysis; dropped once the response frame is
    /// found or the cap is passed (then only `total_bytes` keeps counting).
    buffer: Option<Vec<u8>>,
    total_bytes: u64,
    found: Option<ResponseInfo>,
}

impl SseAnalyzer {
    fn new(
        server: ResolvedMcpServer,
        parsed: Option<ParsedRequest>,
        tags: GatewayTags,
        started: Instant,
        http_status: u16,
    ) -> Self {
        Self {
            server,
            parsed,
            tags,
            started,
            http_status,
            buffer: Some(Vec::new()),
            total_bytes: 0,
            found: None,
        }
    }

    fn feed(&mut self, chunk: &[u8]) {
        self.total_bytes += chunk.len() as u64;
        if self.found.is_some() {
            return;
        }
        let Some(buf) = self.buffer.as_mut() else {
            return;
        };
        buf.extend_from_slice(chunk);
        // Only re-scan when a complete SSE event boundary is in the buffer.
        if chunk.windows(2).any(|w| w == b"\n\n") || buf.windows(2).any(|w| w == b"\n\n") {
            let text = String::from_utf8_lossy(buf);
            if let Some(info) = self
                .parsed
                .as_ref()
                .and_then(|p| frames::analyze_response_sse(&text, p.id.as_ref()))
            {
                self.found = Some(info);
                self.buffer = None;
                return;
            }
        }
        if buf.len() > MAX_ANALYSIS_BYTES {
            self.buffer = None;
        }
    }

    fn finish(self) {
        let status_label = exchange_status(self.http_status, self.found.as_ref());
        record_exchange(
            &self.server,
            self.parsed.as_ref(),
            &self.tags,
            status_label,
            self.started.elapsed().as_millis(),
            self.found,
            self.total_bytes,
        );
    }
}

/// Byte-for-byte tee (same construction as `proxy::usage::tee_stream`): every
/// chunk is forwarded unchanged; the analyzer observes on the side and books
/// the exchange when the upstream ends the stream.
fn tee_sse<S, E>(
    inner: S,
    analyzer: SseAnalyzer,
) -> impl futures::Stream<Item = Result<Bytes, E>> + Send
where
    S: futures::Stream<Item = Result<Bytes, E>> + Send + Unpin + 'static,
    E: Send + 'static,
{
    futures::stream::unfold(
        (inner, Some(analyzer)),
        |(mut inner, mut analyzer)| async move {
            match inner.next().await {
                Some(Ok(chunk)) => {
                    if let Some(a) = analyzer.as_mut() {
                        a.feed(&chunk);
                    }
                    Some((Ok(chunk), (inner, analyzer)))
                }
                Some(err) => Some((err, (inner, analyzer))),
                None => {
                    if let Some(a) = analyzer.take() {
                        a.finish();
                    }
                    None
                }
            }
        },
    )
}

/// Books one exchange into the metering sink (fail-open, never blocks).
fn record_exchange(
    server: &ResolvedMcpServer,
    parsed: Option<&ParsedRequest>,
    tags: &GatewayTags,
    status: &str,
    duration_ms: u128,
    info: Option<ResponseInfo>,
    raw_bytes: u64,
) {
    if !metering::installed() {
        return;
    }
    let (method, tool) = match parsed.map(|p| &p.kind) {
        Some(RequestKind::ToolsCall { tool }) => ("tools/call".to_string(), Some(tool.clone())),
        Some(kind) => (kind.method_label().to_string(), None),
        // Notification / batch / non-JSON body: still a real exchange.
        None => ("passthrough".to_string(), None),
    };
    let (result_bytes, result_tokens, inventory) = match info {
        Some(i) => (i.result_bytes, i.result_tokens, i.tools),
        // No parsed frame (oversized stream / foreign shape): honest byte
        // count with the documented byte→token approximation.
        None => (raw_bytes, approx_tokens_from_bytes(raw_bytes), None),
    };
    metering::record(MeteredExchange {
        event: McpEvent {
            person: tags
                .person
                .clone()
                .unwrap_or_else(|| ANONYMOUS_PERSON.to_string()),
            team: tags.team.clone(),
            project: tags
                .project
                .clone()
                .unwrap_or_else(|| DEFAULT_PROJECT.to_string()),
            server_id: server.id.clone(),
            method,
            tool,
            status: status.to_string(),
            duration_ms: i64::try_from(duration_ms).unwrap_or(i64::MAX),
            result_bytes: i64::try_from(result_bytes).unwrap_or(i64::MAX),
            result_tokens: i64::try_from(result_tokens).unwrap_or(i64::MAX),
            // Priced by the writer (one pricing table load per process).
            context_cost_usd: 0.0,
            reference_model: None,
        },
        inventory,
    });
}

/// `ok` | `error` | `upstream_error` — the three-valued status column.
fn exchange_status(http_status: u16, info: Option<&ResponseInfo>) -> &'static str {
    if info.is_some_and(|i| i.is_error) {
        "error"
    } else if http_status >= 400 {
        "upstream_error"
    } else {
        "ok"
    }
}

/// Tokens from bytes when no frame could be parsed: the standard ≈4 bytes per
/// token heuristic for o200k-family tokenizers, floor 1 for non-empty bodies.
/// An approximation is honest here — the alternative (0) would silently erase
/// real context volume from the cost story.
fn approx_tokens_from_bytes(bytes: u64) -> u64 {
    if bytes == 0 { 0 } else { (bytes / 4).max(1) }
}

/// Request headers the upstream needs — and nothing else. Notably absent:
/// the caller's `Authorization`/`x-api-key` (their *gateway* credential must
/// never reach a tool server) and `x-leanctx-project` (internal tag).
fn forwarded_request_headers(incoming: &HeaderMap) -> HeaderMap {
    const FORWARDED: &[&str] = &[
        "content-type",
        "accept",
        "accept-encoding",
        "user-agent",
        "mcp-session-id",
        "mcp-protocol-version",
        "last-event-id",
    ];
    let mut out = HeaderMap::new();
    for name in FORWARDED {
        if let Some(v) = incoming.get(*name)
            && let Ok(name) = axum::http::header::HeaderName::from_bytes(name.as_bytes())
        {
            out.insert(name, v.clone());
        }
    }
    out
}

/// Response headers relayed to the caller: hop-by-hop headers and
/// `content-length` stay behind (axum reframes the body), everything else —
/// notably `mcp-session-id` and `content-type` — passes through.
fn forwarded_response_headers(upstream: &HeaderMap) -> Vec<(String, HeaderValue)> {
    const SKIP: &[&str] = &[
        "connection",
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
        "content-length",
    ];
    upstream
        .iter()
        .filter(|(name, _)| !SKIP.contains(&name.as_str()))
        .map(|(name, value)| (name.as_str().to_string(), value.clone()))
        .collect()
}

/// Injects the gateway-held upstream credential (`auth_env`). A configured-
/// but-missing env var is a deployment error and surfaces as a loud 502 —
/// the same contract as the LLM registry's `api_key_env`. (The `Err` response
/// is boxed: it only exists on the misconfiguration path.)
fn inject_upstream_credential(
    server: &ResolvedMcpServer,
    headers: &mut HeaderMap,
) -> Result<(), Box<Response>> {
    let Some(env_name) = server.auth_env.as_deref() else {
        return Ok(());
    };
    let key = std::env::var(env_name)
        .ok()
        .filter(|k| !k.trim().is_empty());
    let Some(key) = key else {
        tracing::error!(
            "mcp proxy: server '{}' configures auth_env='{env_name}' but the variable is \
             unset/empty — cannot authenticate upstream (502)",
            server.id
        );
        return Err(Box::new(json_rpc_error(
            StatusCode::BAD_GATEWAY,
            &format!(
                "gateway misconfiguration: auth_env '{env_name}' for MCP server '{}' is unset",
                server.id
            ),
        )));
    };
    if let Ok(v) = HeaderValue::from_str(&format!("Bearer {key}")) {
        headers.insert(axum::http::header::AUTHORIZATION, v);
        Ok(())
    } else {
        tracing::error!("mcp proxy: credential from {env_name} contains invalid header bytes");
        Err(Box::new(json_rpc_error(
            StatusCode::BAD_GATEWAY,
            &format!("gateway misconfiguration: credential in '{env_name}' is not header-safe"),
        )))
    }
}

fn build_response(
    status: reqwest::StatusCode,
    headers: Vec<(String, HeaderValue)>,
    body: Body,
) -> Response {
    let mut resp = Response::new(body);
    *resp.status_mut() = StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    for (name, value) in headers {
        if let Ok(name) = axum::http::header::HeaderName::from_bytes(name.as_bytes()) {
            resp.headers_mut().append(name, value);
        }
    }
    resp
}

fn u16_status(status: reqwest::StatusCode) -> u16 {
    status.as_u16()
}

fn upstream_unreachable(server_id: &str, err: &reqwest::Error) -> Response {
    tracing::warn!("mcp proxy: upstream '{server_id}' unreachable: {err}");
    json_rpc_error(
        StatusCode::BAD_GATEWAY,
        &format!("MCP server '{server_id}' is unreachable through the gateway"),
    )
}

/// Error bodies stay in JSON-RPC shape so MCP clients surface them cleanly
/// instead of choking on a bare-text proxy error.
fn json_rpc_error(status: StatusCode, message: &str) -> Response {
    (
        status,
        axum::Json(serde_json::json!({
            "jsonrpc": "2.0",
            "id": null,
            "error": { "code": -32000, "message": message }
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_header_curation_strips_caller_credentials() {
        let mut incoming = HeaderMap::new();
        incoming.insert("authorization", "Bearer gk-personal-key".parse().unwrap());
        incoming.insert("x-api-key", "sk-something".parse().unwrap());
        incoming.insert("x-leanctx-project", "secret-project".parse().unwrap());
        incoming.insert("content-type", "application/json".parse().unwrap());
        incoming.insert(
            "accept",
            "application/json, text/event-stream".parse().unwrap(),
        );
        incoming.insert("mcp-session-id", "abc123".parse().unwrap());
        incoming.insert("mcp-protocol-version", "2025-06-18".parse().unwrap());
        incoming.insert("last-event-id", "7".parse().unwrap());
        incoming.insert("cookie", "session=steal-me".parse().unwrap());

        let out = forwarded_request_headers(&incoming);
        assert!(
            out.get("authorization").is_none(),
            "gateway key must not leak"
        );
        assert!(out.get("x-api-key").is_none());
        assert!(out.get("x-leanctx-project").is_none());
        assert!(out.get("cookie").is_none());
        assert_eq!(out.get("mcp-session-id").unwrap(), "abc123");
        assert_eq!(out.get("mcp-protocol-version").unwrap(), "2025-06-18");
        assert_eq!(out.get("last-event-id").unwrap(), "7");
        assert_eq!(out.get("content-type").unwrap(), "application/json");
    }

    #[test]
    fn credential_injection_is_loud_on_missing_env_and_replaces_caller_auth() {
        let _lock = crate::core::data_dir::test_env_lock();
        let server = ResolvedMcpServer {
            id: "github".into(),
            url: "https://api.githubcopilot.com/mcp".into(),
            auth_env: Some("LC_TEST_MCP_PAT".into()),
        };

        crate::test_env::remove_var("LC_TEST_MCP_PAT");
        let mut headers = HeaderMap::new();
        assert!(
            inject_upstream_credential(&server, &mut headers).is_err(),
            "missing env must 502, never forward the caller's key"
        );

        crate::test_env::set_var("LC_TEST_MCP_PAT", "ghp-upstream");
        let mut headers = HeaderMap::new();
        inject_upstream_credential(&server, &mut headers).expect("env present");
        assert_eq!(headers.get("authorization").unwrap(), "Bearer ghp-upstream");
        crate::test_env::remove_var("LC_TEST_MCP_PAT");

        // No auth_env → no header injected (public upstream).
        let open = ResolvedMcpServer {
            id: "open".into(),
            url: "https://mcp.example.com/mcp".into(),
            auth_env: None,
        };
        let mut headers = HeaderMap::new();
        inject_upstream_credential(&open, &mut headers).unwrap();
        assert!(headers.get("authorization").is_none());
    }

    #[test]
    fn response_header_relay_drops_hop_by_hop_and_length() {
        let mut upstream = HeaderMap::new();
        upstream.insert("content-type", "text/event-stream".parse().unwrap());
        upstream.insert("mcp-session-id", "s-1".parse().unwrap());
        upstream.insert("transfer-encoding", "chunked".parse().unwrap());
        upstream.insert("content-length", "12".parse().unwrap());
        upstream.insert("connection", "keep-alive".parse().unwrap());

        let out = forwarded_response_headers(&upstream);
        let names: Vec<&str> = out.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"content-type"));
        assert!(names.contains(&"mcp-session-id"));
        assert!(!names.contains(&"transfer-encoding"));
        assert!(!names.contains(&"content-length"));
        assert!(!names.contains(&"connection"));
    }

    #[test]
    fn status_labels_and_byte_approximation_are_stable() {
        assert_eq!(exchange_status(200, None), "ok");
        assert_eq!(exchange_status(500, None), "upstream_error");
        let err_info = ResponseInfo {
            is_error: true,
            result_bytes: 10,
            result_tokens: 3,
            tools: None,
        };
        assert_eq!(exchange_status(200, Some(&err_info)), "error");

        assert_eq!(approx_tokens_from_bytes(0), 0);
        assert_eq!(approx_tokens_from_bytes(2), 1, "non-empty floors at 1");
        assert_eq!(approx_tokens_from_bytes(4000), 1000);
    }

    #[tokio::test]
    async fn sse_tee_passes_bytes_through_verbatim() {
        let server = ResolvedMcpServer {
            id: "s".into(),
            url: "https://mcp.example.com/mcp".into(),
            auth_env: None,
        };
        let chunks: Vec<Result<Bytes, std::convert::Infallible>> = vec![
            Ok(Bytes::from_static(b"data: {\"jsonrpc\":\"2.0\",\"id\":1,")),
            Ok(Bytes::from_static(b"\"result\":{\"content\":[]}}\n\n")),
        ];
        let analyzer = SseAnalyzer::new(
            server,
            frames::parse_request(
                br#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"t"}}"#,
            ),
            GatewayTags::default(),
            Instant::now(),
            200,
        );
        let teed = tee_sse(futures::stream::iter(chunks), analyzer);
        let collected: Vec<_> = teed.collect().await;
        assert_eq!(collected.len(), 2);
        let all: Vec<u8> = collected
            .into_iter()
            .flat_map(|c| c.unwrap().to_vec())
            .collect();
        assert_eq!(
            all, b"data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"content\":[]}}\n\n",
            "tee must never mutate the byte stream"
        );
    }

    #[test]
    fn sse_analyzer_finds_the_frame_and_stops_buffering() {
        let server = ResolvedMcpServer {
            id: "s".into(),
            url: "https://mcp.example.com/mcp".into(),
            auth_env: None,
        };
        let mut a = SseAnalyzer::new(
            server,
            frames::parse_request(
                br#"{"jsonrpc":"2.0","id":42,"method":"tools/call","params":{"name":"get_issue"}}"#,
            ),
            GatewayTags::default(),
            Instant::now(),
            200,
        );
        a.feed(b"data: {\"jsonrpc\":\"2.0\",\"id\":42,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"hi\"}]}}\n\n");
        assert!(a.found.is_some(), "response frame must be detected");
        assert!(a.buffer.is_none(), "buffer drops once the frame is found");
        let before = a.total_bytes;
        a.feed(b"data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/x\"}\n\n");
        assert!(a.total_bytes > before, "bytes keep counting after the find");
    }
}
