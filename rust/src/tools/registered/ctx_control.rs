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
            "Fine-tune context — exclude, include, pin, unpin, evict, set_view, set_priority, mark_outdated, reset, list, history.\n\
            Overlay-based, reversible, scoped to call/session/project.\n\
            WORKFLOW: after ctx_compose, exclude low-relevance files.\n\
            ANTIPATTERN: not for initial context building — use ctx_compose/ctx_read first.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "description": "exclude|include|pin|unpin|evict|set_view|set_priority|mark_outdated|reset|list|history"
                    },
                    "target": { "type": "string", "description": "@F1 handle, file path or item ID" },
                    "targets": { "type": "string", "description": "evict: comma-separated file paths" },
                    "value": { "type": "string", "description": "New content, view name, or priority" },
                    "scope": { "type": "string", "description": "call (this turn only), session (rest of this session), project (persists)" },
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
            let r = control(args, &mut ledger, &mut overlays, &root);
            ledger.save();
            r
        } else {
            let mut ledger = crate::core::context_ledger::ContextLedger::load();
            let r = control(args, &mut ledger, &mut overlays, &root);
            ledger.save();
            r
        };
        let _ = overlays.save_project(&std::path::PathBuf::from(&root));

        Ok(ToolOutput::simple(result))
    }
}

/// `evict` needs the project root to resolve partial paths; every other
/// action is the shared overlay handler (CLI and dashboard use it too).
fn control(
    args: &Map<String, Value>,
    ledger: &mut crate::core::context_ledger::ContextLedger,
    overlays: &mut crate::core::context_overlay::OverlayStore,
    root: &str,
) -> String {
    if args.get("action").and_then(Value::as_str) == Some("evict") {
        let targets = args
            .get("targets")
            .or_else(|| args.get("target"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        return crate::tools::ctx_control::evict(targets, ledger, overlays, root);
    }
    crate::tools::ctx_control::handle(Some(args), ledger, overlays)
}
