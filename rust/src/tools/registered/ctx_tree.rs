use rmcp::model::Tool;
use rmcp::ErrorData;
use serde_json::{json, Map, Value};

use crate::server::tool_trait::{get_bool, get_int, McpTool, ToolContext, ToolOutput};
use crate::tool_defs::tool_def;

pub struct CtxTreeTool;

impl McpTool for CtxTreeTool {
    fn name(&self) -> &'static str {
        "ctx_tree"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_tree",
            "Directory listing with file counts.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Directory path (default: .)" },
                    "depth": { "type": "integer", "description": "Max depth (default: 3)" },
                    "show_hidden": { "type": "boolean", "description": "Show hidden files" }
                }
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let path = ctx.resolved_path("path").unwrap_or(".").to_string();
        let depth = (get_int(args, "depth").unwrap_or(3) as usize).min(10);
        let show_hidden = get_bool(args, "show_hidden").unwrap_or(false);

        let (result, original) = crate::tools::ctx_tree::handle(&path, depth, show_hidden);
        let sent = crate::core::tokens::count_tokens(&result);
        let saved = original.saturating_sub(sent);

        let final_out = crate::core::protocol::append_savings(&result, original, sent);

        Ok(ToolOutput {
            text: final_out,
            original_tokens: original,
            saved_tokens: saved,
            mode: None,
            path: Some(path),
        })
    }
}
