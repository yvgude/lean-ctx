use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_str};
use crate::tool_defs::tool_def;

pub struct CtxDedupTool;

impl McpTool for CtxDedupTool {
    fn name(&self) -> &'static str {
        "ctx_dedup"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_dedup",
            "WORKFLOW: action=analyze first to find shared imports/code across files, then action=apply to register dedup hints for ctx_read output.\n\
            ANTIPATTERN: NOT for permanent dedup — only compression hints for read output.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "description": "analyze (find shared) | apply (register dedup)",
                        "default": "analyze"
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
        let action = get_str(args, "action").unwrap_or_default();
        let cache = ctx
            .cache
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("cache not available", None))?;
        let result = if action == "apply" {
            let Some(mut guard) = crate::server::bounded_lock::write(cache, "ctx_dedup:apply")
            else {
                return Ok(ToolOutput::simple(
                    "[dedup unavailable — cache busy, retry]".to_string(),
                ));
            };
            crate::tools::ctx_dedup::handle_action(&mut guard, &action)
        } else {
            let Some(guard) = crate::server::bounded_lock::read(cache, "ctx_dedup:status") else {
                return Ok(ToolOutput::simple(
                    "[dedup status unavailable — cache busy, retry]".to_string(),
                ));
            };
            crate::tools::ctx_dedup::handle(&guard)
        };
        Ok(ToolOutput::simple(result))
    }
}
