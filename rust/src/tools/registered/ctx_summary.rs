use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_str};
use crate::tool_defs::tool_def;

pub struct CtxSummaryTool;

impl McpTool for CtxSummaryTool {
    fn name(&self) -> &'static str {
        "ctx_summary"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_summary",
            "WORKFLOW: record after tasks → recall with query.\n\
             Compact session digests (task, files, decisions, next steps).\n\
             Actions: recall|record|list. Auto-captured on checkpoints.\n\
             ANTIPATTERN: structured facts → ctx_knowledge.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["recall", "record", "list"],
                        "description": "recall|record|list"
                    },
                    "query": {
                        "type": "string",
                        "description": "Recall query, e.g. \"what did I change?\""
                    },
                    "top_k": {
                        "type": "integer",
                        "description": "Max summaries to return"
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
        let action = get_str(args, "action").unwrap_or_else(|| "recall".to_string());
        let query = get_str(args, "query");
        let top_k = args
            .get("top_k")
            .and_then(Value::as_u64)
            .map_or(5, |n| n as usize);

        let guard = ctx
            .session
            .as_ref()
            .and_then(|s| crate::server::bounded_lock::read(s, "ctx_summary:session"));
        let session_ref = guard.as_deref();
        let root = session_ref
            .and_then(|s| s.project_root.clone())
            .unwrap_or_else(|| ctx.project_root.clone());

        let result =
            crate::tools::ctx_summary::handle(&root, session_ref, &action, query.as_deref(), top_k);
        Ok(ToolOutput::simple(result))
    }
}
