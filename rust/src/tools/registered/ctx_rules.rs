use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_str};
use crate::tool_defs::tool_def;

pub struct CtxRulesTool;

impl McpTool for CtxRulesTool {
    fn name(&self) -> &'static str {
        "ctx_rules"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_rules",
            "Cross-agent rules governance (ContextOps).\n\
             Actions: sync (distribute rules to agents), diff (show drift),\n\
             lint (check consistency), status (sync state), init (create central config).\n\
             WORKFLOW: run status first to check state, then sync if out of date.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["sync", "diff", "lint", "status", "init"]
                    },
                    "agent": {
                        "type": "string",
                        "description": "Target agent name (for sync)"
                    }
                },
                "required": ["action"]
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let action = get_str(args, "action").unwrap_or_default();
        let agent = get_str(args, "agent");

        let result = crate::tools::ctx_rules::handle(&action, agent.as_deref());
        Ok(ToolOutput::simple(result))
    }
}
