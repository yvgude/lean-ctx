//! LLM reverse proxy — the core of the **Gateway pillar**.
//!
//! Intercepts Anthropic, OpenAI, Gemini and ChatGPT traffic, compresses
//! prompts, meters usage, and optionally translates request shapes.
//!
//! # Cross-pillar coupling
//!
//! When the `gateway-server` feature is active, `start_proxy` mounts
//! `gateway_server::user_api` and `gateway_server::mcp::proxy` routes into
//! the Axum router. This is intentional: the self-hosted org gateway is a
//! single process that combines the proxy with the admin/usage store.
//! The dependency is feature-gated and uni-directional at the route level
//! (proxy owns the router, gateway_server provides route handlers).

pub mod anthropic;
#[cfg(test)]
mod auth_tests;
pub mod cache_aligner;
pub mod cache_attribution;
pub mod cache_breakpoint;
pub mod cache_policy;
pub mod cache_safety;
pub mod ccr;
#[cfg(test)]
mod ccr_robustness_tests;
pub mod chatgpt;
pub mod chatgpt_cookies;
pub mod chatgpt_ws;
pub mod cold_prefix;
pub mod compress;
pub mod compress_api;
pub mod cost;
pub mod counterfactual;
pub mod effort;
pub mod effort_routing;
pub mod forward;
pub mod gateway_identity;
pub mod google;
pub mod history_prune;
pub mod holdout;
pub mod image_compression;
pub mod introspect;
pub mod metrics;
pub mod models_api;
pub mod openai;
pub mod openai_responses;
pub mod openai_responses_ws;
pub mod output_savings;
pub mod pii;
pub mod policy_gate;
pub mod prefix_cache_stats;
pub mod prefix_replay;
pub mod prose;
pub mod prose_ranker;
pub mod providers;
pub mod routing;
#[cfg(feature = "shape-xlat")]
pub mod shape_xlat;
pub mod sse_keepalive;
#[cfg(test)]
mod stats_tests;
pub mod sticky_tools;
pub mod tool_kind;
pub mod tool_output;
#[cfg(test)]
mod upstream_tests;
pub mod usage;
pub mod usage_accounting;
pub mod usage_meter;
pub mod usage_sink;
pub mod verbosity;

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::core::config::Upstreams;

use axum::{
    Router,
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
    routing::{any, get, post},
};

#[derive(Clone)]
pub struct ProxyState {
    pub client: reqwest::Client,
    pub port: u16,
    pub stats: Arc<ProxyStats>,
    pub introspect: Arc<introspect::IntrospectState>,
    /// Live provider upstreams, refreshed from config.toml without a proxy
    /// restart (#449). Read per request via [`ProxyState::openai_upstream`] etc.
    pub upstreams: tokio::sync::watch::Receiver<Arc<Upstreams>>,
    /// Shared Cloudflare cookie jar (also wired into `client`), so the Codex
    /// ChatGPT WebSocket passthrough replays the same clearance to chatgpt.com
    /// that the reqwest rail accumulated (#597).
    pub(crate) chatgpt_cookies: Arc<chatgpt_cookies::ChatGptCloudflareCookieStore>,
    /// Resolved `[[gateway_server.mcp_servers]]` registry snapshot (GL#100).
    /// Empty when none are registered; the `/mcp/{server}` routes are only
    /// mounted with the `gateway-server` feature. Startup snapshot, restart
    /// to reload — the same lifecycle as `gateway-keys.toml`.
    pub mcp_servers: Arc<Vec<crate::core::config::ResolvedMcpServer>>,
}

impl ProxyState {
    /// Test-only construction for modules outside `proxy` (the cookie store
    /// field is deliberately `pub(crate)`-narrow): default upstreams, fresh
    /// stats, the given MCP registry. Gated with the sole consumer
    /// (`gateway_server::mcp::e2e_tests`) so feature-reduced builds don't
    /// carry dead test scaffolding.
    #[cfg(all(test, feature = "gateway-server"))]
    pub(crate) fn for_tests(mcp_servers: Vec<crate::core::config::ResolvedMcpServer>) -> Self {
        // Dropping the sender is fine: handlers only `borrow()` the last value.
        let (_tx, rx) = tokio::sync::watch::channel(Arc::new(Upstreams {
            anthropic: "https://api.anthropic.com".into(),
            openai: "https://api.openai.com".into(),
            chatgpt: "https://chatgpt.com".into(),
            gemini: "https://generativelanguage.googleapis.com".into(),
            providers: Vec::new(),
        }));
        Self {
            client: reqwest::Client::new(),
            port: 0,
            stats: Arc::new(ProxyStats::default()),
            introspect: Arc::new(introspect::IntrospectState::default()),
            upstreams: rx,
            chatgpt_cookies: chatgpt_cookies::shared_chatgpt_cloudflare_cookie_store(),
            mcp_servers: Arc::new(mcp_servers),
        }
    }

    /// Consistent snapshot of all upstreams for the current request/response.
    pub fn upstream_snapshot(&self) -> Arc<Upstreams> {
        self.upstreams.borrow().clone()
    }

    /// Current Anthropic upstream (live).
    pub fn anthropic_upstream(&self) -> String {
        self.upstreams.borrow().anthropic.clone()
    }

    /// Current OpenAI upstream (live).
    pub fn openai_upstream(&self) -> String {
        self.upstreams.borrow().openai.clone()
    }

    /// Current ChatGPT upstream (live).
    pub fn chatgpt_upstream(&self) -> String {
        self.upstreams.borrow().chatgpt.clone()
    }

    /// Current Gemini upstream (live).
    pub fn gemini_upstream(&self) -> String {
        self.upstreams.borrow().gemini.clone()
    }

    /// Cloudflare `Cookie` header for the current ChatGPT upstream, used by the
    /// WebSocket passthrough handshake (#597). `None` until a request on the
    /// reqwest rail has seen Cloudflare clearance.
    pub fn chatgpt_cookie_header(&self) -> Option<String> {
        let url = reqwest::Url::parse(&self.chatgpt_upstream()).ok()?;
        self.chatgpt_cookies
            .cookie_header(&url)
            .and_then(|v| v.to_str().ok().map(str::to_owned))
    }
}

