use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_str, get_usize};
use crate::tool_defs::tool_def;

pub struct CtxReviewTool;

impl McpTool for CtxReviewTool {
    fn name(&self) -> &'static str {
        "ctx_review"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_review",
            "Automated code review: combines impact analysis, caller tracking, and test discovery. \
             Actions: review (single file), diff-review (from git diff), checklist (structured review questions).",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["review", "diff-review", "checklist"],
                        "description": "Review action"
                    },
                    "path": {
                        "type": "string",
                        "description": "File path to review (or git diff text for diff-review)"
                    },
                    "depth": {
                        "type": "integer",
                        "description": "Impact analysis depth (default: 3)"
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
        let path = get_str(args, "path");
        let depth = get_usize(args, "depth").map(|d| d.min(64));
        let project_root = if let Some(p) = ctx
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

        let result =
            crate::tools::ctx_review::handle(&action, path.as_deref(), project_root, depth);

        Ok(ToolOutput::simple(result))
    }
}
