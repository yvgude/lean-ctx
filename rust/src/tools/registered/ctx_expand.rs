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
            "Retrieve archived tool output by ID (e.g. id=@F1 from [Archived:ID] hints).\n\
             Use when you see an [Archived:ID] reference in tool output and need the full\n\
             content. Supports head/tail/search to filter lines. action=search_all across\n\
             all archives. action=list shows available archives. Zero-loss: original preserved.\n\
             For reading files, use ctx_read or ctx_compose instead.",
            json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Archive ID or handle ref (@F1)" },
                    "action": { "type": "string", "description": "retrieve (default)|list|search_all" },
                    "start_line": { "type": "integer", "description": "1-based start line in archived output" },
                    "end_line": { "type": "integer", "description": "1-based end line in archived output" },
                    "head": { "type": "integer", "description": "First N lines" },
                    "tail": { "type": "integer", "description": "Last N lines" },
                    "search": { "type": "string", "description": "Only lines matching substring" },
                    "json_keys": { "type": "boolean", "description": "Describe JSON structure" },
                    "json_path": { "type": "string", "description": "JSON path, e.g. data.items.0" },
                    "query": { "type": "string", "description": "search_all query" },
                    "session_id": { "type": "string", "description": "Filter by session ID" }
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
