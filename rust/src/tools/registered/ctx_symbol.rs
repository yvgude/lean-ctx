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
            "Get ONE symbol's body by name — exact, AST-precise (tree-sitter index). Use AFTER\n\
             ctx_compose gave you the overview and you need a specific symbol's full body.\n\
             For multiple symbols or understanding an area, use ctx_compose FIRST (returns\n\
             all relevant symbols grouped by file in one call). name='fnName' returns code block.\n\
             file='path.rs' narrows; kind='fn'|'struct'|'class'|'trait'|'enum' disambiguates.",
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "fn|struct|class|method name" },
                    "file": { "type": "string", "description": "Narrow search to file" },
                    "kind": { "type": "string", "description": "fn|struct|class|trait|enum filter" }
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
