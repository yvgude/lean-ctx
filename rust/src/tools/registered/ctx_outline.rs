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
            "File symbols: path='file.rs'->signatures; kind=fn|struct|class|all filter\n\
             Lists all named symbols in a file with signatures and line numbers.\n\
             Generated via tree-sitter extraction of fn/struct/class/trait declarations.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path" },
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
