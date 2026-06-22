use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_bool};
use crate::tool_defs::tool_def;

pub struct CtxCompressTool;

impl McpTool for CtxCompressTool {
    fn name(&self) -> &'static str {
        "ctx_compress"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_compress",
            "Compress read cache to free token budget. Does not affect session state or knowledge.\n\
            WORKFLOW: check budget with ctx_context first, then reclaim space.",
            json!({
                "type": "object",
                "properties": {
                    "include_signatures": { "type": "boolean", "description": "Keep function/method signatures in compressed output (default: true)" }
                }
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let include_sigs = get_bool(args, "include_signatures").unwrap_or(true);
        let cache = ctx
            .cache
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("cache not available", None))?;
        let Some(guard) = crate::server::bounded_lock::read(cache, "ctx_compress") else {
            return Ok(ToolOutput::simple(
                "[cache temporarily unavailable — retry in a moment]".to_string(),
            ));
        };
        let result = crate::tools::ctx_compress::handle(&guard, include_sigs, ctx.crp_mode);
        Ok(ToolOutput::simple(result))
    }
}
