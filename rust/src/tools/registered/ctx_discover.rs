use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_usize};
use crate::tool_defs::tool_def;

pub struct CtxDiscoverTool;

impl McpTool for CtxDiscoverTool {
    fn name(&self) -> &'static str {
        "ctx_discover"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_discover",
            "Find missed compression opportunities in shell history.",
            json!({
                "type": "object",
                "properties": {
                    "limit": { "type": "integer" }
                }
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let limit = get_usize(args, "limit").unwrap_or(15).min(100_000);
        let history = crate::cli::load_shell_history_pub();
        let result = crate::tools::ctx_discover::discover_from_history(&history, limit);

        Ok(ToolOutput::simple(result))
    }
}
