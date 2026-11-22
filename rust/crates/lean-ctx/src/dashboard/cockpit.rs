// SPDX-License-Identifier: Apache-2.0
//! The Cockpit behind the engine dashboard: one local entry point.
//!
//! The Cockpit is a separate, signed runtime package (`lean-ctx-cockpit`)
//! that listens on loopback only. The dashboard server stays the single
//! address the browser talks to: after its own token check it forwards `/`,
//! the Cockpit's static assets and `/api/gateway/*` to the Cockpit process,
//! and answers every other `/api/*` route itself. The Cockpit recognises
//! requests from the engine by the `X-LeanCTX-Cockpit` header, whose value
//! only the engine and the Cockpit know.
//!
//! Forwarding is byte-exact: request and response bodies are passed through
//! unchanged (fonts, SVG and other binary assets included), response headers
//! are kept apart from hop-by-hop ones, and a redirect is handed to the
//! browser rather than followed here.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use lean_ctx::core::app_presence::AppPresence;
pub(crate) use lean_ctx::core::intelligence_runtime::cockpit::CockpitUnavailable;
use lean_ctx::core::intelligence_runtime::cockpit::{CockpitLaunch, cockpit_endpoint};

/// Proves to the Cockpit that a request comes from this engine.
pub(crate) const COCKPIT_HEADER: &str = "X-LeanCTX-Cockpit";
/// The project a request is about; passed through to the Cockpit unchanged.
pub(crate) const PROJECT_HEADER: &str = "X-LeanCTX-Project";
/// The command that installs (or re-activates) the Cockpit runtime package.
pub(crate) const INSTALL_COMMAND: &str = "lean-ctx cockpit install";

const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
/// Proof reports in the Cockpit can verify large ledgers; bounded anyway.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(130);
/// Upper bound for one Cockpit response (largest assets are fonts/bundles).
const MAX_COCKPIT_RESPONSE: usize = 64 * 1024 * 1024;

/// Where the running Cockpit listens and the secret it expects.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct CockpitTarget {
    pub(crate) port: u16,
    pub(crate) token: String,
}

impl std::fmt::Debug for CockpitTarget {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CockpitTarget")
            .field("port", &self.port)
            .field("token", &"<redacted>")
            .finish()
    }
}

/// Resolves the Cockpit for one request, or why it is unavailable.
pub(crate) type CockpitResolver =
    Arc<dyn Fn() -> Result<CockpitTarget, CockpitUnavailable> + Send + Sync>;

/// The Cockpit of this dashboard, from the runtime package supervisor.
///
/// The supervisor starts the installed, authenticated Cockpit on first use
/// for this dashboard's port, token and project, restarts it after a crash
/// (with backoff) and reports why it is unavailable otherwise; then `/`
/// shows the install page and `/api/gateway/*` falls back to the engine's
/// own handlers.
pub(crate) fn runtime_resolver(
    engine_port: u16,
    engine_token: Option<String>,
    project_root: PathBuf,
) -> CockpitResolver {
    Arc::new(move || {
        let launch = CockpitLaunch {
            engine_port,
            engine_token: engine_token.clone().unwrap_or_default(),
            project_root: project_root.clone(),
        };
        cockpit_endpoint(&launch).map(|endpoint| CockpitTarget {
            port: endpoint.port,
            token: endpoint.cockpit_token,
        })
    })
}

/// Stop this process's Cockpit (dashboard shutdown).
pub(crate) fn retire() {
    lean_ctx::core::intelligence_runtime::cockpit::retire();
}

/// Whether the Cockpit (not the engine) answers `path`. The engine keeps
/// every `/api/*` route except `/api/gateway/*`, and `/metrics`.
pub(crate) fn serves(path: &str) -> bool {
    if path.starts_with("/api/gateway/") {
        return true;
    }
    !(path.starts_with("/api/") || path == "/api" || path == "/metrics")
}

/// Request headers never forwarded: hop-by-hop headers, the framing this
/// proxy sets itself, the dashboard credential (the Cockpit gets its own
/// header instead) and a client-supplied Cockpit header.
fn forwardable_request_header(name: &str) -> bool {
    !matches!(
        name.to_ascii_lowercase().as_str(),
        "host"
            | "connection"
            | "keep-alive"
            | "proxy-connection"
            | "proxy-authorization"
            | "proxy-authenticate"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "content-length"
            | "authorization"
            | "x-leanctx-cockpit"
    )
}

/// Response headers not passed back: hop-by-hop and framing, which this
/// proxy sets for the de-chunked body it writes.
fn forwardable_response_header(name: &str) -> bool {
    !matches!(
        name.to_ascii_lowercase().as_str(),
        "connection"
            | "keep-alive"
            | "proxy-connection"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "content-length"
    )
}

