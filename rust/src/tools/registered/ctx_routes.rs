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
            "HTTP routes: method=GET|POST filter; path='/api' prefix; auto-detects frameworks\n\
             Extracts endpoints from: Express, Flask, FastAPI, Actix, Spring, Rails, Next.js.\n\
             Use to discover API surface without reading route definition files.",
            json!({
                "type": "object",
                "properties": {
                    "method": { "type": "string", "description": "GET|POST|PUT|DELETE filter" },
                    "path": { "type": "string", "description": "Path prefix, e.g. /api/users" }
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
