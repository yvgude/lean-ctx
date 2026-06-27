//! Advanced config sections (proxy, memory subsystems, custom_aliases, setup, llm).
//! Split out of `schema/mod.rs`; `use super::*` re-imports helpers + `SectionSchema`.

#[allow(clippy::wildcard_imports)]
use super::*;
use std::collections::BTreeMap;

pub(super) fn build(sections: &mut BTreeMap<String, SectionSchema>) {
    let cfg = crate::core::config::Config::default();
    let mut proxy = BTreeMap::new();
    proxy.insert(
        "anthropic_upstream".into(),
        key(
            "string?",
            serde_json::json!(cfg.proxy.anthropic_upstream),
            "Custom upstream URL for Anthropic API proxy",
        ),
    );
    proxy.insert(
        "openai_upstream".into(),
        key(
            "string?",
            serde_json::json!(cfg.proxy.openai_upstream),
            "Custom upstream URL for OpenAI API proxy",
        ),
    );
    proxy.insert(
        "chatgpt_upstream".into(),
        key(
            "string?",
            serde_json::json!(cfg.proxy.chatgpt_upstream),
            "Custom upstream URL for ChatGPT/Codex subscription API proxy",
        ),
    );
    proxy.insert(
        "gemini_upstream".into(),
        key(
            "string?",
            serde_json::json!(cfg.proxy.gemini_upstream),
            "Custom upstream URL for Gemini API proxy",
        ),
    );
    proxy.insert(
        "history_mode".into(),
        key_enum_with_env(
            &["cache-aware", "rolling", "off"],
            "cache-aware",
            "History pruning strategy. cache-aware: frozen boundaries that keep provider prompt caches valid (default). rolling: legacy moving window (max raw savings, breaks prompt caching). off: never prune",
            "LEAN_CTX_PROXY_HISTORY_MODE",
        ),
    );
    proxy.insert(
        "allow_insecure_http_upstream".into(),
        key_with_env(
            "bool",
            serde_json::json!(cfg.proxy.allow_insecure_http_upstream.unwrap_or(false)),
            "Allow a non-loopback plaintext http:// upstream (trusted local network only, e.g. http://host.docker.internal:2455 in front of codex-lb). Opt-in; default false",
            "LEAN_CTX_ALLOW_INSECURE_HTTP_UPSTREAM",
        ),
    );
    proxy.insert(
        "meter_openai_usage".into(),
        key(
            "bool",
            serde_json::json!(cfg.proxy.meters_openai_usage()),
            "Inject stream_options.include_usage into streamed OpenAI Chat Completions so the final chunk reports real token usage for the measured spend meter. Default true",
        ),
    );
    proxy.insert(
        "cold_prefix_repack".into(),
        key_with_env(
            "bool",
            serde_json::json!(cfg.proxy.repacks_cold_prefix()),
            "Opt-in big-gap cold-prefix repack (#480): on a session-resume request the proxy may predict (from idle time vs the provider cache TTL) that the client-cached prefix has already expired, then prune that now-cold prefix to re-seed a leaner cache and keep applying the same deterministic compression on later turns so warm follow-ups hit it (sticky; baselines persist across restarts, #499). A wrong guess re-bills cache reads as writes (~12x), so default false",
            "LEAN_CTX_PROXY_COLD_PREFIX_REPACK",
        ),
    );
    proxy.insert(
        "live_compress".into(),
        key_with_env(
            "bool",
            serde_json::json!(cfg.proxy.live_compresses()),
            "Live-compress non-protected tool_result content on the wire (#481). Default true. Set false for a meter-only proxy — real billed/cache token metering with zero request rewriting (combine with history_mode = \"off\" and no role_aggressiveness for a byte-unchanged body)",
            "LEAN_CTX_PROXY_LIVE_COMPRESS",
        ),
    );
    proxy.insert(
        "live_compress_exclude".into(),
        key(
            "string[]",
            serde_json::json!(cfg.proxy.live_compress_exclude_patterns()),
            "Tool-name patterns (case-insensitive substring) whose tool_result is never live-compressed — treated as protected, like a file read (#481). Unset protects Serena's code-reading tools; set an explicit list to narrow it, or [] to disable",
        ),
    );
    proxy.insert(
        "compress_protect".into(),
        key(
            "string[]",
            serde_json::json!(cfg.proxy.compress_protect.clone().unwrap_or_default()),
            "File-path globs whose reads are never compressed (#1150): a matching path is returned verbatim (full) by the read tools, for files where exact bytes matter more than token savings (golden snapshots, byte-asserted fixtures, security-sensitive configs). Globs (*/**/?) match the path and its file name, so *.snap, **/golden/**, tests/fixtures/* all work. Empty (default) protects nothing — the lossless crushers and beneficial gate already keep compression safe; this is an explicit escape hatch",
        ),
    );
    proxy.insert(
        "ccr_inband".into(),
        key_with_env(
            "bool",
            serde_json::json!(cfg.proxy.ccr_inband_enabled()),
            "Opt-in in-band CCR retrieval for a remote proxy with no shared filesystem (#493). When on, a lossy stub advertises a compact <lc_expand:HASH> marker instead of a local tee path; when the model echoes that marker, the proxy splices the verbatim original (from its local tee store) back inline next turn — one turn of latency, no MCP/filesystem on the agent host. The splice is a strict no-op on marker-less turns, so it never perturbs the provider cache prefix unless the model asked to expand. Default false",
            "LEAN_CTX_PROXY_CCR_INBAND",
        ),
    );
    proxy.insert(
        "cache_breakpoint".into(),
        key_with_env(
            "bool",
            serde_json::json!(cfg.proxy.cache_breakpoint_enabled()),
            "Opt-in active prompt-cache breakpoint injection for Anthropic (#939). When on and the client set no cache_control of its own, the proxy adds one cache_control: {type:ephemeral} marker to the system field so an otherwise-uncached, stable system prompt bills later turns at the cached rate (the win a raw API client leaves on the table). Anthropic-only: OpenAI/Gemini cache prefixes automatically and ignore the marker, so those paths stay byte-unchanged. Deterministic, never adds a second breakpoint, and skipped below Anthropic's minimum cacheable size. Default false",
            "LEAN_CTX_PROXY_CACHE_BREAKPOINT",
        ),
    );
    proxy.insert(
        "cache_aligner".into(),
        key_with_env(
            "bool",
            serde_json::json!(cfg.proxy.cache_aligner_enabled()),
            "Cache-aligner volatile-field telemetry (#940), on by default. The proxy scans each unanchored Anthropic system prompt for volatile, cache-busting fields (ISO dates/datetimes, UUIDs, git SHAs) and reports how many it found on /status cache_safety (volatile_system_requests, volatile_fields_detected) - purely to quantify how much prompt-cache the client leaks. Measurement only: the request body is never mutated, so it is strictly cache-safe, which is why it ships on for every proxy (#986 premium defaults). The deterministic scan is the precursor to the opt-in tail-relocate below. Set false to opt out of the per-request scan. Default true",
            "LEAN_CTX_PROXY_CACHE_ALIGNER",
        ),
    );
    proxy.insert(
        "cache_align_relocate".into(),
        key_with_env(
            "bool",
            serde_json::json!(cfg.proxy.cache_align_relocate_enabled()),
            "Opt-in active cache-aligner relocate (#974). When on, the proxy rewrites an unanchored Anthropic system prompt into a stable block (volatile values - ISO dates/datetimes, UUIDs, git SHAs - replaced by constant placeholders) carrying the cache_control breakpoint, plus an uncached trailing block that re-states the relocated values. The cacheable prefix then stays byte-stable turn-to-turn and finally caches; only the small tail is reprocessed. Anthropic-only, Treatment-arm, gated on a client that anchored nothing and on Anthropic's minimum cacheable size. Deterministic (#498) and idempotent. The cache_aligner telemetry is the precursor that quantifies the saving. Default false",
            "LEAN_CTX_PROXY_CACHE_ALIGN_RELOCATE",
        ),
    );
    proxy.insert(
        "cache_policy".into(),
        key_with_env(
            "bool",
            serde_json::json!(cfg.proxy.cache_policy_enabled()),
            "Cache-economics (#986), on by default. Enables prompt-cache miss attribution telemetry (per turn, classify the outcome as cold start / warm reuse / TTL lapse / prefix change and report cumulative gauges on /status cache_attribution) plus a net-cost gate on the cold-prefix repack that skips re-seeding prefixes too small to be cached (below Anthropic's ~1024-token minimum). The telemetry never mutates the body and the gate only makes repacking more conservative, so it can never bust a cache that would otherwise have been kept - both halves are strictly safe, so every proxy gets them out of the box (#986 premium defaults). Set false to opt out (drops the /status attribution gauges and the per-request prefix hash). Default true",
            "LEAN_CTX_PROXY_CACHE_POLICY",
        ),
    );
    proxy.insert(
        "effort".into(),
        key_enum_with_env(
            &["off", "minimal", "low", "medium", "high"],
            "off",
            "Cache-safe cross-provider reasoning-effort control (#834). off (default) = no-op. minimal|low|medium|high pins the model's reasoning depth across providers: lean-ctx translates it to OpenAI reasoning_effort / reasoning.effort, Anthropic output_config.effort, and Gemini thinkingConfig (thinkingLevel on 3.x, thinkingBudget on 2.5 pro/flash), only on models that accept it and only when the client didn't set its own value. The level is a constant, so it never breaks the provider prompt cache (unlike per-turn effort routing). Anthropic is dialed only when the client already requested adaptive thinking",
            "LEAN_CTX_PROXY_EFFORT",
        ),
    );
    proxy.insert(
        "prose_ranker".into(),
        key_enum_with_env(
            &["auto", "extractive", "truncate"],
            "auto",
            "How the proxy squeezes prose it must shrink (#895). auto (default) and extractive use embedding-based extractive ranking — keeping the most central sentences instead of just the prefix — when the local embedding engine is available, else fall back to truncation; truncate keeps the original deterministic FIFO squeeze and never loads the engine. Wire rewrites are memoized per content so the engine's cold→warm transition never changes an already-emitted frozen-region rewrite (cache-safe, #448/#498)",
            "LEAN_CTX_PROXY_PROSE_RANKER",
        ),
    );
    proxy.insert(
        "output_holdout".into(),
        key_with_env(
            "f64",
            serde_json::json!(cfg.proxy.output_holdout_fraction()),
            "Fraction 0.0-1.0 of conversations placed in the output-savings control arm (#895). 0 (default) = no holdout (every conversation is output-shaped). When > 0, a deterministic cohort = blake3(system + first user message) puts ~this fraction in a control arm that skips output-shaping (effort control + verbosity steer) but is still metered, yielding an honest measured output-token reduction (lean-ctx output-savings). The cohort is a pure function of conversation identity, so a conversation keeps one arm across all turns - cache-safe",
            "LEAN_CTX_PROXY_OUTPUT_HOLDOUT",
        ),
    );
    proxy.insert(
        "verbosity_steer".into(),
        key_with_env(
            "bool",
            serde_json::json!(cfg.proxy.verbosity_steer_enabled()),
            "Opt-in cache-safe wire verbosity steer (#895). When true, the proxy appends a single constant 'be concise' instruction to the last user turn of each request - output-shaping for raw API clients that do not load lean-ctx rules. The suffix is constant and appended strictly after the last cache_control breakpoint (a new trailing text block, never modifying a cache-anchored block), so the provider prompt-cache prefix stays byte-stable. Under an output_holdout the control arm skips it so its effect is measured. Default false",
            "LEAN_CTX_PROXY_VERBOSITY_STEER",
        ),
    );
    sections.insert(
        "proxy".into(),
        SectionSchema {
            description: "Proxy upstream configuration for API routing".into(),
            keys: proxy,
        },
    );

    let mut role_aggr = BTreeMap::new();
    role_aggr.insert(
        "system".into(),
        key_with_env(
            "f64",
            serde_json::json!(cfg.proxy.role_aggressiveness.system),
            "Opt-in prose compression intensity (0.0–1.0) for system prompts in the proxy's frozen request region. Unset = leave untouched. Higher = more aggressive. Cache-safe (deterministic, never touches the client-cached prefix)",
            "LEAN_CTX_PROXY_SYSTEM_AGGR",
        ),
    );
    role_aggr.insert(
        "user".into(),
        key_with_env(
            "f64",
            serde_json::json!(cfg.proxy.role_aggressiveness.user),
            "Opt-in prose compression intensity (0.0–1.0) for free-text user turns (never tool results) in the proxy's frozen request region. Unset = leave untouched",
            "LEAN_CTX_PROXY_USER_AGGR",
        ),
    );
    sections.insert(
        "proxy.role_aggressiveness".into(),
        SectionSchema {
            description: "Opt-in per-role prose compression for the proxy's frozen request region (#710). Assistant turns are always passed through verbatim".into(),
            keys: role_aggr,
        },
    );

    let mut cost = BTreeMap::new();
    cost.insert(
        "default_model".into(),
        key(
            "string?",
            serde_json::json!(cfg.cost.default_model),
            "Fallback pricing model for MCP-only IDEs whose real model lean-ctx cannot observe (Cursor, Copilot, Windsurf, …). Unset → blended heuristic. Per-IDE overrides live in [cost.models]",
        ),
    );
    sections.insert(
        "cost".into(),
        SectionSchema {
            description: "Model declaration for measured-vs-estimated cost reporting".into(),
            keys: cost,
        },
    );

    let mem = &cfg.memory;
    let mut mem_knowledge = BTreeMap::new();
    mem_knowledge.insert(
        "max_facts".into(),
        key(
            "usize",
            serde_json::json!(mem.knowledge.max_facts),
            "Maximum number of knowledge facts stored per project",
        ),
    );
    mem_knowledge.insert(
        "max_patterns".into(),
        key(
            "usize",
            serde_json::json!(mem.knowledge.max_patterns),
            "Maximum number of patterns stored",
        ),
    );
    mem_knowledge.insert(
        "max_history".into(),
        key(
            "usize",
            serde_json::json!(mem.knowledge.max_history),
            "Maximum history entries retained",
        ),
    );
    mem_knowledge.insert(
        "contradiction_threshold".into(),
        key(
            "f32",
            clean_f32(mem.knowledge.contradiction_threshold),
            "Confidence threshold for contradiction detection",
        ),
    );
    mem_knowledge.insert(
        "recall_facts_limit".into(),
        key(
            "usize",
            serde_json::json!(mem.knowledge.recall_facts_limit),
            "Maximum facts returned per recall query",
        ),
    );
    mem_knowledge.insert(
        "rooms_limit".into(),
        key(
            "usize",
            serde_json::json!(mem.knowledge.rooms_limit),
            "Maximum number of rooms returned",
        ),
    );
    mem_knowledge.insert(
        "timeline_limit".into(),
        key(
            "usize",
            serde_json::json!(mem.knowledge.timeline_limit),
            "Maximum number of timeline entries returned",
        ),
    );
    mem_knowledge.insert(
        "relations_limit".into(),
        key(
            "usize",
            serde_json::json!(mem.knowledge.relations_limit),
            "Maximum number of relations returned",
        ),
    );
    sections.insert(
        "memory.knowledge".into(),
        SectionSchema {
            description: "Knowledge memory budgets (facts, patterns, gotchas)".into(),
            keys: mem_knowledge,
        },
    );

    let mut mem_episodic = BTreeMap::new();
    mem_episodic.insert(
        "max_episodes".into(),
        key(
            "usize",
            serde_json::json!(mem.episodic.max_episodes),
            "Maximum number of episodes retained",
        ),
    );
    mem_episodic.insert(
        "max_actions_per_episode".into(),
        key(
            "usize",
            serde_json::json!(mem.episodic.max_actions_per_episode),
            "Maximum actions tracked per episode",
        ),
    );
    mem_episodic.insert(
        "summary_max_chars".into(),
        key(
            "usize",
            serde_json::json!(mem.episodic.summary_max_chars),
            "Maximum characters in episode summary",
        ),
    );
    sections.insert(
        "memory.episodic".into(),
        SectionSchema {
            description: "Episodic memory budgets (session episodes)".into(),
            keys: mem_episodic,
        },
    );

    let mut mem_procedural = BTreeMap::new();
    mem_procedural.insert(
        "max_procedures".into(),
        key(
            "usize",
            serde_json::json!(mem.procedural.max_procedures),
            "Maximum number of learned procedures stored",
        ),
    );
    mem_procedural.insert(
        "min_repetitions".into(),
        key(
            "usize",
            serde_json::json!(mem.procedural.min_repetitions),
            "Minimum repetitions before a pattern is stored",
        ),
    );
    mem_procedural.insert(
        "min_sequence_len".into(),
        key(
            "usize",
            serde_json::json!(mem.procedural.min_sequence_len),
            "Minimum sequence length for procedure detection",
        ),
    );
    mem_procedural.insert(
        "max_window_size".into(),
        key(
            "usize",
            serde_json::json!(mem.procedural.max_window_size),
            "Maximum window size for pattern analysis",
        ),
    );
    sections.insert(
        "memory.procedural".into(),
        SectionSchema {
            description: "Procedural memory budgets (learned patterns)".into(),
            keys: mem_procedural,
        },
    );

    let mut mem_lifecycle = BTreeMap::new();
    mem_lifecycle.insert(
        "decay_rate".into(),
        key(
            "f32",
            clean_f32(mem.lifecycle.decay_rate),
            "Rate at which knowledge confidence decays over time",
        ),
    );
    mem_lifecycle.insert(
        "low_confidence_threshold".into(),
        key(
            "f32",
            clean_f32(mem.lifecycle.low_confidence_threshold),
            "Threshold below which facts are considered low-confidence",
        ),
    );
    mem_lifecycle.insert(
        "stale_days".into(),
        key(
            "i64",
            serde_json::json!(mem.lifecycle.stale_days),
            "Days after which unused facts are considered stale",
        ),
    );
    mem_lifecycle.insert(
        "similarity_threshold".into(),
        key(
            "f32",
            clean_f32(mem.lifecycle.similarity_threshold),
            "Similarity threshold for deduplication",
        ),
    );
    mem_lifecycle.insert(
        "forgetting_model".into(),
        key(
            "string",
            serde_json::json!(mem.lifecycle.forgetting_model),
            "Forgetting curve: ebbinghaus (default, exponential + spacing) or linear",
        ),
    );
    mem_lifecycle.insert(
        "base_stability_days".into(),
        key(
            "f32",
            clean_f32(mem.lifecycle.base_stability_days),
            "Characteristic memory stability (days) for the Ebbinghaus curve",
        ),
    );
    mem_lifecycle.insert(
        "archetype_aware_decay".into(),
        key(
            "bool",
            serde_json::json!(mem.lifecycle.archetype_aware_decay),
            "Scale Ebbinghaus stability by fact archetype so structural evidence decays slower than inference (default false)",
        ),
    );
    mem_lifecycle.insert(
        "reclaim_headroom_pct".into(),
        key_with_env(
            "f32",
            clean_f32(mem.lifecycle.reclaim_headroom_pct),
            "Proactive headroom on a capacity reclaim: settle a full store at 1 - this fraction (0.25 = 75%) instead of churning at the cap. Lossless — the reclaimed tail is archived and restorable",
            "LEAN_CTX_LIFECYCLE_RECLAIM_HEADROOM_PCT",
        ),
    );
    mem_lifecycle.insert(
        "reclaim_enabled".into(),
        key_with_env(
            "bool",
            serde_json::json!(mem.lifecycle.reclaim_enabled),
            "Master switch for the proactive capacity reclaim (#995). false trims only the overflow (escape hatch, no headroom); eviction stays lossless either way",
            "LEAN_CTX_LIFECYCLE_RECLAIM_ENABLED",
        ),
    );
    sections.insert(
        "memory.lifecycle".into(),
        SectionSchema {
            description: "Knowledge lifecycle policy (decay, staleness, dedup)".into(),
            keys: mem_lifecycle,
        },
    );

    let mut mem_gotcha = BTreeMap::new();
    mem_gotcha.insert(
        "max_gotchas_per_project".into(),
        key(
            "usize",
            serde_json::json!(mem.gotcha.max_gotchas_per_project),
            "Maximum gotchas stored per project",
        ),
    );
    mem_gotcha.insert(
        "retrieval_budget_per_room".into(),
        key(
            "usize",
            serde_json::json!(mem.gotcha.retrieval_budget_per_room),
            "Maximum gotchas retrieved per room per query",
        ),
    );
    mem_gotcha.insert(
        "default_decay_rate".into(),
        key(
            "f32",
            clean_f32(mem.gotcha.default_decay_rate),
            "Default decay rate for gotcha importance",
        ),
    );
    sections.insert(
        "memory.gotcha".into(),
        SectionSchema {
            description: "Gotcha memory settings (project-specific warnings and pitfalls)".into(),
            keys: mem_gotcha,
        },
    );

    let mut mem_embeddings = BTreeMap::new();
    mem_embeddings.insert(
        "max_facts".into(),
        key(
            "usize",
            serde_json::json!(mem.embeddings.max_facts),
            "Maximum number of embedding facts stored",
        ),
    );
    sections.insert(
        "memory.embeddings".into(),
        SectionSchema {
            description: "Embeddings memory settings for semantic search".into(),
            keys: mem_embeddings,
        },
    );

    let mut aliases = BTreeMap::new();
    aliases.insert(
        "command".into(),
        key(
            "string",
            serde_json::json!(""),
            "The command pattern to match (e.g. 'deploy')",
        ),
    );
    aliases.insert(
        "alias".into(),
        key(
            "string",
            serde_json::json!(""),
            "The alias definition to execute",
        ),
    );
    sections.insert("custom_aliases".into(), SectionSchema {
            description: "Custom command aliases (array of {command, alias} entries). Note: field names are 'command' and 'alias' (not 'name')".into(),
            keys: aliases,
        });

    if let Some(root_section) = sections.get_mut("root") {
        root_section.keys.insert(
            "custom_aliases".into(),
            key(
                "array",
                serde_json::json!([]),
                "Custom command aliases (array of {command, alias} entries)",
            ),
        );
    }

    let mut setup_keys = BTreeMap::new();
    setup_keys.insert(
            "auto_inject_rules".into(),
            key(
                "bool?",
                serde_json::json!(null),
                "Inject agent rule files during setup/update. null=auto (inject if already present), true=always, false=never",
            ),
        );
    setup_keys.insert(
            "auto_inject_skills".into(),
            key(
                "bool?",
                serde_json::json!(null),
                "Install SKILL.md files during setup/update. null=auto (install if rules present), true=always, false=never",
            ),
        );
    setup_keys.insert(
        "auto_update_mcp".into(),
        key(
            "bool",
            serde_json::json!(true),
            "Register lean-ctx MCP server in editor configs during setup/update",
        ),
    );
    sections.insert(
            "setup".into(),
            SectionSchema {
                description: "Controls what lean-ctx injects during setup and updates. Fresh installs default to non-invasive (rules/skills off, MCP on).".into(),
                keys: setup_keys,
            },
        );

    let mut llm_keys = BTreeMap::new();
    llm_keys.insert(
        "enabled".into(),
        key(
            "bool",
            serde_json::json!(false),
            "Enable optional LLM enhancements (query expansion, contradiction explanation)",
        ),
    );
    llm_keys.insert(
        "backend".into(),
        key_enum(
            &["ollama", "openrouter", "anthropic"],
            "ollama",
            "LLM backend provider",
        ),
    );
    llm_keys.insert(
        "model".into(),
        key(
            "string",
            serde_json::json!("llama3.2"),
            "Model name for the selected backend",
        ),
    );
    llm_keys.insert(
        "api_key".into(),
        key(
            "string",
            serde_json::json!(""),
            "API key for OpenRouter or Anthropic backends",
        ),
    );
    llm_keys.insert(
        "timeout_secs".into(),
        key(
            "u64",
            serde_json::json!(10),
            "HTTP timeout for LLM requests",
        ),
    );
    sections.insert("llm".into(), SectionSchema {
            description: "Optional LLM enhancement settings (query expansion, contradiction explanation). Deterministic fallback when disabled or unreachable.".into(),
            keys: llm_keys,
        });
}
