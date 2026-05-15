use rmcp::model::Tool;
use rmcp::ErrorData;
use serde_json::{json, Map, Value};

use crate::server::tool_trait::{get_str, McpTool, ToolContext, ToolOutput};
use crate::tool_defs::tool_def;

pub struct CtxRefactorTool;

impl McpTool for CtxRefactorTool {
    fn name(&self) -> &'static str {
        "ctx_refactor"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_refactor",
            "LSP-powered refactoring. Actions: rename, references, definition, implementations. \
             Requires a running language server (rust-analyzer, typescript-language-server, pylsp, gopls).",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["rename", "references", "definition", "implementations"],
                        "description": "Refactoring action"
                    },
                    "path": { "type": "string", "description": "File path" },
                    "line": { "type": "integer", "description": "1-indexed line number" },
                    "column": { "type": "integer", "description": "0-indexed character offset" },
                    "new_name": { "type": "string", "description": "New name (only for rename action)" }
                },
                "required": ["action", "path", "line"]
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let args_value = Value::Object(args.clone());
        let result = crate::tools::ctx_refactor::handle(&args_value, &ctx.project_root);

        let action = get_str(args, "action").unwrap_or_default();
        Ok(ToolOutput {
            text: result,
            original_tokens: 0,
            saved_tokens: 0,
            mode: Some(action),
            path: get_str(args, "path"),
        })
    }
}
