use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_bool, get_str};
use crate::tool_defs::tool_def;

pub struct CtxSessionTool;

impl McpTool for CtxSessionTool {
    fn name(&self) -> &'static str {
        "ctx_session"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_session",
            "Cross-session memory: action=task/finding/decision persists; load session_id=X resumes\n\
             Use at session end to persist progress; at start to restore previous work.\n\
             action=status for current snapshot; action=save commits state; action=reset clears.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "description": "status|load|save|task|finding|decision|reset|list|cleanup|snapshot|restore|resume|profile|role|budget|slo|diff|verify|episodes|procedures"
                    },
                    "value": { "type": "string", "description": "Value for task/finding/decision actions" },
                    "session_id": { "type": "string", "description": "Session ID to load (omitting loads latest)" }
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
        let value = get_str(args, "value");
        let sid = get_str(args, "session_id");
        let format = get_str(args, "format");
        let path = get_str(args, "path");
        let write = get_bool(args, "write").unwrap_or(false);
        let privacy = get_str(args, "privacy");
        let terse = get_bool(args, "terse");

        let tool_calls_handle = ctx
            .tool_calls
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("tool_calls not available", None))?;
        let call_durations: Vec<(String, u64)> = {
            let tc = tool_calls_handle.blocking_read();
            tc.iter().map(|c| (c.tool.clone(), c.duration_ms)).collect()
        };

        let session_handle = ctx
            .session
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("session not available", None))?;
        let mut session = session_handle.blocking_write();
        let result = crate::tools::ctx_session::handle(
            &mut session,
            &call_durations,
            &action,
            value.as_deref(),
            sid.as_deref(),
            crate::tools::ctx_session::SessionToolOptions {
                format: format.as_deref(),
                path: path.as_deref(),
                write,
                privacy: privacy.as_deref(),
                terse,
            },
        );
        drop(session);

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