/// The request target without the dashboard credential in the query string.
pub(crate) fn forwarded_target(raw_path: &str) -> String {
    let Some((path, query)) = raw_path.split_once('?') else {
        return raw_path.to_string();
    };
    let kept: Vec<&str> = query
        .split('&')
        .filter(|pair| {
            let key = pair.split_once('=').map_or(*pair, |(k, _)| k);
            !pair.is_empty() && key != "token" && key != "access_token"
        })
        .collect();
    if kept.is_empty() {
        path.to_string()
    } else {
        format!("{path}?{}", kept.join("&"))
    }
}

/// The HTTP/1.1 request sent to the Cockpit for one browser request.
pub(crate) fn build_request(
    target: &CockpitTarget,
    method: &str,
    raw_path: &str,
    header_section: &str,
    body: &[u8],
) -> Vec<u8> {
    let mut head = format!(
        "{method} {} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n",
        forwarded_target(raw_path),
        target.port
    );
    for line in header_section.lines().skip(1) {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() || !forwardable_request_header(name) {
            continue;
        }
        head.push_str(name);
        head.push_str(": ");
        head.push_str(value.trim());
        head.push_str("\r\n");
    }
    head.push_str(&format!(
        "{COCKPIT_HEADER}: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        target.token,
        body.len()
    ));
    let mut request = head.into_bytes();
    request.extend_from_slice(body);
    request
}

/// Decode a `Transfer-Encoding: chunked` body; `None` when malformed.
fn decode_chunked(mut raw: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(raw.len());
    loop {
        let line_end = raw.windows(2).position(|w| w == b"\r\n")?;
        let size_text = std::str::from_utf8(&raw[..line_end]).ok()?;
        let size_text = size_text.split(';').next()?.trim();
        let size = usize::from_str_radix(size_text, 16).ok()?;
        raw = &raw[line_end + 2..];
        if size == 0 {
            return Some(out);
        }
        if raw.len() < size + 2 || &raw[size..size + 2] != b"\r\n" {
            return None;
        }
        out.extend_from_slice(&raw[..size]);
        raw = &raw[size + 2..];
    }
}

/// Re-frame the Cockpit's raw response for the browser: same status line,
/// same end-to-end headers, the body byte-exact (de-chunked when needed)
/// with an explicit length. `None` for an unreadable response.
pub(crate) fn reframe_response(raw: &[u8]) -> Option<Vec<u8>> {
    let header_end = raw.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = std::str::from_utf8(&raw[..header_end]).ok()?;
    let mut lines = head.split("\r\n");
    let status_line = lines.next()?;
    if !status_line.starts_with("HTTP/1.") {
        return None;
    }
    let mut chunked = false;
    let mut declared_length: Option<usize> = None;
    let mut headers = String::new();
    for line in lines {
        let (name, value) = line.split_once(':')?;
        let name = name.trim();
        let value = value.trim();
        if name.eq_ignore_ascii_case("transfer-encoding") {
            chunked = value.to_ascii_lowercase().contains("chunked");
        }
        if name.eq_ignore_ascii_case("content-length") {
            declared_length = Some(value.parse().ok()?);
        }
        if forwardable_response_header(name) {
            headers.push_str(name);
            headers.push_str(": ");
            headers.push_str(value);
            headers.push_str("\r\n");
        }
    }
    let rest = &raw[header_end + 4..];
    let body = if chunked {
        decode_chunked(rest)?
    } else if let Some(length) = declared_length {
        if rest.len() < length {
            return None;
        }
        rest[..length].to_vec()
    } else {
        rest.to_vec()
    };
    let mut response = format!(
        "{status_line}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    response.extend_from_slice(&body);
    Some(response)
}

/// Forward one request to the Cockpit and return the response to write back.
/// `None` when the Cockpit cannot be reached or answers unreadably.
pub(crate) async fn forward(
    target: &CockpitTarget,
    method: &str,
    raw_path: &str,
    header_section: &str,
    body: &[u8],
) -> Option<Vec<u8>> {
    let address = std::net::SocketAddr::from(([127, 0, 0, 1], target.port));
    let mut stream = tokio::time::timeout(CONNECT_TIMEOUT, tokio::net::TcpStream::connect(address))
        .await
        .ok()?
        .ok()?;
    let request = build_request(target, method, raw_path, header_section, body);
    let exchange = async {
        stream.write_all(&request).await.ok()?;
        let mut raw = Vec::new();
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            let n = stream.read(&mut buf).await.ok()?;
            if n == 0 {
                break;
            }
            raw.extend_from_slice(&buf[..n]);
            if raw.len() > MAX_COCKPIT_RESPONSE {
                return None;
            }
        }
        Some(raw)
    };
    let raw = tokio::time::timeout(RESPONSE_TIMEOUT, exchange)
        .await
        .ok()??;
    reframe_response(&raw)
}

