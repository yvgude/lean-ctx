use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_str};
use crate::tool_defs::tool_def;

pub struct CtxDiscoverToolsTool;

impl McpTool for CtxDiscoverToolsTool {
    fn name(&self) -> &'static str {
        "ctx_discover_tools"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_discover_tools",
            "Search available lean-ctx tools by keyword — use to find the right tool.\n\
             Empty query lists all tools. query=\"keyword\" returns matching names and descriptions.\n\
             Use before ctx_call or when unsure which tool fits your task.",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Search keyword (empty returns all)" }
                }
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let query = get_str(args, "query").unwrap_or_default();
        let result = crate::tool_defs::discover_tools(&query);
        Ok(ToolOutput::simple(result))
    }
}
