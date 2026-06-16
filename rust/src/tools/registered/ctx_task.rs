use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_str};
use crate::tool_defs::tool_def;

pub struct CtxTaskTool;

impl McpTool for CtxTaskTool {
    fn name(&self) -> &'static str {
        "ctx_task"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_task",
            "Multi-agent task orchestration. Actions: create|update|list|get|cancel|message|info.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["create", "update", "list", "get", "cancel", "message", "info"],
                        "description": "Task operation"
                    },
                    "task_id": { "type": "string", "description": "Task ID (required for update|get|cancel|message)" },
                    "to_agent": { "type": "string", "description": "Target agent ID (required for create)" },
                    "description": { "type": "string", "description": "Task description (for create)" },
                    "state": { "type": "string", "description": "New state for update (working|input-required|completed|failed|canceled)" },
                    "message": { "type": "string", "description": "Optional message / reason" }
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
        let action = get_str(args, "action").unwrap_or_else(|| "list".to_string());
        let current_agent_id = ctx
            .agent_id
            .as_ref()
            .map(|a| a.blocking_read().clone())
            .unwrap_or_default();
        let task_id = get_str(args, "task_id");
        let to_agent = get_str(args, "to_agent");
        let description = get_str(args, "description");
        let state = get_str(args, "state");
        let message = get_str(args, "message");

        let result = crate::tools::ctx_task::handle(
            &action,
            current_agent_id.as_deref(),
            task_id.as_deref(),
            to_agent.as_deref(),
            description.as_deref(),
            state.as_deref(),
            message.as_deref(),
        );

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
