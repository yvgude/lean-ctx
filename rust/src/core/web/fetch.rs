//! Bounded, SSRF-aware HTTP fetch built on `ureq`.
//!
//! Redirects are followed manually so every hop passes back through
//! [`url_guard`], closing the redirect-to-internal SSRF hole that automatic
//! redirect following would open. Response bodies are capped to a byte budget so
//! a hostile server cannot exhaust memory.

use std::io::Read;
use std::time::Duration;

use super::url_guard::{self, SafeUrl};

/// Default response body cap (4 MiB) — generous for articles, safe for memory.
pub const DEFAULT_MAX_BYTES: usize = 4 * 1024 * 1024;
/// Default total request timeout in seconds.
pub const DEFAULT_TIMEOUT_SECS: u64 = 20;

const MAX_REDIRECTS: u32 = 5;
const USER_AGENT: &str = "lean-ctx/3.7 (+https://leanctx.com; ctx_url_read)";
const ACCEPT: &str = "text/html,application/xhtml+xml,text/plain;q=0.9,*/*;q=0.5";

/// A fetched document with its raw body bytes and resolved metadata.
///
/// The body is kept as bytes so binary payloads (e.g. PDF) survive intact;
/// textual callers use [`FetchedDoc::body_text`] for a lossy UTF-8 view.
pub struct FetchedDoc {
    pub final_url: String,
    /// Lower-cased MIME type without parameters (e.g. `text/html`).
    pub content_type: String,
    pub bytes: Vec<u8>,
    pub status: u16,
    pub truncated: bool,
}

impl FetchedDoc {
    /// Lossy UTF-8 view of the body, for textual content (HTML, JSON, …).
    #[must_use]
    pub fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.bytes).into_owned()
    }
}

/// Fetch `url`, following up to `MAX_REDIRECTS` re-validated redirects.
pub fn fetch(url: &str, max_bytes: usize, timeout_secs: u64) -> Result<FetchedDoc, String> {
    let mut current = url_guard::validate(url).map_err(|e| e.to_string())?;
    current
        .ensure_resolves_safely()
        .map_err(|e| e.to_string())?;

    let agent = build_agent(timeout_secs);
    let mut hops = 0u32;

    loop {
        let resp = agent
            .get(&current.normalized)
            .header("user-agent", USER_AGENT)
            .header("accept", ACCEPT)
            .header("accept-language", "en,*;q=0.5")
            .call()
            .map_err(|e| format!("request failed: {e}"))?;

        let status = resp.status().as_u16();

        if (300..400).contains(&status)
            && hops < MAX_REDIRECTS
            && let Some(location) = header_value(&resp, "location")
        {
            let next = resolve_redirect(&current, &location);
            let next_url = url_guard::validate(&next).map_err(|e| e.to_string())?;
            next_url
                .ensure_resolves_safely()
                .map_err(|e| e.to_string())?;
            current = next_url;
            hops += 1;
            continue;
        }

        let content_type = header_value(&resp, "content-type")
            .and_then(|v| v.split(';').next().map(|m| m.trim().to_ascii_lowercase()))
            .unwrap_or_default();
        let (bytes, truncated) = read_bounded(resp, max_bytes)?;

        return Ok(FetchedDoc {
            final_url: current.normalized.clone(),
            content_type,
            bytes,
            status,
            truncated,
        });
    }
}

/// POST `body` to `url` (SSRF-guarded, bounded, redirects not followed).
///
/// Needed for JSON-RPC style endpoints — e.g. `YouTube`'s `InnerTube` `player`
/// API, whose caption URLs are server-fetchable (unlike the watch-page ones).
/// `user_agent` is explicit because some APIs validate it against the declared
/// client.
pub fn post(
    url: &str,
    content_type: &str,
    user_agent: &str,
    body: &str,
    max_bytes: usize,
    timeout_secs: u64,
) -> Result<FetchedDoc, String> {
    let target = url_guard::validate(url).map_err(|e| e.to_string())?;
    target.ensure_resolves_safely().map_err(|e| e.to_string())?;

    let agent = build_agent(timeout_secs);
    let resp = agent
        .post(&target.normalized)
        .header("user-agent", user_agent)
        .header("content-type", content_type)
        .header("accept", "application/json, text/xml;q=0.9, */*;q=0.5")
        .send(body.as_bytes())
        .map_err(|e| format!("request failed: {e}"))?;

    let status = resp.status().as_u16();
    let content_type = header_value(&resp, "content-type")
        .and_then(|v| v.split(';').next().map(|m| m.trim().to_ascii_lowercase()))
        .unwrap_or_default();
    let (bytes, truncated) = read_bounded(resp, max_bytes)?;

    Ok(FetchedDoc {
        final_url: target.normalized,
        content_type,
        bytes,
        status,
        truncated,
    })
}

