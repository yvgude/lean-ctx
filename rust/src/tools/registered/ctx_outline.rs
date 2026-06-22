use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_str, require_resolved_path};
use crate::tool_defs::tool_def;

pub struct CtxOutlineTool;

impl McpTool for CtxOutlineTool {
    fn name(&self) -> &'static str {
        "ctx_outline"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_outline",
            "List file symbols with signatures and line numbers — path='file.rs' returns fn, struct,\n\
             class, and trait declarations via tree-sitter extraction. kind=fn|struct|class|all\n\
             to filter. Use for a quick API overview of a file. For deeper understanding,\n\
             use ctx_compose. For full file content, use ctx_read.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path" },
                    "kind": { "type": "string", "description": "fn|struct|class|all filter" }
                },
                "required": ["path"]
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let path = require_resolved_path(ctx, args, "path")?;
        let kind = get_str(args, "kind");

        let (result, original) = crate::tools::ctx_outline::handle(&path, kind.as_deref());
        let sent = crate::core::tokens::count_tokens(&result);
        let saved = original.saturating_sub(sent);

        Ok(ToolOutput {
            text: result,
            original_tokens: original,
            saved_tokens: saved,
            mode: kind,
            path: Some(path),
            changed: false,
            shell_outcome: None,
        })
    }
}
