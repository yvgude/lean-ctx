use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{
    McpTool, ToolContext, ToolOutput, get_f64, get_str, get_str_array,
};
use crate::tool_defs::tool_def;

pub struct CtxKnowledgeTool;

impl McpTool for CtxKnowledgeTool {
    fn name(&self) -> &'static str {
        "ctx_knowledge"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_knowledge",
            "Persistent memory across sessions — remember decisions, patterns, and facts for recall.\n\
             WORKFLOW: save after completing significant tasks; recall at session start.\n\
             action=remember key='X' value='Y' saves a fact (both required).\n\
             action=recall query='X' retrieves it. action=status shows all categories.\n\
action=consolidate imports latest session if present, runs lifecycle, then frees 25% facts/history/procedures capacity.\n\
             action=gotcha trigger='X' resolution='Y' for known pitfalls.\n\
             mode=semantic|exact for recall. category groups related facts.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "description": "remember|recall|search|pattern|gotcha|relate|relations|consolidate|restore|status|timeline|rooms|wakeup|remove|export (also: feedback, unrelate, relations_diagram, health, lifecycle_report, policy, embeddings_*)"
                    },
                    "trigger": { "type": "string", "description": "gotcha trigger pattern" },
                    "resolution": { "type": "string", "description": "gotcha resolution/fix" },
                    "severity": { "type": "string", "description": "gotcha: critical|warning|info" },
                    "category": { "type": "string", "description": "Fact category" },
                    "key": { "type": "string" },
                    "value": { "type": "string" },
                    "query": { "type": "string", "description": "Query for recall/search/relate/restore" },
                    "mode": { "type": "string", "description": "auto|exact|semantic|hybrid" },
                    "as_of": { "type": "string", "description": "YYYY-MM-DD date filter" },
                    "pattern_type": { "type": "string" },
                    "examples": { "type": "array", "items": { "type": "string" } },
                    "confidence": { "type": "number", "description": "0.0-1.0" },
                    "store": { "type": "string", "description": "restore: facts|history|procedures|patterns (default: all)" },
                    "limit": { "type": "number", "description": "restore: max items to recover (default 50)" },
                    "dry_run": { "type": "boolean", "description": "consolidate: preview imports/reclaim without writing" }
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
        let category = get_str(args, "category");
        let key = get_str(args, "key");
        let value = get_str(args, "value");
        let query = get_str(args, "query");
        let mode = get_str(args, "mode");
        let as_of = get_str(args, "as_of");
        let pattern_type = get_str(args, "pattern_type");
        let examples = get_str_array(args, "examples");
        let confidence = get_f64(args, "confidence").map(|v| v as f32);

        let session_handle = ctx
            .session
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("session not available", None))?;
        let (session_id, project_root) = {
            let timeout_dur =
                crate::core::io_health::adaptive_timeout(std::time::Duration::from_secs(10));
            let read_result = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current()
                    .block_on(tokio::time::timeout(timeout_dur, session_handle.read()))
            });
            if let Ok(session) = read_result {
                let sid = session.id.clone();
                let root = session
                    .project_root
                    .clone()
                    .unwrap_or_else(|| ctx.project_root.clone());
                (sid, root)
            } else {
                tracing::warn!("ctx_knowledge: session read-lock timeout, using fallback");
                ("unknown".to_string(), ctx.project_root.clone())
            }
        };

        if action == "gotcha" {
            let trigger = get_str(args, "trigger").unwrap_or_default();
            let resolution = get_str(args, "resolution").unwrap_or_default();
            let severity = get_str(args, "severity").unwrap_or_default();
            let cat = category.as_deref().unwrap_or("convention");

            if trigger.is_empty() || resolution.is_empty() {
                return Ok(text_output(
                    &action,
                    "ERROR: trigger and resolution are required for gotcha action".to_string(),
                ));
            }

            let mut store = crate::core::gotcha_tracker::GotchaStore::load(&project_root);
            let msg = match store.report_gotcha(&trigger, &resolution, cat, &severity, &session_id)
            {
                Some(gotcha) => {
                    let conf = (gotcha.confidence * 100.0) as u32;
                    let label = gotcha.category.short_label();
                    format!("Gotcha recorded: [{label}] {trigger} (confidence: {conf}%)")
                }
                None => {
                    format!("Gotcha noted: {trigger} (evicted by higher-confidence entries)")
                }
            };
            let _ = store.save(&project_root);
            return Ok(text_output(&action, msg));
        }

        // Restore (#995 Phase 6): explicit cross-store undo from archive. Handled
        // inline (like `gotcha`) so `store`/`limit` can be passed without widening
        // the shared `handle()` signature.
        if action == "restore" {
            let store = match get_str(args, "store").as_deref() {
                Some(s) => match crate::core::memory_archive::MemoryStore::parse(s) {
                    Some(ms) => Some(ms),
                    None => {
                        return Ok(text_output(
                            &action,
                            format!(
                                "Unknown store: {s}. Use: facts, history, procedures, patterns"
                            ),
                        ));
                    }
                },
                None => None,
            };
            let limit = get_f64(args, "limit")
                .map_or(crate::tools::ctx_knowledge::DEFAULT_RESTORE_LIMIT, |v| {
                    v as usize
                });
            let opts =
                crate::tools::ctx_knowledge::RestoreOptions::new(store, query.clone(), limit);
            let text = match crate::tools::ctx_knowledge::run_restore(&project_root, &opts) {
                Ok(report) => crate::tools::ctx_knowledge::format_restore_report(&report),
                Err(e) => e,
            };
            return Ok(text_output(&action, text));
        }

        // Dry-run consolidate: preview imports + reclaim with no writes.
        let dry_run = args
            .get("dry_run")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if action == "consolidate" && dry_run {
            let text = match crate::tools::ctx_knowledge::consolidate_project_knowledge_with(
                &project_root,
                &crate::core::consolidation_engine::ConsolidateOptions::manual().into_dry_run(),
            ) {
                Ok(report) => crate::tools::ctx_knowledge::format_consolidation_report(&report),
                Err(e) => e,
            };
            return Ok(text_output(&action, text));
        }

        let result = crate::tools::ctx_knowledge::handle(
            &project_root,
            &action,
            category.as_deref(),
            key.as_deref(),
            value.as_deref(),
            query.as_deref(),
            &session_id,
            pattern_type.as_deref(),
            examples,
            confidence,
            mode.as_deref(),
            as_of.as_deref(),
        );

        Ok(text_output(&action, result))
    }
}

/// A plain text `ToolOutput` tagged with the action as its mode. `ctx_knowledge`
/// results are already compressed prose, so token accounting is left at zero.
fn text_output(action: &str, text: String) -> ToolOutput {
    ToolOutput {
        text,
        original_tokens: 0,
        saved_tokens: 0,
        mode: Some(action.to_string()),
        path: None,
        changed: false,
        shell_outcome: None,
    }
}
