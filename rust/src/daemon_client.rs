use anyhow::{Context, Result};
use std::sync::OnceLock;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::runtime::Runtime;

use crate::daemon;
use crate::ipc;

static DELIVERY_RUNTIME: OnceLock<Result<Runtime, String>> = OnceLock::new();

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(not(unix), allow(dead_code))] // the daemon route is cfg(unix) in read_cmd
pub(crate) enum DaemonToolCallOutcome {
    Unavailable,
    Completed { text: String, is_error: bool },
    Uncertain { message: String },
}

/// Cross-agent delivery lookups are an optional cache tier. They must never
/// inherit the daemon client's normal control-plane timeout on an interactive
/// read or shell path.
const BEST_EFFORT_DELIVERY_IPC_TIMEOUT: Duration = Duration::from_millis(250);
const DAEMON_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const DAEMON_IO_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RequestSendState {
    NotSent,
    PossiblyDispatched,
}

#[derive(Debug, PartialEq, Eq)]
enum DaemonRequestOutcome {
    Unavailable(String),
    Completed(String),
    Uncertain(String),
}

fn delivery_runtime() -> Result<&'static Runtime> {
    match DELIVERY_RUNTIME.get_or_init(|| Runtime::new().map_err(|error| error.to_string())) {
        Ok(runtime) => Ok(runtime),
        Err(error) => Err(anyhow::anyhow!("initialize delivery IPC runtime: {error}")),
    }
}

/// Safely run an async future on the delivery runtime from any thread context.
/// When called from inside a tokio runtime (e.g. tool handler async pipeline /
/// prefetch re-entry), spawns a scoped thread to avoid "Cannot start a runtime
/// from within a runtime". When called from outside (CLI), blocks directly.
fn delivery_block_on<F, O>(fut: F) -> Option<O>
where
    F: std::future::Future<Output = O> + Send,
    O: Send,
{
    let rt = delivery_runtime().ok()?;
    if tokio::runtime::Handle::try_current().is_ok() {
        let mut result = None;
        std::thread::scope(|s| {
            s.spawn(|| {
                result = Some(rt.block_on(fut));
            });
        });
        result
    } else {
        Some(rt.block_on(fut))
    }
}

/// Send an HTTP request to the daemon over the IPC channel.
/// Returns the response body as a string.
pub async fn daemon_request(method: &str, path: &str, body: &str) -> Result<String> {
    match daemon_request_observed(method, path, body).await {
        DaemonRequestOutcome::Completed(body) => Ok(body),
        DaemonRequestOutcome::Unavailable(message) | DaemonRequestOutcome::Uncertain(message) => {
            Err(anyhow::anyhow!(message))
        }
    }
}

async fn daemon_request_observed(method: &str, path: &str, body: &str) -> DaemonRequestOutcome {
    let addr = daemon::daemon_addr();
    daemon_request_at_observed(&addr, method, path, body).await
}

async fn daemon_request_at_observed(
    addr: &ipc::DaemonAddr,
    method: &str,
    path: &str,
    body: &str,
) -> DaemonRequestOutcome {
    if !addr.is_listening() {
        return DaemonRequestOutcome::Unavailable(format!(
            "Daemon endpoint not found at {}. Is the daemon running?",
            addr.display()
        ));
    }

    let request = format_http_request(method, path, body);
    let addr_display = addr.display();

    #[cfg(unix)]
    {
        daemon_request_with_connector(ipc::connect(addr), &addr_display, request).await
    }

    #[cfg(windows)]
    {
        daemon_request_with_connector(ipc::connect(addr), &addr_display, request).await
    }
}

async fn daemon_request_with_connector<S, F, E>(
    connect: F,
    addr_display: &str,
    request: String,
) -> DaemonRequestOutcome
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    F: std::future::Future<Output = std::result::Result<S, E>>,
    E: std::fmt::Display,
{
    let stream = match tokio::time::timeout(DAEMON_CONNECT_TIMEOUT, connect).await {
        Ok(Ok(stream)) => stream,
        Ok(Err(error)) => {
            return request_send_failure(
                RequestSendState::NotSent,
                format!("cannot connect to daemon at {addr_display}: {error}"),
            );
        }
        Err(_) => {
            return request_send_failure(
                RequestSendState::NotSent,
                format!(
                    "connect to daemon timed out ({}s)",
                    DAEMON_CONNECT_TIMEOUT.as_secs()
                ),
            );
        }
    };

    daemon_request_on_stream(stream, request).await
}

