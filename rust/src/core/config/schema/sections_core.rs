//! Core config sections (root, ide_paths, lsp, archive, search, embedding).
//! Split out of `schema/mod.rs`; `use super::*` re-imports the key-builder
//! helpers and `SectionSchema`.

#[allow(clippy::wildcard_imports)]
use super::*;
use std::collections::BTreeMap;

pub(super) fn build(sections: &mut BTreeMap<String, SectionSchema>) {
    let cfg = crate::core::config::Config::default();
    let mut root = BTreeMap::new();
    root.insert(
        "ultra_compact".into(),
        key(
            "bool",
            serde_json::json!(false),
            "Legacy flag for maximum compression (use compression_level instead)",
        ),
    );
    root.insert(
        "tee_mode".into(),
        key_enum(
            &["never", "failures", "always"],
            "failures",
            "Controls when shell output is tee'd to disk for later retrieval",
        ),
    );
    root.insert(
        "output_density".into(),
        key_enum_with_env(
            &["normal", "terse", "ultra"],
            "normal",
            "Controls how dense/compact MCP tool output is formatted",
            "LEAN_CTX_OUTPUT_DENSITY",
        ),
    );
    root.insert(
        "checkpoint_interval".into(),
        key(
            "u32",
            serde_json::json!(cfg.checkpoint_interval),
            "Session checkpoint interval in minutes",
        ),
    );
    root.insert(
        "excluded_commands".into(),
        key(
            "string[]",
            serde_json::json!(cfg.excluded_commands),
            "Commands to exclude from shell hook interception",
        ),
    );
    root.insert(
        "passthrough_urls".into(),
        key(
            "string[]",
            serde_json::json!(cfg.passthrough_urls),
            "URLs to pass through without proxy interception",
        ),
    );
    root.insert(
            "preserve_compact_formats".into(),
            key(
                "string[]",
                serde_json::json!(cfg.preserve_compact_formats),
                "Already-compact output formats preserved verbatim instead of recompressed (e.g. [\"toon\"]). Set to [] to disable",
            ),
        );
    root.insert("slow_command_threshold_ms".into(), key("u64", serde_json::json!(cfg.slow_command_threshold_ms), "Commands taking longer than this (ms) are recorded in the slow log. Set to 0 to disable"));
    root.insert(
        "theme".into(),
        key(
            "string",
            serde_json::json!(cfg.theme),
            "Dashboard color theme",
        ),
    );
    root.insert(
        "buddy_enabled".into(),
        key(
            "bool",
            serde_json::json!(cfg.buddy_enabled),
            "Enable the buddy system for multi-agent coordination",
        ),
    );
    root.insert(
            "enable_wakeup_ctx".into(),
            key(
                "bool",
                serde_json::json!(cfg.enable_wakeup_ctx),
                "Append wakeup briefing (facts, session summary) to ctx_overview output. Set false to reduce context bloat when calling ctx_overview frequently.",
            ),
        );
    root.insert(
        "redirect_exclude".into(),
        key(
            "string[]",
            serde_json::json!(cfg.redirect_exclude),
            "URL patterns to exclude from proxy redirection",
        ),
    );
    root.insert(
        "disabled_tools".into(),
        key(
            "string[]",
            serde_json::json!(cfg.disabled_tools),
            "Tools to exclude from the MCP tool list",
        ),
    );
    root.insert(
            "default_tool_categories".into(),
            key(
                "string[]",
                serde_json::json!(cfg.default_tool_categories),
                "Tool categories active by default (core, arch, debug, memory, metrics, session). Override via LCTX_DEFAULT_CATEGORIES",
            ),
        );
    root.insert(
        "no_degrade".into(),
        key(
            "boolean",
            serde_json::json!(cfg.no_degrade),
            "Disable all automatic read-mode degradation. Override via LCTX_NO_DEGRADE=1",
        ),
    );
    root.insert(
            "profile".into(),
            key(
                "string",
                serde_json::json!(cfg.profile.as_deref().unwrap_or("")),
                "Persistent profile name. Checked after LEAN_CTX_PROFILE env var. Set via: lean-ctx config set profile passthrough",
            ),
        );
    root.insert(
            "tool_profile".into(),
            key_enum(
                &["minimal", "standard", "power"],
                cfg.tool_profile.as_deref().unwrap_or(""),
                "Tool visibility profile: minimal (6 tools), standard (21), power (all). Override via LEAN_CTX_TOOL_PROFILE",
            ),
        );
    root.insert(
        "tools_enabled".into(),
        key(
            "string[]",
            serde_json::json!(cfg.tools_enabled),
            "Explicit list of enabled tool names (overrides tool_profile when non-empty)",
        ),
    );
    root.insert(
        "rules_scope".into(),
        key_enum(
            &["both", "global", "project"],
            "both",
            "Where agent rule files are installed. Override via LEAN_CTX_RULES_SCOPE",
        ),
    );
    root.insert(
        "extra_ignore_patterns".into(),
        key(
            "string[]",
            serde_json::json!(cfg.extra_ignore_patterns),
            "Extra glob patterns to ignore in graph/overview/preload",
        ),
    );
    root.insert(
        "terse_agent".into(),
        key_enum_with_env(
            &["off", "lite", "full", "ultra"],
            "off",
            "Controls agent output verbosity via instructions injection",
            "LEAN_CTX_TERSE_AGENT",
        ),
    );
    root.insert(
            "compression_level".into(),
            key_enum_with_env(
                &["off", "lite", "standard", "max"],
                "lite",
                "Unified output-style level for the model's prose (not tool-output compression). lite=plain concise (default), standard/max=denser symbolic 'power modes'",
                "LEAN_CTX_COMPRESSION",
            ),
        );
    root.insert(
        "allow_paths".into(),
        key_with_env(
            "string[]",
            serde_json::json!(cfg.allow_paths),
            "Additional paths allowed by PathJail (absolute)",
            "LEAN_CTX_ALLOW_PATH",
        ),
    );
    root.insert(
        "extra_roots".into(),
        key_with_env(
            "string[]",
            serde_json::json!(cfg.extra_roots),
            "Extra project roots for multi-root workspaces (auto-added to PathJail allow-list)",
            "LEAN_CTX_EXTRA_ROOTS",
        ),
    );
    root.insert(
        "content_defined_chunking".into(),
        key(
            "bool",
            serde_json::json!(false),
            "Enable Rabin-Karp chunking for cache-optimal output ordering",
        ),
    );
    root.insert(
        "minimal_overhead".into(),
        key_with_env(
            "bool",
            serde_json::json!(true),
            "Skip session/knowledge/gotcha blocks in MCP instructions",
            "LEAN_CTX_MINIMAL",
        ),
    );
    root.insert(
        "symbol_map_auto".into(),
        key(
            "bool",
            serde_json::json!(true),
            "Auto-enable SymbolMap for projects with >50 source files",
        ),
    );
    root.insert(
        "journal_enabled".into(),
        key(
            "bool",
            serde_json::json!(true),
            "Write human-readable activity journal to ~/.lean-ctx/journal.md",
        ),
    );
    root.insert(
        "auto_capture".into(),
        key(
            "bool",
            serde_json::json!(true),
            "Automatic knowledge capture from tool findings",
        ),
    );
    root.insert(
            "cache_policy".into(),
            key_with_env(
                "enum(aggressive|safe|off)",
                serde_json::json!("aggressive"),
                "Cache policy for ctx_read: aggressive (13-tok stubs), safe (map on hit), off (always disk)",
                "LEAN_CTX_CACHE_POLICY",
            ),
        );
    root.insert(
            "shadow_mode".into(),
            key_with_env(
                "bool",
                serde_json::json!(false),
                "Transparently intercept native Read/Grep/Shell calls via hooks and route them through lean-ctx",
                "LEAN_CTX_SHADOW_MODE",
            ),
        );
    root.insert(
        "shell_hook_disabled".into(),
        key_with_env(
            "bool",
            serde_json::json!(false),
            "Disable shell hook injection",
            "LEAN_CTX_NO_HOOK",
        ),
    );
    root.insert(
        "shell_activation".into(),
        key_enum_with_env(
            &["always", "agents-only", "off"],
            "always",
            "Controls when the shell hook auto-activates aliases",
            "LEAN_CTX_SHELL_ACTIVATION",
        ),
    );
    root.insert(
        "update_check_disabled".into(),
        key_with_env(
            "bool",
            serde_json::json!(false),
            "Disable the daily version check",
            "LEAN_CTX_NO_UPDATE_CHECK",
        ),
    );
    root.insert(
        "bm25_max_cache_mb".into(),
        key_with_env(
            "u64",
            serde_json::json!(cfg.bm25_max_cache_mb),
            "Maximum BM25 cache file size in MB",
            "LEAN_CTX_BM25_MAX_CACHE_MB",
        ),
    );
    root.insert(
            "graph_index_max_files".into(),
            key(
                "u64",
                serde_json::json!(cfg.graph_index_max_files),
                "Maximum files in graph index. 0 = unlimited (default). Set >0 to cap for constrained systems",
            ),
        );
    root.insert(
        "memory_profile".into(),
        key_enum_with_env(
            &["low", "balanced", "performance"],
            "performance",
            "Controls RAM vs feature trade-off (performance = max quality)",
            "LEAN_CTX_MEMORY_PROFILE",
        ),
    );
    root.insert(
        "memory_cleanup".into(),
        key_enum_with_env(
            &["aggressive", "shared"],
            "aggressive",
            "Controls how aggressively memory is freed when idle",
            "LEAN_CTX_MEMORY_CLEANUP",
        ),
    );
    root.insert(
            "savings_footer".into(),
            key_enum_with_env(
                &["auto", "always", "never"],
                "always",
                "Controls visibility of token savings footers: always (default, show on every response), never, auto (context-dependent). Also: LEAN_CTX_SHOW_SAVINGS=1|0",
                "LEAN_CTX_SAVINGS_FOOTER",
            ),
        );
    root.insert(
        "max_ram_percent".into(),
        key_with_env(
            "u8",
            serde_json::json!(cfg.max_ram_percent),
            "Maximum percentage of system RAM that lean-ctx may use (1-50, default 5)",
            "LEAN_CTX_MAX_RAM_PERCENT",
        ),
    );
    root.insert(
        "max_disk_mb".into(),
        key_with_env(
            "u64",
            serde_json::json!(cfg.max_disk_mb),
            "Simplified disk budget in MB (0 = disabled). Distributes: archive ~25%, BM25 ~10%",
            "LEAN_CTX_MAX_DISK_MB",
        ),
    );
    root.insert(
        "max_staleness_days".into(),
        key_with_env(
            "u32",
            serde_json::json!(cfg.max_staleness_days),
            "Auto-purge data older than N days (0 = disabled). Flows into archive.max_age_hours",
            "LEAN_CTX_MAX_STALENESS_DAYS",
        ),
    );
    root.insert(
        "project_root".into(),
        key_with_env(
            "string?",
            serde_json::json!(null),
            "Explicit project root directory. Prevents accidental home-directory scans",
            "LEAN_CTX_PROJECT_ROOT",
        ),
    );
    root.insert(
            "proxy_enabled".into(),
            key(
                "bool?",
                serde_json::json!(null),
                "Enable/disable the proxy layer. null = auto-detect, true = force on, false = force off",
            ),
        );
    root.insert(
            "proxy_port".into(),
            key(
                "u16?",
                serde_json::json!(null),
                "Custom proxy port (default: 4444). Useful for multi-user systems. Env: LEAN_CTX_PROXY_PORT",
            ),
        );
    root.insert(
            "proxy_timeout_ms".into(),
            key(
                "u64?",
                serde_json::json!(null),
                "Proxy reachability timeout in ms (default: 200). Override via LEAN_CTX_PROXY_TIMEOUT_MS",
            ),
        );
    root.insert(
        "response_verbosity".into(),
        key_enum_with_env(
            &["normal", "compact", "minimal"],
            "normal",
            "Controls how verbose tool responses are",
            "LEAN_CTX_RESPONSE_VERBOSITY",
        ),
    );
    root.insert(
        "allow_auto_reroot".into(),
        key_with_env(
            "bool",
            serde_json::json!(false),
            "Allow automatic project-root re-rooting when absolute paths outside the jail are seen",
            "LEAN_CTX_ALLOW_REROOT",
        ),
    );
    root.insert(
        "sandbox_level".into(),
        key_with_env(
            "u8",
            serde_json::json!(0),
            "Sandbox strictness level (0=default, 1=strict, 2=paranoid)",
            "LEAN_CTX_SANDBOX_LEVEL",
        ),
    );
    root.insert(
        "reference_results".into(),
        key_with_env(
            "bool",
            serde_json::json!(false),
            "Store large tool outputs as references instead of inline content",
            "LEAN_CTX_REFERENCE_RESULTS",
        ),
    );
    root.insert(
        "agent_token_budget".into(),
        key(
            "usize",
            serde_json::json!(0),
            "Default per-agent token budget. 0 = unlimited",
        ),
    );
    root.insert(
        "shell_allowlist".into(),
        key_with_env(
            "array",
            serde_json::json!([]),
            "Optional shell command allowlist. When non-empty, only listed binaries are permitted",
            "LEAN_CTX_SHELL_ALLOWLIST",
        ),
    );
    root.insert(
            "shell_allowlist_extra".into(),
            key(
                "array",
                serde_json::json!([]),
                "Commands merged on top of shell_allowlist without replacing the defaults. Managed via `lean-ctx allow <cmd>`",
            ),
        );
    root.insert(
        "shell_strict_mode".into(),
        key(
            "bool",
            serde_json::json!(false),
            "Block $(), backticks, <() in shell arguments. Default false = warn only.",
        ),
    );

    sections.insert(
        "root".into(),
        SectionSchema {
            description: "Top-level configuration keys".into(),
            keys: root,
        },
    );

    sections.insert(
            "ide_paths".into(),
            SectionSchema {
                description: "Per-IDE allowed paths. Keys are agent names (cursor, codex, opencode, antigravity, etc.), values are arrays of paths to index for that agent".into(),
                keys: BTreeMap::new(),
            },
        );

    let mut lsp_keys = BTreeMap::new();
    lsp_keys.insert(
        "rust".into(),
        key(
            "string?",
            serde_json::json!(null),
            "Custom path to rust-analyzer binary",
        ),
    );
    lsp_keys.insert(
        "typescript".into(),
        key(
            "string?",
            serde_json::json!(null),
            "Custom path to typescript-language-server binary",
        ),
    );
    lsp_keys.insert(
        "python".into(),
        key(
            "string?",
            serde_json::json!(null),
            "Custom path to pylsp binary",
        ),
    );
    lsp_keys.insert(
        "go".into(),
        key(
            "string?",
            serde_json::json!(null),
            "Custom path to gopls binary",
        ),
    );
    sections.insert(
        "lsp".into(),
        SectionSchema {
            description: "LSP server binary overrides. Map language name to custom binary path"
                .into(),
            keys: lsp_keys,
        },
    );

    let mut archive = BTreeMap::new();
    archive.insert(
        "enabled".into(),
        key(
            "bool",
            serde_json::json!(cfg.archive.enabled),
            "Enable zero-loss compression archive",
        ),
    );
    archive.insert(
        "threshold_chars".into(),
        key(
            "usize",
            serde_json::json!(cfg.archive.threshold_chars),
            "Minimum output size (chars) to trigger archiving",
        ),
    );
    archive.insert(
        "max_age_hours".into(),
        key(
            "u64",
            serde_json::json!(cfg.archive.max_age_hours),
            "Maximum age of archived entries before cleanup",
        ),
    );
    archive.insert(
        "max_disk_mb".into(),
        key(
            "u64",
            serde_json::json!(cfg.archive.max_disk_mb),
            "Maximum total disk usage for the archive",
        ),
    );
    archive.insert(
            "ephemeral".into(),
            key("bool", serde_json::json!(cfg.archive.ephemeral), "Replace large results with summary+ref (ctx_expand to retrieve). Env: LEAN_CTX_EPHEMERAL"),
        );
    sections.insert(
        "archive".into(),
        SectionSchema {
            description:
                "Settings for the zero-loss compression archive (large tool outputs saved to disk)"
                    .into(),
            keys: archive,
        },
    );

    let mut search = BTreeMap::new();
    search.insert(
        "bm25_weight".into(),
        key(
            "f64",
            serde_json::json!(cfg.search.bm25_weight),
            "BM25 lexical search weight in RRF fusion",
        ),
    );
    search.insert(
        "dense_weight".into(),
        key(
            "f64",
            serde_json::json!(cfg.search.dense_weight),
            "Dense vector search weight in RRF fusion",
        ),
    );
    search.insert(
        "bm25_candidates".into(),
        key(
            "usize",
            serde_json::json!(cfg.search.bm25_candidates),
            "Number of BM25 candidates to retrieve before fusion",
        ),
    );
    search.insert(
        "dense_candidates".into(),
        key(
            "usize",
            serde_json::json!(cfg.search.dense_candidates),
            "Number of dense candidates to retrieve before fusion",
        ),
    );
    search.insert(
        "splade_weight".into(),
        key(
            "f64",
            serde_json::json!(cfg.search.splade_weight),
            "SPLADE expansion weight (0.0 to disable)",
        ),
    );
    sections.insert("search".into(), SectionSchema {
            description: "Hybrid search weights for ctx_semantic_search (BM25 + dense vector + SPLADE + graph proximity)".into(),
            keys: search,
        });

    let mut embedding = BTreeMap::new();
    embedding.insert(
            "model".into(),
            key_with_env(
                "string",
                serde_json::json!("minilm"),
                "Local ONNX embedding model for ctx_semantic_search. One of: minilm (all-MiniLM-L6-v2, 384d, default), jina-code-v2 (768d, code-optimized), nomic (768d). Switching models re-indexes once on the next search.",
                "LEAN_CTX_EMBEDDING_MODEL",
            ),
        );
    sections.insert(
        "embedding".into(),
        SectionSchema {
            description:
                "Semantic-embedding engine settings (model selection for ctx_semantic_search)"
                    .into(),
            keys: embedding,
        },
    );
}
