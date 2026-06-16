//! API proxy upstream overrides (`config.toml`).

use serde::{Deserialize, Serialize};

/// API proxy upstream overrides. `None` = use provider default.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProxyConfig {
    pub anthropic_upstream: Option<String>,
    pub openai_upstream: Option<String>,
    pub gemini_upstream: Option<String>,
    /// History-pruning strategy for proxied chat requests.
    /// "cache-aware" (default) | "rolling" | "off". See [`HistoryMode`].
    pub history_mode: Option<String>,
    /// Allow a non-loopback plaintext `http://` upstream (trusted local network
    /// only). Opt-in; see [`ProxyConfig::allows_insecure_http_upstream`]. (#440)
    pub allow_insecure_http_upstream: Option<bool>,
}

/// How the proxy prunes old tool results from conversation history.
///
/// Provider prompt caches (Anthropic `cache_control`, OpenAI automatic prompt
/// caching) bill cached prefix tokens at a fraction of the base rate but only
/// match *exact* prefixes. Any mutation whose position depends on the current
/// conversation length (a rolling window) rewrites a previously-stable message
/// every turn, invalidating the cache from that point — turning cheap cache
/// reads into full-price writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryMode {
    /// Prune only at frozen generation boundaries that advance in large,
    /// deterministic steps. Between jumps the request prefix is byte-stable,
    /// so provider prompt caches keep hitting. Default.
    CacheAware,
    /// Legacy behaviour: summarize everything older than the last N messages.
    /// Maximum raw-token reduction, but defeats provider prompt caching.
    Rolling,
    /// Never prune history (tool-result compression still applies — it is
    /// content-deterministic and therefore prefix-stable).
    Off,
}

impl ProxyConfig {
    /// Resolved history mode: `LEAN_CTX_PROXY_HISTORY_MODE` env var wins,
    /// then `[proxy].history_mode` in config.toml, then cache-aware.
    /// Unknown values fall back to the default so a typo can never silently
    /// re-enable the cache-hostile rolling mode.
    pub fn resolved_history_mode(&self) -> HistoryMode {
        let raw = std::env::var("LEAN_CTX_PROXY_HISTORY_MODE")
            .ok()
            .or_else(|| self.history_mode.clone());
        match raw.as_deref().map(str::trim) {
            Some(s) if s.eq_ignore_ascii_case("rolling") => HistoryMode::Rolling,
            Some(s) if s.eq_ignore_ascii_case("off") => HistoryMode::Off,
            _ => HistoryMode::CacheAware,
        }
    }

    /// Whether a non-loopback plaintext `http://` upstream is allowed. Opt-in
    /// only — a deliberate downgrade for a trusted local-network service such as
    /// `http://host.docker.internal:2455` in front of codex-lb (#440).
    /// `LEAN_CTX_ALLOW_INSECURE_HTTP_UPSTREAM` (any value) wins, then
    /// `[proxy] allow_insecure_http_upstream` in config.toml, default `false`.
    pub fn allows_insecure_http_upstream(&self) -> bool {
        std::env::var("LEAN_CTX_ALLOW_INSECURE_HTTP_UPSTREAM").is_ok()
            || self.allow_insecure_http_upstream.unwrap_or(false)
    }