pub struct ProxyStats {
    pub requests_total: AtomicU64,
    pub requests_compressed: AtomicU64,
    pub tokens_saved: AtomicU64,
    pub bytes_original: AtomicU64,
    pub bytes_compressed: AtomicU64,
    pub anthropic: ProviderStats,
    pub openai: ProviderStats,
    pub chatgpt: ProviderStats,
    pub gemini: ProviderStats,
    /// Grok dual-rail (`/providers/grok-chat`, `/providers/xai`) — OpenAI wire
    /// shape, separate identity so `proxy status` does not fold xAI traffic
    /// into the OpenAI bucket.
    pub grok: ProviderStats,
}

#[derive(Default)]
pub struct ProviderStats {
    pub requests_total: AtomicU64,
    pub requests_compressed: AtomicU64,
    pub tokens_saved: AtomicU64,
    pub bytes_original: AtomicU64,
    pub bytes_compressed: AtomicU64,
}

impl Default for ProxyStats {
    fn default() -> Self {
        Self {
            requests_total: AtomicU64::new(0),
            requests_compressed: AtomicU64::new(0),
            tokens_saved: AtomicU64::new(0),
            bytes_original: AtomicU64::new(0),
            bytes_compressed: AtomicU64::new(0),
            anthropic: ProviderStats::default(),
            openai: ProviderStats::default(),
            chatgpt: ProviderStats::default(),
            gemini: ProviderStats::default(),
            grok: ProviderStats::default(),
        }
    }
}

impl ProxyStats {
    pub fn record_request(&self, original: usize, compressed: usize) {
        self.record_totals(original, compressed);
    }

    pub fn record_provider_request(
        &self,
        provider_label: &str,
        original: usize,
        compressed: usize,
    ) {
        let (effective_compressed, saved_tokens, compressed_request) =
            self.record_totals(original, compressed);

        if let Some(provider) = self.provider(provider_label) {
            provider.record(
                original,
                effective_compressed,
                compressed_request,
                saved_tokens,
            );
        }
    }

    fn record_totals(&self, original: usize, compressed: usize) -> (usize, u64, bool) {
        self.requests_total.fetch_add(1, Ordering::Relaxed);
        self.bytes_original
            .fetch_add(original as u64, Ordering::Relaxed);
        let effective_compressed = compressed.min(original);
        self.bytes_compressed
            .fetch_add(effective_compressed as u64, Ordering::Relaxed);
        if compressed < original {
            self.requests_compressed.fetch_add(1, Ordering::Relaxed);
        }
        let saved_tokens = (original.saturating_sub(effective_compressed) / 4) as u64;
        self.tokens_saved.fetch_add(saved_tokens, Ordering::Relaxed);
        (effective_compressed, saved_tokens, compressed < original)
    }

    pub fn compression_ratio(&self) -> f64 {
        let original = self.bytes_original.load(Ordering::Relaxed);
        if original == 0 {
            return 0.0;
        }
        let compressed = self.bytes_compressed.load(Ordering::Relaxed);
        (1.0 - compressed as f64 / original as f64) * 100.0
    }

    /// Maps a proxy `provider_label` to its per-upstream bucket. Unknown labels
    /// return `None` (still counted in the totals, never misattributed to a bucket);
    /// every real upstream — Gemini included — passes an explicit label.
    /// `"Grok"` is the identity label for registry routes `grok-chat` / `xai`
    /// (OpenAI wire shape; see `providers::stats_label`).
    fn provider(&self, provider_label: &str) -> Option<&ProviderStats> {
        match provider_label {
            "Anthropic" => Some(&self.anthropic),
            "OpenAI" => Some(&self.openai),
            "ChatGPT" => Some(&self.chatgpt),
            "Gemini" => Some(&self.gemini),
            "Grok" => Some(&self.grok),
            _ => None,
        }
    }

    pub fn provider_summary(&self) -> serde_json::Value {
        serde_json::json!({
            "anthropic": self.anthropic.summary(),
            "openai": self.openai.summary(),
            "chatgpt": self.chatgpt.summary(),
            "gemini": self.gemini.summary(),
            "grok": self.grok.summary(),
        })
    }
}

impl ProviderStats {
    fn record(
        &self,
        original: usize,
        effective_compressed: usize,
        compressed_request: bool,
        saved_tokens: u64,
    ) {
        self.requests_total.fetch_add(1, Ordering::Relaxed);
        if compressed_request {
            self.requests_compressed.fetch_add(1, Ordering::Relaxed);
        }
        self.tokens_saved.fetch_add(saved_tokens, Ordering::Relaxed);
        self.bytes_original
            .fetch_add(original as u64, Ordering::Relaxed);
        self.bytes_compressed
            .fetch_add(effective_compressed as u64, Ordering::Relaxed);
    }

    fn compression_ratio(&self) -> f64 {
        let original = self.bytes_original.load(Ordering::Relaxed);
        if original == 0 {
            return 0.0;
        }
        let compressed = self.bytes_compressed.load(Ordering::Relaxed);
        (1.0 - compressed as f64 / original as f64) * 100.0
    }

    fn summary(&self) -> serde_json::Value {
        serde_json::json!({
            "requests_total": self.requests_total.load(Ordering::Relaxed),
            "requests_compressed": self.requests_compressed.load(Ordering::Relaxed),
            "tokens_saved": self.tokens_saved.load(Ordering::Relaxed),
            "bytes_original": self.bytes_original.load(Ordering::Relaxed),
            "bytes_compressed": self.bytes_compressed.load(Ordering::Relaxed),
            "compression_ratio_pct": format!("{:.1}", self.compression_ratio()),
        })
    }
}

