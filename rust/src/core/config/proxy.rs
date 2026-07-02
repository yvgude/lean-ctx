//! API proxy upstream overrides (`config.toml`).

use serde::{Deserialize, Serialize};

/// API proxy upstream overrides. `None` = use provider default.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProxyConfig {
    pub anthropic_upstream: Option<String>,
    pub openai_upstream: Option<String>,
    pub chatgpt_upstream: Option<String>,
    pub gemini_upstream: Option<String>,
    /// Universal provider registry (`[[proxy.providers]]`): additional upstream
    /// providers beyond the four built-ins, declared as data — id + wire shape +
    /// base URL — so a new OpenAI/Anthropic/Gemini-compatible endpoint (Azure AI
    /// Foundry, OpenRouter, Groq, vLLM/Ollama, a corporate gateway…) is a pure
    /// config entry, never a code change. Reachable under
    /// `/providers/{id}/...` on the proxy and addressable by the router.
    /// The legacy `*_upstream` fields above stay authoritative for the four
    /// built-in provider routes (backwards compatible).
    pub providers: Vec<ProviderEntry>,
    /// History-pruning strategy for proxied chat requests.
    /// "cache-aware" (default) | "rolling" | "off". See [`HistoryMode`].
    pub history_mode: Option<String>,
    /// Allow a non-loopback plaintext `http://` upstream (trusted local network
    /// only). Opt-in; see [`ProxyConfig::allows_insecure_http_upstream`]. (#440)
    pub allow_insecure_http_upstream: Option<bool>,
    /// Allow a custom (non-allowlisted) **HTTPS** upstream host — e.g. a corporate
    /// gateway in front of the provider API. Opt-in; see
    /// [`ProxyConfig::allows_custom_upstream`]. Mirrors `allow_insecure_http_upstream`
    /// so the long-lived managed proxy (LaunchAgent / systemd), which only reads
    /// `config.toml` and never the shell's `LEAN_CTX_ALLOW_CUSTOM_UPSTREAM`, can
    /// honor a custom upstream too (#590).
    pub allow_custom_upstream: Option<bool>,
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
    /// File-path globs whose reads are never compressed (#1150). A read whose path
    /// matches any of these is returned verbatim (`full`) by the read tools — for
    /// files where exact bytes matter more than token savings: golden snapshots,
    /// byte-asserted fixtures, security-sensitive configs. Globs (`*`/`**`/`?`,
    /// the `glob` crate) are matched against the path and its file name, so
    /// `*.snap`, `**/golden/**`, and `tests/fixtures/*` all work. `None`/empty (the
    /// default) protects nothing — the lossless crushers and beneficial gate
    /// already keep compression safe, so this is an explicit escape hatch, not a
    /// default. See [`ProxyConfig::is_path_compress_protected`].
    pub compress_protect: Option<Vec<String>>,
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
    /// Opt-in active prompt-cache breakpoint injection for Anthropic (#939). When
    /// enabled and the client set no `cache_control` of its own, the proxy adds a
    /// single `cache_control: {type:"ephemeral"}` breakpoint to the `system`
    /// field so an otherwise-uncached, stable system prompt bills later turns at
    /// the cached rate. Anthropic-only: OpenAI/Gemini cache prefixes automatically
    /// and ignore the marker, so those paths stay byte-unchanged. The injection is
    /// deterministic, never adds a second breakpoint, and is skipped below
    /// Anthropic's minimum cacheable size. `None`/`false` (the default) leaves the
    /// request untouched. See [`ProxyConfig::cache_breakpoint_enabled`].
    pub cache_breakpoint: Option<bool>,
    /// Opt-in cache-aligner volatile-field telemetry (#940). When enabled, the
    /// proxy scans each *unanchored* Anthropic system prompt for volatile,
    /// cache-busting fields (ISO dates/datetimes, UUIDs, git SHAs) and records how
    /// many it found on `/status` `cache_safety` — purely to quantify how much
    /// prompt-cache the client is leaking. **Measurement only**: the request body
    /// is never mutated, so it is strictly cache-safe. `None` (the default) enables
    /// it — every proxy ships cache-leak visibility out of the box (#986 premium
    /// defaults); set `false` to opt out of the per-request scan. See
    /// [`ProxyConfig::cache_aligner_enabled`].
    pub cache_aligner: Option<bool>,
    /// Opt-in active cache-aligner relocate (#974). When enabled, the proxy
    /// rewrites an *unanchored* Anthropic `system` prompt into a stable block
    /// (volatile values — ISO dates/datetimes, UUIDs, git SHAs — replaced by
    /// constant placeholders) carrying the `cache_control` breakpoint, plus an
    /// *uncached* trailing block that re-states the relocated values. The cacheable
    /// prefix then stays byte-stable turn-to-turn and finally caches; only the
    /// small tail is reprocessed. Anthropic-only, Treatment-arm, gated on a client
    /// that anchored nothing and on Anthropic's minimum cacheable size.
    /// Deterministic (#498) and idempotent. `None`/`false` (the default) leaves the
    /// request untouched. The `cache_aligner` telemetry above is the precursor that
    /// quantifies how much this would save. See
    /// [`ProxyConfig::cache_align_relocate_enabled`].
    pub cache_align_relocate: Option<bool>,
    /// Cache-economics (#986), **on by default**. Bundles two strictly-safe halves
    /// behind one flag: (1) prompt-cache **miss attribution** telemetry — per turn,
    /// classify why the cache hit or missed (cold start / warm reuse / TTL lapse /
    /// prefix change) and expose cumulative gauges on `/status`
    /// ([`crate::proxy::cache_attribution`]); and (2) a **net-cost gate** on the
    /// cold-prefix repack ([`crate::proxy::cache_policy::worth_repacking`]) that
    /// skips re-seeding prefixes too small to be cached. The telemetry never
    /// touches the body and the gate only makes repacking *more* conservative, so
    /// it can never bust a cache that would otherwise have been kept. `None` (the
    /// default) enables both — every proxy gets the diagnosis and the safer repack
    /// out of the box (#986 premium defaults); set `false` to opt out. See
    /// [`ProxyConfig::cache_policy_enabled`].
    pub cache_policy: Option<bool>,
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
    /// Opt-in: route a Codex *ChatGPT-subscription* login through the proxy for
    /// model-turn compression. Default `None`/`false` keeps Codex native (history
    /// visible, cloud/remote intact, no #597). When `true`, Codex setup pins the
    /// generated `leanctx-chatgpt` provider + `chatgpt_base_url`; that scopes Codex
    /// history to the provider (#597), so it stays opt-in. Toggle durably with
    /// `lean-ctx proxy codex-chatgpt on|off`; resolved via
    /// [`ProxyConfig::codex_chatgpt_proxy_enabled`].
    pub codex_chatgpt_proxy: Option<bool>,
    /// Active request routing (`[proxy.routing]`, enterprise#13): model aliases
    /// and intent-tier downgrades applied in the forward path. Off by default —
    /// an empty/absent table is a strict passthrough. See [`RoutingRules`].
    pub routing: RoutingRules,
    /// Counterfactual-baseline parameters (`[proxy.baseline]`, enterprise#15/#18)
    /// for the avoided-cost evidence chain. See [`BaselineConfig`].
    pub baseline: BaselineConfig,
}

