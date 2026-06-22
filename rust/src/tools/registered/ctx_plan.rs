use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput};
use crate::tool_defs::tool_def;

pub struct CtxPlanTool;

impl McpTool for CtxPlanTool {
    fn name(&self) -> &'static str {
        "ctx_plan"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_plan",
            "Context planning (CFT) — selects WHICH files/modules to include in context via Phi scoring, budget\n\
             allocation, and policy-driven view selection. Scans the project to pick the most relevant content.\n\
             task=description (short English), budget=token limit (default 12000), profile=ultra_lean|balanced|forensic.\n\
             For compressing already-chosen files to fit a budget, use ctx_fill instead.",
            json!({
                "type": "object",
                "properties": {
                    "task": { "type": "string", "description": "Task description (short English preferred)" },
                    "budget": { "type": "integer", "description": "Token budget (default: 12000)" },
                    "profile": { "type": "string", "description": "ultra_lean|balanced|forensic" }
                },
                "required": ["task"]
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let ledger = crate::core::context_ledger::ContextLedger::load();

        let root = if let Some(ref session_lock) = ctx.session {
            crate::server::bounded_lock::read(session_lock, "ctx_plan:session")
                .as_ref()
                .and_then(|s| s.project_root.clone())
                .unwrap_or_else(|| ctx.project_root.clone())
        } else {
            ctx.project_root.clone()
        };

        let policies = crate::core::context_policies::PolicySet::load_project(
            &std::path::PathBuf::from(&root),
        );
        let result = crate::tools::ctx_plan::handle(Some(args), &ledger, &policies);

        Ok(ToolOutput::simple(result))
    }
}
