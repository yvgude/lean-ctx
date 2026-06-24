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
            "Index orchestration — manage code graph index.\n\
             Use: status to check state, build to (re)build indexes.\n\
             For a full rebuild: lean-ctx index build --mode full.\n\
             Actions: status (state), build (incremental rebuild).",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["status", "build"],
                        "description": "status|build"
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

        Ok(ToolOutput::simple(result))
    }
}
