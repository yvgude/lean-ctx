//! Reverse-proxy subpath support for `lean-ctx dashboard --base-path` (#355).
//!
//! When the dashboard is mounted under a subpath (e.g. `/dashboard/`) by an
//! nginx-style reverse proxy, two things must happen:
//!
//! 1. Every root-absolute URL in the served HTML/CSS/JS (`/static/…`, `/api/…`,
//!    `/favicon…`) must be prefixed with the base path, otherwise the browser
//!    resolves them against the origin root and bypasses the subpath.
//! 2. The server must accept incoming requests **both with and without** the
//!    prefix, so it works whether or not the reverse proxy strips it.
//!
//! All functions are pure and `base`-gated (empty base → exact no-op), so the
//! default behaviour is byte-for-byte identical to a dashboard without a subpath.

/// Normalizes a user-supplied base path into a canonical form: an empty string
/// for "no prefix", otherwise a single leading slash and no trailing slash.
///
/// `""`/`"/"` → `""`, `"dashboard"`/`"/dashboard/"`/`"//dashboard//"` →
/// `"/dashboard"`.
#[must_use]
pub fn normalize(input: &str) -> String {
    let trimmed = input.trim().trim_matches('/');
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("/{trimmed}")
    }
}

/// Strips the base-path prefix from an incoming request path so downstream
/// routing always sees a root-relative path. Requests that already arrive
/// root-relative (proxy stripped the prefix, or direct local access) pass
/// through unchanged.
#[must_use]
pub fn strip<'a>(path: &'a str, base: &str) -> &'a str {
    if base.is_empty() {
        return path;
    }
    if path == base {
        return "/";
    }
    match path.strip_prefix(base) {
        Some(rest) if rest.starts_with('/') => rest,
        _ => path,
    }
}

/// Prefixes every root-absolute asset/API/favicon URL in a served text body with
/// the base path. Only the quote/paren-delimited forms that actually occur in the
/// dashboard assets (`"/static/`, `'/api/`, `` `/api/ ``, `url(/static/`, …) are
/// rewritten, so ordinary text is never touched. No-op when `base` is empty.
#[must_use]
pub fn rewrite_asset_urls(body: &str, base: &str) -> String {
    if base.is_empty() {
        return body.to_string();
    }
    // Root-absolute prefixes used by the dashboard. `/favicon` has no trailing
    // slash on purpose (covers both `/favicon.svg` and `/favicon.ico`).
    const PREFIXES: &[&str] = &["/static/", "/api/", "/favicon"];
    // Delimiters that introduce a URL literal in HTML attributes, JS string and
    // template literals, and CSS `url(...)`.
    const DELIMS: &[char] = &['"', '\'', '`', '('];

    let mut out = body.to_string();
    for prefix in PREFIXES {
        for &d in DELIMS {
            let from = format!("{d}{prefix}");
            if out.contains(&from) {
                let to = format!("{d}{base}{prefix}");
                out = out.replace(&from, &to);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_canonicalizes() {
        assert_eq!(normalize(""), "");
        assert_eq!(normalize("/"), "");
        assert_eq!(normalize("   "), "");
        assert_eq!(normalize("dashboard"), "/dashboard");
        assert_eq!(normalize("/dashboard"), "/dashboard");
        assert_eq!(normalize("/dashboard/"), "/dashboard");
        assert_eq!(normalize("//dashboard//"), "/dashboard");
        assert_eq!(normalize("/lean/ctx"), "/lean/ctx");
    }

    #[test]
    fn strip_empty_base_is_noop() {
        assert_eq!(strip("/api/stats", ""), "/api/stats");
        assert_eq!(strip("/", ""), "/");
    }

    #[test]
    fn strip_removes_prefix() {
        assert_eq!(strip("/dashboard", "/dashboard"), "/");
        assert_eq!(strip("/dashboard/", "/dashboard"), "/");
        assert_eq!(strip("/dashboard/api/stats", "/dashboard"), "/api/stats");
        assert_eq!(
            strip("/dashboard/static/style.css", "/dashboard"),
            "/static/style.css"
        );
    }

    #[test]
    fn strip_accepts_already_rootrelative() {
        // Reverse proxy already stripped the prefix (or direct local access).
        assert_eq!(strip("/api/stats", "/dashboard"), "/api/stats");
        assert_eq!(strip("/", "/dashboard"), "/");
    }

    #[test]
    fn strip_does_not_match_partial_segment() {
        // `/dashboardx` must NOT be treated as `/dashboard` + `x`.
        assert_eq!(strip("/dashboardx/api", "/dashboard"), "/dashboardx/api");
    }

    #[test]
    fn rewrite_empty_base_is_noop() {
        let html = r#"<script src="/static/lib/api.js"></script>"#;
        assert_eq!(rewrite_asset_urls(html, ""), html);
    }

    #[test]
    fn rewrite_html_attributes() {
        let html = r#"<link href="/static/style.css"><script src="/static/lib/api.js"></script><link rel="icon" href="/favicon.svg">"#;
        let out = rewrite_asset_urls(html, "/dashboard");
        assert!(out.contains(r#"href="/dashboard/static/style.css""#));
        assert!(out.contains(r#"src="/dashboard/static/lib/api.js""#));
        assert!(out.contains(r#"href="/dashboard/favicon.svg""#));
        assert!(!out.contains(r#"href="/static/"#));
    }

    #[test]
    fn rewrite_js_string_and_template_literals() {
        let js = "fetch('/api/stats'); const u = `/api/search?q=${q}`; api('/api/pulse');";
        let out = rewrite_asset_urls(js, "/dashboard");
        assert!(out.contains("fetch('/dashboard/api/stats')"));
        assert!(out.contains("`/dashboard/api/search?q=${q}`"));
        assert!(out.contains("api('/dashboard/api/pulse')"));
    }

    #[test]
    fn rewrite_css_url() {
        let css = "src: url('/static/fonts/inter-variable.woff2');";
        let out = rewrite_asset_urls(css, "/dashboard");
        assert!(out.contains("url('/dashboard/static/fonts/inter-variable.woff2')"));
    }

    #[test]
    fn rewrite_fetch_interceptor_guard() {
        // The HTML interceptor checks `url.startsWith('/api/')`; after rewrite it
        // must check the prefixed form so Bearer auth still attaches.
        let js = "if (url.startsWith('/api/')) attachToken();";
        let out = rewrite_asset_urls(js, "/dashboard");
        assert!(out.contains("url.startsWith('/dashboard/api/')"));
    }

    #[test]
    fn rewrite_does_not_double_prefix() {
        let html = r#"<script src="/static/x.js"></script>"#;
        let once = rewrite_asset_urls(html, "/dashboard");
        // Re-running would double-prefix only if source already had the base; the
        // canonical source never does, so a single pass is correct and stable.
        assert_eq!(once.matches("/dashboard/static/x.js").count(), 1);
        assert!(!once.contains("/dashboard/dashboard/"));
    }
}
