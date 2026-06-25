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
    /// Inject `stream_options.include_usage = true` into streamed `OpenAI` Chat
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
    /// Live tool-result compression on the wire (#481). `true` (the default)
    /// keeps today's behaviour: the proxy compresses non-protected `tool_result`
    /// content on every request. `false` turns it off so the proxy can run
    /// **meter-only** — real billed/cache token metering with zero request
    /// rewriting (combine with `history_mode = "off"` and no `role_aggressiveness`
    /// for a fully byte-unchanged body). Env `LEAN_CTX_PROXY_LIVE_COMPRESS`.
    /// See [`ProxyConfig::live_compresses`].
    pub live_compress: Option<bool>,
    /// Per-tool exclusion list for live tool-result compression (#481). Tool
    /// names are matched case-insensitively as substrings (the same style as
    /// [`crate::proxy::tool_kind::classify_tool_name`]); a match is treated as
    /// protected, exactly like a file read. `None` (the default) protects
    /// Serena's code-reading tools (`find_symbol`/`find_referencing_symbols`/
    /// `search_for_pattern` return source bodies the model edits, but are
    /// mis-bucketed as `Search` by name). Set an explicit list to narrow it, or
    /// `[]` to disable the exclusion. See [`ProxyConfig::is_tool_live_compress_excluded`].
    pub live_compress_exclude: Option<Vec<String>>,
    /// Opt-in in-band CCR retrieval for a remote proxy with no shared filesystem
    /// (#493, follow-up to #482). When enabled, a lossy stub advertises a compact
    /// `<lc_expand:HASH>` marker (instead of a local tee path the remote agent
    /// can't read); when the model echoes that marker back, the proxy splices the
    /// verbatim original — recovered from its **local** tee store — inline on the
    /// next request, costing one turn of latency and needing no MCP/FS on the
    /// agent host. `None`/`false` (the default) keeps the path-handle stub. The
    /// splice is a strict no-op on marker-less turns, so it never perturbs the
    /// provider cache prefix unless the model explicitly asked to expand. See
    /// [`ProxyConfig::ccr_inband_enabled`].
    pub ccr_inband: Option<bool>,
    /// Cache-safe, cross-provider reasoning-effort control (#834). One of
    /// `minimal|low|medium|high` pins the model's reasoning depth across every
    /// provider; `None`/`"off"` (the default) is a strict no-op. The value is a
    /// constant — identical on every request — so the provider prompt-cache
    /// prefix stays byte-stable (#448/#498) and only the model's reasoning depth
    /// changes. lean-ctx translates it to each provider's native parameter and
    /// only ever *fills* it (never overrides a client-set value), on models that
    /// accept it. Per-turn effort switching is deliberately unsupported — it
    /// would invalidate the prompt cache. Env `LEAN_CTX_PROXY_EFFORT`. See
    /// [`ProxyConfig::resolved_effort`].
    pub effort: Option<String>,
    /// How the proxy squeezes prose it must shrink (#895): `"auto"` (default) and
    /// `"extractive"` use embedding-based extractive ranking — keeping the most
    /// central sentences instead of just the prefix — when the local embedding
    /// engine is available, falling back to truncation otherwise; `"truncate"`
    /// keeps the original deterministic FIFO squeeze (and no engine). Wire
    /// rewrites are memoized per content so the engine's cold→warm transition
    /// never changes an already-emitted frozen-region rewrite (#448/#498). Env
    /// `LEAN_CTX_PROXY_PROSE_RANKER`. See [`ProxyConfig::resolved_prose_ranker`].
    pub prose_ranker: Option<String>,
    /// Fraction `0.0..=1.0` of conversations placed in the output-savings control
    /// arm (#895 Track B). `0` (default) = no holdout (every conversation is
    /// shaped). When `> 0`, a deterministic cohort = `blake3(system + first user
    /// msg)` puts ~this fraction of conversations in a control arm that skips
    /// output-shaping (effort control + verbosity steer) but is still metered —
    /// giving an honest measured output-token reduction. The cohort is a pure
    /// function of conversation identity, so a conversation stays in one arm
    /// across turns (cache-safe). Env `LEAN_CTX_PROXY_OUTPUT_HOLDOUT`. See
    /// [`ProxyConfig::output_holdout_fraction`].
    pub output_holdout: Option<f64>,
    /// Opt-in cache-safe wire verbosity steer (#895). When `true`, the proxy
    /// appends a single constant "be concise" instruction to the last user turn
    /// of each request (output-shaping for non-rules-aware API clients). The
    /// suffix is constant and appended strictly after the last `cache_control`
    /// breakpoint, so the provider prompt-cache prefix stays byte-stable. Default
    /// `false`. Env `LEAN_CTX_PROXY_VERBOSITY_STEER`. See
    /// [`ProxyConfig::verbosity_steer_enabled`].
    pub verbosity_steer: Option<bool>,
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
    /// Aggressiveness for system prompts (Anthropic `system` / `OpenAI` `system`
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

