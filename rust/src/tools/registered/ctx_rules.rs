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
            "Cross-agent rules governance (ContextOps). Actions: sync (distribute rules to agents), diff (show drift), lint (check consistency), status (show sync state), init (create central config).",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["sync", "diff", "lint", "status", "init"],
                        "description": "Rules action to perform"
                    },
                    "agent": {
                        "type": "string",
                        "description": "Target agent name (for sync action only, e.g. 'cursor', 'claude')"
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
