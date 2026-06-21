use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_str};
use crate::tool_defs::tool_def;

pub struct CtxComposeTool;

impl McpTool for CtxComposeTool {
    fn name(&self) -> &'static str {
        "ctx_compose"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_compose",
            "PRIMARY TOOL — call FIRST for understanding code, before editing, debugging, or\n\
             answering 'how does X work'. Pass a task/question or symbol names. One call replaces\n\
             ctx_search + ctx_read + ctx_symbol chains: returns ranked files with relevant symbol\n\
             source inline grouped by file. Combines BM25 lexical + semantic search + associative\n\
             retrieval + submodular optimization. Do NOT chain search→read→symbol — one compose\n\
             does it all. Do NOT Read files whose source compose already returned — it IS the source.\n\
             Fire independent ctx_read or ctx_compose calls for different areas in PARALLEL.",
            json!({
                "type": "object",
                "properties": {
                    "task": { "type": "string", "description": "Short English task/question or symbol names" },
                    "path": { "type": "string", "description": "Project root (default: .)" }
                },
                "required": ["task"]
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let task = get_str(args, "task")
            .ok_or_else(|| ErrorData::invalid_params("task is required", None))?;
        let path = if let Some(p) = ctx.resolved_path("path") {
            p.to_string()
        } else if let Some(err) = ctx.path_error("path") {
            return Err(ErrorData::invalid_params(format!("path: {err}"), None));
        } else {
            ctx.project_root.clone()
        };

        // Share the resident BM25 cache with the composed semantic search.
        if let Some(ref cache) = ctx.bm25_cache {
            crate::tools::ctx_semantic_search::set_thread_cache(cache.clone());
        }

        let (text, sent) = tokio::task::block_in_place(|| {
            crate::tools::ctx_compose::handle(&task, &path, ctx.crp_mode)
        });

        if text.starts_with("ERROR") {
            return Err(ErrorData::invalid_params(text, None));
        }

        Ok(ToolOutput {
            text,
            original_tokens: sent,
            saved_tokens: 0,
            mode: Some("compose".to_string()),
            path: Some(path),
            changed: false,
            shell_outcome: None,
        })
    }
}
