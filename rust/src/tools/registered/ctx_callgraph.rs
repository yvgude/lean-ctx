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
            "Callers/callees analysis — who calls a function and what it calls.\n\
             action=callers symbol='fn' returns every call site with file:line.\n\
             For END-TO-END flow tracing (how does X reach Y), use ctx_compose FIRST\n\
             — one call returns the path + source. Use ctx_callgraph only when you need\n\
             exhaustive enumeration of ALL callers/callees for a single symbol.\n\
             action=trace from→to finds path between two symbols. depth=N for BFS depth.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "description": "callers|callees|trace|risk (default: callers)",
                        "enum": ["callers", "callees", "trace", "risk"]
                    },
                    "symbol": {
                        "type": "string",
                        "description": "Symbol name (required for callers/callees/risk)"
                    },
                    "file": {
                        "type": "string",
                        "description": "Scope results to file"
                    },
                    "depth": {
                        "type": "integer",
                        "description": "BFS depth for callers/callees (1–5, default 1)",
                        "minimum": 1,
                        "maximum": 5
                    },
                    "from": {
                        "type": "string",
                        "description": "Source symbol for trace action"
                    },
                    "to": {
                        "type": "string",
                        "description": "Target symbol for trace action"
                    }
                }
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let action = get_str(args, "action")
            .or_else(|| get_str(args, "direction"))
            .unwrap_or_else(|| "callers".to_string());

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
        })
    }
}