/// How the proxy squeezes prose it must shrink (#895).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProseRanker {
    /// Extractive embedding ranking when the engine is available, else truncate.
    /// The default — strictly better than truncation, and cache-safe via the
    /// per-content memo in [`crate::proxy::prose_ranker`].
    Auto,
    /// Same engine path as `Auto` (kept distinct so an operator can express
    /// intent / so a future "require engine" semantic has a name).
    Extractive,
    /// Original deterministic FIFO squeeze; never touches the embedding engine.
    Truncate,
}

/// How the proxy prunes old tool results from conversation history.
///
/// Provider prompt caches (Anthropic `cache_control`, `OpenAI` automatic prompt
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
    /// `OpenAI` Chat Completions to meter real spend. `[proxy] meter_openai_usage`
    /// in config.toml, default `true`.
    #[must_use]
    pub fn meters_openai_usage(&self) -> bool {
        self.meter_openai_usage.unwrap_or(true)
    }

    /// Resolved prose-ranker strategy (#895). Precedence: the
    /// `LEAN_CTX_PROXY_PROSE_RANKER` env var, then `[proxy] prose_ranker` in
    /// config.toml, then `Auto`. Unknown values resolve to `Auto` so a typo can
    /// never silently disable the premium path; `"truncate"`/`"off"` selects the
    /// legacy squeeze.
    #[must_use]
    pub fn resolved_prose_ranker(&self) -> ProseRanker {
        let raw = std::env::var("LEAN_CTX_PROXY_PROSE_RANKER")
            .ok()
            .or_else(|| self.prose_ranker.clone());
        match raw.as_deref().map(str::trim) {
            Some(s) if s.eq_ignore_ascii_case("truncate") || s.eq_ignore_ascii_case("off") => {
                ProseRanker::Truncate
            }
            Some(s) if s.eq_ignore_ascii_case("extractive") => ProseRanker::Extractive,
            _ => ProseRanker::Auto,
        }
    }

    /// Resolved output-savings holdout fraction (#895 Track B), clamped to
    /// `[0,1]`. Precedence: `LEAN_CTX_PROXY_OUTPUT_HOLDOUT` env > `[proxy]
    /// output_holdout` > `0.0` (no holdout). An unparseable/blank env value is
    /// ignored so a typo can never silently change the experiment fraction.
    #[must_use]
    pub fn output_holdout_fraction(&self) -> f64 {
        let from_env = std::env::var("LEAN_CTX_PROXY_OUTPUT_HOLDOUT")
            .ok()
            .and_then(|v| v.trim().parse::<f64>().ok());
        from_env
            .or(self.output_holdout)
            .unwrap_or(0.0)
            .clamp(0.0, 1.0)
    }

    /// Whether the cache-safe wire verbosity steer (#895) is enabled. Precedence:
    /// `LEAN_CTX_PROXY_VERBOSITY_STEER` env (`1`/`true`/`on`) > `[proxy]
    /// verbosity_steer` > `false` (off).
    #[must_use]
    pub fn verbosity_steer_enabled(&self) -> bool {
        if let Ok(raw) = std::env::var("LEAN_CTX_PROXY_VERBOSITY_STEER") {
            let v = raw.trim();
            return v.eq_ignore_ascii_case("1")
                || v.eq_ignore_ascii_case("true")
                || v.eq_ignore_ascii_case("on")
                || v.eq_ignore_ascii_case("yes");
        }
        self.verbosity_steer.unwrap_or(false)
    }

    /// Whether the opt-in cold-prefix repack (#480) is enabled. A wrong "cold"
    /// guess re-bills cache reads as writes (~12x), so this is off by default and
    /// must be explicitly enabled. `LEAN_CTX_PROXY_COLD_PREFIX_REPACK` (any
    /// value) wins, then `[proxy] cold_prefix_repack` in config.toml, else
    /// `false`.
    #[must_use]
    pub fn repacks_cold_prefix(&self) -> bool {
        std::env::var("LEAN_CTX_PROXY_COLD_PREFIX_REPACK").is_ok()
            || self.cold_prefix_repack.unwrap_or(false)
    }

    /// Whether opt-in in-band CCR retrieval (#493) is enabled. Off by default:
    /// the splice mutates provider-visible conversation content for the one turn
    /// the model asks to expand, so it must be an explicit opt-in.
    /// `LEAN_CTX_PROXY_CCR_INBAND` (any value) wins, then `[proxy] ccr_inband` in
    /// config.toml, else `false`.
    #[must_use]
    pub fn ccr_inband_enabled(&self) -> bool {
        std::env::var("LEAN_CTX_PROXY_CCR_INBAND").is_ok() || self.ccr_inband.unwrap_or(false)
    }

    /// Resolved cross-provider reasoning effort (#834), or `None` when the
    /// feature is off (the default — a strict no-op that preserves the
    /// byte-unchanged meter-only path). Precedence: `LEAN_CTX_PROXY_EFFORT` env
    /// (`off` disables, a valid level wins, an unparseable/blank value is
    /// ignored) > `[proxy] effort` in config.toml. Any unknown value resolves to
    /// `None` so a typo can never silently enable reasoning steering.
    #[must_use]
    pub fn resolved_effort(&self) -> Option<super::Effort> {
        if let Ok(raw) = std::env::var("LEAN_CTX_PROXY_EFFORT") {
            let trimmed = raw.trim();
            if trimmed.eq_ignore_ascii_case("off") {
                return None;
            }
            if let Some(effort) = super::Effort::parse(trimmed) {
                return Some(effort);
            }
            // Blank/unknown env → ignore and fall through to config, mirroring
            // `live_compresses` so a typo never flips the configured behaviour.
        }
        self.effort.as_deref().and_then(super::Effort::parse)
    }

    /// Whether the proxy live-compresses non-protected `tool_result` content
    /// (#481). `LEAN_CTX_PROXY_LIVE_COMPRESS` (`0`/`false`/`off`/`no` → off,
    /// `1`/`true`/`on`/`yes` → on) wins, then `[proxy] live_compress` in
    /// config.toml, else `true`. An unparseable/blank env value is ignored so a
    /// typo can never silently flip the mode.
    #[must_use]
    pub fn live_compresses(&self) -> bool {
        if let Ok(raw) = std::env::var("LEAN_CTX_PROXY_LIVE_COMPRESS") {
            match raw.trim().to_ascii_lowercase().as_str() {
                "0" | "false" | "off" | "no" => return false,
                "1" | "true" | "on" | "yes" => return true,
                _ => {}
            }
        }
        self.live_compress.unwrap_or(true)
    }

    /// Resolved per-tool live-compress exclusion patterns (#481). `None` in
    /// config falls back to the built-in default (protect Serena); an explicit
    /// list — including the empty list — is used verbatim so operators can narrow
    /// or fully clear it.
    #[must_use]
    pub fn live_compress_exclude_patterns(&self) -> Vec<String> {
        self.live_compress_exclude
            .clone()
            .unwrap_or_else(default_live_compress_exclude)
    }

    /// Whether `tool_name` is on the live-compress exclusion list (#481) and must
    /// therefore reach the model intact, like a protected file read. Matching is
    /// case-insensitive substring, mirroring `tool_kind::classify_tool_name`.
    #[must_use]
    pub fn is_tool_live_compress_excluded(&self, tool_name: &str) -> bool {
        let name = tool_name.to_ascii_lowercase();
        self.live_compress_exclude_patterns().iter().any(|p| {
            let p = p.trim().to_ascii_lowercase();
            !p.is_empty() && name.contains(p.as_str())
        })
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
    #[must_use]
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
    #[must_use]
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
    #[must_use]
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
    #[must_use]
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
#[must_use]
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
#[must_use]
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

/// Built-in default live-compress exclusion (#481). Serena's code-reading tools
/// (`find_symbol`/`find_referencing_symbols`/`search_for_pattern`) return source
/// bodies the model edits, yet are mis-bucketed as `Search` by name, so the proxy
/// would otherwise gut them. Protect anything namespaced `serena` by default.
fn default_live_compress_exclude() -> Vec<String> {
    vec!["serena".to_string()]
}

#[must_use]
pub fn normalize_url(value: &str) -> String {
    value.trim().trim_end_matches('/').to_string()
}

#[must_use]
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

#[must_use]
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
    fn ccr_inband_is_opt_in_and_config_enables() {
        // #493: off by default (the splice mutates provider-visible content for
        // the expand turn), enabled via config. Isolate from a developer shell
        // that may export the env override.
        let _lock = crate::core::data_dir::test_env_lock();
        crate::test_env::remove_var("LEAN_CTX_PROXY_CCR_INBAND");
        assert!(
            !ProxyConfig::default().ccr_inband_enabled(),
            "in-band CCR must be opt-in (off by default)"
        );
        let cfg = ProxyConfig {
            ccr_inband: Some(true),
            ..Default::default()
        };
        assert!(cfg.ccr_inband_enabled());
    }

    #[test]
    fn effort_defaults_off_and_config_sets_it() {
        // #834: cache-safe effort control is opt-in. Isolate from a developer
        // shell that may export the env override.
        let _lock = crate::core::data_dir::test_env_lock();
        crate::test_env::remove_var("LEAN_CTX_PROXY_EFFORT");
        assert_eq!(
            ProxyConfig::default().resolved_effort(),
            None,
            "effort control must be opt-in (off by default)"
        );
        let cfg = ProxyConfig {
            effort: Some("low".into()),
            ..Default::default()
        };
        assert_eq!(
            cfg.resolved_effort(),
            Some(crate::core::config::Effort::Low)
        );
        // An unknown configured value resolves to off — never a silent default.
        let typo = ProxyConfig {
            effort: Some("lowish".into()),
            ..Default::default()
        };
        assert_eq!(typo.resolved_effort(), None);
    }

    #[test]
    fn effort_env_overrides_and_off_disables() {
        use crate::core::config::Effort;
        let _lock = crate::core::data_dir::test_env_lock();
        let cfg = ProxyConfig {
            effort: Some("high".into()),
            ..Default::default()
        };
        // A valid env level wins over config.
        crate::test_env::set_var("LEAN_CTX_PROXY_EFFORT", "minimal");
        assert_eq!(cfg.resolved_effort(), Some(Effort::Minimal));
        // `off` explicitly disables even a configured level.
        crate::test_env::set_var("LEAN_CTX_PROXY_EFFORT", "off");
        assert_eq!(cfg.resolved_effort(), None);
        // A blank/garbage env value is ignored → falls back to config.
        crate::test_env::set_var("LEAN_CTX_PROXY_EFFORT", "   ");
        assert_eq!(cfg.resolved_effort(), Some(Effort::High));
        crate::test_env::remove_var("LEAN_CTX_PROXY_EFFORT");
    }

    #[test]
    fn prose_ranker_defaults_to_auto_and_config_sets_it() {
        // #895: premium extractive path is the default; `truncate`/`off` selects
        // the legacy squeeze; a typo can never silently disable the premium path.
        let _lock = crate::core::data_dir::test_env_lock();
        crate::test_env::remove_var("LEAN_CTX_PROXY_PROSE_RANKER");
        assert_eq!(
            ProxyConfig::default().resolved_prose_ranker(),
            ProseRanker::Auto
        );
        let truncate = ProxyConfig {
            prose_ranker: Some("truncate".into()),
            ..Default::default()
        };
        assert_eq!(truncate.resolved_prose_ranker(), ProseRanker::Truncate);
        let off = ProxyConfig {
            prose_ranker: Some("off".into()),
            ..Default::default()
        };
        assert_eq!(off.resolved_prose_ranker(), ProseRanker::Truncate);
        let extractive = ProxyConfig {
            prose_ranker: Some("extractive".into()),
            ..Default::default()
        };
        assert_eq!(extractive.resolved_prose_ranker(), ProseRanker::Extractive);
        let typo = ProxyConfig {
            prose_ranker: Some("extractiveish".into()),
            ..Default::default()
        };
        assert_eq!(
            typo.resolved_prose_ranker(),
            ProseRanker::Auto,
            "unknown value must resolve to Auto, never silently off"
        );
    }

    #[test]
    fn output_holdout_defaults_off_and_clamps() {
        let _lock = crate::core::data_dir::test_env_lock();
        crate::test_env::remove_var("LEAN_CTX_PROXY_OUTPUT_HOLDOUT");
        assert_eq!(ProxyConfig::default().output_holdout_fraction(), 0.0);
        let cfg = ProxyConfig {
            output_holdout: Some(0.2),
            ..Default::default()
        };
        assert!((cfg.output_holdout_fraction() - 0.2).abs() < f64::EPSILON);
        let over = ProxyConfig {
            output_holdout: Some(5.0),
            ..Default::default()
        };
        assert_eq!(over.output_holdout_fraction(), 1.0, "clamped into [0,1]");
    }

    #[test]
    fn verbosity_steer_defaults_off_and_env_overrides() {
        let _lock = crate::core::data_dir::test_env_lock();
        crate::test_env::remove_var("LEAN_CTX_PROXY_VERBOSITY_STEER");
        assert!(!ProxyConfig::default().verbosity_steer_enabled());
        let cfg = ProxyConfig {
            verbosity_steer: Some(true),
            ..Default::default()
        };
        assert!(cfg.verbosity_steer_enabled());
        crate::test_env::set_var("LEAN_CTX_PROXY_VERBOSITY_STEER", "on");
        assert!(ProxyConfig::default().verbosity_steer_enabled());
        crate::test_env::remove_var("LEAN_CTX_PROXY_VERBOSITY_STEER");
    }

    #[test]
    fn prose_ranker_env_overrides_config() {
        let _lock = crate::core::data_dir::test_env_lock();
        let cfg = ProxyConfig {
            prose_ranker: Some("auto".into()),
            ..Default::default()
        };
        crate::test_env::set_var("LEAN_CTX_PROXY_PROSE_RANKER", "truncate");
        assert_eq!(cfg.resolved_prose_ranker(), ProseRanker::Truncate);
        crate::test_env::remove_var("LEAN_CTX_PROXY_PROSE_RANKER");
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

    #[test]
    fn live_compress_defaults_on_and_config_disables() {
        // #481: default ON (today's behaviour); a config `false` opts into the
        // meter-only mode. Isolate from a developer shell exporting the override.
        let _lock = crate::core::data_dir::test_env_lock();
        crate::test_env::remove_var("LEAN_CTX_PROXY_LIVE_COMPRESS");
        assert!(
            ProxyConfig::default().live_compresses(),
            "live_compress must default to true"
        );
        let cfg = ProxyConfig {
            live_compress: Some(false),
            ..Default::default()
        };
        assert!(!cfg.live_compresses());
    }

    #[test]
    fn live_compress_env_overrides_config() {
        let _lock = crate::core::data_dir::test_env_lock();
        // env `off` wins over a config `true`.
        crate::test_env::set_var("LEAN_CTX_PROXY_LIVE_COMPRESS", "off");
        let cfg = ProxyConfig {
            live_compress: Some(true),
            ..Default::default()
        };
        assert!(!cfg.live_compresses(), "env off must win over config true");
        // A garbage env value is ignored → falls back to config.
        crate::test_env::set_var("LEAN_CTX_PROXY_LIVE_COMPRESS", "maybe");
        assert!(
            cfg.live_compresses(),
            "unparseable env must fall back to config, not flip the mode"
        );
        crate::test_env::remove_var("LEAN_CTX_PROXY_LIVE_COMPRESS");
    }

    #[test]
    fn live_compress_exclude_defaults_to_serena() {
        // #481: an unset list protects Serena's code-reading tools, which return
        // source bodies but are mis-bucketed as `Search` by name.
        let cfg = ProxyConfig::default();
        assert!(cfg.is_tool_live_compress_excluded("mcp__serena__find_symbol"));
        assert!(cfg.is_tool_live_compress_excluded("Serena.search_for_pattern"));
        assert!(!cfg.is_tool_live_compress_excluded("ctx_shell"));
    }

    #[test]
    fn live_compress_exclude_explicit_list_replaces_default() {
        // An explicit list narrows the exclusion (Serena no longer protected).
        let cfg = ProxyConfig {
            live_compress_exclude: Some(vec!["my_reader".into()]),
            ..Default::default()
        };
        assert!(cfg.is_tool_live_compress_excluded("acme_my_reader_v2"));
        assert!(!cfg.is_tool_live_compress_excluded("mcp__serena__find_symbol"));
    }

    #[test]
    fn live_compress_exclude_empty_list_disables_protection() {
        // `[]` fully clears the exclusion (operator opts every tool back in).
        let cfg = ProxyConfig {
            live_compress_exclude: Some(vec![]),
            ..Default::default()
        };
        assert!(!cfg.is_tool_live_compress_excluded("mcp__serena__find_symbol"));
    }
}
