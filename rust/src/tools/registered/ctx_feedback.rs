use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_int, get_str};
use crate::tool_defs::tool_def;

pub struct CtxFeedbackTool;

impl McpTool for CtxFeedbackTool {
    fn name(&self) -> &'static str {
        "ctx_feedback"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_feedback",
            "Record and report LLM token/latency metrics (local-first) — use to track efficiency.\n\
             Actions: record (log event), report (readable summary), json (machine-readable),\n\
             reset (clear data), status (storage info). record requires llm_input_tokens + llm_output_tokens.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["record", "report", "json", "reset", "status"],
                        "description": "record (log event) | report (summary) | json (data) | reset (clear) | status (storage)"
                    },
                    "agent_id": { "type": "string", "description": "Agent ID (default: current agent)" },
                    "intent": { "type": "string", "description": "Intent/task string" },
                    "model": { "type": "string", "description": "Model identifier" },
                    "llm_input_tokens": { "type": "integer", "description": "Required for action=record" },
                    "llm_output_tokens": { "type": "integer", "description": "Required for action=record" },
                    "latency_ms": { "type": "integer", "description": "Latency in ms (for record)" },
                    "note": { "type": "string", "description": "Note (no prompts/PII)" },
                    "limit": { "type": "integer", "description": "Max recent events (default: 500)" }
                }
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let action = get_str(args, "action").unwrap_or_else(|| "report".to_string());
        let limit = get_int(args, "limit").map_or(500, |n| n.max(1) as usize);

        let result = match action.as_str() {
            "record" => {
                let current_agent_id = ctx
                    .agent_id
                    .as_ref()
                    .and_then(|a| tokio::task::block_in_place(|| a.blocking_read()).clone());
                let agent_id = get_str(args, "agent_id").or(current_agent_id);
                let agent_id = agent_id.ok_or_else(|| {
                    ErrorData::invalid_params(
                        "agent_id is required (or register an agent via project_root detection first)",
                        None,
                    )
                })?;

                let (ctx_read_last_mode, ctx_read_modes) = if let Some(ref tc) = ctx.tool_calls {
                    let calls = tokio::task::block_in_place(|| tc.blocking_read());
                    let mut last: Option<String> = None;
                    let mut modes: std::collections::BTreeMap<String, u64> =
                        std::collections::BTreeMap::new();
                    for rec in calls.iter().rev().take(50) {
                        if rec.tool != "ctx_read" {
                            continue;
                        }
                        if let Some(m) = rec.mode.as_ref() {
                            *modes.entry(m.clone()).or_insert(0) += 1;
                            if last.is_none() {
                                last = Some(m.clone());
                            }
                        }
                    }
                    (last, if modes.is_empty() { None } else { Some(modes) })
                } else {
                    (None, None)
                };

                let llm_input_tokens = get_int(args, "llm_input_tokens").ok_or_else(|| {
                    ErrorData::invalid_params("llm_input_tokens is required", None)
                })?;
                let llm_output_tokens = get_int(args, "llm_output_tokens").ok_or_else(|| {
                    ErrorData::invalid_params("llm_output_tokens is required", None)
                })?;
                if llm_input_tokens <= 0 || llm_output_tokens <= 0 {
                    return Err(ErrorData::invalid_params(
                        "llm_input_tokens and llm_output_tokens must be > 0",
                        None,
                    ));
                }

                let ev = crate::core::llm_feedback::LlmFeedbackEvent {
                    agent_id,
                    intent: get_str(args, "intent"),
                    model: get_str(args, "model"),
                    llm_input_tokens: llm_input_tokens as u64,
                    llm_output_tokens: llm_output_tokens as u64,
                    latency_ms: get_int(args, "latency_ms").map(|n| n.max(0) as u64),
                    note: get_str(args, "note"),
                    ctx_read_last_mode,
                    ctx_read_modes,
                    timestamp: chrono::Local::now().to_rfc3339(),
                };
                crate::tools::ctx_feedback::record(&ev)
                    .unwrap_or_else(|e| format!("Error recording feedback: {e}"))
            }
            "status" => crate::tools::ctx_feedback::status(),
            "json" => crate::tools::ctx_feedback::json(limit),
            "reset" => crate::tools::ctx_feedback::reset(),
            _ => crate::tools::ctx_feedback::report(limit),
        };

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