/// TCP connect timeout (seconds). Configurable via `LEAN_CTX_PROXY_CONNECT_TIMEOUT_SECS`.
fn connect_timeout_secs() -> u64 {
    std::env::var("LEAN_CTX_PROXY_CONNECT_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|s| *s > 0)
        .unwrap_or(15)
}

/// Idle read timeout (seconds) between bytes from upstream. Generous by default
/// so long extended-thinking phases (which still emit SSE keepalives) are never
/// cut, while a truly dead connection eventually fails. Configurable via
/// `LEAN_CTX_PROXY_READ_TIMEOUT_SECS`.
fn read_idle_timeout_secs() -> u64 {
    std::env::var("LEAN_CTX_PROXY_READ_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|s| *s > 0)
        .unwrap_or(300)
}

/// How often (seconds) a running proxy re-reads config.toml for upstream
/// changes. `LEAN_CTX_PROXY_RELOAD_SECS` overrides; default 5s.
fn upstream_reload_secs() -> u64 {
    std::env::var("LEAN_CTX_PROXY_RELOAD_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|s| *s > 0)
        .unwrap_or(5)
}

/// Background task: re-resolves the provider upstreams from config.toml on an
/// interval and publishes any change to the live request handlers (#449). Ends
/// once every receiver (the proxy itself) has been dropped.
///
/// `Config::load()` already keeps an internal content-hash cache, so re-reading
/// an unchanged `config.toml` skips the TOML parse + merge and costs only a small
/// file read; combined with the relaxed default interval (#453) the idle steady
/// state is negligible without needing a separate stat pre-check.
fn spawn_upstream_refresh(tx: tokio::sync::watch::Sender<Arc<Upstreams>>, initial: Upstreams) {
    let interval = std::time::Duration::from_secs(upstream_reload_secs());
    tokio::spawn(async move {
        let mut last = initial;
        loop {
            tokio::time::sleep(interval).await;
            let next = crate::core::config::Config::load()
                .proxy
                .refresh_upstreams(&last);
            if next != last {
                log_upstream_change(&last, &next);
                last = next.clone();
                if tx.send(Arc::new(next)).is_err() {
                    break;
                }
            }
        }
    });
}

/// One stdout line per changed provider, matching the startup banner style so a
/// running proxy's log shows when (and to what) an upstream switched.
fn log_upstream_change(old: &Upstreams, new: &Upstreams) {
    if old.anthropic != new.anthropic {
        println!("  ↻ Anthropic upstream → {}", new.anthropic);
    }
    if old.openai != new.openai {
        println!("  ↻ OpenAI upstream → {}", new.openai);
    }
    if old.chatgpt != new.chatgpt {
        println!("  ↻ ChatGPT upstream → {}", new.chatgpt);
    }
    if old.gemini != new.gemini {
        println!("  ↻ Gemini upstream → {}", new.gemini);
    }
    if old.providers != new.providers {
        let ids: Vec<&str> = new.providers.iter().map(|p| p.id.as_str()).collect();
        println!("  ↻ provider registry → [{}]", ids.join(", "));
    }
}

pub async fn start_proxy(port: u16) -> anyhow::Result<()> {
    let token = crate::core::session_token::resolve_proxy_token("LEAN_CTX_PROXY_TOKEN");
    start_proxy_with_token(port, Some(token)).await
}

/// Security invariant: the proxy NEVER runs unauthenticated. `None` does not
/// mean "no auth" — it means "resolve the session token for me". Provider
/// routes additionally accept provider API keys (see `proxy_auth_guard`), so
/// IDE clients keep working without any setup.
fn effective_auth_token(auth_token: Option<String>) -> String {
    auth_token
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| crate::core::session_token::resolve_proxy_token("LEAN_CTX_PROXY_TOKEN"))
}

/// Install the process-default rustls `CryptoProvider` for the Codex ChatGPT
/// WebSocket passthrough (#597).
///
/// `tokio-tungstenite`'s rustls connector builds its `ClientConfig` from the
/// process-default provider. Our tree pulls *both* aws-lc-rs (reqwest) and ring
/// (lettre/ureq), so rustls cannot auto-pick one and the `wss://chatgpt.com`
/// handshake aborts with *"Could not automatically determine the process-level
/// CryptoProvider"*. reqwest is unaffected (it configures aws-lc-rs explicitly),
/// so we match it here. Idempotent: a prior install just returns the provider
/// back as `Err`, which we ignore.
fn install_default_crypto_provider() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}

pub async fn start_proxy_with_token(port: u16, auth_token: Option<String>) -> anyhow::Result<()> {
    use crate::core::config::{Config, is_local_proxy_url};

    // Must run before any WebSocket passthrough opens a wss:// upstream (#597).
    install_default_crypto_provider();

    let auth_token = effective_auth_token(auth_token);

    // A single total timeout aborts long streaming generations (e.g. Opus doing
    // a big refactor) mid-response. Use a connect timeout plus a read (idle)
    // timeout instead: a genuinely hung upstream still fails, but a slow-but-
    // alive stream is never cut off. Both are configurable for edge networks.
    let chatgpt_cookies = chatgpt_cookies::shared_chatgpt_cloudflare_cookie_store();
    let client = chatgpt_cookies::with_chatgpt_cloudflare_cookie_store(
        reqwest::Client::builder(),
        chatgpt_cookies.clone(),
    )
    .connect_timeout(std::time::Duration::from_secs(connect_timeout_secs()))
    .read_timeout(std::time::Duration::from_secs(read_idle_timeout_secs()))
    .build()?;

    // Seed the measured-spend meter from disk so a proxy restart never zeroes
    // the user's cumulative real provider bill.
    usage_meter::resume_from_disk();
    // Live model prices (#1179): load the cached provider price list and keep
    // it fresh in the background, so new models are billed at their real
    // market rates instead of family heuristics. Fail-open, kill switch:
    // LEAN_CTX_LIVE_PRICING=off.
    crate::core::gain::live_pricing::spawn_background_refresh();
    // Seed the cold-prefix baselines too so a long idle gap that straddles a
    // proxy restart is still detected and the repack can fire (#499).
    cold_prefix::resume_from_disk();

    let cfg = Config::load();
    // Read once at startup — avoids a Config::load() on every proxied request.
    let bind_host = cfg.resolved_proxy_bind_host();
    let loopback_bind = bind_host.is_loopback();
    // Gateway mode (non-loopback bind, enterprise#8) hard-requires the Bearer
    // token: the provider-key fallback's whole justification is "loopback only",
    // so it is disabled by construction once the listener is reachable from the
    // network — regardless of the config flag.
    let require_token = cfg.proxy_require_token || !loopback_bind;
    let loopback_open = cfg.proxy_loopback_open && loopback_bind;
    let allowed_hosts: Arc<Vec<String>> = Arc::new(
        cfg.proxy_allowed_hosts
            .iter()
            .map(|h| h.trim().trim_end_matches('.').to_ascii_lowercase())
            .filter(|h| !h.is_empty())
            .collect(),
    );
    // Rate limit (enterprise#37): explicit config wins; gateway mode ships a
    // sane default floor; loopback stays unlimited unless configured. `0`
    // disables the limiter explicitly.
    let rate_limiter = match (cfg.proxy_max_rps, loopback_bind) {
        (Some(rps), _) if rps > 0 => Some(Arc::new(RateLimiter::new(rps, rps.saturating_mul(2)))),
        (None, false) => Some(Arc::new(RateLimiter::new(50, 100))),
        _ => None,
    };
    let initial = cfg.proxy.resolve_all();

    // The proxy reads its upstreams live from a watch channel: a background task
    // re-resolves them from config.toml on an interval and publishes any change,
    // so `lean-ctx config set proxy.*_upstream` (or any config.toml edit) takes
    // effect on the running proxy within seconds, without a restart (#449).
    let (upstream_tx, upstream_rx) = tokio::sync::watch::channel(Arc::new(initial.clone()));
    // Clone before moving into ProxyState — auth guard needs live registry
    // snapshots to decide client-auth provider fallback under require_token.
    let auth_upstreams = upstream_rx.clone();
    spawn_upstream_refresh(upstream_tx, initial.clone());

    let Upstreams {
        anthropic: anthropic_upstream,
        openai: openai_upstream,
        chatgpt: chatgpt_upstream,
        gemini: gemini_upstream,
        providers: initial_providers,
    } = initial;

    // Governed MCP reverse proxy registry (GL#91/#100): resolved once at
    // startup (restart to reload — the gateway-keys.toml lifecycle). The
    // `/mcp/{server}` routes are mounted below with the gateway-server
    // feature; the snapshot lives in ProxyState either way.
    let mcp_servers = Arc::new(
        cfg.gateway_server
            .resolve_mcp_servers(cfg.proxy.allows_insecure_http_upstream()),
    );

    let state = ProxyState {
        client,
        port,
        stats: Arc::new(ProxyStats::default()),
        introspect: Arc::new(introspect::IntrospectState::default()),
        upstreams: upstream_rx,
        chatgpt_cookies,
        mcp_servers: mcp_servers.clone(),
    };

    // `mut` is only exercised by the gateway-server merge below.
    #[cfg_attr(not(feature = "gateway-server"), allow(unused_mut))]
    let mut app = Router::new()
        .route("/health", get(health))
        .route("/status", get(status_handler))
        .route("/v1/messages", any(anthropic::handler))
        .route("/v1/messages/{*rest}", any(anthropic::handler))
        .route("/v1/chat/completions", any(openai::handler))
        // POST → HTTP/SSE forwarder; GET → Codex/OpenAI WebSocket bridge (#440).
        .route(
            "/v1/responses",
            post(openai_responses::handler).get(openai_responses::ws_handler),
        )
        .route("/v1/responses/{*rest}", any(openai_responses::handler))
        // Bare provider endpoints (no `/v1` prefix). Clients whose base URL points
        // at the proxy root — notably OpenCode via `@ai-sdk/openai`, whose
        // Responses-API requests hit `/responses` — dispatch here. The
        // `normalize_provider_path` layer rewrites the URI to its canonical
        // `/v1/...` form before the handler forwards upstream (#353).
        .route("/messages", any(anthropic::handler))
        .route("/messages/{*rest}", any(anthropic::handler))
        .route("/chat/completions", any(openai::handler))
        .route(
            "/responses",
            post(openai_responses::handler).get(openai_responses::ws_handler),
        )
        .route("/responses/{*rest}", any(openai_responses::handler))
        .route(
            "/backend-api/codex/responses",
            post(chatgpt::codex_responses_handler).get(chatgpt::codex_responses_ws_handler),
        )
        .route(
            "/backend-api/codex/responses/{*rest}",
            any(chatgpt::codex_responses_handler),
        )
        // Non-model ChatGPT backend calls (including codex_apps MCP) are not
        // prompt JSON. Keep them as credential-preserving passthrough traffic.
        .route("/backend-api", any(chatgpt::backend_api_handler))
        .route("/backend-api/{*rest}", any(chatgpt::backend_api_handler))
        .route("/v1/references/{id}", get(v1_resolve_reference))
        // LiteLLM headroom-guardrail CCR retrieval (#702): resolves the 24-hex
        // `hash=` marker `/v1/compress` emits back to the verbatim original.
        .route("/v1/retrieve/{hash}", get(v1_retrieve_ccr))
        // Org model catalog (enterprise#63): IDE clients discover the curated
        // alias namespace (`zuehlke/fast` → provider:model) and verify their
        // key. Exact-match only — `/v1/models/{...}` subpaths stay Gemini
        // passthrough in the fallback router.
        .route("/v1/models", get(models_api::handler))
        .route("/models", get(models_api::handler))
        // Drop-in `compress(messages, model)` contract (#739): deterministic
        // messages-in / messages-out compression for SDK clients.
        .route("/v1/compress", post(compress_api::handler))
        // Universal provider registry (`[[proxy.providers]]`, enterprise#7):
        // `/providers/{id}/...` forwards to the registry entry with that id,
        // speaking its declared wire shape. New provider = config, not code.
        .route("/providers/{id}/{*rest}", any(providers::handler))
        .fallback(fallback_router);

    // Personal usage view (enterprise#64): `/me` shell + guarded `/api/me/*`.
    // Merged before the guard layers so host_guard and auth wrap it too; the
    // shell paths themselves are exempted inside `proxy_auth_guard`.
    #[cfg(feature = "gateway-server")]
    {
        app = app.merge(crate::gateway_server::user_api::router());
    }

    // Governed MCP reverse proxy (GL#91/#100): `/mcp/{server}` fronts the
    // `[[gateway_server.mcp_servers]]` registry. Registered before the guard
    // layers, so the same Bearer auth + host allowlist + rate limit wrap the
    // tool channel — and `/mcp/*` is not a provider route, so the loopback
    // provider-key fallback never authenticates it.
    #[cfg(feature = "gateway-server")]
    {
        use crate::gateway_server::mcp::proxy::handler as mcp_handler;
        app = app
            .route("/mcp/{server}", any(mcp_handler))
            .route("/mcp/{server}/", any(mcp_handler));
    }

    let mut app = app
        .layer(axum::middleware::from_fn(move |req, next| {
            let allowed = allowed_hosts.clone();
            host_guard(req, next, allowed)
        }))
        .with_state(state);

    // Per-person gateway keys (enterprise#11): sha256(bearer) → person/team/
    // default_project. Loaded once at startup; rotation = restart (the standard
    // secret-mount flow). A malformed file fails the start loudly.
    let gateway_keys = match gateway_identity::GatewayKeys::load_default() {
        Ok(keys) => {
            if !keys.is_empty() {
                println!(
                    "  Identity:  {} gateway key(s) loaded ({})",
                    keys.len(),
                    gateway_identity::GatewayKeys::default_path().display()
                );
            }
            Arc::new(keys)
        }
        Err(e) => anyhow::bail!("gateway-keys.toml: {e}"),
    };

    {
        let expected = auth_token.clone();
        let keys = gateway_keys.clone();
        let upstreams = auth_upstreams;
        app = app.layer(axum::middleware::from_fn(move |req, next| {
            let expected = expected.clone();
            let keys = keys.clone();
            let upstreams = upstreams.clone();
            proxy_auth_guard(
                req,
                next,
                expected,
                require_token,
                loopback_open,
                keys,
                upstreams,
            )
        }));
    }

    if let Some(limiter) = rate_limiter {
        app = app.layer(axum::middleware::from_fn(move |req, next| {
            let limiter = limiter.clone();
            rate_limit_guard(req, next, limiter)
        }));
    }

    // Outermost layer (runs first): normalize bare provider endpoints to their
    // canonical `/v1/...` form so auth, routing and upstream forwarding all agree,
    // regardless of whether the client's base URL includes `/v1` (#353).
    app = app.layer(axum::middleware::from_fn(normalize_provider_path));

    let addr = SocketAddr::from((bind_host, port));
    if loopback_open {
        println!("lean-ctx proxy listening on http://{addr} (loopback-open: auth disabled)");
    } else {
        println!("lean-ctx proxy listening on http://{addr} (token auth enabled)");
    }
    if !loopback_bind {
        println!(
            "  ⚠ gateway mode: non-loopback bind — Bearer token REQUIRED for gateway-held \
             keys and non-registry routes; client-auth /providers/* (api_key_env unset) \
             still accept provider credentials. Host allowlist + rate limit active"
        );
    }
    println!("  Anthropic: POST /v1/messages → {anthropic_upstream}");
    println!("  OpenAI:    POST /v1/chat/completions → {openai_upstream}");
    println!(
        "  OpenAI:    POST /v1/responses → {openai_upstream}  (bare /responses also accepted)"
    );
    println!("  ChatGPT:   POST /backend-api/codex/responses → {chatgpt_upstream}");
    println!("  ChatGPT:   any  /backend-api/* → {chatgpt_upstream}");
    println!("  Gemini:    POST /v1beta/models/... → {gemini_upstream}");
    println!("  Compress:  POST /v1/compress (deterministic messages-in/out, local)");
    // Codex defaults to a WebSocket Responses transport (ws://…/responses). The
    // proxy now bridges it to the HTTP/SSE upstream (#440), so Codex works as a
    // drop-in without a `supports_websockets = false` workaround.
    println!(
        "  Codex:     WS  ws://{addr}/responses → bridged to {openai_upstream} (HTTP/SSE, #440)"
    );
    for p in &initial_providers {
        println!(
            "  Provider:  any  /providers/{}/... → {} ({} shape{})",
            p.id,
            p.base_url,
            p.shape.as_str(),
            if p.api_key_env.is_some() {
                ", gateway-held key"
            } else {
                ""
            }
        );
    }
    for s in mcp_servers.iter() {
        println!(
            "  MCP:       any  /mcp/{} → {} (observed{})",
            s.id,
            s.url,
            if s.auth_env.is_some() {
                ", gateway-held credential"
            } else {
                ""
            }
        );
    }
    if openai_upstream.starts_with("http://") && !is_local_proxy_url(&openai_upstream) {
        println!(
            "  ⚠ OpenAI upstream is plaintext HTTP to a non-loopback host \
             (allow_insecure_http_upstream) — use only on a trusted local network"
        );
    }

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    println!("lean-ctx proxy shut down cleanly.");
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();

    #[cfg(unix)]
    {
        // Fall back to Ctrl-C only if the SIGTERM handler cannot be installed,
        // rather than panicking the proxy on startup.
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sigterm) => {
                tokio::select! {
                    _ = ctrl_c => {},
                    _ = sigterm.recv() => {},
                }
            }
            Err(e) => {
                tracing::warn!("lean-ctx proxy: SIGTERM handler unavailable ({e}); Ctrl-C only");
                ctrl_c.await.ok();
            }
        }
    }

    #[cfg(not(unix))]
    {
        ctrl_c.await.ok();
    }

    println!("lean-ctx proxy: received shutdown signal, draining…");
}

