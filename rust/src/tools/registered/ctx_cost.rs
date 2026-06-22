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
            "Cost attribution — track tokens and cost per agent/tool call. Local-first, no external billing.\n\
            Actions: report (summary), agent (per-agent), tools (per-tool), json (machine), status (live), reset (zero).\n\
            WORKFLOW: call report to find top cost drivers, then agent/tools for detail.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["report", "agent", "tools", "json", "reset", "status"],
                        "description": "report|agent|tools|json|reset|status"
                    },
                    "agent_id": {
                        "type": "string",
                        "description": "Agent ID"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Max rows"
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
