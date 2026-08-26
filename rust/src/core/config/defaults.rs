use std::collections::HashMap;

#[allow(clippy::wildcard_imports)]
use super::*;
/// Default BM25 cache cap from config (also used by `bm25_index` heuristics).
pub fn default_bm25_max_cache_mb() -> u64 {
    serde_defaults::default_bm25_max_cache_mb()
}

/// Effective on-disk ceiling (MB) for the persisted BM25 index when nothing is
/// explicitly configured (no `bm25_max_cache_mb`, no `max_disk_mb` budget).
///
/// Deliberately decoupled from the RAM `MemoryProfile` (64/128/512 MB): this is
/// a *disk* file, and tying it to the profile silently refused persistence on
/// large repos under Low/Balanced, forcing a cold rebuild on every call (the
/// perpetual "index warming" of issue #249). 512 MB compressed covers
/// essentially every real repo; RAM pressure is governed separately by the
/// eviction orchestrator (which measures real heap).
pub const DEFAULT_BM25_PERSIST_MB: u64 = 512;

// Compile-time regression guard (#249): the default disk ceiling must stay well
// above the old RAM-profile caps (64/128 MB) that starved large repos.
const _: () = assert!(DEFAULT_BM25_PERSIST_MB >= 512);

/// lean-ctx tools whose sole purpose is editing the user's source files. When
/// `prefer_native_editor` is set (#454) these are hidden from `list_tools` and
/// refused at dispatch so the host's native editor handles edits instead.
///
/// Deliberately narrow: only the dedicated edit tools are blocked — `ctx_edit`
/// (str_replace) and `ctx_patch` (anchored, #1008). LSP refactor
/// (`ctx_refactor`) also exposes read-only sub-actions (references/definition),
/// so it is left available; users wanting it gone can add it to `disabled_tools`.
pub const EDIT_TOOL_NAMES: &[&str] = &["ctx_edit", "ctx_patch"];

