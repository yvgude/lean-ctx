use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput};
use crate::tool_defs::tool_def;

pub struct CtxContextTool;

impl McpTool for CtxContextTool {
    fn name(&self) -> &'static str {
        "ctx_context"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_context",
            "Session context overview — cached files, seen files, session state.",
            json!({
                "type": "object",
                "properties": {}
            }),
        )
    }

    fn handle(
        &self,
        _args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let cache = ctx
            .cache
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("cache not available", None))?;
        let Some(guard) = crate::server::bounded_lock::read(cache, "ctx_context") else {
            return Ok(ToolOutput::simple(
                "[context status temporarily unavailable — retry]".to_string(),
            ));
        };
        let turn = ctx
            .call_count
            .as_ref()
            .map_or(0, |c| c.load(std::sync::atomic::Ordering::Relaxed));
        let result = crate::tools::ctx_context::handle_status(&guard, turn, ctx.crp_mode);
        Ok(ToolOutput::simple(result))
    }
}
