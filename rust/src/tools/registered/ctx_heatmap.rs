use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_str};
use crate::tool_defs::tool_def;

pub struct CtxHeatmapTool;

impl McpTool for CtxHeatmapTool {
    fn name(&self) -> &'static str {
        "ctx_heatmap"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_heatmap",
            "File access heatmap — shows most frequently accessed files.",
            json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "description": "status|detail" },
                    "path": { "type": "string" }
                }
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let action = get_str(args, "action").unwrap_or_else(|| "status".to_string());
        let path = get_str(args, "path");
        let result = crate::tools::ctx_heatmap::handle(&action, path.as_deref());
        Ok(ToolOutput {
            text: result,
            original_tokens: 0,
            saved_tokens: 0,
            mode: Some(action),
            path: None,
            changed: false,
            shell_outcome: None,
        })
    }
}