async fn health() -> impl IntoResponse {
    let body = serde_json::json!({
        "status": "ok",
        "service": "lean-ctx-proxy",
        "pid": std::process::id(),
    });
    (StatusCode::OK, axum::Json(body))
}

async fn v1_resolve_reference(
    axum::extract::Path(id): axum::extract::Path<String>,
) -> impl IntoResponse {
    match crate::server::reference_store::resolve(&id) {
        Some(content) => (StatusCode::OK, content),
        None => (
            StatusCode::NOT_FOUND,
            "Reference expired or not found".to_string(),
        ),
    }
}

/// `GET /v1/retrieve/{hash}` (#702) — LiteLLM headroom-guardrail CCR contract
/// (BerriAI/litellm#31681): resolve a 24-hex `hash=` marker emitted by
/// `/v1/compress` back to the verbatim original from the tee store. The reply
/// carries `original_content` (the field LiteLLM's `_call_retrieve` reads
/// first). The optional `?query=` the guardrail forwards is accepted but the
/// full original is always returned — a superset of any ranked slice, and the
/// stored blobs are single tool outputs, not corpora worth ranking. Auth: the
/// standard proxy bearer guard wraps this route (LiteLLM sends the configured
/// `api_key` as a Bearer token); the tee store is loopback-scoped local state.
async fn v1_retrieve_ccr(
    axum::extract::Path(hash): axum::extract::Path<String>,
) -> impl IntoResponse {
    match ccr::retrieve_litellm(&hash) {
        Some(content) => (
            StatusCode::OK,
            axum::Json(serde_json::json!({ "original_content": content })),
        ),
        None => (
            StatusCode::NOT_FOUND,
            axum::Json(serde_json::json!({
                "error": "hash not found or expired",
                "hash": hash,
            })),
        ),
    }
}

