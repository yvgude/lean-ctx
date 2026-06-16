use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput};
use crate::tool_defs::tool_def;

pub struct CtxControlTool;

impl McpTool for CtxControlTool {
    fn name(&self) -> &'static str {
        "ctx_control"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_control",
            "Universal context manipulation (Context Field Theory). Actions: exclude|include|pin|unpin|set_view|set_priority|mark_outdated|reset|list|history. Overlay-based, reversible, scoped.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "description": "exclude|include|pin|unpin|set_view|set_priority|mark_outdated|reset|list|history"
                    },
                    "target": { "type": "string", "description": "@F1 or path or item ID" },
                    "value": { "type": "string", "description": "New content, view name, or priority" },
                    "scope": { "type": "string", "description": "call|session|project (default: session)" },
                    "reason": { "type": "string", "description": "Reason for the action" }
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
        let root = if let Some(ref session_lock) = ctx.session {
            crate::server::bounded_lock::read(session_lock, "ctx_control:session")
                .as_ref()
                .and_then(|s| s.project_root.clone())
                .unwrap_or_else(|| ctx.project_root.clone())
        } else {
            ctx.project_root.clone()
        };

        let mut overlays = crate::core::context_overlay::OverlayStore::load_project(
            &std::path::PathBuf::from(&root),
        );

        let result = if let Some(ref ledger_lock) = ctx.ledger {
            let Some(mut ledger) =
                crate::server::bounded_lock::write(ledger_lock, "ctx_control:ledger")
            else {
                return Ok(ToolOutput::simple(
                    "[control unavailable — ledger busy, retry]".to_string(),
                ));
            };
            let r = crate::tools::ctx_control::handle(Some(args), &mut ledger, &mut overlays);
            ledger.save();
            r
        } else {
            let mut ledger = crate::core::context_ledger::ContextLedger::load();
            let r = crate::tools::ctx_control::handle(Some(args), &mut ledger, &mut overlays);
            ledger.save();
            r
        };
        let _ = overlays.save_project(&std::path::PathBuf::from(&root));

        Ok(ToolOutput::simple(result))
    }
}
