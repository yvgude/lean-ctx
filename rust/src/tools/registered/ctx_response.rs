use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_str};
use crate::tool_defs::tool_def;

pub struct CtxResponseTool;

impl McpTool for CtxResponseTool {
    fn name(&self) -> &'static str {
        "ctx_response"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_response",
            "Compress LLM response text (structural de-duplication).",
            json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string" }
                },
                "required": ["text"]
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let text = get_str(args, "text")
            .ok_or_else(|| ErrorData::invalid_params("text is required", None))?;
        let output = crate::tools::ctx_response::handle(&text, crate::tools::CrpMode::effective());
        Ok(ToolOutput::simple(output))
    }
}
