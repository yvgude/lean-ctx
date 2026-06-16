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
            "Retrieve archived/firewalled tool output (zero-loss). Use the ID from an [Archived:/Firewalled: ...] hint.",
            json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Archive ID or handle ref (@F1)" },
                    "action": { "type": "string", "description": "retrieve (default)|list|search_all" },
                    "start_line": { "type": "integer" },
                    "end_line": { "type": "integer" },
                    "head": { "type": "integer", "description": "First N lines" },
                    "tail": { "type": "integer", "description": "Last N lines" },
                    "search": { "type": "string", "description": "Only lines matching substring" },
                    "json_keys": { "type": "boolean", "description": "Describe JSON structure" },
                    "json_path": { "type": "string", "description": "JSON path, e.g. data.items.0" },
                    "query": { "type": "string", "description": "search_all query" },
                    "session_id": { "type": "string" }
                }
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