/// `[proxy.baseline]` — the contract-frozen counterfactual parameters that make
/// the success fee provable (enterprise#15, Doc 04 §6 / Doc 08 §2).
///
/// - `reference_model`: the model the customer *would have used* without
///   lean-ctx. Frozen per deployment/contract (calibration: enterprise#41);
///   every usage event stores `reference_cost_usd` = the request's
///   **uncompressed** input tokens priced at this model's input rate — the
///   counterfactual cost the avoided-cost ledger settles against.
/// - `local_shadow_rate_per_mtok`: USD per 1M tokens booked as the actual cost
///   of locally served (loopback) inference. Local compute is never free —
///   hardware and power are real — so the shadow rate keeps local-model savings
///   honest instead of infinite. Default: `0.25` USD/MTok, a conservative
///   self-hosting cost estimate; calibrate per deployment.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct BaselineConfig {
    /// Counterfactual reference model (`None` = baseline evidence off).
    pub reference_model: Option<String>,
    /// USD per 1M tokens for local/loopback inference (default 0.25, never 0).
    pub local_shadow_rate_per_mtok: Option<f64>,
}

/// Default local shadow rate (USD per 1M tokens) when `[proxy.baseline]` sets
/// none: a conservative self-hosted inference cost so `is_local` usage is
/// never booked at $0 (Doc 04 §6 "local-free ≠ cost-free").
pub const DEFAULT_LOCAL_SHADOW_RATE_PER_MTOK: f64 = 0.25;