async fn daemon_request_on_stream<S>(mut stream: S, request: String) -> DaemonRequestOutcome
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    // Reserve this state before write_all: every write failure is possibly
    // dispatched and therefore cannot be retried or reported as unavailable.
    let send_state = RequestSendState::PossiblyDispatched;

    match tokio::time::timeout(DAEMON_IO_TIMEOUT, stream.write_all(request.as_bytes())).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            return request_send_failure(
                send_state,
                format!("failed to write request to daemon: {error}"),
            );
        }
        Err(_) => return request_send_failure(send_state, "write to daemon timed out".into()),
    }

    let mut response_buf = Vec::with_capacity(4096);
    match tokio::time::timeout(DAEMON_IO_TIMEOUT, stream.read_to_end(&mut response_buf)).await {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => {
            return DaemonRequestOutcome::Uncertain(format!(
                "failed to read response from daemon: {error}"
            ));
        }
        Err(_) => return DaemonRequestOutcome::Uncertain("read from daemon timed out".into()),
    }

    match parse_http_response(&response_buf) {
        Ok(body) => DaemonRequestOutcome::Completed(body),
        Err(error) => DaemonRequestOutcome::Uncertain(format!("{error:#}")),
    }
}

fn request_send_failure(state: RequestSendState, message: String) -> DaemonRequestOutcome {
    match state {
        RequestSendState::NotSent => DaemonRequestOutcome::Unavailable(message),
        RequestSendState::PossiblyDispatched => DaemonRequestOutcome::Uncertain(message),
    }
}

/// Check if the daemon is reachable by hitting /health.
pub async fn daemon_health_check() -> bool {
    match daemon_request("GET", "/health", "").await {
        Ok(body) => body.trim() == "ok",
        Err(_) => false,
    }
}

/// Call a tool on the daemon's REST API.
pub async fn daemon_tool_call(name: &str, arguments: Option<&serde_json::Value>) -> Result<String> {
    daemon_request(
        "POST",
        "/v1/tools/call",
        &format_daemon_tool_call_body(name, arguments),
    )
    .await
}

fn format_daemon_tool_call_body(name: &str, arguments: Option<&serde_json::Value>) -> String {
    serde_json::json!({
        "name": name,
        "arguments": arguments,
    })
    .to_string()
}

fn format_http_request(method: &str, path: &str, body: &str) -> String {
    if body.is_empty() {
        format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
    } else {
        let content_length = body.len();
        format!(
            "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {content_length}\r\nConnection: close\r\n\r\n{body}"
        )
    }
}

fn parse_http_response(raw: &[u8]) -> Result<String> {
    let response_str = std::str::from_utf8(raw).context("daemon response is not valid UTF-8")?;

    let Some(header_end) = response_str.find("\r\n\r\n") else {
        anyhow::bail!("malformed HTTP response from daemon (no header boundary)");
    };

    let headers = &response_str[..header_end];
    let body = &response_str[header_end + 4..];

    let status_line = headers.lines().next().unwrap_or("");
    let Some(status_code) = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
    else {
        anyhow::bail!("malformed HTTP response from daemon (invalid status line)");
    };

    if !(200..300).contains(&status_code) {
        anyhow::bail!("daemon returned HTTP {status_code}: {body}");
    }

    Ok(body.to_string())
}

/// Attempt to connect to the daemon. Returns `None` if not running.
pub async fn try_daemon_request(method: &str, path: &str, body: &str) -> Option<String> {
    if !daemon::is_daemon_running() {
        return None;
    }
    daemon_request(method, path, body).await.ok()
}

/// Tell a *running* daemon to drop its in-memory read cache (`SessionCache`).
/// Returns `true` if a daemon was reached. Never auto-starts a daemon — if none
/// is running there is no cache to flush. Force-rebuild CLI commands call this so
/// `ctx_read` map/signatures stop serving pre-rebuild output from the daemon's
/// long-lived cache, which CLI index rebuilds otherwise can't reach (#420).
pub fn notify_cache_clear() -> bool {
    if !daemon::is_daemon_running() {
        return false;
    }
    let Ok(rt) = tokio::runtime::Runtime::new() else {
        return false;
    };
    let body = serde_json::json!({
        "name": "ctx_cache",
        "arguments": { "action": "clear" },
    });
    rt.block_on(async {
        try_daemon_request("POST", "/v1/tools/call", &body.to_string())
            .await
            .is_some()
    })
}

