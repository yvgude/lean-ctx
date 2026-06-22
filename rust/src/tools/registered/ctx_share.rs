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
            "Share cached file contexts between agents for collaborative workflows.\n\
             Actions: push (share files from your cache to another agent),\n\
             pull (receive files shared by others), list (show shared contexts),\n\
             clear (remove your shares). Omit to_agent for broadcast;\n\
             set to_agent='agent-id' for targeted sharing.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["push", "pull", "list", "clear"],
                        "description": "push|pull|list|clear"
                    },
                    "paths": {
                        "type": "string",
                        "description": "Comma-separated paths (for push)"
                    },
                    "to_agent": {
                        "type": "string",
                        "description": "Target agent ID (omit for broadcast)"
                    },
                    "message": {
                        "type": "string",
                        "description": "Context message about what was shared"
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