/// Default locations for shell output capture.
///
/// `/private/tmp` is the canonical target behind macOS's `/tmp` symlink, while
/// `temp_dir()` also covers per-user scratch directories such as `$TMPDIR`.
pub(crate) fn default_shell_write_allow_paths() -> Vec<String> {
    #[allow(unused_mut)]
    let mut paths = vec![std::env::temp_dir().to_string_lossy().into_owned()];
    // Always include Unix scratch paths — agents generate `/tmp` redirects
    // even on Windows (Git Bash, WSL, cross-platform scripts).  (#1467)
    for path in ["/tmp", "/private/tmp", "/var/tmp"] {
        if !paths.iter().any(|existing| existing == path) {
            paths.push(path.to_string());
        }
    }
    paths
}
impl Default for Config {
    fn default() -> Self {
        Self {
            ultra_compact: false,
            tee_mode: TeeMode::default(),
            recovery_hints: RecoveryHints::default(),
            output_density: OutputDensity::default(),
            checkpoint_interval: 15,
            excluded_commands: Vec::new(),
            passthrough_urls: Vec::new(),
            custom_aliases: Vec::new(),
            preserve_compact_formats: serde_defaults::default_preserve_compact_formats(),
            crush_verbatim_json: false,
            slow_command_threshold_ms: 5000,
            theme: serde_defaults::default_theme(),
            telemetry: TelemetryConfig::default(),
            cloud: CloudConfig::default(),
            gain: GainConfig::default(),
            cost: CostConfig::default(),
            decision_loop: DecisionLoopConfig::default(),
            code_health: CodeHealthConfig::default(),
            autonomy: AutonomyConfig::default(),
            providers: ProvidersConfig::default(),
            knowledge_routing: KnowledgeRoutingConfig::default(),
            proxy: ProxyConfig::default(),
            conversation: ConversationConfig::default(),
            response_shaping: ResponseShapingConfig::default(),
            ocla: OclaConfig::default(),
            shadow: ShadowConfig::default(),
            cache: CacheConfig::default(),
            agents: AgentsConfig::default(),
            proxy_enabled: None,
            proxy_port: None,
            proxy_timeout_ms: None,
            proxy_require_token: false,
            proxy_loopback_open: false,
            proxy_bind_host: None,
            proxy_allowed_hosts: Vec::new(),
            proxy_max_rps: None,
            dashboard_auth: true,
            dashboard_cache_hit_rate: None,
            buddy_enabled: serde_defaults::default_buddy_enabled(),
            enable_wakeup_ctx: true,
            redirect_exclude: Vec::new(),
            disabled_tools: Vec::new(),
            prefer_native_editor: false,
            default_tool_categories: Vec::new(),
            no_degrade: false,
            delta_explicit: false,
            profile: None,
            config_profile: None,
            profiles: std::collections::BTreeMap::new(),
            tool_profile: None,
            tools_enabled: Vec::new(),
            persona: None,
            loop_detection: LoopDetectionConfig::default(),
            rules_scope: None,
            rules_injection: None,
            permission_inheritance: Some("on".to_string()),
            extra_ignore_patterns: Vec::new(),
            terse_agent: TerseAgent::default(),
            compression_level: CompressionLevel::default(),
            cognitive_mode: CognitiveMode::default(),
            compression_aggressiveness: None,
            archive: ArchiveConfig::default(),
            memory: MemoryPolicy::default(),
            allow_paths: Vec::new(),
            allow_ide_config_dirs: None,
            extra_roots: Vec::new(),
            read_only_roots: Vec::new(),
            allow_symlink_roots: Vec::new(),
            content_defined_chunking: false,
            minimal_overhead: true,
            symbol_map_auto: false,
            structure_first: true,
            progressive_disclosure: true,
            progressive_threshold_lines: serde_defaults::default_progressive_threshold_lines(),
            progressive_signatures_max: serde_defaults::default_progressive_signatures_max(),
            auto_mode_learning: false,
            team_url: None,
            team_token: None,
            team_auto_push: false,
            journal_enabled: true,
            auto_capture: true,
            search: crate::core::hybrid_search::HybridConfig::default(),
            graph: GraphConfig::default(),
            index: IndexConfig::default(),
            skillify: SkillifyConfig::default(),
            summaries: SummariesConfig::default(),
            llm: crate::core::llm_enhance::LlmConfig::default(),
            embedding: EmbeddingConfig::default(),
            shell_hook_disabled: false,
            shadow_mode: true,
            shell_hook_mode: None,
            prompt_reinject: None,
            hook_mode: None,
            tool_surface: None,
            debug_log: false,
            shell_activation: ShellActivation::default(),
            skip_agent_aliases: false,
            read_redirect: ReadRedirect::default(),
            read_dedup: ReadDedup::default(),
            update_check_disabled: false,
            updates: UpdatesConfig::default(),
            context: ContextConfig::default(),
            graph_index_max_files: serde_defaults::default_graph_index_max_files(),
            bm25_max_cache_mb: serde_defaults::default_bm25_max_cache_mb(),
            memory_profile: MemoryProfile::default(),
            memory_cleanup: MemoryCleanup::default(),
            max_ram_percent: serde_defaults::default_max_ram_percent(),
            max_disk_mb: 0,
            max_staleness_days: 0,
            max_index_threads: 0,
            savings_footer: SavingsFooter::default(),
            compression_annotation: CompressionAnnotation::default(),
            annotation_threshold_pct: serde_defaults::default_annotation_threshold_pct(),
            behavior_nudges: serde_defaults::default_behavior_nudges(),
            turn_fresh_limit: serde_defaults::default_turn_fresh_limit(),
            session_token_limit: serde_defaults::default_session_token_limit(),
            project_root: None,
            lsp: std::collections::HashMap::new(),
            ide_paths: HashMap::new(),
            model_context_windows: HashMap::new(),
            response_verbosity: ResponseVerbosity::default(),
            active_kit: None,
            bypass_hints: None,
            cache_policy: None,
            cache_max_tokens: 0,
            boundary_policy: crate::core::memory_boundary::BoundaryPolicy::default(),
            secret_detection: SecretDetectionConfig::default(),
            sensitivity: crate::core::sensitivity::SensitivityConfig::default(),
            gateway: crate::core::mcp_catalog::GatewayConfig::default(),
            gateway_server: GatewayServerConfig::default(),
            enterprise: EnterpriseConfig::default(),
            allow_auto_reroot: true,
            hook_binary: None,
            path_jail: None,
            sandbox_level: 0,
            reference_results: false,
            agent_token_budget: 0,
            shell_allowlist: default_shell_allowlist(),
            shell_allowlist_extra: Vec::new(),
            shell_strict_mode: false,
            shell_security: None,
            shell_timeout_secs: None,
            shell_heavy_timeout_secs: None,
            shell_heavy_prefixes: Vec::new(),
            shell_allow_writes: false,
            write_allow_paths: Vec::new(),
            shell_allow_inline_scripts: false,
            setup: SetupConfig::default(),
            solution: super::solution::SolutionConfig::default(),
            provenance: super::provenance::ProvenanceConfig::default(),
            cross_agent: super::provenance::CrossAgentConfig::default(),
        }
    }
}
