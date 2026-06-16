use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput};
use crate::tool_defs::tool_def;

pub struct CtxCompileTool;

impl McpTool for CtxCompileTool {
    fn name(&self) -> &'static str {
        "ctx_compile"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_compile",
            "Context compilation (CFT). Builds minimal context package via greedy knapsack + Boltzmann view selection. Modes: handles|compressed|full.",
            json!({
                "type": "object",
                "properties": {
                    "mode": { "type": "string", "description": "handles|compressed|full (default: handles)" },
                    "budget": { "type": "integer", "description": "Token budget (default: 12000)" }
                }
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
            crate::server::bounded_lock::read(session_lock, "ctx_compile:session")
                .as_ref()
                .and_then(|s| s.project_root.clone())
                .unwrap_or_else(|| ctx.project_root.clone())
        } else {
            ctx.project_root.clone()
        };

        let policies = crate::core::context_policies::PolicySet::load_project(
            &std::path::PathBuf::from(&root),
        );
        let result = crate::tools::ctx_compile::handle(Some(args), &ledger, &policies);

        Ok(ToolOutput::simple(result))
    }
}