async fn ensure_daemon_ready() -> bool {
    let addr = daemon::daemon_addr();
    let mut ready = addr.is_listening() && daemon_health_check().await;

    if !ready {
        // Connect-only for shadow-mode hooks (#566): a hook child reaches a live
        // daemon via the `ready` fast-path above (full detector parity), but when
        // none is listening it must bail to the standalone fallback instead of
        // auto-starting one. This guard MUST stay inside `if !ready` — hoisting it
        // to the top of the function would also block hooks from reusing a running
        // daemon, silently regressing loop/bounce/adaptive parity.
        if crate::core::runtime_flags::hook_child_enabled() {
            return false;
        }

        let lock = crate::core::startup_guard::try_acquire_lock(
            "daemon-start",
            Duration::from_millis(1200),
            Duration::from_secs(5),
        );

        if let Some(g) = lock {
            g.touch();
            let mut did_start = false;

            if !daemon::is_daemon_running() {
                if daemon::start_daemon(&[]).is_ok() {
                    did_start = true;
                } else {
                    return false;
                }
            }

            for _ in 0..60 {
                if addr.is_listening() && daemon_health_check().await {
                    ready = true;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }

            if ready && did_start && crate::core::protocol::meta_visible() {
                eprintln!("\x1b[2m▸ daemon auto-started\x1b[0m");
            }
        } else {
            for _ in 0..60 {
                if addr.is_listening() && daemon_health_check().await {
                    ready = true;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }

    ready
}

/// Blocking helper for CLI commands: routes a tool call through the daemon.
///
/// Returns `None` if no daemon can serve the call (caller then renders the tool
/// locally / standalone). Behaviour splits on caller identity:
///
/// - **Normal CLI invocation**: connects to a running daemon, and *auto-starts*
///   one if none is listening (so the long-lived `LeanCtxServer` state — caches,
///   indexes, detectors — is reused across commands).
/// - **Shadow-mode hook child** (`LEAN_CTX_HOOK_CHILD` set): *connect-only*. It
///   reuses an already-running daemon for full parity (the `ready` fast-path
///   below routes straight through `/v1/tools/call` → `call_tool_guarded`, so the
///   in-memory `LoopDetector`, correction-loop auto-degrade, bounce tracker and
///   adaptive thresholds all fire on the daemon's long-lived state — #566), but
///   it MUST NEVER auto-start a daemon. A hook fires once per intercepted
///   read/grep as a fresh process; auto-starting from there would spawn daemons
///   uncontrollably. With no live daemon it returns `None` and the caller falls
///   back to the enriched standalone path (disk-backed learning sinks + Context
///   IR from #550/#569).
#[allow(clippy::needless_pass_by_value)]
pub fn try_daemon_tool_call_blocking(
    name: &str,
    arguments: Option<serde_json::Value>,
) -> Option<String> {
    if std::env::var_os("__LEAN_CTX_NO_DAEMON").is_some() {
        return None;
    }

    let rt = Runtime::new().ok()?;
    if !rt.block_on(ensure_daemon_ready()) {
        return None;
    }

    if let Some(out) = rt.block_on(async { daemon_tool_call(name, arguments.as_ref()).await.ok() })
    {
        return Some(out);
    }

    for _ in 0..5 {
        std::thread::sleep(Duration::from_millis(50));
        if let Some(out) =
            rt.block_on(async { daemon_tool_call(name, arguments.as_ref()).await.ok() })
        {
            return Some(out);
        }
    }

    None
}

#[cfg_attr(not(unix), allow(dead_code))] // the daemon route is cfg(unix) in read_cmd
async fn daemon_tool_call_observed(
    name: &str,
    arguments: Option<&serde_json::Value>,
) -> DaemonRequestOutcome {
    daemon_request_observed(
        "POST",
        "/v1/tools/call",
        &format_daemon_tool_call_body(name, arguments),
    )
    .await
}

#[cfg_attr(not(unix), allow(dead_code))] // the daemon route is cfg(unix) in read_cmd
pub(crate) fn try_daemon_tool_call_blocking_observed(
    name: &str,
    arguments: Option<serde_json::Value>,
) -> DaemonToolCallOutcome {
    if std::env::var_os("__LEAN_CTX_NO_DAEMON").is_some() {
        return DaemonToolCallOutcome::Unavailable;
    }

    let Some(outcome) = delivery_block_on(async move {
        if !ensure_daemon_ready().await {
            return DaemonToolCallOutcome::Unavailable;
        }
        observed_tool_call_outcome(daemon_tool_call_observed(name, arguments.as_ref()).await)
    }) else {
        return DaemonToolCallOutcome::Unavailable;
    };
    outcome
}

#[cfg_attr(not(unix), allow(dead_code))] // the daemon route is cfg(unix) in read_cmd
fn observed_tool_call_outcome(outcome: DaemonRequestOutcome) -> DaemonToolCallOutcome {
    match outcome {
        DaemonRequestOutcome::Unavailable(_) => DaemonToolCallOutcome::Unavailable,
        DaemonRequestOutcome::Uncertain(message) => DaemonToolCallOutcome::Uncertain { message },
        DaemonRequestOutcome::Completed(body) => {
            let value: serde_json::Value = match serde_json::from_str(&body) {
                Ok(value) => value,
                Err(_) => {
                    return DaemonToolCallOutcome::Uncertain {
                        message: "invalid daemon tool response".into(),
                    };
                }
            };
            let Some(result) = value.get("result") else {
                return DaemonToolCallOutcome::Uncertain {
                    message: "daemon tool response has no result".into(),
                };
            };
            let parsed: rmcp::model::CallToolResult = match serde_json::from_value(result.clone()) {
                Ok(parsed) => parsed,
                Err(_) => {
                    return DaemonToolCallOutcome::Uncertain {
                        message: "daemon tool response has an invalid result".into(),
                    };
                }
            };
            let is_error = parsed.is_error.unwrap_or(false);
            let empty_text = result
                .get("content")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|content| {
                    content.iter().all(|item| {
                        item.get("type").and_then(serde_json::Value::as_str) == Some("text")
                            && item.get("text").and_then(serde_json::Value::as_str) == Some("")
                    })
                });
            let text = if empty_text {
                String::new()
            } else {
                // Preserve the legacy JSON representation for valid non-text
                // content instead of silently dropping images/resources.
                unwrap_mcp_tool_text(&body).unwrap_or(body)
            };

            DaemonToolCallOutcome::Completed { text, is_error }
        }
    }
}

fn unwrap_mcp_tool_text(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let result = v.get("result")?;

    if let Some(content) = result.get("content").and_then(|c| c.as_array()) {
        let mut texts: Vec<String> = Vec::new();
        for item in content {
            if let Some(text) = item.get("text").and_then(|t| t.as_str())
                && !text.is_empty()
            {
                texts.push(text.to_string());
            }
        }
        if !texts.is_empty() {
            return Some(texts.join("\n"));
        }
    }

    if let Some(text) = result.get("text").and_then(|t| t.as_str()) {
        return Some(text.to_string());
    }

    result.as_str().map(std::string::ToString::to_string)
}

/// Like `try_daemon_tool_call_blocking`, but unwraps MCP JSON responses to text for CLI output.
pub fn try_daemon_tool_call_blocking_text(
    name: &str,
    arguments: Option<serde_json::Value>,
) -> Option<String> {
    let body = try_daemon_tool_call_blocking(name, arguments)?;
    let trimmed = body.trim_start();
    if !trimmed.starts_with('{') {
        return Some(body);
    }
    Some(match unwrap_mcp_tool_text(&body) {
        Some(text) if !text.is_empty() => text,
        Some(_) | None => body,
    })
}

pub fn scoped_delivery_check_blocking(
    profile: &crate::core::a2a::task::delivery_authority::DeliverySigningProfileV1,
    path: &str,
    blake3: [u8; 12],
    conversation_id: &str,
) -> anyhow::Result<Option<crate::core::ocla::types::DeliveryRecord>> {
    let request = profile.sign_request(
        crate::core::a2a::task::delivery_authority::DeliveryOperation::Check {
            path: path.into(),
            blake3,
            conversation_id: Some(conversation_id.into()),
        },
    )?;
    delivery_block_on(async {
        let response = scoped_delivery_request(&request).await?;
        decode_scoped_delivery_hit(&request, response)
    })
    .ok_or_else(|| anyhow::anyhow!("delivery runtime unavailable"))?
}

pub fn scoped_delivery_record_blocking(
    profile: &crate::core::a2a::task::delivery_authority::DeliverySigningProfileV1,
    entry: crate::core::ocla::types::DeliveryEntry,
) -> anyhow::Result<()> {
    let request = profile.sign_request(
        crate::core::a2a::task::delivery_authority::DeliveryOperation::Record { entry },
    )?;
    delivery_block_on(async {
        let response = scoped_delivery_request(&request).await?;
        let _: crate::core::ocla::types::DeliveryRecordResult = serde_json::from_value(response)?;
        Ok(())
    })
    .ok_or_else(|| anyhow::anyhow!("delivery runtime unavailable"))?
}

/// Send a signed scoped request without falling back to any legacy endpoint.
/// The caller provisions the signing identity and grants; this never creates
/// authority or starts a daemon implicitly.
pub async fn scoped_delivery_request(
    request: &crate::core::a2a::task::delivery_authority::SignedDeliveryRequest,
) -> anyhow::Result<serde_json::Value> {
    // The bounded IPC exchange is authoritative. A PID-file probe adds work
    // to every read and can unlink a live socket when stale metadata survives.
    scoped_delivery_request_at(&daemon::daemon_addr(), request).await
}

/// Explicit local endpoint, used by isolated hosts and transport acceptance tests.
pub(crate) async fn scoped_delivery_request_at(
    addr: &ipc::DaemonAddr,
    request: &crate::core::a2a::task::delivery_authority::SignedDeliveryRequest,
) -> anyhow::Result<serde_json::Value> {
    anyhow::ensure!(
        request.authority.action == request.request.action()
            && request.authority.request_digest == request.bound_request_digest(),
        "scoped delivery request does not match its signed authority"
    );
    let body = serde_json::to_string(request)?;
    let response = tokio::time::timeout(
        Duration::from_secs(3),
        daemon_request_at_observed(addr, "POST", "/ocla/v1/delivery/scoped", &body),
    )
    .await
    .map_err(|_| anyhow::anyhow!("scoped delivery timed out"))?;
    let response = match response {
        DaemonRequestOutcome::Completed(body) => body,
        DaemonRequestOutcome::Unavailable(message) | DaemonRequestOutcome::Uncertain(message) => {
            anyhow::bail!(message);
        }
    };
    Ok(serde_json::from_str(&response)?)
}

/// Decode a scoped hit only when its namespace, content and privacy match the
/// signed lookup. A malformed or legacy response is an error, never a cache miss.
pub fn decode_scoped_delivery_hit(
    request: &crate::core::a2a::task::delivery_authority::SignedDeliveryRequest,
    response: serde_json::Value,
) -> anyhow::Result<Option<crate::core::ocla::types::DeliveryRecord>> {
    use crate::core::a2a::task::delivery_authority::DeliveryOperation;
    let DeliveryOperation::Check {
        path,
        blake3,
        conversation_id,
    } = &request.request
    else {
        anyhow::bail!("scoped delivery response requires a lookup request");
    };
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct LookupResponse {
        hit: bool,
        record: Option<crate::core::ocla::types::DeliveryRecord>,
    }
    let response: LookupResponse = serde_json::from_value(response)?;
    anyhow::ensure!(
        response.hit == response.record.is_some(),
        "inconsistent delivery response"
    );
    let Some(record) = response.record else {
        return Ok(None);
    };
    let scope = lean_ctx_ocla::delivery_scope::DeliveryScopeV1::new(
        request.authority.tenant_id.clone(),
        request.authority.project_id.clone(),
    )
    .map_err(anyhow::Error::msg)?;
    let access = record
        .access
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("unscoped delivery response"))?;
    anyhow::ensure!(
        record.path == *path
            && record.blake3 == *blake3
            && access.privacy.permits(
                &access.scope,
                &scope,
                &record.agent_id,
                &request.authority.sender
            )
            && conversation_id
                .as_ref()
                .is_none_or(|id| *id != record.conversation_id),
        "delivery response does not match authorized lookup"
    );
    Ok(Some(record))
}

/// Check the daemon's cross-agent delivery registry for a content hash.
/// Returns `None` if daemon unreachable or no hit. Connect-only — never
/// auto-starts a daemon (delivery is best-effort).
pub fn try_delivery_check_blocking(
    blake3: &[u8; 12],
    mtime: u64,
    path: &str,
    requester_agent_id: Option<&str>,
    requester_conversation_id: Option<&str>,
) -> Option<crate::core::ocla::types::DeliveryRecord> {
    if !daemon::is_daemon_running() {
        return None;
    }
    let body = serde_json::json!({
        "blake3": blake3,
        "mtime": mtime,
        "path": path,
        "requester_agent_id": requester_agent_id,
        "requester_conversation_id": requester_conversation_id,
    });
    let body_str = body.to_string();
    let resp = delivery_block_on(async move {
        tokio::time::timeout(
            BEST_EFFORT_DELIVERY_IPC_TIMEOUT,
            try_daemon_request("POST", "/ocla/v1/delivery/check", &body_str),
        )
        .await
        .ok()
        .flatten()
    })??;
    let v: serde_json::Value = serde_json::from_str(&resp).ok()?;
    if !v.get("hit")?.as_bool()? {
        return None;
    }
    Some(crate::core::ocla::types::DeliveryRecord {
        access: None,
        blake3: *blake3,
        path: v.get("path")?.as_str()?.to_string(),
        line_count: v.get("line_count")?.as_u64()? as u32,
        token_count: v.get("token_count").and_then(serde_json::Value::as_u64)?,
        agent_id: v.get("agent_id")?.as_str()?.to_string(),
        conversation_id: v.get("conversation_id")?.as_str()?.to_string(),
        read_at: v.get("read_at")?.as_u64()?,
        mtime: v
            .get("mtime")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(mtime),
        fresh: v
            .get("fresh")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true),
        relay_content: v
            .get("relay_content")
            .and_then(serde_json::Value::as_str)
            .map(String::from),
        relay_mode: v
            .get("relay_mode")
            .and_then(serde_json::Value::as_str)
            .map(String::from),
    })
}

/// Record a delivery in the daemon's cross-agent registry.
/// Fire-and-forget: errors and slow daemon responses are intentionally dropped.
pub fn try_delivery_record_blocking(entry: &crate::core::ocla::types::DeliveryEntry) {
    if !daemon::is_daemon_running() {
        return;
    }
    let Ok(body) = serde_json::to_string(entry) else {
        return;
    };
    let Ok(rt) = delivery_runtime() else {
        return;
    };
    drop(rt.spawn(async move {
        let _ = tokio::time::timeout(
            Duration::from_secs(3),
            try_daemon_request("POST", "/ocla/v1/delivery/record", &body),
        )
        .await;
    }));
}

/// Check the daemon's generalized cross-agent cache (all DeliveryKinds).
/// Returns the cached entry on hit, None on miss or daemon unreachable.
pub fn try_cache_check_blocking(
    key: &crate::core::ocla::cache_types::CacheKey,
    validator: &crate::core::ocla::cache_types::CacheValidator,
    requester_agent_id: Option<&str>,
    requester_conversation_id: Option<&str>,
) -> Option<crate::core::ocla::cache_types::DeliveryEntryV2> {
    if !daemon::is_daemon_running() {
        return None;
    }
    let validator_str = match validator {
        crate::core::ocla::cache_types::CacheValidator::Immutable => "immutable".into(),
        crate::core::ocla::cache_types::CacheValidator::File { mtime_ns } => {
            format!("file:{mtime_ns}")
        }
        crate::core::ocla::cache_types::CacheValidator::Directory { mtime_ns } => {
            format!("directory:{mtime_ns}")
        }
    };
    let body = serde_json::json!({
        "key": key.0,
        "validator": validator_str,
        "requester_agent_id": requester_agent_id,
        "requester_conversation_id": requester_conversation_id,
    });
    let body_str = body.to_string();
    let resp = delivery_block_on(async move {
        tokio::time::timeout(
            BEST_EFFORT_DELIVERY_IPC_TIMEOUT,
            try_daemon_request("POST", "/ocla/v1/cache/check", &body_str),
        )
        .await
        .ok()
        .flatten()
    })??;
    let v: serde_json::Value = serde_json::from_str(&resp).ok()?;
    if !v.get("hit")?.as_bool()? {
        return None;
    }
    serde_json::from_value(v.get("entry")?.clone()).ok()
}

/// Record a generalized cache entry via daemon IPC. Fire-and-forget.
pub fn try_cache_record_blocking(entry: &crate::core::ocla::cache_types::DeliveryEntryV2) {
    if !daemon::is_daemon_running() {
        return;
    }
    let Ok(body) = serde_json::to_string(entry) else {
        return;
    };
    let Ok(rt) = delivery_runtime() else { return };
    drop(rt.spawn(async move {
        let _ = tokio::time::timeout(
            Duration::from_secs(3),
            try_daemon_request("POST", "/ocla/v1/cache/record", &body),
        )
        .await;
    }));
}

#[cfg(test)]
mod tests {
    use std::{
        future::Future,
        io,
        pin::Pin,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        task::{Context, Poll},
    };

    use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, DuplexStream, ReadBuf};
    use tokio::runtime::Runtime;

    use super::{
        DaemonRequestOutcome, DaemonToolCallOutcome, daemon_request_on_stream,
        daemon_request_with_connector, delivery_runtime, format_http_request,
        observed_tool_call_outcome,
    };

    fn block_on<F: Future>(future: F) -> F::Output {
        Runtime::new()
            .expect("test runtime initializes")
            .block_on(future)
    }

    fn http_response(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    async fn exchange(response_body: &str) -> DaemonRequestOutcome {
        let (client, mut server) = tokio::io::duplex(4096);
        let response = http_response(response_body);
        let server_task = tokio::spawn(async move {
            let mut request = [0u8; 4096];
            let read = server
                .read(&mut request)
                .await
                .expect("duplex server reads request");
            assert!(read > 0);
            server
                .write_all(response.as_bytes())
                .await
                .expect("duplex server writes response");
            server.shutdown().await.expect("duplex server shuts down");
        });

        let outcome =
            daemon_request_on_stream(client, format_http_request("POST", "/v1/tools/call", "{}"))
                .await;
        server_task.await.expect("duplex server task joins");
        outcome
    }

    struct PartialWriteThenError {
        inner: DuplexStream,
        write_calls: Arc<AtomicUsize>,
    }

    impl AsyncRead for PartialWriteThenError {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            read_buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            Pin::new(&mut self.inner).poll_read(cx, read_buf)
        }
    }

    impl AsyncWrite for PartialWriteThenError {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            if self.write_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                let partial = buf.len().min(4);
                Pin::new(&mut self.inner).poll_write(cx, &buf[..partial])
            } else {
                Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "injected partial write failure",
                )))
            }
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    struct PartialReadThenError {
        response: Vec<u8>,
        offset: usize,
    }

    impl AsyncRead for PartialReadThenError {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            read_buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            if self.offset < self.response.len() {
                let end = (self.offset + read_buf.remaining()).min(self.response.len());
                read_buf.put_slice(&self.response[self.offset..end]);
                self.offset = end;
                Poll::Ready(Ok(()))
            } else {
                Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::ConnectionReset,
                    "injected partial read failure",
                )))
            }
        }
    }

    impl AsyncWrite for PartialReadThenError {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            Poll::Ready(Ok(buf.len()))
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    #[test]
    fn delivery_runtime_is_shared() {
        let first = delivery_runtime().expect("shared delivery runtime initializes");
        let second = delivery_runtime().expect("shared delivery runtime remains available");
        assert!(std::ptr::eq(first, second));
    }

    #[test]
    fn observed_success_text_is_completed() {
        let body = r#"{"result":{"content":[{"type":"text","text":"hello"}]}}"#;
        let outcome = observed_tool_call_outcome(block_on(exchange(body)));
        assert_eq!(
            outcome,
            DaemonToolCallOutcome::Completed {
                text: "hello".into(),
                is_error: false,
            }
        );
    }

    #[test]
    fn observed_empty_text_is_completed_empty() {
        let body = r#"{"result":{"content":[{"type":"text","text":""}]}}"#;
        let outcome = observed_tool_call_outcome(block_on(exchange(body)));
        assert_eq!(
            outcome,
            DaemonToolCallOutcome::Completed {
                text: String::new(),
                is_error: false,
            }
        );
    }

    #[test]
    fn invalid_terminal_payload_is_uncertain_and_legacy_empty_fallback_is_preserved() {
        for body in [
            "garbage",
            "null",
            "[]",
            r#"{"result":{"content":[{"type":"text","text":7}]}}"#,
        ] {
            assert!(matches!(
                observed_tool_call_outcome(DaemonRequestOutcome::Completed(body.to_owned())),
                DaemonToolCallOutcome::Uncertain { .. }
            ));
        }
        let body = r#"{"result":{"content":[],"text":"legacy fallback"}}"#;
        assert_eq!(
            super::unwrap_mcp_tool_text(body),
            Some("legacy fallback".to_owned())
        );
        assert_eq!(
            observed_tool_call_outcome(DaemonRequestOutcome::Completed(
                r#"{"result":{"content":[]}}"#.to_owned()
            )),
            DaemonToolCallOutcome::Completed {
                text: String::new(),
                is_error: false
            }
        );
    }

    #[test]
    fn observed_tool_error_is_terminal_completed_error() {
        let body = r#"{"result":{"content":[{"type":"text","text":"denied"}],"isError":true}}"#;
        let outcome = observed_tool_call_outcome(block_on(exchange(body)));
        assert_eq!(
            outcome,
            DaemonToolCallOutcome::Completed {
                text: "denied".into(),
                is_error: true,
            }
        );
    }

    #[test]
    fn connect_failure_is_unavailable_before_dispatch() {
        let outcome = block_on(daemon_request_with_connector(
            async {
                Err::<DuplexStream, _>(io::Error::new(
                    io::ErrorKind::NotFound,
                    "injected connect failure",
                ))
            },
            "test-daemon",
            format_http_request("POST", "/v1/tools/call", "{}"),
        ));
        assert!(matches!(
            outcome,
            DaemonRequestOutcome::Unavailable(message)
                if message.contains("cannot connect to daemon")
        ));
    }

    #[test]
    fn partial_write_is_uncertain_and_connector_runs_once() {
        let (client, _server) = tokio::io::duplex(1024);
        let connector_calls = Arc::new(AtomicUsize::new(0));
        let write_calls = Arc::new(AtomicUsize::new(0));
        let stream = PartialWriteThenError {
            inner: client,
            write_calls: Arc::clone(&write_calls),
        };
        let connector_calls_for_future = Arc::clone(&connector_calls);
        let outcome = block_on(daemon_request_with_connector(
            async move {
                connector_calls_for_future.fetch_add(1, Ordering::SeqCst);
                Ok::<_, io::Error>(stream)
            },
            "test-daemon",
            format_http_request("POST", "/v1/tools/call", "{}"),
        ));
        assert!(matches!(
            outcome,
            DaemonRequestOutcome::Uncertain(message)
                if message.contains("failed to write request")
        ));
        assert_eq!(connector_calls.load(Ordering::SeqCst), 1);
        assert_eq!(write_calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn partial_read_is_uncertain_after_dispatch() {
        let stream = PartialReadThenError {
            response: b"HTTP/1.1 200 OK\r\n\r\n{\"result\"".to_vec(),
            offset: 0,
        };
        let outcome = block_on(daemon_request_on_stream(
            stream,
            format_http_request("POST", "/v1/tools/call", "{}"),
        ));
        assert!(matches!(
            outcome,
            DaemonRequestOutcome::Uncertain(message)
                if message.contains("failed to read response")
        ));
    }

    #[test]
    fn non_success_http_is_uncertain_not_unavailable() {
        let (client, mut server) = tokio::io::duplex(4096);
        let response = "HTTP/1.1 503 Service Unavailable\r\n\r\nrejected".to_string();
        let request = format_http_request("POST", "/v1/tools/call", "{}");
        let expected_request = request.clone();
        let outcome = block_on(async move {
            let server_task = tokio::spawn(async move {
                let mut received = vec![0u8; expected_request.len()];
                server
                    .read_exact(&mut received)
                    .await
                    .expect("server reads request");
                assert_eq!(received, expected_request.as_bytes());
                server
                    .write_all(response.as_bytes())
                    .await
                    .expect("server writes rejection");
                server.shutdown().await.expect("server shuts down");
            });
            let outcome = daemon_request_on_stream(client, request).await;
            server_task.await.expect("server task joins");
            outcome
        });
        assert!(
            matches!(outcome, DaemonRequestOutcome::Uncertain(message) if message.contains("503"))
        );
    }
}
