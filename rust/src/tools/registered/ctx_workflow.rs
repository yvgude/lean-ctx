use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_str};
use crate::tool_defs::tool_def;

pub struct CtxWorkflowTool;

impl McpTool for CtxWorkflowTool {
    fn name(&self) -> &'static str {
        "ctx_workflow"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_workflow",
            "Workflow rails (state machine + evidence). Actions: start|status|transition|complete|evidence_add|evidence_list|stop.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["start", "status", "transition", "complete", "evidence_add", "evidence_list", "stop"],
                        "description": "Workflow operation (default: status)"
                    },
                    "name": { "type": "string", "description": "Optional workflow name override (action=start)" },
                    "spec": { "type": "string", "description": "WorkflowSpec JSON (action=start). If omitted, uses builtin plan_code_test." },
                    "to": { "type": "string", "description": "Target state (action=transition)" },
                    "key": { "type": "string", "description": "Evidence key (action=evidence_add)" },
                    "value": { "type": "string", "description": "Optional evidence value / transition note" }
                }
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let action = get_str(args, "action").unwrap_or_else(|| "status".to_string());

        let agent_id_str = ctx
            .agent_id
            .as_ref()
            .and_then(|h| h.blocking_read().clone());

        let result = {
            let session_handle = ctx
                .session
                .as_ref()
                .ok_or_else(|| ErrorData::internal_error("session not available", None))?;
            let mut session = session_handle.blocking_write();
            crate::tools::ctx_workflow::handle_with_session_agent(
                Some(args),
                &mut session,
                agent_id_str.as_deref(),
            )
        };

        if let Some(workflow_handle) = ctx.workflow.as_ref() {
            let mut wf = workflow_handle.blocking_write();
            *wf = crate::core::workflow::load_active_for_agent(agent_id_str.as_deref())
                .ok()
                .flatten();
        }

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
