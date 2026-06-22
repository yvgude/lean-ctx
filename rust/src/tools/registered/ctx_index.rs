use std::path::Path;

use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_str};
use crate::tool_defs::tool_def;

pub struct CtxIndexTool;

impl McpTool for CtxIndexTool {
    fn name(&self) -> &'static str {
        "ctx_index"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_index",
            "Index orchestration — manage the code graph index.\n\
             Actions: status (current state), build (incremental update), build-full (complete rebuild).\n\
             Use when the graph index is stale and ctx_graph returns empty or outdated results.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["status", "build", "build-full"],
                        "description": "status|build|build-full"
                    },
                    "project_root": {
                        "type": "string",
                        "description": "Project root"
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
        let root = if let Some(p) = ctx
            .resolved_path("project_root")
            .or(ctx.resolved_path("root"))
        {
            p
        } else if let Some(err) = ctx.path_error("project_root").or(ctx.path_error("root")) {
            return Err(ErrorData::invalid_params(
                format!("project_root: {err}"),
                None,
            ));
        } else {
            &ctx.project_root
        };

        let result = crate::tools::ctx_index::handle(&action, Path::new(root));

        // #420: `build-full` is an explicit "make everything fresh". The CLI path
        // flushes the running daemon's read cache via `notify_cache_clear()`; the
        // MCP tool runs in the process that owns this session's `SessionCache`, so
        // clear it in-process here. Otherwise `ctx_read` map/signatures keep
        // serving pre-rebuild output from the long-lived cache.
        if action == "build-full"
            && let Some(cache) = ctx.cache.as_ref()
            && let Some(mut guard) = crate::server::bounded_lock::write(cache, "ctx_index")
        {
            guard.clear();
        }

        Ok(ToolOutput::simple(result))
    }
}