async fn status_handler(State(state): State<ProxyState>) -> impl IntoResponse {
    use std::sync::atomic::Ordering::Relaxed;
    let s = &state.stats;
    let i = &state.introspect;

    let last_breakdown = i
        .last_breakdown
        .lock()
        .ok()
        .and_then(|guard| guard.as_ref().map(|b| serde_json::to_value(b).ok()))
        .flatten();

    let spend = usage_meter::snapshot();
    let spend_total: f64 = spend.iter().map(|m| m.cost_usd).sum();

    // Live upstreams the proxy is forwarding to right now (#449). This is the
    // single source of truth for "where is my traffic actually going" — it
    // reflects config.toml hot-reloads and any start-time env override.
    let up = state.upstream_snapshot();

    // Resolve the effort level fresh so /status reflects config.toml hot-reloads
    // and env overrides, matching the upstream snapshot above (#834).
    let active_effort = crate::core::config::Config::load().proxy.resolved_effort();

    let introspect_total_input = i.total_input_tokens.load(Relaxed);
    let introspect_bulk_candidates = i.total_bulk_candidate_tokens.load(Relaxed);
    let introspect_bulk_share =
        introspect::token_share_basis_points(introspect_bulk_candidates, introspect_total_input);

    let body = serde_json::json!({
        "status": "running",
        "proxy_mode": format!("{:?}", crate::core::config::Config::load().proxy.resolved_proxy_mode()),
        "port": state.port,
        "upstreams": {
            "anthropic": up.anthropic.clone(),
            "openai": up.openai.clone(),
            "chatgpt": up.chatgpt.clone(),
            "gemini": up.gemini.clone(),
        },
        // Universal registry (`[[proxy.providers]]`, enterprise#7). Key names
        // only — never the key material.
        "providers": up.providers.iter().map(|p| serde_json::json!({
            "id": p.id,
            "shape": p.shape.as_str(),
            "base_url": p.base_url,
            "gateway_key": p.api_key_env.is_some(),
        })).collect::<Vec<_>>(),
        "requests_total": s.requests_total.load(Relaxed),
        "requests_compressed": s.requests_compressed.load(Relaxed),
        "tokens_saved": s.tokens_saved.load(Relaxed),
        "tokens_saved_estimated": true,
        // Provider-verified savings (#701, opt-in counterfactual metering):
        // both sides counted by Anthropic on the same request. `null` until
        // the first probe-covered request lands.
        "verified_savings": usage_meter::verified_savings(),
        "bytes_original": s.bytes_original.load(Relaxed),
        "bytes_compressed": s.bytes_compressed.load(Relaxed),
        "compression_ratio_pct": format!("{:.1}", s.compression_ratio()),
        "per_upstream": s.provider_summary(),
        "prefix_cache": prefix_cache_stats::snapshot(),
        "cache_safety": cache_safety::snapshot(),
        "cache_attribution": cache_attribution::snapshot(),
        "effort": effort::snapshot(active_effort),
        "per_model": cost::snapshot(),
        "spend": {
            "source": "measured",
            "total_usd": spend_total,
            "per_model": spend,
            "note": "Actual provider bill: real model + billed tokens (incl. cache reads/writes & reasoning) read from upstream responses for proxy-routed clients."
        },
        "note": "Savings are request-side (tokens removed before forwarding); they do not subtract any re-reads the agent performs. Token figures are estimates; USD uses the shared model price table.",
        "introspect": {
            "total_requests_analyzed": i.total_requests.load(Relaxed),
            "total_input_tokens": introspect_total_input,
            "total_system_prompt_tokens": i.total_system_prompt_tokens.load(Relaxed),
            "total_bulk_candidate_tokens": introspect_bulk_candidates,
            "bulk_candidate_share_basis_points": introspect_bulk_share,
            "vision_encoding_decision_gate_met": introspect_bulk_share.is_some_and(|share| share >= 2_000),
            "last_breakdown": last_breakdown,
        }
    });
    (StatusCode::OK, axum::Json(body))
}

