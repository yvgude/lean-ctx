use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{
    McpTool, ToolContext, ToolOutput, get_bool, get_int, get_str, get_str_array, get_usize,
};
use crate::tool_defs::tool_def;

pub struct CtxSemanticSearchTool;

impl McpTool for CtxSemanticSearchTool {
    fn name(&self) -> &'static str {
        "ctx_semantic_search"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_semantic_search",
            "Concept/semantic code search (hybrid BM25+embeddings). Use when keyword ctx_search misses intent.",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Natural-language or symbol query" },
                    "path": { "type": "string", "description": "Project root (default: .)" },
                    "top_k": { "type": "integer", "description": "Result count (default 10)" },
                    "action": {
                        "type": "string",
                        "enum": ["search", "reindex", "find_related"],
                        "description": "search (default)|reindex|find_related"
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["bm25", "dense", "hybrid"],
                        "description": "bm25|dense|hybrid (default hybrid)"
                    },
                    "file_path": { "type": "string", "description": "find_related: source file (rel)" },
                    "line": { "type": "integer", "description": "find_related: line number" },
                    "languages": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Restrict to languages/exts"
                    },
                    "path_glob": { "type": "string", "description": "Glob over rel paths" }
                },
                "required": ["query"]
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let query = get_str(args, "query")
            .ok_or_else(|| ErrorData::invalid_params("query is required", None))?;
        let path = if let Some(p) = ctx.resolved_path("path") {
            p.to_string()
        } else if let Some(err) = ctx.path_error("path") {
            return Err(ErrorData::invalid_params(format!("path: {err}"), None));
        } else {
            ctx.project_root.clone()
        };
        let top_k = get_usize(args, "top_k").unwrap_or(10).min(1000);
        let action = get_str(args, "action").unwrap_or_default();
        let mode = get_str(args, "mode");
        let languages = get_str_array(args, "languages");
        let path_glob = get_str(args, "path_glob");
        let workspace = get_bool(args, "workspace").unwrap_or(false);
        let artifacts = get_bool(args, "artifacts").unwrap_or(false);

        #[cfg(feature = "qdrant")]
        {
            let mode_effective = mode
                .as_deref()
                .unwrap_or("hybrid")
                .trim()
                .to_ascii_lowercase();
            if action != "reindex"
                && !artifacts
                && matches!(mode_effective.as_str(), "dense" | "hybrid")
                && matches!(
                    crate::core::dense_backend::DenseBackendKind::try_from_env(),
                    Ok(crate::core::dense_backend::DenseBackendKind::Qdrant)
                )
                && let Some(ref session_lock) = ctx.session
            {
                let value =
                    format!("tool=ctx_semantic_search mode={mode_effective} workspace={workspace}");
                let mut session = tokio::task::block_in_place(|| session_lock.blocking_write());
                session.record_manual_evidence("remote:qdrant_query", Some(&value));
            }
        }

        let file_path_param = get_str(args, "file_path");
        let line_param = get_int(args, "line");

        if let Some(ref cache) = ctx.bm25_cache {
            crate::tools::ctx_semantic_search::set_thread_cache(cache.clone());
        }

        if let Some(ref ps) = ctx.progress_sender
            && let Some(sender) = ps
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
        {
            sender.send(0.0, Some(1.0), Some("Starting search...".to_string()));
        }

        let result = if action == "reindex" {
            if let Some(ref ps) = ctx.progress_sender
                && let Some(sender) = ps
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .as_ref()
            {
                sender.send(0.0, Some(1.0), Some("Rebuilding BM25 index...".to_string()));
            }
            if artifacts {
                crate::tools::ctx_semantic_search::handle_reindex_artifacts(&path, workspace)
            } else {
                crate::tools::ctx_semantic_search::handle_reindex(&path)
            }
        } else if action == "find_related" {
            let fp = file_path_param.unwrap_or_default();
            let line = line_param.unwrap_or(1) as usize;
            if fp.is_empty() {
                return Err(ErrorData::invalid_params(
                    "find_related requires file_path and line parameters",
                    None,
                ));
            }
            crate::tools::ctx_semantic_search::handle_find_related(
                &fp,
                line,
                &path,
                top_k,
                ctx.crp_mode,
            )
        } else {
            crate::tools::ctx_semantic_search::handle(
                &query,
                &path,
                top_k,
                ctx.crp_mode,
                languages.as_deref(),
                path_glob.as_deref(),
                mode.as_deref(),
                Some(workspace),
                Some(artifacts),
            )
        };

        if let Some(ref ps) = ctx.progress_sender
            && let Some(sender) = ps
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
        {
            sender.send(1.0, Some(1.0), Some("Search complete".to_string()));
        }

        let repeat_hint = if action == "reindex" {
            String::new()
        } else if let Some(ref autonomy) = ctx.autonomy {
            autonomy
                .track_search(&query, &path)
                .map(|h| format!("\n{h}"))
                .unwrap_or_default()
        } else {
            String::new()
        };

        Ok(ToolOutput {
            text: format!("{result}{repeat_hint}"),
            original_tokens: 0,
            saved_tokens: 0,
            mode: Some("semantic".to_string()),
            path: None,
            changed: false,
            shell_outcome: None,
        })
    }
}
