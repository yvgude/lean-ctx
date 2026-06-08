use rmcp::model::Tool;
use rmcp::ErrorData;
use serde_json::{json, Map, Value};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput};
use crate::tool_defs::tool_def;

/// `ctx_tools` — MCP Tool-Catalog Gateway (#210). Aggregates downstream MCP
/// servers and returns a per-query top-N shortlist instead of injecting every
/// downstream schema, then proxies the real call.
pub struct CtxToolsTool;

impl McpTool for CtxToolsTool {
    fn name(&self) -> &'static str {
        "ctx_tools"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_tools",
            "Gateway to downstream MCP servers — unlimited external tools at ~constant context cost.\n\
             actions: find (query → top-N relevant tools as ChoiceCards) | call (proxy a `server::tool`) | list (servers+counts) | refresh.\n\
             Use find to discover, then call the chosen `server::tool`. Off by default ([gateway] config).",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["find", "call", "list", "refresh"],
                        "description": "Operation to perform (default: find)"
                    },
                    "query": { "type": "string", "description": "What you want to do; ranks the catalog (find)" },
                    "tool": { "type": "string", "description": "A `server::tool` handle from find (call)" },
                    "arguments": { "type": "object", "description": "Arguments forwarded verbatim to the downstream tool (call)" }
                }
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        match crate::tools::ctx_tools::run(args) {
            Ok(text) => Ok(ToolOutput::simple(text)),
            Err(e) => Err(ErrorData::invalid_params(e, None)),
        }
    }
}
