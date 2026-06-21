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
            "Context checkpoint: compresses read cache to free budget in long sessions\n\
             include_signatures=true (default) preserves API surface in compressed state.\n\
             Does not affect session state or knowledge—only the read cache compaction.",
            json!({
                "type": "object",
                "properties": {
                    "include_signatures": { "type": "boolean", "description": "Include signatures (default: true)" }
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
