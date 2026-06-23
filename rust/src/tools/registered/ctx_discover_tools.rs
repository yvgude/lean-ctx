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
            "WORKFLOW: call FIRST when unsure which tool fits your task — lists all tools on empty query.\n\
             Then use ctx_call to invoke discovered tools (for static-tool-list clients).\n\
             ANTIPATTERN: not for runtime invocation — use ctx_call(name=..., arguments=...) directly.",
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
