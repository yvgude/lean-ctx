use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_str};
use crate::tool_defs::tool_def;

pub struct CtxPackageTool;

impl McpTool for CtxPackageTool {
    fn name(&self) -> &'static str {
        "ctx_package"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_package",
            "Save or resume portable context packages — self-contained JSON bundles with session state, summaries, and knowledge. Use to hand off context between agents, persist session snapshots for later, or onboard a new agent into a previous session's context. Actions: save (export current session), resume (import from a package file), list (show saved packages), info (inspect a package without importing).",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["save", "resume", "list", "info"],
                        "description": "Package action (default: save)"
                    },
                    "path": {
                        "type": "string",
                        "description": "File path for resume/info, or custom output path for save"
                    },
                    "description": {
                        "type": "string",
                        "description": "Human-readable description for the saved package"
                    }
                },
                "required": []
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let action = get_str(args, "action").unwrap_or_else(|| "save".to_string());
        let path = get_str(args, "path");
        let description = get_str(args, "description");

        let guard = ctx
            .session
            .as_ref()
            .and_then(|s| crate::server::bounded_lock::read(s, "ctx_package:session"));
        let session_ref = guard.as_deref();
        let root = session_ref
            .and_then(|s| s.project_root.clone())
            .unwrap_or_else(|| ctx.project_root.clone());

        let agent_id_guard = ctx.agent_id.as_ref().map(|a| a.blocking_read());
        let agent_id = agent_id_guard.as_ref().and_then(|g| g.as_deref());
        let result = crate::tools::ctx_package::handle(
            &root,
            session_ref,
            &action,
            path.as_deref(),
            agent_id,
            description.as_deref(),
        );
        Ok(ToolOutput::simple(result))
    }
}