impl BaselineConfig {
    /// Effective shadow rate: configured value (clamped positive) or default.
    #[must_use]
    pub fn effective_local_shadow_rate(&self) -> f64 {
        match self.local_shadow_rate_per_mtok {
            Some(r) if r > 0.0 => r,
            _ => DEFAULT_LOCAL_SHADOW_RATE_PER_MTOK,
        }
    }
}

/// `[proxy.routing]` — the active router's rule set (enterprise#13).
///
/// Two mechanisms, both **within-shape** in M1 (the target must speak the same
/// wire dialect as the request; N×M shape translation is M2):
///
/// - **Aliases**: exact requested-model → target. Lets an org expose stable
///   names (`acme/fast`) or transparently swap one concrete model for another.
/// - **Tiers**: intent-based downgrade. The request's last user message is
///   classified (`intent_router`); the resulting tier (`fast|standard|premium`)
///   picks a target from this table. An absent tier key (or `""`) keeps the
///   requested model — premium work is never silently downgraded unless the
///   operator says so.
///
/// A target is `"model"` (swap the model, keep the upstream) or
/// `"provider:model"` where `provider` is a `[[proxy.providers]]` registry id
/// or a built-in (`anthropic|openai|gemini`) — then the request is also
/// re-targeted to that provider's upstream.
///
/// **Fail-open by construction:** any lookup/classification/validation miss
/// routes nothing and forwards the request unchanged.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RoutingRules {
    /// Master switch; `false`/absent = passthrough (no body rewrite at all).
    pub enabled: Option<bool>,
    /// Exact model-name aliases: requested model → `"provider:model"` | `"model"`.
    /// BTreeMap for deterministic iteration/serialization (#498).
    pub aliases: std::collections::BTreeMap<String, String>,
    /// Intent-tier targets: `fast|standard|premium` → `"provider:model"` |
    /// `"model"` | `""` (= keep requested model).
    pub tiers: std::collections::BTreeMap<String, String>,
}

impl RoutingRules {
    /// True when the router should run at all.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.enabled.unwrap_or(false) && !(self.aliases.is_empty() && self.tiers.is_empty())
    }
}

/// A parsed routing target: optional provider id + model name.
/// `"foundry:gpt-4o-mini"` → provider `foundry`, model `gpt-4o-mini`;
/// `"claude-haiku-4-5"` → model only (upstream unchanged).
#[must_use]
pub fn parse_route_target(target: &str) -> Option<(Option<&str>, &str)> {
    let t = target.trim();
    if t.is_empty() {
        return None;
    }
    match t.split_once(':') {
        Some((provider, model)) => {
            let (provider, model) = (provider.trim(), model.trim());
            if provider.is_empty() || model.is_empty() {
                None
            } else {
                Some((Some(provider), model))
            }
        }
        None => Some((None, t)),
    }
}

/// The API dialect an upstream endpoint speaks — deliberately separate from the
/// provider's *identity*. lean-ctx understands three wire shapes; any number of
/// configured providers (Foundry, OpenRouter, Groq, a local vLLM…) map onto
/// them. New shape = code; new provider = config (universal-provider-framework).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WireShape {
    /// Anthropic Messages API (`/v1/messages`).
    Anthropic,
    /// OpenAI Chat Completions / Responses API (also spoken by Azure AI
    /// Foundry, OpenRouter, Groq, vLLM, Ollama, LM Studio…).
    OpenAi,
    /// Google Gemini `generateContent` API.
    Gemini,
}

impl WireShape {
    /// Stable lowercase name (serde representation) for logs and `/status`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            WireShape::Anthropic => "anthropic",
            WireShape::OpenAi => "openai",
            WireShape::Gemini => "gemini",
        }
    }
}

