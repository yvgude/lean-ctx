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
    /// Inject `stream_options.include_usage = true` into streamed OpenAI Chat
    /// Completions so the final chunk reports real token usage for the measured
    /// spend meter. Default on; set `false` for a client that mishandles the
    /// trailing usage chunk. Anthropic/Gemini/OpenAI-Responses report usage
    /// without any request change, so this only affects Chat Completions.
    pub meter_openai_usage: Option<bool>,
    /// Opt-in "big-gap cold-prefix repack" (#480). When the proxy can confidently
    /// predict (from idle time vs the provider cache TTL) that the client-cached
    /// prefix has already expired, it overrides the normal "never rewrite the
    /// cached prefix" rule for that one resume request and prunes the now-cold
    /// prefix too, re-seeding a leaner cache. `None`/`false` (the default) keeps
    /// the prefix always protected. See [`ProxyConfig::repacks_cold_prefix`].
    pub cold_prefix_repack: Option<bool>,
    /// Opt-in per-role prose compression for the proxy's frozen request region
    /// (#710). `None` for a role (the default) leaves that role untouched —
    /// today's behaviour. See [`RoleAggressiveness`].
    pub role_aggressiveness: RoleAggressiveness,
}

/// Per-role prose-compression intensity for the proxy's frozen request region.
///
/// Each value is a `0.0–1.0` aggressiveness level reusing the same mapping as
/// the `ctx_read` knob (#708): `0.0` keeps everything, `1.0` is most aggressive.
/// `None` (the default) means "do not compress this role's prose" so the proxy
/// stays byte-for-byte unchanged until an operator opts in. The `assistant`
/// role is never represented here — model turns are always passed through
/// verbatim (the #710 passthrough guarantee).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct RoleAggressiveness {
    /// Aggressiveness for system prompts (Anthropic `system` / OpenAI `system`
    /// messages / Gemini `systemInstruction`). `None` = leave untouched.
    pub system: Option<f64>,
    /// Aggressiveness for user prose (free-text user turns, never tool results).
    /// `None` = leave untouched.
    pub user: Option<f64>,
}

