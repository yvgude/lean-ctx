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
        let mut props = serde_json::Map::new();
        props.insert(
            "query".into(),
            json!({ "type": "string", "description": "Natural language or symbol query" }),
        );
        props.insert(
            "path".into(),
            json!({ "type": "string", "description": "Project root" }),
        );
        props.insert(
            "top_k".into(),
            json!({ "type": "integer", "description": "Max results (default: 10)" }),
        );
        props.insert(
            "action".into(),
            json!({
                "type": "string",
                "enum": ["search", "reindex", "find_related"]
            }),
        );

        let mode_values: Vec<Value> = {
            #[cfg(not(feature = "embeddings"))]
            let v: Vec<&str> = vec!["bm25"];
            #[cfg(feature = "embeddings")]
            let v: Vec<&str> = vec!["bm25", "dense", "hybrid"];
            v.into_iter()
                .map(|s| Value::String(s.to_string()))
                .collect()
        };
        props.insert("mode".into(), json!({
            "type": "string",
            "enum": mode_values,
            "description": "Search algorithm: bm25 (keyword, always available), dense (embedding vector), hybrid (both)"
        }));

        props.insert(
            "file_path".into(),
            json!({ "type": "string", "description": "Source file for find_related" }),
        );
        props.insert(
            "line".into(),
            json!({ "type": "integer", "description": "Line for find_related" }),
        );
        props.insert(
            "languages".into(),
            json!({
                "type": "array",
                "items": { "type": "string" },
                "description": "Restrict to extensions, e.g. ['rust','ts']"
            }),
        );
        props.insert(
            "path_glob".into(),
            json!({ "type": "string", "description": "Glob over relative file paths" }),
        );

        tool_def(
            "ctx_semantic_search",
            "Search code by MEANING (BM25) — use when you know the concept but not the exact\n\
             symbol name. query='user auth' finds relevant code even with no keyword match.\n\
             Different from ctx_search (regex): use ctx_search for exact patterns, this for\n\
             fuzzy/conceptual. For understanding code end-to-end, use ctx_compose FIRST.\n\
             find_related(file_path, line) for context neighbors.",
            json!({
                "type": "object",
                "properties": props,
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
                .unwrap_or(if cfg!(feature = "embeddings") {
                    "hybrid"
                } else {
                    "bm25"
                })
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

        if let Some(ref cache) = ctx.bm25_cache {
            crate::tools::ctx_semantic_search::set_thread_cache(cache.clone());
        }

        let send_progress = |progress: f64, msg: &str| {
            #[allow(clippy::unwrap_or_default)]
            if let Some(ref ps) = ctx.progress_sender
                && let Some(sender) = ps
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .as_ref()
            {
                sender.send(progress, Some(1.0), Some(msg.to_string()));
            }
        };

        send_progress(0.0, "Starting search...");

        let result = if action == "reindex" {
            send_progress(0.0, "Rebuilding BM25 index...");
            if artifacts {
                crate::tools::ctx_semantic_search::handle_reindex_artifacts(&path, workspace)
            } else {
                crate::tools::ctx_semantic_search::handle_reindex(&path)
            }
        } else if action == "find_related" {
            let fp = get_str(args, "file_path").unwrap_or_default();
            let line = get_int(args, "line").unwrap_or(1) as usize;
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

        send_progress(1.0, "Search complete");

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
