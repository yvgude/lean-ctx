// SPDX-License-Identifier: Apache-2.0
use super::ResolvedProvider;

/// The resolved provider upstreams a running proxy forwards to. Published
/// to request handlers via a `tokio::sync::watch` channel so a config change is
/// picked up live, without a proxy restart (#449).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upstreams {
    pub anthropic: String,
    pub openai: String,
    pub chatgpt: String,
    pub gemini: String,
    /// Registry providers from `[[proxy.providers]]` (universal framework),
    /// validated and live-reloadable exactly like the built-ins.
    pub providers: Vec<ResolvedProvider>,
}

impl Upstreams {
    /// Look up a registry provider by id (`/providers/{id}/...` route, router
    /// upstream overrides). Built-ins are not addressed here.
    #[must_use]
    pub fn provider_by_id(&self, id: &str) -> Option<&ResolvedProvider> {
        self.providers.iter().find(|p| p.id == id)
    }
}

#[derive(Debug, Clone, Copy)]
pub enum ProxyProvider {
    Anthropic,
    OpenAi,
    ChatGpt,
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
        ProxyProvider::ChatGpt => "LEAN_CTX_CHATGPT_UPSTREAM",
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
    "chatgpt.com",
    "generativelanguage.googleapis.com",
    // xAI Grok (OpenAI-compatible). Used by auto-seeded registry providers
    // when `lean-ctx proxy enable` wires Grok (API-key rail + subscription rail).
    "api.x.ai",
    "cli-chat-proxy.grok.com",
    // Command Code gateway. Used by the auto-seeded `commandcode` registry
    // provider when `lean-ctx proxy enable` wires Command Code.
    "api.commandcode.ai",
];

pub(in crate::core::config) fn validate_upstream_url(
    url: &str,
    allow_insecure_http: bool,
    allow_custom_host: bool,
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
    if ALLOWED_UPSTREAM_HOSTS.contains(&host_no_port) || allow_custom_host {
        Ok(normalized)
    } else {
        Err(format!(
            "upstream host '{host_no_port}' not in allowlist {ALLOWED_UPSTREAM_HOSTS:?} (for a \
             custom upstream host opt in with LEAN_CTX_ALLOW_CUSTOM_UPSTREAM=1 or \
             `[proxy] allow_custom_upstream = true`)"
        ))
    }
}

/// True when `url` is an HTTPS upstream whose host is not in the built-in
/// allowlist (and not loopback) — the case the `allow_custom_upstream` opt-in
/// governs. Plaintext-HTTP custom hosts are governed by
/// `allow_insecure_http_upstream` instead, so they are excluded here.
pub(super) fn is_custom_upstream_host(url: &str) -> bool {
    let n = normalize_url(url);
    if is_local_proxy_url(&n) {
        return false;
    }
    let Some(host_segment) = n.strip_prefix("https://") else {
        return false;
    };
    let host = host_segment.split('/').next().unwrap_or("");
    let host_no_port = host.split(':').next().unwrap_or(host);
    !host_no_port.is_empty() && !ALLOWED_UPSTREAM_HOSTS.contains(&host_no_port)
}

pub fn is_local_proxy_url(value: &str) -> bool {
    let n = normalize_url(value);
    n.starts_with("http://127.0.0.1:")
        || n.starts_with("http://localhost:")
        || n.starts_with("http://[::1]:")
}