/// What `/` shows while the Cockpit is not installed or not running, for
/// this machine (managed by LeanCTX Code or not).
pub(crate) fn unavailable_page(reason: CockpitUnavailable) -> String {
    render_unavailable_page(reason, lean_ctx::core::app_presence::detect().as_ref())
}

/// "Managed in LeanCTX Code" with the link that opens the app, when it gave
/// one. The link passed `app_presence::valid_open_url`, so it needs no
/// escaping.
fn managed_section(presence: &AppPresence) -> String {
    let open = presence.open_url.as_deref().map_or_else(
        || "<p>Open LeanCTX Code from your applications.</p>".to_string(),
        |url| format!("<p><a href=\"{url}\">Open LeanCTX Code</a></p>"),
    );
    format!(
        "<section class=\"managed\" data-managed=\"leanctx-code\">\n<h2>Managed in LeanCTX Code</h2>\n<p>LeanCTX Code manages LeanCTX on this machine. The Cockpit, settings and updates are there.</p>\n{open}\n</section>\n"
    )
}

/// The page itself: what the Cockpit is, why it is not shown (`reason`),
/// and either the calm "Managed in LeanCTX Code" state (`managed`) or how to
/// install it (not on a source build, which cannot), plus the terminal
/// alternative.
pub(crate) fn render_unavailable_page(
    reason: CockpitUnavailable,
    managed: Option<&AppPresence>,
) -> String {
    let install = if let Some(presence) = managed {
        managed_section(presence)
    } else if reason == CockpitUnavailable::SourceBuild {
        String::new()
    } else {
        format!(
            "<h2>Install the Cockpit</h2>\n<pre><code>{INSTALL_COMMAND}</code></pre>\n<p class=\"tag\">Status: <code>lean-ctx setup runtime status --kind cockpit</code></p>\n"
        )
    };
    // A managed machine reads calmly: the reason stays in `data-reason`,
    // its fix (an install command) is the app's business.
    let status = if managed.is_some() {
        "The Cockpit opens in LeanCTX Code on this machine.".to_string()
    } else {
        reason
            .message()
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>LeanCTX Cockpit</title>
<style>
:root {{ color-scheme: light dark; --fg: #1d1d1f; --muted: #5f6368; --bg: #fafafa; --card: #ffffff; --line: #dadce0; }}
@media (prefers-color-scheme: dark) {{ :root {{ --fg: #e8eaed; --muted: #9aa0a6; --bg: #151517; --card: #1f1f22; --line: #3c4043; }} }}
body {{ margin: 0; background: var(--bg); color: var(--fg); font: 16px/1.55 system-ui, -apple-system, "Segoe UI", sans-serif; }}
main {{ max-width: 38rem; margin: 12vh auto; padding: 0 16px; }}
h1 {{ font-size: 1.6rem; margin: 0 0 .25rem; }}
.tag {{ color: var(--muted); margin: 0 0 1.5rem; letter-spacing: .02em; }}
h2 {{ font-size: 1rem; margin: 1.75rem 0 .5rem; }}
pre {{ background: var(--card); border: 1px solid var(--line); border-radius: 8px; padding: .75rem 1rem; overflow-x: auto; }}
code {{ font: .95rem/1.4 ui-monospace, SFMono-Regular, Menlo, monospace; }}
.managed {{ background: var(--card); border: 1px solid var(--line); border-radius: 8px; padding: .25rem 1rem .5rem; margin-top: 1.5rem; }}
a {{ color: inherit; }}
</style>
</head>
<body>
<main>
<h1>LeanCTX Cockpit</h1>
<p class="tag">Select · Control · Prove</p>
<p>The Cockpit is the local view of LeanCTX: which context was selected, which controls applied, and the proof of what was delivered. It runs on this machine as a signed runtime package and is free for everyone.</p>
<p class="status" data-reason="{code}">{status}</p>
{install}<h2>In the terminal</h2>
<pre><code>lean-ctx gain</code></pre>
</main>
</body>
</html>
"#,
        code = reason.as_str(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> CockpitTarget {
        CockpitTarget {
            port: 4100,
            token: "cockpit-secret".into(),
        }
    }

    #[test]
    fn the_engine_keeps_its_api_and_metrics_and_the_cockpit_gets_the_rest() {
        for engine in ["/api/stats", "/api/roi", "/api/gateway", "/metrics", "/api"] {
            assert!(!serves(engine), "{engine} belongs to the engine");
        }
        for cockpit in [
            "/",
            "/index.html",
            "/app.js",
            "/assets/fonts/inter.woff2",
            "/assets/brand/mark.svg",
            "/api/gateway/pro/memory",
        ] {
            assert!(serves(cockpit), "{cockpit} belongs to the Cockpit");
        }
    }

    #[test]
    fn the_forwarded_request_carries_the_cockpit_secret_but_never_the_dashboard_credential() {
        let headers = "GET /api/gateway/receipts?token=dash&limit=5 HTTP/1.1\r\n\
                       Host: localhost:3333\r\n\
                       Authorization: Bearer dash\r\n\
                       X-LeanCTX-Cockpit: forged\r\n\
                       X-LeanCTX-Project: /work/repo\r\n\
                       Accept: application/json\r\n\
                       Connection: keep-alive";
        let request = build_request(
            &target(),
            "GET",
            "/api/gateway/receipts?token=dash&limit=5",
            headers,
            b"",
        );
        let text = String::from_utf8(request).unwrap();

        assert!(text.starts_with("GET /api/gateway/receipts?limit=5 HTTP/1.1\r\n"));
        assert!(text.contains("Host: 127.0.0.1:4100\r\n"));
        assert!(text.contains("X-LeanCTX-Cockpit: cockpit-secret\r\n"));
        assert!(text.contains("X-LeanCTX-Project: /work/repo\r\n"));
        assert!(text.contains("Accept: application/json\r\n"));
        assert!(!text.contains("dash"), "dashboard token leaked: {text}");
        assert!(!text.contains("forged"));
        assert_eq!(text.matches("Connection:").count(), 1);
    }

    #[test]
    fn a_binary_body_survives_chunked_framing_byte_for_byte() {
        let font: Vec<u8> = (0u8..=255).cycle().take(5000).collect();
        let mut raw = b"HTTP/1.1 200 OK\r\nContent-Type: font/woff2\r\nTransfer-Encoding: chunked\r\nCache-Control: max-age=60\r\n\r\n".to_vec();
        for chunk in font.chunks(1500) {
            raw.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
            raw.extend_from_slice(chunk);
            raw.extend_from_slice(b"\r\n");
        }
        raw.extend_from_slice(b"0\r\n\r\n");

        let framed = reframe_response(&raw).expect("readable response");
        let end = framed.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let head = std::str::from_utf8(&framed[..end]).unwrap();
        assert!(head.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(head.contains("Content-Type: font/woff2"));
        assert!(head.contains("Cache-Control: max-age=60"));
        assert!(head.contains(&format!("Content-Length: {}", font.len())));
        assert!(!head.to_ascii_lowercase().contains("transfer-encoding"));
        assert_eq!(&framed[end + 4..], font.as_slice());
    }

    #[test]
    fn a_redirect_is_handed_to_the_browser_not_followed() {
        let raw = b"HTTP/1.1 302 Found\r\nLocation: http://elsewhere.invalid/\r\nContent-Length: 0\r\n\r\n";
        let framed = String::from_utf8(reframe_response(raw).unwrap()).unwrap();
        assert!(framed.starts_with("HTTP/1.1 302 Found\r\n"));
        assert!(framed.contains("Location: http://elsewhere.invalid/\r\n"));
    }

    #[test]
    fn a_truncated_response_is_refused() {
        let raw =
            b"HTTP/1.1 200 OK\r\nContent-Type: image/svg+xml\r\nContent-Length: 10\r\n\r\n<svg/>";
        assert!(reframe_response(raw).is_none());
        let broken = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n";
        assert!(reframe_response(broken).is_none());
    }

    #[test]
    fn a_machine_managed_in_leanctx_code_shows_that_calmly_without_install_controls() {
        use lean_ctx::core::app_presence::AppPresence;
        let presence = AppPresence {
            open_url: Some("leanctx://cockpit".into()),
        };
        let page = render_unavailable_page(CockpitUnavailable::NotInstalled, Some(&presence));
        assert!(page.contains("Managed in LeanCTX Code"), "{page}");
        assert!(page.contains(r#"<a href="leanctx://cockpit">Open LeanCTX Code</a>"#));
        assert!(
            !page.contains(INSTALL_COMMAND),
            "no duplicate install control"
        );
        assert!(
            page.contains("data-reason="),
            "the reason stays machine-readable"
        );
        assert!(!page.contains("Cloud"));

        let without_link = render_unavailable_page(
            CockpitUnavailable::StartFailed,
            Some(&AppPresence { open_url: None }),
        );
        assert!(without_link.contains("Managed in LeanCTX Code"));
        assert!(!without_link.contains("href="));
        assert!(!without_link.contains(INSTALL_COMMAND));

        let unmanaged = render_unavailable_page(CockpitUnavailable::NotInstalled, None);
        assert!(unmanaged.contains(INSTALL_COMMAND));
        assert!(!unmanaged.contains("Managed in"));
        assert_eq!(
            unmanaged,
            render_unavailable_page(CockpitUnavailable::NotInstalled, None)
        );
    }
}
