pub mod ctx_agent;
pub mod ctx_analyze;
pub mod ctx_architecture;
pub mod ctx_artifacts;
pub mod ctx_benchmark;
pub mod ctx_cache;
pub mod ctx_callgraph;
pub mod ctx_compile;
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
pub mod ctx_feedback;
pub mod ctx_fill;
pub mod ctx_gain;
pub mod ctx_graph;
pub mod ctx_handoff;
pub mod ctx_heatmap;
pub mod ctx_impact;
pub mod ctx_index;
pub mod ctx_intent;
pub mod ctx_knowledge;
pub mod ctx_metrics;
pub mod ctx_multi_read;
pub mod ctx_outline;
pub mod ctx_overview;
pub mod ctx_pack;
pub mod ctx_plan;
pub mod ctx_prefetch;
pub mod ctx_preload;
pub mod ctx_proof;
pub mod ctx_provider;
pub mod ctx_radar;
pub mod ctx_read;
pub mod ctx_refactor;
pub mod ctx_response;
pub mod ctx_retrieve;
pub mod ctx_review;
pub mod ctx_routes;
pub mod ctx_search;
pub mod ctx_semantic_search;
pub mod ctx_session;
pub mod ctx_share;
pub mod ctx_shell;
pub mod ctx_smart_read;
pub mod ctx_smells;
pub mod ctx_symbol;
pub mod ctx_task;
pub mod ctx_tree;
pub mod ctx_verify;
pub mod ctx_workflow;

/// Resolve a relative path against session state (sync version).
/// Replicates the core logic of `LeanCtxServer::resolve_path` without
/// the re-rooting fallback (which needs `startup_project_root`).
/// Must be called within `tokio::task::block_in_place`.
pub(crate) fn resolve_path_sync(
    session: &crate::core::session::SessionState,
    raw: &str,
) -> Result<String, String> {
    let normalized = crate::core::pathutil::normalize_tool_path(raw);
    if normalized.is_empty() || normalized == "." {
        return Ok(normalized);
    }
    let p = std::path::Path::new(&normalized);

    let jail_root = session
        .project_root
        .as_deref()
        .or(session.shell_cwd.as_deref())
        .unwrap_or(".")
        .to_string();

    let resolved = if p.is_absolute() || p.exists() {
        std::path::PathBuf::from(&normalized)
    } else if let Some(ref root) = session.project_root {
        let joined = std::path::Path::new(root).join(&normalized);
        if joined.exists() {
            joined
        } else if let Some(ref cwd) = session.shell_cwd {
            std::path::Path::new(cwd).join(&normalized)
        } else {
            std::path::Path::new(&jail_root).join(&normalized)
        }
    } else if let Some(ref cwd) = session.shell_cwd {
        std::path::Path::new(cwd).join(&normalized)
    } else {
        std::path::Path::new(&jail_root).join(&normalized)
    };

    let jail_root_path = std::path::Path::new(&jail_root);
    let jailed = crate::core::pathjail::jail_path(&resolved, jail_root_path)?;
    crate::core::io_boundary::check_secret_path_for_tool("resolve_path", &jailed)?;

    Ok(crate::core::pathutil::normalize_tool_path(
        &jailed.to_string_lossy().replace('\\', "/"),
    ))
}
