use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_str};
use crate::tool_defs::tool_def;

pub struct CtxArchitectureTool;

impl McpTool for CtxArchitectureTool {
    fn name(&self) -> &'static str {
        "ctx_architecture"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_architecture",
            "Architecture analysis: action=overview→high-level; clusters|communities→groupings\n\
             layers|cycles→dependency violations; entrypoints|hotspots→risk areas; health→quality.\n\
             Use to understand module structure without reading every file. action=module path='src/' to zoom.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["overview", "clusters", "communities", "layers", "cycles", "entrypoints", "hotspots", "health", "module"],
                        "description": "Architecture operation (default: overview)"
                    },
                    "path": {
                        "type": "string",
                        "description": "Module/file path for action=module"
                    },
                    "root": {
                        "type": "string",
                        "description": "Project root (default: .)"
                    },
                    "format": {
                        "type": "string",
                        "description": "Output format"
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
        let action = get_str(args, "action").unwrap_or_else(|| "overview".to_string());
        let path = get_str(args, "path");
        let format = get_str(args, "format");
        let root = if let Some(p) = ctx
            .resolved_path("root")
            .or(ctx.resolved_path("project_root"))
        {
            p
        } else if let Some(err) = ctx.path_error("root").or(ctx.path_error("project_root")) {
            return Err(ErrorData::invalid_params(format!("root: {err}"), None));
        } else {
            &ctx.project_root
        };

        let result = crate::tools::ctx_architecture::handle(
            &action,
            path.as_deref(),
            root,
            format.as_deref(),
        );

        Ok(ToolOutput::simple(result))
    }
}
