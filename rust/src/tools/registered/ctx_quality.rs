use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_str};
use crate::tool_defs::tool_def;

pub struct CtxQualityTool;

impl McpTool for CtxQualityTool {
    fn name(&self) -> &'static str {
        "ctx_quality"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_quality",
            "WORKFLOW: report (project score+hotspots+$ tax) → file (one file) → delta (vs HEAD).\n\
             Code health = clean code as a token-cost lever: cognitive complexity, naming,\n\
             and the estimated token 'quality tax' of over-threshold functions.\n\
             ANTIPATTERN: NOT a linter/style checker — it scores navigability, not formatting.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["report", "file", "delta"],
                        "description": "report|file|delta"
                    },
                    "path": {
                        "type": "string",
                        "description": "File to analyze (required for file|delta)"
                    },
                    "root": {
                        "type": "string",
                        "description": "Project root"
                    },
                    "format": {
                        "type": "string",
                        "description": "Output format (text|json)"
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
        let action = get_str(args, "action").unwrap_or_else(|| "report".to_string());
        let format = get_str(args, "format");
        let path = if let Some(p) = ctx.resolved_path("path") {
            Some(p.to_string())
        } else if let Some(err) = ctx.path_error("path") {
            return Err(ErrorData::invalid_params(format!("path: {err}"), None));
        } else {
            None
        };
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

        let result =
            crate::tools::ctx_quality::handle(&action, path.as_deref(), root, format.as_deref());

        Ok(ToolOutput::simple(result))
    }
}