fn build_agent(timeout_secs: u64) -> ureq::Agent {
    ureq::Agent::new_with_config(
        ureq::config::Config::builder()
            .timeout_global(Some(Duration::from_secs(timeout_secs)))
            .max_redirects(0)
            .http_status_as_error(false)
            .build(),
    )
}

fn header_value<B>(resp: &ureq::http::Response<B>, name: &str) -> Option<String> {
    resp.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

fn read_bounded(
    resp: ureq::http::Response<ureq::Body>,
    max_bytes: usize,
) -> Result<(Vec<u8>, bool), String> {
    let mut reader = resp.into_body().into_reader();
    let mut buf: Vec<u8> = Vec::with_capacity(8192.min(max_bytes.max(1)));
    let mut chunk = [0u8; 8192];
    let mut truncated = false;

    loop {
        let n = reader
            .read(&mut chunk)
            .map_err(|e| format!("failed to read body: {e}"))?;
        if n == 0 {
            break;
        }
        let remaining = max_bytes.saturating_sub(buf.len());
        if remaining == 0 {
            truncated = true;
            break;
        }
        let take = n.min(remaining);
        buf.extend_from_slice(&chunk[..take]);
        if take < n {
            truncated = true;
            break;
        }
    }

    Ok((buf, truncated))
}

/// Resolve a (possibly relative) `location` (redirect target or link href)
/// against a base URL.
pub(crate) fn resolve_redirect(base: &SafeUrl, location: &str) -> String {
    let loc = location.trim();

    if loc.starts_with("http://") || loc.starts_with("https://") {
        return loc.to_string();
    }
    if let Some(rest) = loc.strip_prefix("//") {
        return format!("{}://{rest}", base.scheme);
    }
    if loc.starts_with('/') {
        return format!("{}://{}{loc}", base.scheme, base.authority);
    }

    // Path-relative: join against the directory of the current path.
    let base_path = base_path(base);
    let dir = match base_path.rfind('/') {
        Some(i) => &base_path[..=i],
        None => "/",
    };
    format!("{}://{}{dir}{loc}", base.scheme, base.authority)
}

fn base_path(base: &SafeUrl) -> &str {
    let prefix_len = base.scheme.len() + 3 + base.authority.len();
    let path = base.normalized.get(prefix_len..).unwrap_or("");
    let path = path.split(['?', '#']).next().unwrap_or("");
    if path.is_empty() { "/" } else { path }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn safe(url: &str) -> SafeUrl {
        url_guard::validate(url).unwrap()
    }

    #[test]
    fn redirect_absolute_is_passthrough() {
        let base = safe("https://a.com/x");
        assert_eq!(
            resolve_redirect(&base, "https://b.com/y"),
            "https://b.com/y"
        );
    }

    #[test]
    fn redirect_scheme_relative() {
        let base = safe("https://a.com/x");
        assert_eq!(resolve_redirect(&base, "//c.com/z"), "https://c.com/z");
    }

    #[test]
    fn redirect_root_relative() {
        let base = safe("https://a.com/deep/path?q=1");
        assert_eq!(resolve_redirect(&base, "/new"), "https://a.com/new");
    }

    #[test]
    fn redirect_path_relative_joins_dir() {
        let base = safe("https://a.com/dir/page.html");
        assert_eq!(
            resolve_redirect(&base, "other.html"),
            "https://a.com/dir/other.html"
        );
    }

    #[test]
    fn redirect_path_relative_from_root() {
        let base = safe("https://a.com");
        assert_eq!(resolve_redirect(&base, "page"), "https://a.com/page");
    }
}