#[allow(clippy::result_large_err)]
async fn proxy_auth_guard(
    mut req: axum::extract::Request,
    next: axum::middleware::Next,
    expected_token: String,
    require_token: bool,
    loopback_open: bool,
    gateway_keys: Arc<gateway_identity::GatewayKeys>,
    upstreams: tokio::sync::watch::Receiver<Arc<Upstreams>>,
) -> Result<Response, Response> {
    let path = req.uri().path();
    if path == "/health" || me_shell_path(path) {
        return Ok(next.run(req).await);
    }

    // #755: loopback-open mode skips all auth — every local process is trusted.
    if loopback_open {
        attach_gateway_tags(&mut req, gateway_identity::GatewayTags::default());
        return Ok(next.run(req).await);
    }

    let bearer = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|auth| auth.strip_prefix("Bearer "))
        .map(str::to_string);

    if let Some(token) = bearer.as_deref()
        && constant_time_eq(token.as_bytes(), expected_token.as_bytes())
    {
        attach_gateway_tags(&mut req, gateway_identity::GatewayTags::default());
        return Ok(next.run(req).await);
    }

    // Per-person gateway keys (enterprise#11): a bearer key whose SHA-256 is in
    // gateway-keys.toml authenticates AND identifies — its person/team/project
    // tags travel with the request and end up on the usage record.
    if let Some(token) = bearer.as_deref()
        && let Some(tags) = gateway_keys.lookup(token)
    {
        attach_gateway_tags(&mut req, tags);
        return Ok(next.run(req).await);
    }

    // Accept provider API keys on provider routes (host_guard runs first).
    // Client-auth registry rails (`/providers/{id}` with api_key_env=None,
    // e.g. Grok dual-rail) stay open even when require_token is set — the
    // Authorization / x-xai-token-auth header must carry the upstream session
    // and cannot also hold the lean-ctx Bearer.
    let client_auth_registry = is_client_auth_registry_route(path, &upstreams.borrow());
    if provider_key_fallback_allowed(
        require_token,
        has_provider_api_key(&req),
        is_provider_route(path),
        client_auth_registry,
    ) {
        attach_gateway_tags(&mut req, gateway_identity::GatewayTags::default());
        return Ok(next.run(req).await);
    }

    Err(auth_error_response(path))
}

fn auth_error_response(path: &str) -> Response {
    let is_mcp = path.starts_with("/mcp");
    let hint = if is_mcp {
        "MCP Streamable HTTP requires a Bearer token. Get it with: lean-ctx proxy token"
    } else if is_provider_route(path) {
        "Provider route requires authentication. Set your API key header or use: lean-ctx proxy token"
    } else {
        "This endpoint requires a lean-ctx Bearer token. Get it with: lean-ctx proxy token  \
         — or set proxy_loopback_open = true to disable auth on localhost"
    };

    let body = serde_json::json!({
        "type": "error",
        "error": {
            "type": "authentication_error",
            "message": format!("401 Unauthorized — {hint}")
        }
    });

    (StatusCode::UNAUTHORIZED, axum::Json(body)).into_response()
}