/// The conversation roles whose prose the proxy may compress in the frozen
/// region. Deliberately excludes `assistant` — model turns are never rewritten.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProseRole {
    System,
    User,
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
    /// so provider prompt caches keep hitting. Content the client has marked
    /// with a `cache_control` breakpoint is never rewritten, so an advancing
    /// boundary can no longer invalidate the already-cached prefix (#448).
    /// Default.
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

    /// Whether the proxy injects `stream_options.include_usage` into streamed
    /// OpenAI Chat Completions to meter real spend. `[proxy] meter_openai_usage`
    /// in config.toml, default `true`.
    pub fn meters_openai_usage(&self) -> bool {
        self.meter_openai_usage.unwrap_or(true)
    }

    /// Whether the opt-in cold-prefix repack (#480) is enabled. A wrong "cold"
    /// guess re-bills cache reads as writes (~12x), so this is off by default and
    /// must be explicitly enabled. `LEAN_CTX_PROXY_COLD_PREFIX_REPACK` (any
    /// value) wins, then `[proxy] cold_prefix_repack` in config.toml, else
    /// `false`.
    pub fn repacks_cold_prefix(&self) -> bool {
        std::env::var("LEAN_CTX_PROXY_COLD_PREFIX_REPACK").is_ok()
            || self.cold_prefix_repack.unwrap_or(false)
    }

    /// Resolved prose-compression aggressiveness for `role`, clamped to `[0,1]`,
    /// or `None` when prose compression is off for that role (the default).
    ///
    /// Precedence: the role's env override (`LEAN_CTX_PROXY_SYSTEM_AGGR` /
    /// `LEAN_CTX_PROXY_USER_AGGR`) wins, then `[proxy.role_aggressiveness]` in
    /// config.toml. An unparseable or blank env value is ignored so a typo can
    /// never silently disable the configured behaviour.
    #[must_use]
    pub fn resolved_role_aggressiveness(&self, role: ProseRole) -> Option<f64> {
        let (env_var, configured) = match role {
            ProseRole::System => (
                "LEAN_CTX_PROXY_SYSTEM_AGGR",
                self.role_aggressiveness.system,
            ),
            ProseRole::User => ("LEAN_CTX_PROXY_USER_AGGR", self.role_aggressiveness.user),
        };
        let from_env = std::env::var(env_var)
            .ok()
            .and_then(|v| v.trim().parse::<f64>().ok());
        from_env.or(configured).map(|a| a.clamp(0.0, 1.0))
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

    /// `(env var, configured value, provider default)` for one provider.
    fn provider_spec(&self, provider: ProxyProvider) -> (&'static str, Option<&str>, &'static str) {
        match provider {
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
        }
    }

    /// Resolve one upstream with precedence `LEAN_CTX_*_UPSTREAM` env var >
    /// `[proxy].*_upstream` (config.toml) > provider default.
    ///
    /// Returns `Err` when a value is *present but invalid* so a live reload can
    /// keep the last good value instead of silently rerouting to the default; an
    /// *absent* value resolves to the provider default (`Ok`).
    fn resolve_upstream_checked(&self, provider: ProxyProvider) -> Result<String, String> {
        self.resolve_upstream_inner(provider, true)
    }

    /// Shared resolver for [`resolve_upstream_checked`] and the disk-only view.
    /// `use_env = false` ignores the `LEAN_CTX_*_UPSTREAM` override and yields
    /// the config.toml truth a freshly (re)started managed proxy would serve.
    fn resolve_upstream_inner(
        &self,
        provider: ProxyProvider,
        use_env: bool,
    ) -> Result<String, String> {
        let (env_var, config_val, default) = self.provider_spec(provider);
        let env_val = if use_env {
            std::env::var(env_var)
                .ok()
                .and_then(|v| normalize_url_opt(&v))
        } else {
            None
        };
        let candidate = env_val.or_else(|| config_val.and_then(normalize_url_opt));
        match candidate {
            None => Ok(normalize_url(default)),
            Some(url) => validate_upstream_url(&url, self.allows_insecure_http_upstream()),
        }
    }

    /// Effective upstream for a provider (env > config > default). An invalid
    /// configured/env value falls back to the provider default (logged) — the
    /// safe choice at startup.
    pub fn resolve_upstream(&self, provider: ProxyProvider) -> String {
        match self.resolve_upstream_checked(provider) {
            Ok(url) => url,
            Err(e) => {
                tracing::warn!("upstream validation failed, using default: {e}");
                normalize_url(self.provider_spec(provider).2)
            }
        }
    }

    /// Resolve all three upstreams at once (startup snapshot, env-aware).
    pub fn resolve_all(&self) -> Upstreams {
        Upstreams {
            anthropic: self.resolve_upstream(ProxyProvider::Anthropic),
            openai: self.resolve_upstream(ProxyProvider::OpenAi),
            gemini: self.resolve_upstream(ProxyProvider::Gemini),
        }
    }

    /// Resolve all upstreams from config.toml only (ignoring `LEAN_CTX_*` env) —
    /// the values a freshly (re)started managed proxy would serve. Used by
    /// status/doctor to detect drift from a running proxy's live upstream (#449).
    pub fn resolve_all_disk(&self) -> Upstreams {
        let pick = |provider: ProxyProvider| {
            self.resolve_upstream_inner(provider, false)
                .unwrap_or_else(|_| normalize_url(self.provider_spec(provider).2))
        };
        Upstreams {
            anthropic: pick(ProxyProvider::Anthropic),
            openai: pick(ProxyProvider::OpenAi),
            gemini: pick(ProxyProvider::Gemini),
        }
    }

    /// Re-resolve upstreams for a *running* proxy (#449). For any provider whose
    /// currently configured/env value fails validation, the last good value is
    /// kept instead of rerouting live traffic to the provider default — so a typo
    /// in config.toml can never silently redirect in-flight requests.
    pub fn refresh_upstreams(&self, last: &Upstreams) -> Upstreams {
        let keep = |provider: ProxyProvider, prev: &str| {
            self.resolve_upstream_checked(provider).unwrap_or_else(|e| {
                tracing::warn!("upstream invalid, keeping {prev}: {e}");
                prev.to_string()
            })
        };
        Upstreams {
            anthropic: keep(ProxyProvider::Anthropic, &last.anthropic),
            openai: keep(ProxyProvider::OpenAi, &last.openai),
            gemini: keep(ProxyProvider::Gemini, &last.gemini),
        }
    }
}

/// The three resolved provider upstreams a running proxy forwards to. Published
/// to request handlers via a `tokio::sync::watch` channel so a config change is
/// picked up live, without a proxy restart (#449).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upstreams {
    pub anthropic: String,
    pub openai: String,
    pub gemini: String,
}

#[derive(Debug, Clone, Copy)]
pub enum ProxyProvider {
    Anthropic,
    OpenAi,
    Gemini,
}

/// Why a running proxy's live upstream differs from what the operator expects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpstreamDrift {
    /// A `LEAN_CTX_*_UPSTREAM` env var is set in *this* process but the proxy
    /// serves a different value — the env never reached the MCP/service-spawned
    /// proxy. This is the #449 trap: Codex (and other MCP hosts) launch the
    /// server with a stripped, allowlisted env that omits `LEAN_CTX_*_UPSTREAM`,
    /// so the proxy it spawns never sees it. Fix: persist it to config.toml,
    /// which the proxy reads live.
    EnvNotApplied,
    /// The proxy serves a value other than config.toml resolves to: it was
    /// started with an env override that now masks a later config edit. Fix:
    /// `lean-ctx proxy restart`.
    ConfigNotApplied,
}

