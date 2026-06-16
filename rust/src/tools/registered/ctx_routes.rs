use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_str};
use crate::tool_defs::tool_def;

pub struct CtxRoutesTool;

impl McpTool for CtxRoutesTool {
    fn name(&self) -> &'static str {
        "ctx_routes"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_routes",
            "List HTTP routes/endpoints extracted from the project. Supports Express, Flask, FastAPI, Actix, Spring, Rails, Next.js.",
            json!({
                "type": "object",
                "properties": {
                    "method": { "type": "string", "description": "Optional: GET, POST, PUT, DELETE" },
                    "path": { "type": "string", "description": "Optional: path prefix filter, e.g. /api/users" }
                }
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let method = get_str(args, "method");
        // "path" here is an HTTP route prefix, not a filesystem path
        let path_prefix = get_str(args, "path");

        let result = crate::tools::ctx_routes::handle(
            method.as_deref(),
            path_prefix.as_deref(),
            &ctx.project_root,
        );

        Ok(ToolOutput::simple(result))
    }
}
