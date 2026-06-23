use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

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
             actions: find (query → top-N relevant tools) | call (proxy a server::tool) |\n\
             list (servers+counts) | refresh.\n\
             WORKFLOW: find to discover, then call the chosen server::tool.\n\
             ANTIPATTERN: not for built-in tools — use those directly.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["find", "call", "list", "refresh"],
                        "description": "find|call|list|refresh"
                    },
                    "query": { "type": "string", "description": "What you want to do (for find)" },
                    "tool": { "type": "string", "description": "`server::tool` handle (for call)" },
                    "arguments": { "type": "object", "description": "Arguments for downstream tool (call)" }
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
