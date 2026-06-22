use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_str};
use crate::tool_defs::tool_def;

pub struct CtxPluginsTool;

impl McpTool for CtxPluginsTool {
    fn name(&self) -> &'static str {
        "ctx_plugins"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_plugins",
            "WORKFLOW: list -> info/name -> enable/disable.\n\
            ANTIPATTERN: NOT for tool listing (use ctx_discover_tools).\n\
            Plugin management — list, enable, disable, info, hooks.\n\
            name required for enable/disable/info. Extends tool functionality.\n\
            Saves tokens: loads only needed plugins.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["list", "enable", "disable", "info", "hooks"],
                        "description": "Plugin action to perform"
                    },
                    "name": {
                        "type": "string",
                        "description": "Plugin name (required for enable, disable, info)"
                    }
                },
                "required": ["action"]
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let action = get_str(args, "action").unwrap_or_default();
        let name = get_str(args, "name");

        let result = crate::tools::ctx_plugins::handle(&action, name.as_deref());
        Ok(ToolOutput::simple(result))
    }
}
