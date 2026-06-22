use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_str};
use crate::tool_defs::tool_def;

pub struct CtxSmellsTool;

impl McpTool for CtxSmellsTool {
    fn name(&self) -> &'static str {
        "ctx_smells"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_smells",
            "Code smell detection engine.\n\
             Actions: scan (run all rules on project), summary (aggregate counts),\n\
             rules (list available rules with descriptions), file (scan a single file).\n\
             Supports rule='name' and path='file' filters for targeted analysis.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["scan", "summary", "rules", "file"],
                        "description": "scan|summary|rules|file"
                    },
                    "rule": {
                        "type": "string",
                        "description": "Filter by rule name (for scan)"
                    },
                    "path": {
                        "type": "string",
                        "description": "Filter by file path"
                    },
                    "root": {
                        "type": "string",
                        "description": "Project root"
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
        let action = get_str(args, "action").unwrap_or_else(|| "summary".to_string());
        let rule = get_str(args, "rule");
        let path = get_str(args, "path");
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

        let result = crate::tools::ctx_smells::handle(
            &action,
            rule.as_deref(),
            path.as_deref(),
            root,
            format.as_deref(),
        );

        Ok(ToolOutput::simple(result))
    }
}
