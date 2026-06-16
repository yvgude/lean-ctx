use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_str, get_usize};
use crate::tool_defs::tool_def;

pub struct CtxCostTool;

impl McpTool for CtxCostTool {
    fn name(&self) -> &'static str {
        "ctx_cost"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_cost",
            "Cost attribution (local-first). Actions: report|agent|tools|json|reset.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["report", "agent", "tools", "json", "reset", "status"],
                        "description": "Operation to perform (default: report)"
                    },
                    "agent_id": {
                        "type": "string",
                        "description": "Agent ID for action=agent (optional)"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Max rows (default: 10)"
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
        let action = get_str(args, "action").unwrap_or_else(|| "report".to_string());
        let agent_id = get_str(args, "agent_id");
        let limit = get_usize(args, "limit").map(|n| n.min(100_000));

        let result = crate::tools::ctx_cost::handle(&action, agent_id.as_deref(), limit);

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
