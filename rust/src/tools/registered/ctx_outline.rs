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
            "WORKFLOW: call BEFORE ctx_read to preview API surface.\n\
            ANTIPATTERN: NOT for file content (use ctx_read) or deep understanding (use ctx_compose).\n\
            Returns fn/struct/class/trait signatures + line numbers via tree-sitter.\n\
            kind=fn|struct|class|all filters. Saves tokens: only the API surface.",
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
