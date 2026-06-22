use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_str, get_usize};
use crate::tool_defs::tool_def;

pub struct CtxImpactTool;

impl McpTool for CtxImpactTool {
    fn name(&self) -> &'static str {
        "ctx_impact"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_impact",
            "Change impact analysis — assess blast radius before refactoring.\n\
             action=analyze path=\"file.rs\" maps downstream dependents; action=diff compares git refs;\n\
             action=chain traces from→to dependency paths. depth controls traversal (default 5).",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["analyze", "diff", "chain", "build", "update", "status"],
                        "description": "analyze|diff|chain|build|update|status"
                    },
                    "path": {
                        "type": "string",
                        "description": "File path or type name"
                    },
                    "root": {
                        "type": "string",
                        "description": "Project root"
                    },
                    "depth": {
                        "type": "integer",
                        "description": "Max depth (default: 5)"
                    },
                    "format": {
                        "type": "string",
                        "description": "Output format"
                    }
                }
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let action = get_str(args, "action").unwrap_or_else(|| "analyze".to_string());
        let path = get_str(args, "path");
        let depth = get_usize(args, "depth").map(|d| d.min(64));
        let format = get_str(args, "format");
        let root = if let Some(p) = ctx
            .resolved_path("root")
            .or(ctx.resolved_path("project_root"))
        {
            p
        } else if let Some(err) = ctx.path_error("root").or(ctx.path_error("project_root")) {
            return Err(ErrorData::invalid_params(format!("root: {err}"), None));
        } else {
            &ctx.project_root
        };

        let result = crate::tools::ctx_impact::handle(
            &action,
            path.as_deref(),
            root,
            depth,
            format.as_deref(),
        );

        Ok(ToolOutput::simple(result))
    }
}