    pub fn resolve_upstream(&self, provider: ProxyProvider) -> String {
        let (env_var, config_val, default) = match provider {
            ProxyProvider::Anthropic => (
                "LEAN_CTX_ANTHROPIC_UPSTREAM",
                self.anthropic_upstream.as_deref(),
                "https://api.anthropic.com",
            ),
            ProxyProvider::OpenAi => (
                "LEAN_CTX_OPENAI_UPSTREAM",
                self.openai_upstream.as_deref(),
                "https://api.openai.com",
            ),
            ProxyProvider::Gemini => (
                "LEAN_CTX_GEMINI_UPSTREAM",
                self.gemini_upstream.as_deref(),
                "https://generativelanguage.googleapis.com",
            ),
        };
        let resolved = std::env::var(env_var)
            .ok()
            .and_then(|v| normalize_url_opt(&v))
            .or_else(|| config_val.and_then(normalize_url_opt))
            .unwrap_or_else(|| normalize_url(default));
        match validate_upstream_url(&resolved, self.allows_insecure_http_upstream()) {
            Ok(url) => url,
            Err(e) => {
                tracing::warn!("upstream validation failed, using default: {e}");
                normalize_url(default)
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum ProxyProvider {
    Anthropic,
    OpenAi,
    Gemini,
}

pub fn normalize_url(value: &str) -> String {
    value.trim().trim_end_matches('/').to_string()
}

pub fn normalize_url_opt(value: &str) -> Option<String> {
    let trimmed = normalize_url(value);
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

const ALLOWED_UPSTREAM_HOSTS: &[&str] = &[
    "api.anthropic.com",
    "api.openai.com",
    "generativelanguage.googleapis.com",
];

pub(super) fn validate_upstream_url(
    url: &str,
    allow_insecure_http: bool,
) -> Result<String, String> {
    let normalized = normalize_url(url);
    // Loopback HTTP never leaves the machine — always allowed.
    if is_local_proxy_url(&normalized) {
        return Ok(normalized);
    }

    // A non-loopback plaintext `http://` upstream is reachable only through the
    // explicit opt-in (#440). The old code rejected it on the HTTPS check *before*
    // any override could apply, and pointed at `LEAN_CTX_ALLOW_CUSTOM_UPSTREAM`,
    // which never lifted the scheme restriction. Handle it up front: the opt-in
    // implies a deliberate custom host on a trusted local network, so it needs no
    // separate allowlist check; otherwise give a hint that actually works.
    if normalized.starts_with("http://") {
        if allow_insecure_http {
            return Ok(normalized);
        }
        return Err(format!(
            "upstream URL must use HTTPS: {normalized} (for a trusted local-network HTTP \
             upstream opt in with LEAN_CTX_ALLOW_INSECURE_HTTP_UPSTREAM=1 or \
             `[proxy] allow_insecure_http_upstream = true`)"
        ));
    }
    let Some(host_segment) = normalized.strip_prefix("https://") else {
        return Err(format!(
            "upstream URL must start with http:// or https://: {normalized}"
        ));
    };

    let host = host_segment.split('/').next().unwrap_or("");
    let host_no_port = host.split(':').next().unwrap_or(host);
    if ALLOWED_UPSTREAM_HOSTS.contains(&host_no_port)
        || std::env::var("LEAN_CTX_ALLOW_CUSTOM_UPSTREAM").is_ok()
    {
        Ok(normalized)
    } else {
        Err(format!(
            "upstream host '{host_no_port}' not in allowlist {ALLOWED_UPSTREAM_HOSTS:?} (set LEAN_CTX_ALLOW_CUSTOM_UPSTREAM=1 to override)"
        ))
    }
}

pub fn is_local_proxy_url(value: &str) -> bool {
    let n = normalize_url(value);
    n.starts_with("http://127.0.0.1:")
        || n.starts_with("http://localhost:")
        || n.starts_with("http://[::1]:")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_http_is_always_allowed() {
        assert_eq!(
            validate_upstream_url("http://127.0.0.1:4444", false).unwrap(),
            "http://127.0.0.1:4444"
        );
        assert_eq!(
            validate_upstream_url("http://localhost:2455/", false).unwrap(),
            "http://localhost:2455"
        );
    }

    #[test]
    fn https_allowlisted_host_is_allowed() {
        assert_eq!(
            validate_upstream_url("https://api.openai.com", false).unwrap(),
            "https://api.openai.com"
        );
    }

    #[test]
    fn non_loopback_http_is_rejected_without_optin() {
        let err = validate_upstream_url("http://host.docker.internal:2455", false).unwrap_err();
        // The hint must point at the flag that actually lifts the scheme check
        // (#440). The old message pointed at LEAN_CTX_ALLOW_CUSTOM_UPSTREAM,
        // which never bypassed the HTTPS requirement.
        assert!(
            err.contains("LEAN_CTX_ALLOW_INSECURE_HTTP_UPSTREAM"),
            "hint must name the working opt-in, got: {err}"
        );
    }

    #[test]
    fn non_loopback_http_is_allowed_with_optin() {
        assert_eq!(
            validate_upstream_url("http://host.docker.internal:2455", true).unwrap(),
            "http://host.docker.internal:2455"
        );
    }

    #[test]
    fn unknown_scheme_is_rejected() {
        assert!(validate_upstream_url("ftp://example.com", true).is_err());
    }

    #[test]
    fn config_flag_enables_insecure_http_optin() {
        // `Some(true)` resolves to `true` regardless of the environment, so this
        // assertion is robust without mutating process-global env vars.
        let cfg = ProxyConfig {
            allow_insecure_http_upstream: Some(true),
            ..Default::default()
        };
        assert!(cfg.allows_insecure_http_upstream());
    }
}
