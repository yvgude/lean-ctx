use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_str};
use crate::tool_defs::tool_def;

pub struct CtxVerifyTool;

impl McpTool for CtxVerifyTool {
    fn name(&self) -> &'static str {
        "ctx_verify"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_verify",
            "Verification observability — tool call statistics and claim-based verification.\n\
             WORKFLOW: action=stats to monitor tool usage; action=proof|v2 for Lean4 proof verification.\n\
             Actions: stats|proof|v2 (format=summary|json|both, default summary).\n\
             ANTIPATTERN: not for runtime verification during active development — use for periodic audit.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["stats", "proof", "v2"],
                        "description": "stats|proof|v2"
                    },
                    "format": {
                        "type": "string",
                        "enum": ["summary", "json", "both"],
                        "description": "Output format: summary|json|both (default summary)"
                    }
                }
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let action = get_str(args, "action").unwrap_or_else(|| "stats".to_string());
        let format = get_str(args, "format");
        match action.as_str() {
            "stats" => {
                let out = crate::tools::ctx_verify::handle_stats(format.as_deref())
                    .map_err(|e| ErrorData::invalid_params(e, None))?;
                Ok(ToolOutput {
                    text: out,
                    original_tokens: 0,
                    saved_tokens: 0,
                    mode: Some(action),
                    path: None,
                    changed: false,
                    shell_outcome: None,
                })
            }
            "proof" | "v2" => {
                let out = crate::tools::ctx_verify::handle_proof(format.as_deref())
                    .map_err(|e| ErrorData::invalid_params(e, None))?;
                Ok(ToolOutput {
                    text: out,
                    original_tokens: 0,
                    saved_tokens: 0,
                    mode: Some(action),
                    path: None,
                    changed: false,
                    shell_outcome: None,
                })
            }
            _ => Err(ErrorData::invalid_params(
                "unsupported action (expected: stats, proof, v2)",
                None,
            )),
        }
    }
}
