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
            "Build minimal context package within token budget. Modes: handles (references), compressed (content), full (all cached).\nWORKFLOW: after ctx_read/ctx_compose, package focused context for handoff/subagent.\nANTIPATTERN: not for exploration — use ctx_compose/ctx_read first.\nrun_id_version=1 preserves legacy timestamp/PID IDs; run_id_version=2 opts into deterministic IDs.",
            json!({
                "type": "object",
                "properties": {
                    "mode": { "type": "string", "description": "handles|compressed|full" },
                    "budget": { "type": "integer", "description": "Token budget (default: session budget or 12000)" },
                    "run_id_version": {
                        "type": "integer",
                        "enum": [1, 2],
                        "default": 1,
                        "description": "Run ID format: 1=legacy timestamp/PID (default), 2=deterministic"
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::tool_trait::McpTool;

    #[test]
    fn schema_advertises_opt_in_run_id_version() {
        let schema = serde_json::Value::Object((*CtxCompileTool.tool_def().input_schema).clone());
        let version = &schema["properties"]["run_id_version"];

        assert_eq!(version["type"], "integer");
        assert_eq!(version["default"], 1);
        assert_eq!(version["enum"], serde_json::json!([1, 2]));
    }
}
