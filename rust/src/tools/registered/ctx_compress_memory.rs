use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, require_resolved_path};
use crate::tool_defs::tool_def;

pub struct CtxCompressMemoryTool;

impl McpTool for CtxCompressMemoryTool {
    fn name(&self) -> &'static str {
        "ctx_compress_memory"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_compress_memory",
            "Compress a memory/config file (CLAUDE.md, .cursorrules) preserving code, URLs, and paths. Creates .original.md backup. Use to reduce token overhead of persistent instruction files.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" }
                },
                "required": ["path"]
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let path = require_resolved_path(ctx, args, "path")?;

        let result = crate::tools::ctx_compress_memory::handle(&path);
        Ok(ToolOutput::simple(result))
    }
}
