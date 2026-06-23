use std::path::Path;

use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_str, get_usize};
use crate::tool_defs::tool_def;

pub struct CtxArtifactsTool;

impl McpTool for CtxArtifactsTool {
    fn name(&self) -> &'static str {
        "ctx_artifacts"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_artifacts",
            "Context artifact registry with BM25 search — manage and query indexed code artifacts.\n\
            WORKFLOW: index artifacts first (index/reindex), then search with query for semantic retrieval.\n\
            Actions: list|status|index|reindex|search|remove.\n\
            ANTIPATTERN: NOT for general code search — use ctx_semantic_search for codebase queries.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["list", "status", "index", "reindex", "search", "remove"],
                        "description": "list|status|index|reindex|search|remove"
                    },
                    "project_root": {
                        "type": "string",
                        "description": "Project root"
                    },
                    "query": {
                        "type": "string",
                        "description": "Search query"
                    },
                    "name": {
                        "type": "string",
                        "description": "Artifact name"
                    },
                    "top_k": {
                        "type": "integer",
                        "description": "Max results (default depends on action, capped at 1000)"
                    },
                    "format": {
                        "type": "string",
                        "enum": ["json", "markdown"],
                        "description": "json|markdown"
                    }
                },
                "required": ["action"]
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let action = get_str(args, "action")
            .ok_or_else(|| ErrorData::invalid_params("action is required", None))?;
        let format = get_str(args, "format");
        let query = get_str(args, "query");
        let name = get_str(args, "name");
        let top_k = get_usize(args, "top_k").map(|d| d.min(1000));
        let root = if let Some(p) = ctx
            .resolved_path("project_root")
            .or(ctx.resolved_path("root"))
        {
            p
        } else if let Some(err) = ctx.path_error("project_root").or(ctx.path_error("root")) {
            return Err(ErrorData::invalid_params(format!("root: {err}"), None));
        } else {
            &ctx.project_root
        };

        let result = crate::tools::ctx_artifacts::handle(
            &action,
            Path::new(root),
            name.as_deref().or(query.as_deref()),
            top_k,
            format.as_deref(),
        );

        Ok(ToolOutput::simple(result))
    }
}