/// Resolves the final identity tags for an authenticated request and inserts
/// them as a request extension (read by `forward.rs::wire_context`).
///
/// Project resolution (enterprise#11): the `x-leanctx-project` header wins over
/// the key's `default_project` — one person books work onto different projects
/// per request. The header also works without a gateway key (solo/local mode:
/// project tagging without identity). It is an internal gateway header, not on
/// `ALLOWED_REQUEST_HEADERS`, so it never reaches the upstream.
fn attach_gateway_tags(req: &mut axum::extract::Request, mut tags: gateway_identity::GatewayTags) {
    if let Some(project) = req
        .headers()
        .get("x-leanctx-project")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|p| !p.is_empty() && p.len() <= 128 && !p.chars().any(char::is_control))
    {
        tags.project = Some(project.to_string());
    }
    // GDPR pseudonymization (enterprise#39): applied at this single
    // choke-point, so budgets, usage rows, dashboards and logs only ever see
    // the pseudonym. No-op unless [gateway_server].pseudonymize_persons.
    if let Some(person) = tags.person.as_deref()
        && pii::enabled()
    {
        tags.person = Some(pii::pseudonymize(person));
    }
    if !tags.is_empty() {
        req.extensions_mut().insert(tags);
    }
}

/// The personal view's static shell (`/me` + assets) renders without a key —
/// like the admin console's login screen, every number behind it comes from
/// the guarded `/api/me/usage`. Compiled out with the `gateway-server` feature.
fn me_shell_path(path: &str) -> bool {
    #[cfg(feature = "gateway-server")]
    {
        crate::gateway_server::user_api::is_shell_path(path)
    }
    #[cfg(not(feature = "gateway-server"))]
    {
        let _ = path;
        false
    }
}

fn has_provider_api_key(req: &axum::extract::Request) -> bool {
    let headers = req.headers();
    // Provider-specific key headers: Anthropic `x-api-key`, Google
    // `x-goog-api-key`, Azure `api-key`, Grok subscription `x-xai-token-auth`.
    // Any non-empty value authenticates. Grok dual-rail may send only the
    // session header (no Authorization) against `/providers/grok-chat`.
    for key in ["x-api-key", "x-goog-api-key", "api-key", "x-xai-token-auth"] {
        if headers
            .get(key)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| !v.trim().is_empty())
        {
            return true;
        }
    }
    // OpenAI-style `Authorization` auth. Accept ANY non-empty credential, not
    // just `Bearer sk-`/`gsk_`: OpenAI-*compatible* providers driven through
    // OpenCode/Codex (Azure, OpenRouter, Groq, vLLM/Ollama gateways, project &
    // service-account keys) issue keys that don't carry those prefixes. The proxy
    // binds to loopback only and never injects upstream credentials — it forwards
    // this header verbatim, so an invalid key is rejected by the real upstream,
    // never silently honoured. Gating provider routes on key *shape* only ever
    // produced false 401s for those clients (#362).
    if let Some(auth) = headers.get("authorization").and_then(|v| v.to_str().ok()) {
        let auth = auth.trim();
        let credential = auth
            .strip_prefix("Bearer ")
            .or_else(|| auth.strip_prefix("bearer "))
            .unwrap_or(auth)
            .trim();
        // Reject an empty value or a bare scheme keyword carrying no token.
        return !credential.is_empty() && !credential.eq_ignore_ascii_case("bearer");
    }
    false
}

/// `/providers/{id}/...` where the registered provider has no gateway-held
/// key (`api_key_env = None`). These dual-rail / client-auth rails put the
/// upstream session in Authorization or provider-specific headers.
fn is_client_auth_registry_route(path: &str, upstreams: &Upstreams) -> bool {
    let Some(rest) = path.strip_prefix("/providers/") else {
        return false;
    };
    let id = rest.split('/').next().unwrap_or("");
    if id.is_empty() {
        return false;
    }
    upstreams
        .providers
        .iter()
        .any(|p| p.id == id && p.api_key_env.is_none())
}

fn is_provider_route(path: &str) -> bool {
    path.starts_with("/v1/")
        || path.starts_with("/v1beta/")
        || path.starts_with("/chat/completions")
        || path.starts_with("/responses")
        || path.starts_with("/messages")
        || path.starts_with("/backend-api")
        // Bare model-catalog discovery (enterprise#63): clients whose base URL
        // omits `/v1` send `GET /models` with their provider key.
        || path == "/models"
        // Universal provider registry (enterprise#7): clients point their base
        // URL at `/providers/{id}/v1` (Grok subscription:
        // `GROK_CLI_CHAT_PROXY_BASE_URL`, Azure Foundry, OpenRouter, …) and
        // authenticate with their own provider/session Bearer. Without this,
        // model-list + chat land as lean-ctx 401s — Grok cannot select a mode.
        || path.starts_with("/providers/")
}

/// Decides whether a request authenticates via a provider API key alone, without
/// the lean-ctx Bearer token.
///
/// * Built-in provider routes (`/v1/*`, bare endpoints, …): only when
///   `require_token` is false (loopback-friendly default). Gateway mode
///   (non-loopback bind) forces `require_token` so these require the lean-ctx
///   Bearer (enterprise#8).
/// * Client-auth registry routes (`/providers/{id}` with `api_key_env = None`,
///   e.g. Grok dual-rail): always allowed when a provider credential is present.
///   The Authorization / `x-xai-token-auth` header must carry the upstream
///   session and cannot also hold the lean-ctx Bearer. Gateway-held keys
///   (`api_key_env` set) never take this path — those still need the lean-ctx
///   token so inject cannot be abused with a forged provider header.
///
/// Pure (except the precomputed `client_auth_registry` flag), so the policy is
/// unit-testable without axum middleware plumbing.
#[allow(clippy::fn_params_excessive_bools)] // deliberate pure predicate for unit tests
fn provider_key_fallback_allowed(
    require_token: bool,
    has_provider_key: bool,
    is_provider_route: bool,
    client_auth_registry: bool,
) -> bool {
    if !has_provider_key || !is_provider_route {
        return false;
    }
    if client_auth_registry {
        return true;
    }
    !require_token
}