/// One `[[proxy.providers]]` registry entry (see [`ProxyConfig::providers`]).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderEntry {
    /// Registry id, used in the `/providers/{id}/...` route and in routing
    /// rules. Lowercase alphanumeric plus `-`/`_`; must not shadow a built-in
    /// provider name (`anthropic`, `openai`, `chatgpt`, `gemini`).
    pub id: String,
    /// Which API dialect the endpoint speaks (`anthropic|openai|gemini`).
    pub shape: WireShape,
    /// Endpoint base URL. HTTPS for any non-loopback host; a declared registry
    /// entry is itself the custom-host opt-in (no separate allowlist flag).
    pub base_url: String,
    /// Name of the environment variable holding the upstream API key the
    /// gateway injects (replacing the caller's credential headers). `None` =
    /// forward the caller's own credentials verbatim (default, loopback mode).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    /// Set `false` to keep the entry in config but take it out of service.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Marks this endpoint as local inference (Ollama/vLLM/…): usage is booked
    /// at the transparent `local_shadow_rate` instead of provider list prices
    /// (enterprise#15/#18). Unset = derived from the URL (loopback hosts are
    /// local). Set it explicitly when the endpoint is local but not loopback —
    /// the containerized gateway reaching the host's Ollama
    /// (`host.docker.internal`) or an in-cluster server (`ollama.svc.cluster.local`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local: Option<bool>,
}

/// A validated, ready-to-serve registry provider (runtime view of
/// [`ProviderEntry`], published inside [`Upstreams`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedProvider {
    pub id: String,
    pub shape: WireShape,
    pub base_url: String,
    pub api_key_env: Option<String>,
    /// Billed as local inference (shadow rate). Explicit `local` flag when the
    /// entry declares one, otherwise loopback-URL derivation.
    pub local: bool,
}

/// Built-in provider route names a registry entry must not shadow.
const BUILTIN_PROVIDER_IDS: &[&str] = &["anthropic", "openai", "chatgpt", "gemini"];

