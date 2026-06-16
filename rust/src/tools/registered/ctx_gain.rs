use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_str, get_usize};
use crate::tool_defs::tool_def;

pub struct CtxGainTool;

impl McpTool for CtxGainTool {
    fn name(&self) -> &'static str {
        "ctx_gain"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_gain",
            "Gain report (includes Wrapped via action=wrapped).",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["status", "report", "score", "cost", "tasks", "heatmap", "wrapped", "agents", "json"]
                    },
                    "period": {
                        "type": "string",
                        "enum": ["week", "month", "all"]
                    },
                    "model": {
                        "type": "string"
                    },
                    "limit": {
                        "type": "integer"
                    }
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
        let period = get_str(args, "period");
        let model = get_str(args, "model");
        let limit = get_usize(args, "limit").map(|n| n.min(100_000));

        let result =
            crate::tools::ctx_gain::handle(&action, period.as_deref(), model.as_deref(), limit);

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
