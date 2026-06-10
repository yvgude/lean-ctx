use rmcp::model::Tool;
use rmcp::ErrorData;
use serde_json::{json, Map, Value};

use crate::server::tool_trait::{get_str, get_str_array, McpTool, ToolContext, ToolOutput};
use crate::tool_defs::tool_def;

pub struct CtxKnowledgeTool;

impl McpTool for CtxKnowledgeTool {
    fn name(&self) -> &'static str {
        "ctx_knowledge"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_knowledge",
            "Persistent project knowledge across sessions (facts, patterns, history). Supports recall modes, embeddings, feedback, and typed relations.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["policy", "remember", "recall", "pattern", "feedback", "relate", "unrelate", "relations", "relations_diagram", "consolidate", "status", "health", "remove", "export", "timeline", "rooms", "search", "wakeup", "embeddings_status", "embeddings_reset", "embeddings_reindex"],
                        "description": "Knowledge operation to perform."
                    },
                    "trigger": {
                        "type": "string",
                        "description": "For gotcha action: what triggers the bug"
                    },
                    "resolution": {
                        "type": "string",
                        "description": "For gotcha action: how to fix/avoid it"
                    },
                    "severity": {
                        "type": "string",
                        "enum": ["critical", "warning", "info"],
                        "description": "For gotcha action: severity level (default: warning)"
                    },
                    "category": {
                        "type": "string",
                        "description": "Fact category (architecture, api, testing, deployment, conventions, dependencies)"
                    },
                    "key": {
                        "type": "string",
                        "description": "Fact key/identifier"
                    },
                    "value": {
                        "type": "string",
                        "description": "Value for action (fact value, pattern text, feedback up/down, relation kind)."
                    },
                    "query": {
                        "type": "string",
                        "description": "Query/target for recall/relate/relations."
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["auto", "exact", "semantic", "hybrid"],
                        "description": "Recall mode (default: auto)."
                    },
                    "as_of": {
                        "type": "string",
                        "description": "Temporal recall: only facts valid at this time (RFC 3339 or YYYY-MM-DD). Shows superseded facts with validity windows."
                    },
                    "pattern_type": {
                        "type": "string",
                        "description": "Pattern type for pattern action"
                    },
                    "examples": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Examples for pattern action"
                    },
                    "confidence": {
                        "type": "number",
                        "description": "Confidence score 0.0-1.0 for remember action (default: 0.8)"
                    }
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
        let confidence: Option<f32> = args
            .get("confidence")
            .and_then(serde_json::Value::as_f64)
            .map(|v| v as f32);

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
                return Ok(ToolOutput {
                    text: "ERROR: trigger and resolution are required for gotcha action"
                        .to_string(),
                    original_tokens: 0,
                    saved_tokens: 0,
                    mode: Some(action),
                    path: None,
                    changed: false,
                });
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
            return Ok(ToolOutput {
                text: msg,
                original_tokens: 0,
                saved_tokens: 0,
                mode: Some(action),
                path: None,
                changed: false,
            });
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

        Ok(ToolOutput {
            text: result,
            original_tokens: 0,
            saved_tokens: 0,
            mode: Some(action),
            path: None,
            changed: false,
        })
    }
}