/// The `LEAN_CTX_*_UPSTREAM` override visible to *this* process for a provider,
/// normalized (`None` if unset/blank). Lets status/doctor explain why an env var
/// a user exported in their shell never reaches an MCP/service-spawned proxy.
pub fn env_upstream_override(provider: ProxyProvider) -> Option<String> {
    let var = match provider {
        ProxyProvider::Anthropic => "LEAN_CTX_ANTHROPIC_UPSTREAM",
        ProxyProvider::OpenAi => "LEAN_CTX_OPENAI_UPSTREAM",
        ProxyProvider::Gemini => "LEAN_CTX_GEMINI_UPSTREAM",
    };
    std::env::var(var).ok().and_then(|v| normalize_url_opt(&v))
}

/// Diagnose upstream drift for one provider from the CLI-visible env override
/// (`env`), the config.toml value (`disk`) and the proxy's live value (`live`).
/// `None` means in sync.
pub fn diagnose_drift(env: Option<&str>, disk: &str, live: &str) -> Option<UpstreamDrift> {
    if let Some(env) = env {
        // An env override is present in this process: the proxy honours it only
        // if it was started with it. If the proxy serves something else, the env
        // never reached it (#449). If it matches, that is consistent (no drift).
        return (env != live).then_some(UpstreamDrift::EnvNotApplied);
    }
    // No env override here: the proxy should mirror config.toml.
    (disk != live).then_some(UpstreamDrift::ConfigNotApplied)
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
    fn cold_prefix_repack_is_opt_in_and_config_enables() {
        // #480: off by default (a wrong cold guess re-bills reads as writes ~12x),
        // enabled via config. Isolate from a developer shell that may export the
        // env override.
        let _lock = crate::core::data_dir::test_env_lock();
        crate::test_env::remove_var("LEAN_CTX_PROXY_COLD_PREFIX_REPACK");
        assert!(
            !ProxyConfig::default().repacks_cold_prefix(),
            "cold-prefix repack must be opt-in (off by default)"
        );
        let cfg = ProxyConfig {
            cold_prefix_repack: Some(true),
            ..Default::default()
        };
        assert!(cfg.repacks_cold_prefix());
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

    /// `resolve_all_disk` ignores `LEAN_CTX_*_UPSTREAM` env by construction, so
    /// these assertions are env-independent (no lock needed). Loopback HTTP is an
    /// always-valid custom upstream (no allowlist / opt-in required).
    #[test]
    fn resolve_all_disk_uses_config_then_default() {
        let cfg = ProxyConfig {
            openai_upstream: Some("http://127.0.0.1:19101".into()),
            ..Default::default()
        };
        let up = cfg.resolve_all_disk();
        assert_eq!(up.openai, "http://127.0.0.1:19101");
        assert_eq!(up.anthropic, "https://api.anthropic.com");
        assert_eq!(up.gemini, "https://generativelanguage.googleapis.com");
    }

    #[test]
    fn resolve_all_disk_normalizes_trailing_slash() {
        let cfg = ProxyConfig {
            openai_upstream: Some("http://127.0.0.1:19101/".into()),
            ..Default::default()
        };
        assert_eq!(cfg.resolve_all_disk().openai, "http://127.0.0.1:19101");
    }

    #[test]
    fn refresh_keeps_last_good_on_invalid_config() {
        // `refresh_upstreams` is env-aware; isolate from a developer's shell that
        // may export LEAN_CTX_OPENAI_UPSTREAM (e.g. while reproducing #449).
        let _lock = crate::core::data_dir::test_env_lock();
        crate::test_env::remove_var("LEAN_CTX_OPENAI_UPSTREAM");

        // A typo in config.toml must never reroute a live proxy to the default.
        let last = Upstreams {
            anthropic: "https://api.anthropic.com".into(),
            openai: "http://127.0.0.1:19101".into(),
            gemini: "https://generativelanguage.googleapis.com".into(),
        };
        let cfg = ProxyConfig {
            openai_upstream: Some("not-a-valid-url".into()),
            ..Default::default()
        };
        assert_eq!(
            cfg.refresh_upstreams(&last).openai,
            "http://127.0.0.1:19101",
            "invalid upstream → keep last good, never silently fall to default"
        );
    }

    #[test]
    fn refresh_adopts_valid_config_change() {
        let _lock = crate::core::data_dir::test_env_lock();
        crate::test_env::remove_var("LEAN_CTX_OPENAI_UPSTREAM");

        let last = Upstreams {
            anthropic: "https://api.anthropic.com".into(),
            openai: "http://127.0.0.1:19101".into(),
            gemini: "https://generativelanguage.googleapis.com".into(),
        };
        let cfg = ProxyConfig {
            openai_upstream: Some("http://127.0.0.1:19102".into()),
            ..Default::default()
        };
        assert_eq!(
            cfg.refresh_upstreams(&last).openai,
            "http://127.0.0.1:19102"
        );
    }

    #[test]
    fn diagnose_drift_env_set_but_proxy_serves_other() {
        // The exact #449 / Codex case: env exported in the shell, but the
        // MCP-spawned proxy serves config.toml → the env never reached it.
        assert_eq!(
            diagnose_drift(
                Some("http://127.0.0.1:2455"),
                "https://api.openai.com",
                "https://api.openai.com"
            ),
            Some(UpstreamDrift::EnvNotApplied)
        );
    }

    #[test]
    fn diagnose_drift_env_consistent_is_in_sync() {
        // Proxy was started with the env value and serves it → not drift.
        assert_eq!(
            diagnose_drift(
                Some("http://127.0.0.1:2455"),
                "https://api.openai.com",
                "http://127.0.0.1:2455"
            ),
            None
        );
    }

    #[test]
    fn diagnose_drift_config_changed_needs_restart() {
        assert_eq!(
            diagnose_drift(None, "http://127.0.0.1:2455", "https://api.openai.com"),
            Some(UpstreamDrift::ConfigNotApplied)
        );
    }

    #[test]
    fn diagnose_drift_in_sync() {
        assert_eq!(
            diagnose_drift(None, "https://api.openai.com", "https://api.openai.com"),
            None
        );
    }

    #[test]
    fn role_aggressiveness_defaults_to_off() {
        // Opt-in: a fresh config compresses no prose, so the proxy stays
        // byte-for-byte unchanged until an operator sets a value (#710).
        let cfg = ProxyConfig::default();
        // Isolate from a developer shell that may export the override.
        let _lock = crate::core::data_dir::test_env_lock();
        crate::test_env::remove_var("LEAN_CTX_PROXY_SYSTEM_AGGR");
        crate::test_env::remove_var("LEAN_CTX_PROXY_USER_AGGR");
        assert_eq!(cfg.resolved_role_aggressiveness(ProseRole::System), None);
        assert_eq!(cfg.resolved_role_aggressiveness(ProseRole::User), None);
    }

    #[test]
    fn role_aggressiveness_reads_config_and_clamps() {
        let _lock = crate::core::data_dir::test_env_lock();
        crate::test_env::remove_var("LEAN_CTX_PROXY_SYSTEM_AGGR");
        crate::test_env::remove_var("LEAN_CTX_PROXY_USER_AGGR");
        let cfg = ProxyConfig {
            role_aggressiveness: RoleAggressiveness {
                system: Some(0.7),
                user: Some(1.5),
            },
            ..Default::default()
        };
        assert_eq!(
            cfg.resolved_role_aggressiveness(ProseRole::System),
            Some(0.7)
        );
        // Out-of-range config values are clamped into [0,1].
        assert_eq!(cfg.resolved_role_aggressiveness(ProseRole::User), Some(1.0));
    }

    #[test]
    fn role_aggressiveness_env_overrides_config() {
        let _lock = crate::core::data_dir::test_env_lock();
        crate::test_env::set_var("LEAN_CTX_PROXY_SYSTEM_AGGR", "0.25");
        let cfg = ProxyConfig {
            role_aggressiveness: RoleAggressiveness {
                system: Some(0.9),
                user: None,
            },
            ..Default::default()
        };
        assert_eq!(
            cfg.resolved_role_aggressiveness(ProseRole::System),
            Some(0.25),
            "env override must win over the configured value"
        );
        crate::test_env::remove_var("LEAN_CTX_PROXY_SYSTEM_AGGR");
    }

    #[test]
    fn role_aggressiveness_ignores_blank_env() {
        let _lock = crate::core::data_dir::test_env_lock();
        crate::test_env::set_var("LEAN_CTX_PROXY_USER_AGGR", "  ");
        let cfg = ProxyConfig {
            role_aggressiveness: RoleAggressiveness {
                system: None,
                user: Some(0.4),
            },
            ..Default::default()
        };
        assert_eq!(
            cfg.resolved_role_aggressiveness(ProseRole::User),
            Some(0.4),
            "a blank/garbage env value must fall back to config, not disable it"
        );
        crate::test_env::remove_var("LEAN_CTX_PROXY_USER_AGGR");
    }
}
