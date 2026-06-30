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
             WORKFLOW: see [Archived:ID] → ctx_expand id=ID to restore full content.\n\
             Supports head/tail/search to filter lines and save tokens on re-read.\n\
             action=list browses all archives. action=search_all queries across archives.\n\
             Zero-loss: original preserved.\n\
             NO MCP? The same bytes are a real file — every [Archived]/tee/firewall hint\n\
             shows its on-disk path; read that path directly with any tool instead.\n\
             ANTIPATTERN: not for reading project files — use ctx_read or ctx_compose.",
            json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Archive ID or @F1 ref" },
                    "action": { "type": "string", "description": "retrieve|list|search_all" },
                    "start_line": { "type": "integer", "description": "1-based start line" },
                    "end_line": { "type": "integer", "description": "1-based end line" },
                    "head": { "type": "integer", "description": "First N lines" },
                    "tail": { "type": "integer", "description": "Last N lines" },
                    "search": { "type": "string", "description": "Lines matching substring" },
                    "json_keys": { "type": "boolean", "description": "List JSON keys" },
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
