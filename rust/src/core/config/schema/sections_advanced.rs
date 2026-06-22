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
            "Opt-in big-gap cold-prefix repack (#480): on a session-resume request the proxy may predict (from idle time vs the provider cache TTL) that the client-cached prefix has already expired, then prune that now-cold prefix once to re-seed a leaner cache. A wrong guess re-bills cache reads as writes (~12x), so default false",
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
