use anyhow::{Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::daemon;
use crate::ipc;

/// Send an HTTP request to the daemon over the IPC channel.
/// Returns the response body as a string.
pub async fn daemon_request(method: &str, path: &str, body: &str) -> Result<String> {
    use std::time::Duration;
    use tokio::time::timeout;

    const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
    const IO_TIMEOUT: Duration = Duration::from_secs(10);

    let addr = daemon::daemon_addr();
    if !addr.is_listening() {
        anyhow::bail!(
            "Daemon endpoint not found at {}. Is the daemon running?",
            addr.display()
        );
    }

    let request = format_http_request(method, path, body);

    #[cfg(unix)]
    {
        let mut stream = timeout(CONNECT_TIMEOUT, ipc::connect(&addr))
            .await
            .with_context(|| {
                format!(
                    "connect to daemon timed out ({}s)",
                    CONNECT_TIMEOUT.as_secs()
                )
            })?
            .with_context(|| format!("cannot connect to daemon at {}", addr.display()))?;

        timeout(IO_TIMEOUT, stream.write_all(request.as_bytes()))
            .await
            .context("write to daemon timed out")?
            .context("failed to write request to daemon")?;

        let mut response_buf = Vec::with_capacity(4096);
        timeout(IO_TIMEOUT, stream.read_to_end(&mut response_buf))
            .await
            .context("read from daemon timed out")?
            .context("failed to read response from daemon")?;

        parse_http_response(&response_buf)
    }

    #[cfg(windows)]
    {
        let mut stream = timeout(CONNECT_TIMEOUT, ipc::connect(&addr))
            .await
            .with_context(|| {
                format!(
                    "connect to daemon timed out ({}s)",
                    CONNECT_TIMEOUT.as_secs()
                )
            })?
            .with_context(|| format!("cannot connect to daemon at {}", addr.display()))?;

        timeout(IO_TIMEOUT, stream.write_all(request.as_bytes()))
            .await
            .context("write to daemon timed out")?
            .context("failed to write request to daemon")?;

        let mut response_buf = Vec::with_capacity(4096);
        timeout(IO_TIMEOUT, stream.read_to_end(&mut response_buf))
            .await
            .context("read from daemon timed out")?
            .context("failed to read response from daemon")?;

        parse_http_response(&response_buf)
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
    let body = serde_json::json!({
        "name": name,
        "arguments": arguments,
    });
    daemon_request("POST", "/v1/tools/call", &body.to_string()).await
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
    let status_code = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);

    if status_code >= 400 {
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

/// Blocking helper for CLI commands: calls a daemon tool if the daemon is running.
/// Returns `None` if the daemon is not running or the call fails.
/// Attempts to auto-start the daemon if it's not already running.
#[allow(clippy::needless_pass_by_value)]
pub fn try_daemon_tool_call_blocking(
    name: &str,
    arguments: Option<serde_json::Value>,
) -> Option<String> {
    use std::time::Duration;

    let rt = tokio::runtime::Runtime::new().ok()?;

    let addr = daemon::daemon_addr();
    let mut ready = addr.is_listening() && rt.block_on(async { daemon_health_check().await });

    if !ready {
        if std::env::var("LEAN_CTX_HOOK_CHILD").is_ok() {
            return None;
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
                    return None;
                }
            }

            for _ in 0..60 {
                if addr.is_listening() && rt.block_on(async { daemon_health_check().await }) {
                    ready = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }

            if ready && did_start && crate::core::protocol::meta_visible() {
                eprintln!("\x1b[2m▸ daemon auto-started\x1b[0m");
            }
        } else {
            for _ in 0..60 {
                if addr.is_listening() && rt.block_on(async { daemon_health_check().await }) {
                    ready = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }

    if !ready {
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
    Some(unwrap_mcp_tool_text(&body).unwrap_or(body))
}
