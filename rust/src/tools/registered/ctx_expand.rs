use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput};
use crate::tool_defs::tool_def;

pub struct CtxExpandTool;

impl McpTool for CtxExpandTool {
    fn name(&self) -> &'static str {
        "ctx_expand"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_expand",
            "Retrieve archived tool output: see [Archived:ID] → ctx_expand id=ID \
             (zero-loss, original preserved). head/tail/search filter lines; \
             action=list|search_all browses/queries archives.\n\
             ANTIPATTERN: not for project files — use ctx_read or ctx_compose.",
            json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Archive ID or @F1 ref" },
                    "action": { "type": "string", "description": "retrieve|list|search_all" },
                    "start_line": { "type": "integer" },
                    "end_line": { "type": "integer" },
                    "head": { "type": "integer" },
                    "tail": { "type": "integer" },
                    "search": { "type": "string" },
                    "json_keys": { "type": "boolean" },
                    "json_path": { "type": "string", "description": "e.g. data.items.0" },
                    "query": { "type": "string" },
                    "session_id": { "type": "string" }
                },
                "allOf": [
                    { "if": { "properties": { "action": { "const": "search_all" } }, "required": ["action"] }, "then": { "required": ["query"] } },
                    { "if": { "not": { "properties": { "action": { "enum": ["list", "search_all"] } }, "required": ["action"] } }, "then": { "required": ["id"] } }
                ]
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let args_val = Value::Object(args.clone());
        let result = crate::tools::ctx_expand::handle(&args_val);
        Ok(ToolOutput::simple(result))
    }
}