/// Maps a bare provider endpoint to its canonical `/v1/...` form, preserving any
/// sub-path. Returns `None` when the path is already canonical or not a known
/// provider endpoint.
///
/// Some OpenAI-compatible clients treat the configured base URL as the API root
/// and append the bare endpoint, so they send `POST /responses` or
/// `/chat/completions` instead of `/v1/responses` — notably OpenCode via
/// `@ai-sdk/openai`, whose Responses-API requests land on `/responses`. The proxy
/// and every upstream only know the `/v1/...` paths, so an un-prefixed request
/// would 401 (not a provider route) and then 404 (no handler). (#353)
fn canonical_provider_path(path: &str) -> Option<String> {
    // Inverse case of the bare-endpoint rewrite below: the advertised
    // OPENAI_BASE_URL includes `/v1` (#366), so a client that treats the base URL
    // as an origin and appends `/v1/...` itself produces `/v1/v1/...`.
    if let Some(rest) = path.strip_prefix("/v1/v1/") {
        return Some(format!("/v1/{rest}"));
    }
    const BARE_TO_CANONICAL: &[(&str, &str, &str)] = &[
        ("/responses", "/v1/responses", "/responses/"),
        (
            "/chat/completions",
            "/v1/chat/completions",
            "/chat/completions/",
        ),
        ("/messages", "/v1/messages", "/messages/"),
    ];
    for (bare, canonical, bare_with_slash) in BARE_TO_CANONICAL {
        if path == *bare {
            return Some((*canonical).to_string());
        }
        if let Some(rest) = path.strip_prefix(bare_with_slash) {
            return Some(format!("{canonical}/{rest}"));
        }
    }
    None
}

/// Returns the canonicalized URI for a bare provider endpoint (query preserved),
/// or `None` when no rewrite is needed. Pure, so the rewrite is unit-testable
/// without constructing axum middleware plumbing.
fn normalized_provider_uri(uri: &axum::http::Uri) -> Option<axum::http::Uri> {
    let canonical = canonical_provider_path(uri.path())?;
    let new_path_and_query = match uri.query() {
        Some(q) => format!("{canonical}?{q}"),
        None => canonical,
    };
    new_path_and_query.parse::<axum::http::Uri>().ok()
}

/// Rewrites the request URI in place when it targets a bare provider endpoint, so
/// downstream auth (`is_provider_route`), routing and upstream forwarding all see
/// the canonical `/v1/...` path. (#353)
async fn normalize_provider_path(
    mut req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    if let Some(uri) = normalized_provider_uri(req.uri()) {
        *req.uri_mut() = uri;
    }
    next.run(req).await
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    if a.len() != b.len() {
        return false;
    }
    bool::from(a.ct_eq(b))
}

async fn host_guard(
    req: axum::extract::Request,
    next: axum::middleware::Next,
    allowed_hosts: Arc<Vec<String>>,
) -> Result<Response, StatusCode> {
    if let Some(host) = req.headers().get("host").and_then(|v| v.to_str().ok())
        && host_allowed(host, &allowed_hosts)
    {
        return Ok(next.run(req).await);
    }
    Err(StatusCode::FORBIDDEN)
}

/// DNS-rebinding guard: loopback Host headers always pass (today's local
/// behavior); in gateway mode the operator additionally allowlists the names
/// the gateway is reachable under (`proxy_allowed_hosts`, enterprise#8).
/// Matching is case-insensitive on the host with the port stripped.
fn host_allowed(host_header: &str, allowed: &[String]) -> bool {
    // `[::1]:8080` carries the port after the bracket; plain hosts after `:`.
    let host = host_header.trim();
    let h = if let Some(bracketed) = host.strip_prefix('[') {
        bracketed
            .split(']')
            .next()
            .map(|inner| format!("[{inner}]"))
    } else {
        host.split(':').next().map(str::to_string)
    };
    let Some(h) = h else {
        return false;
    };
    let h = h.trim_end_matches('.').to_ascii_lowercase();
    matches!(h.as_str(), "127.0.0.1" | "localhost" | "[::1]") || allowed.contains(&h)
}

/// Proxy-wide token-bucket rate limit (enterprise#37). `/health` is exempt so
/// orchestrator liveness probes never get throttled into a false restart.
async fn rate_limit_guard(
    req: axum::extract::Request,
    next: axum::middleware::Next,
    limiter: Arc<RateLimiter>,
) -> Result<Response, StatusCode> {
    if req.uri().path() != "/health" && !limiter.allow().await {
        return Err(StatusCode::TOO_MANY_REQUESTS);
    }
    Ok(next.run(req).await)
}

/// Token bucket: `max_rps` sustained, `burst` peak. Mirrors the team server's
/// limiter; lives here so the proxy stays independent of `http_server`
/// internals.
pub(crate) struct RateLimiter {
    max_rps: f64,
    burst: f64,
    state: tokio::sync::Mutex<RateLimiterState>,
}

struct RateLimiterState {
    tokens: f64,
    last: std::time::Instant,
}

impl RateLimiter {
    pub(crate) fn new(max_rps: u32, burst: u32) -> Self {
        Self {
            max_rps: f64::from(max_rps.max(1)),
            burst: f64::from(burst.max(1)),
            state: tokio::sync::Mutex::new(RateLimiterState {
                tokens: f64::from(burst.max(1)),
                last: std::time::Instant::now(),
            }),
        }
    }

    pub(crate) async fn allow(&self) -> bool {
        let mut s = self.state.lock().await;
        let now = std::time::Instant::now();
        let refill = now.saturating_duration_since(s.last).as_secs_f64() * self.max_rps;
        s.tokens = (s.tokens + refill).min(self.burst);
        s.last = now;
        if s.tokens >= 1.0 {
            s.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

async fn fallback_router(State(state): State<ProxyState>, req: Request<Body>) -> Response {
    let path = req.uri().path().to_string();

    if path.starts_with("/v1beta/models/") || path.starts_with("/v1/models/") {
        match google::handler(State(state), req).await {
            Ok(resp) => resp,
            Err(status) => Response::builder()
                .status(status)
                .body(Body::from("proxy error"))
                .expect("BUG: building error response with valid status should never fail"),
        }
    } else {
        let method = req.method().to_string();
        eprintln!("lean-ctx proxy: unmatched {method} {path}");
        Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::from(format!(
                "lean-ctx proxy: no handler for {method} {path}"
            )))
            .expect("BUG: building 404 response should never fail")
    }
}
