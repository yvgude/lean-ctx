use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_str};
use crate::tool_defs::tool_def;

pub struct CtxShareTool;

impl McpTool for CtxShareTool {
    fn name(&self) -> &'static str {
        "ctx_share"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_share",
            "Share cached file contexts between agents. Actions: push (share files from your cache to another agent), \
pull (receive files shared by other agents), list (show all shared contexts), clear (remove your shared contexts).",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["push", "pull", "list", "clear"],
                        "description": "Share operation to perform"
                    },
                    "paths": {
                        "type": "string",
                        "description": "Comma-separated file paths to share (for push action)"
                    },
                    "to_agent": {
                        "type": "string",
                        "description": "Target agent ID (omit for broadcast to all agents)"
                    },
                    "message": {
                        "type": "string",
                        "description": "Optional context message explaining what was shared"
                    }
                },
                "required": ["action"]
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let action = get_str(args, "action")
            .ok_or_else(|| ErrorData::invalid_params("action is required", None))?;
        let to_agent = get_str(args, "to_agent");
        let paths = get_str(args, "paths");
        let message = get_str(args, "message");

        let from_agent = ctx
            .agent_id
            .as_ref()
            .map(|a| a.blocking_read().clone())
            .unwrap_or_default();

        let cache_handle = ctx
            .cache
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("cache not available", None))?;
        let cache = cache_handle.blocking_read();
        let result = crate::tools::ctx_share::handle(
            &action,
            from_agent.as_deref(),
            to_agent.as_deref(),
            paths.as_deref(),
            message.as_deref(),
            &cache,
            &ctx.project_root,
        );
        drop(cache);

        Ok(ToolOutput {
            text: result,
            original_tokens: 0,
            saved_tokens: 0,
            mode: Some(action),
            path: None,
            changed: false,
            shell_outcome: None,
        })
    }
}
