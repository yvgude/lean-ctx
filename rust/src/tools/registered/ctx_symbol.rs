use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_str};
use crate::tool_defs::tool_def;

pub struct CtxSymbolTool;

impl McpTool for CtxSymbolTool {
    fn name(&self) -> &'static str {
        "ctx_symbol"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_symbol",
            "Read a specific symbol (function, struct, class) by name. Returns only the symbol \
code block instead of the entire file. 90-97% fewer tokens than full file read.",
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Symbol name (function, struct, class, method)" },
                    "file": { "type": "string", "description": "Optional: file path to narrow search" },
                    "kind": { "type": "string", "description": "Optional: fn|struct|class|method|trait|enum" }
                },
                "required": ["name"]
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let sym_name = get_str(args, "name")
            .ok_or_else(|| ErrorData::invalid_params("name is required", None))?;
        let file = get_str(args, "file");
        let kind = get_str(args, "kind");

        let (result, original) = crate::tools::ctx_symbol::handle(
            &sym_name,
            file.as_deref(),
            kind.as_deref(),
            &ctx.project_root,
        );
        let sent = crate::core::tokens::count_tokens(&result);
        let saved = original.saturating_sub(sent);

        Ok(ToolOutput {
            text: result,
            original_tokens: original,
            saved_tokens: saved,
            mode: kind,
            path: file,
            changed: false,
            shell_outcome: None,
        })
    }
}
