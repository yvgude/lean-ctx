use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, require_resolved_path};
use crate::tool_defs::tool_def;

pub struct CtxAnalyzeTool;

impl McpTool for CtxAnalyzeTool {
    fn name(&self) -> &'static str {
        "ctx_analyze"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_analyze",
            "Entropy analysis — recommends optimal compression mode for a file path.\n\
            WORKFLOW: Use BEFORE ctx_read to pick the best mode (full/signatures/auto).\n\
            Saves tokens by selecting the mode that minimizes size while retaining information.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path to analyze for optimal compression mode" }
                },
                "required": ["path"]
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let path = require_resolved_path(ctx, args, "path")?;

        let result = crate::tools::ctx_analyze::handle(&path, crate::tools::CrpMode::effective());

        Ok(ToolOutput {
            text: result,
            original_tokens: 0,
            saved_tokens: 0,
            mode: None,
            path: Some(path),
            changed: false,
            shell_outcome: None,
        })
    }
}