/// True when `id` is usable as a registry id: non-empty, lowercase alnum plus
/// `-`/`_` (it becomes a URL path segment), and not a built-in provider name.
fn is_valid_provider_id(id: &str) -> bool {
    !id.is_empty()
        && !BUILTIN_PROVIDER_IDS.contains(&id)
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
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

    /// Resolved Codex ChatGPT-subscription proxy opt-in (default off).
    /// `LEAN_CTX_CODEX_CHATGPT_PROXY` (any value) forces it on for the current
    /// process, then `[proxy] codex_chatgpt_proxy` in config.toml, else `false`.
    pub fn codex_chatgpt_proxy_enabled(&self) -> bool {
        std::env::var("LEAN_CTX_CODEX_CHATGPT_PROXY").is_ok()
            || self.codex_chatgpt_proxy.unwrap_or(false)
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

    /// Whether opt-in in-band CCR retrieval (#493) is enabled. Off by default:
    /// the splice mutates provider-visible conversation content for the one turn
    /// the model asks to expand, so it must be an explicit opt-in.
    /// `LEAN_CTX_PROXY_CCR_INBAND` (any value) wins, then `[proxy] ccr_inband` in
    /// config.toml, else `false`.
    pub fn ccr_inband_enabled(&self) -> bool {
        std::env::var("LEAN_CTX_PROXY_CCR_INBAND").is_ok() || self.ccr_inband.unwrap_or(false)
    }

    /// Whether opt-in Anthropic prompt-cache breakpoint injection (#939) is
    /// enabled. Off by default: it mutates the provider-visible `system` shape
    /// (string → cache-marked block array), so it must be an explicit opt-in.
    /// `LEAN_CTX_PROXY_CACHE_BREAKPOINT` (any value) wins, then `[proxy]
    /// cache_breakpoint` in config.toml, else `false`.
    pub fn cache_breakpoint_enabled(&self) -> bool {
        std::env::var("LEAN_CTX_PROXY_CACHE_BREAKPOINT").is_ok()
            || self.cache_breakpoint.unwrap_or(false)
    }

    /// Whether opt-in cache-aligner volatile-field telemetry (#940) is enabled.
    /// On by default (#986 premium defaults): the scan is pure measurement and
    /// never mutates the body, so every proxy ships cache-leak visibility out of
    /// the box. Strictly cache-safe. `LEAN_CTX_PROXY_CACHE_ALIGNER=on|off` wins,
    /// then `[proxy] cache_aligner` in config.toml, else `true`. Opt **out** only
    /// to drop the per-request system-prompt scan.
    pub fn cache_aligner_enabled(&self) -> bool {
        env_bool_or("LEAN_CTX_PROXY_CACHE_ALIGNER", self.cache_aligner, true)
    }

    /// Whether opt-in active cache-aligner relocate (#974) is enabled. Off by
    /// default: it reshapes the provider-visible `system` field (moving volatile
    /// values to an uncached tail block), so it must be an explicit opt-in.
    /// `LEAN_CTX_PROXY_CACHE_ALIGN_RELOCATE` (any value) wins, then `[proxy]
    /// cache_align_relocate` in config.toml, else `false`.
    pub fn cache_align_relocate_enabled(&self) -> bool {
        std::env::var("LEAN_CTX_PROXY_CACHE_ALIGN_RELOCATE").is_ok()
            || self.cache_align_relocate.unwrap_or(false)
    }

    /// Whether cache-economics (#986) is enabled: prompt-cache miss attribution
    /// telemetry plus the net-cost repack gate. Both are strictly safe
    /// (measurement + a more-conservative repack that never busts a cache the
    /// default kept), so this is **on by default** — every proxy gets the
    /// diagnosis and the safer repack out of the box.
    /// `LEAN_CTX_PROXY_CACHE_POLICY=on|off` wins, then `[proxy] cache_policy` in
    /// config.toml, else `true`. Opt out to keep `/status` free of the attribution
    /// gauges and skip the per-request prefix hash.
    pub fn cache_policy_enabled(&self) -> bool {
        env_bool_or("LEAN_CTX_PROXY_CACHE_POLICY", self.cache_policy, true)
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

    /// Compiled `compress_protect` globs (#1150), skipping any that fail to parse
    /// so one malformed entry never disables the rest. Empty when unset — the
    /// default — which makes [`Self::is_path_compress_protected`] a fast no-op.
    #[must_use]
    pub fn compress_protect_globs(&self) -> Vec<glob::Pattern> {
        self.compress_protect
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter_map(|p| glob::Pattern::new(p.trim()).ok())
            .collect()
    }

    /// Whether `path` is on the never-compress list (#1150) and must be returned
    /// verbatim. Each glob is tried against both the full path (with backslashes
    /// normalised to `/`) and the bare file name, so `*.snap` matches anywhere
    /// while `**/golden/**` can still target a directory. Empty list → always
    /// `false` (today's behaviour), so a default proxy pays nothing.
    #[must_use]
    pub fn is_path_compress_protected(&self, path: &str) -> bool {
        let patterns = self.compress_protect_globs();
        if patterns.is_empty() {
            return false;
        }
        let norm = path.replace('\\', "/");
        let base = norm.rsplit('/').next().unwrap_or(norm.as_str());
        patterns.iter().any(|p| p.matches(&norm) || p.matches(base))
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

    /// Whether a custom (non-allowlisted) HTTPS upstream host is allowed. Opt-in
    /// only — lifting the built-in host allowlist points the proxy at a host you
    /// control (e.g. a corporate gateway), so it must be deliberate.
    /// `LEAN_CTX_ALLOW_CUSTOM_UPSTREAM` (any value) wins, then
    /// `[proxy] allow_custom_upstream` in config.toml, default `false`.
    ///
    /// Unlike the env var, the **config flag reaches the managed (service-spawned)
    /// proxy**, which only reads `config.toml` — that is the whole point of #590:
    /// `proxy enable`/`restart` start the proxy via launchd/systemd, which never
    /// inherits the shell's `LEAN_CTX_ALLOW_CUSTOM_UPSTREAM`.
    pub fn allows_custom_upstream(&self) -> bool {
        std::env::var("LEAN_CTX_ALLOW_CUSTOM_UPSTREAM").is_ok()
            || self.allow_custom_upstream.unwrap_or(false)
    }

    /// True when any `*_upstream` configured in `config.toml` (env-independent) is a
    /// custom HTTPS host outside the built-in allowlist — i.e. one that resolves
    /// only with the [`Self::allows_custom_upstream`] opt-in. Plaintext-HTTP custom
    /// hosts are governed by `allow_insecure_http_upstream` instead, so they are
    /// excluded here. Lets `proxy enable`/`restart` persist the opt-in (so the
    /// managed proxy honors it) and `proxy status` explain a blocked upstream,
    /// without touching the allowlisted-host case (#590).
    #[must_use]
    pub fn has_custom_host_upstream(&self) -> bool {
        [
            self.anthropic_upstream.as_deref(),
            self.openai_upstream.as_deref(),
            self.chatgpt_upstream.as_deref(),
            self.gemini_upstream.as_deref(),
        ]
        .into_iter()
        .flatten()
        .filter_map(normalize_url_opt)
        .any(|u| is_custom_upstream_host(&u))
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
            ProxyProvider::ChatGpt => (
                "LEAN_CTX_CHATGPT_UPSTREAM",
                self.chatgpt_upstream.as_deref(),
                "https://chatgpt.com",
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
            Some(url) => validate_upstream_url(
                &url,
                self.allows_insecure_http_upstream(),
                self.allows_custom_upstream(),
            ),
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
            chatgpt: self.resolve_upstream(ProxyProvider::ChatGpt),
            gemini: self.resolve_upstream(ProxyProvider::Gemini),
            providers: self.resolve_providers(),
        }
    }

    /// Validate + resolve the `[[proxy.providers]]` registry. Invalid entries
    /// are logged and skipped (one typo must never take the proxy down or
    /// disable the remaining registry); duplicates keep the first occurrence.
    /// A declared registry entry is itself the deliberate custom-host opt-in,
    /// so any HTTPS host is accepted; plaintext HTTP still requires loopback or
    /// the explicit insecure-HTTP opt-in (same rule as the built-ins).
    #[must_use]
    pub fn resolve_providers(&self) -> Vec<ResolvedProvider> {
        let mut seen: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        let mut out = Vec::new();
        for entry in &self.providers {
            if !entry.enabled.unwrap_or(true) {
                continue;
            }
            let id = entry.id.trim();
            if !is_valid_provider_id(id) {
                tracing::warn!(
                    "[proxy.providers] invalid id '{id}' (lowercase alnum/-/_ only, \
                     must not shadow a built-in provider) — entry skipped"
                );
                continue;
            }
            if !seen.insert(id) {
                tracing::warn!("[proxy.providers] duplicate id '{id}' — keeping first entry");
                continue;
            }
            match validate_upstream_url(&entry.base_url, self.allows_insecure_http_upstream(), true)
            {
                Ok(base_url) => {
                    // Explicit `local` flag wins; otherwise loopback URLs are
                    // local (host.docker.internal etc. need the explicit flag).
                    let local = entry.local.unwrap_or_else(|| is_local_proxy_url(&base_url));
                    out.push(ResolvedProvider {
                        id: id.to_string(),
                        shape: entry.shape,
                        base_url,
                        api_key_env: entry
                            .api_key_env
                            .as_deref()
                            .map(str::trim)
                            .filter(|v| !v.is_empty())
                            .map(str::to_string),
                        local,
                    });
                }
                Err(e) => {
                    tracing::warn!("[proxy.providers] '{id}' has invalid base_url — skipped: {e}");
                }
            }
        }
        out
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
            chatgpt: pick(ProxyProvider::ChatGpt),
            gemini: pick(ProxyProvider::Gemini),
            providers: self.resolve_providers(),
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
            chatgpt: keep(ProxyProvider::ChatGpt, &last.chatgpt),
            gemini: keep(ProxyProvider::Gemini, &last.gemini),
            // Registry re-resolution is deterministic from config; an entry
            // that turned invalid is dropped with a warning (see
            // `resolve_providers`), the rest keep serving.
            providers: self.resolve_providers(),
        }
    }
}

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

/// Resolve a tri-state boolean toggle for the default-**on** proxy features: an
/// explicit `on`/`off`-style environment variable wins, then the config
/// `Option<bool>`, else `default`. Lets an operator force a feature on **or** off
/// from the shell; an unparseable value is ignored so a typo can never silently
/// flip it (mirrors [`ProxyConfig::live_compresses`]).
fn env_bool_or(env_key: &str, configured: Option<bool>, default: bool) -> bool {
    if let Ok(raw) = std::env::var(env_key) {
        match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => return true,
            "0" | "false" | "no" | "off" => return false,
            _ => {}
        }
    }
    configured.unwrap_or(default)
}

/// Built-in default live-compress exclusion (#481). Serena's code-reading tools
/// (`find_symbol`/`find_referencing_symbols`/`search_for_pattern`) return source
/// bodies the model edits, yet are mis-bucketed as `Search` by name, so the proxy
/// would otherwise gut them. Protect anything namespaced `serena` by default.
fn default_live_compress_exclude() -> Vec<String> {
    vec!["serena".to_string()]
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
];

pub(super) fn validate_upstream_url(
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
fn is_custom_upstream_host(url: &str) -> bool {
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

#[cfg(test)]
#[path = "proxy_tests.rs"]
mod tests;
