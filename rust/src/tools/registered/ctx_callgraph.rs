use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_int, get_str};
use crate::tool_defs::tool_def;

pub struct CtxCallgraphTool;

impl McpTool for CtxCallgraphTool {
    fn name(&self) -> &'static str {
        "ctx_callgraph"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_callgraph",
            "Callers/callees for one symbol (function call edges, not const/var refs).\n\
             action=callers|callees symbol='fn' → every call site with file:line.\n\
             action=trace from→to finds the path between two symbols (depth=N).\n\
             For end-to-end flow understanding use ctx_compose FIRST.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["callers", "callees", "trace", "risk"]
                    },
                    "symbol": { "type": "string" },
                    "file": { "type": "string", "description": "Scope results to file" },
                    "depth": { "type": "integer", "minimum": 1, "maximum": 5 },
                    "from": { "type": "string" },
                    "to": { "type": "string" }
                },
                "required": ["action"],
                "allOf": [
                    { "if": { "properties": { "action": { "enum": ["callers", "callees", "risk"] } }, "required": ["action"] }, "then": { "required": ["symbol"] } },
                    { "if": { "properties": { "action": { "const": "trace" } }, "required": ["action"] }, "then": { "required": ["from", "to"] } }
                ]
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let action = get_str(args, "action").unwrap_or_else(|| "callers".to_string());

        let action_normalized = match action.to_lowercase().as_str() {
            "callers" | "caller" => "callers",
            "callees" | "callee" => "callees",
            "trace" => "trace",
            "risk" => "risk",
            _ => action.as_str(),
        }
        .to_string();

        let symbol = get_str(args, "symbol");
        let file = get_str(args, "file");
        let depth = get_int(args, "depth").unwrap_or(1).clamp(1, 5) as usize;
        let from = get_str(args, "from");
        let to = get_str(args, "to");

        let result = crate::tools::ctx_callgraph::handle(
            &action_normalized,
            symbol.as_deref(),
            file.as_deref(),
            &ctx.project_root,
            depth,
            from.as_deref(),
            to.as_deref(),
        );

        Ok(ToolOutput {
            text: result,
            original_tokens: 0,
            saved_tokens: 0,
            mode: Some(action_normalized),
            path: None,
            changed: false,
            shell_outcome: None,
            content_blocks: None,
        })
    }
}
