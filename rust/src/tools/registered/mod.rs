pub mod ctx_agent;
pub mod ctx_analyze;
pub mod ctx_architecture;
pub mod ctx_artifacts;
pub mod ctx_benchmark;
pub mod ctx_cache;
pub mod ctx_call;
pub mod ctx_callgraph;
pub mod ctx_checkpoint;
pub mod ctx_compare;
pub mod ctx_compile;
pub mod ctx_compose;
pub mod ctx_compress;
pub mod ctx_compress_memory;
pub mod ctx_context;
pub mod ctx_control;
pub mod ctx_cost;
pub mod ctx_dedup;
pub mod ctx_delta;
pub mod ctx_discover;
pub mod ctx_discover_tools;
pub mod ctx_edit;
pub mod ctx_execute;
pub mod ctx_expand;
pub mod ctx_explore;
pub mod ctx_feedback;
pub mod ctx_fill;
pub mod ctx_gain;
pub mod ctx_git_read;
pub mod ctx_glob;
pub mod ctx_graph;
pub mod ctx_handoff;
pub mod ctx_heatmap;
pub mod ctx_impact;
pub mod ctx_index;
pub mod ctx_intent;
pub mod ctx_knowledge;
pub mod ctx_ledger;
pub mod ctx_load_tools;
pub mod ctx_metrics;
pub mod ctx_multi_read;
pub mod ctx_multi_repo;
pub mod ctx_outline;
pub mod ctx_overview;
pub mod ctx_pack;
pub mod ctx_package;
pub mod ctx_patch;
pub mod ctx_plan;
pub mod ctx_plugins;
pub mod ctx_prefetch;
pub mod ctx_preload;
pub mod ctx_proof;
pub mod ctx_provider;
pub mod ctx_quality;
pub mod ctx_radar;
pub mod ctx_read;
pub mod ctx_refactor;
pub mod ctx_repomap;
pub mod ctx_response;
pub mod ctx_retrieve;
pub mod ctx_review;
pub mod ctx_routes;
pub mod ctx_rules;
pub mod ctx_search;
pub mod ctx_semantic_search;
pub mod ctx_session;
pub mod ctx_share;
pub mod ctx_shell;
pub mod ctx_skillify;
pub mod ctx_smart_read;
pub mod ctx_smells;
pub mod ctx_summary;
pub mod ctx_symbol;
pub mod ctx_task;
pub mod ctx_tools;
pub mod ctx_transcript_compact;
pub mod ctx_tree;
pub mod ctx_url_read;
pub mod ctx_verify;
pub mod ctx_workflow;
pub mod plugin_tool;
pub mod shell_alias;

/// Resolve a relative path against session state (sync version).
/// Must be called within `tokio::task::block_in_place`.
pub(crate) fn resolve_path_sync(
    session: &crate::core::session::SessionState,
    raw: &str,
) -> Result<String, String> {
    crate::core::path_resolve::resolve_tool_path_with_roots(
        session.project_root.as_deref(),
        session.shell_cwd.as_deref(),
        raw,
        &session.extra_roots,
    )
}

// #168 removed: tool descriptions no longer steer toward ctx_* over native.
// Replacement guidance lives in AGENTS.md / CLAUDE.md rules files.
