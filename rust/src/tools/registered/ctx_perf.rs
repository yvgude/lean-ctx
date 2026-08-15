use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput};
use crate::tool_defs::tool_def;

pub struct CtxPerfTool;

impl McpTool for CtxPerfTool {
    fn name(&self) -> &'static str {
        "ctx_perf"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_perf",
            "Current-session proxy compression performance, agent budget, and triage profile.",
            json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        )
    }

    fn handle(
        &self,
        _args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let metrics = crate::proxy::value_gate_proxy::session_metrics();
        let leaderboard = crate::proxy::leaderboard::compute_current_rank();
        let leaderboard_message = crate::proxy::leaderboard::format_rank_message(&leaderboard);
        let result = json!({
            "request_count": metrics.request_count,
            "tokens_saved_total": metrics.total_tokens_pruned,
            "avg_compression_ratio": crate::proxy::value_gate_proxy::compression_ratio(),
            "turn_budget_remaining": turn_budget_remaining(ctx),
            "triage_profile": triage_profile(ctx),
            "leaderboard": {
                "entry": leaderboard,
                "message": leaderboard_message,
            },
        });

        Ok(ToolOutput::simple(result.to_string()))
    }

    fn produces_machine_readable(&self, _args: Option<&Map<String, Value>>) -> bool {
        true
    }
}

fn turn_budget_remaining(ctx: &ToolContext) -> Option<usize> {
    let agent_id = ctx
        .agent_id
        .as_ref()
        .and_then(|agent_id| agent_id.try_read().ok().and_then(|id| id.clone()))?;
    let budget = crate::core::agent_budget::get_status(&agent_id);
    (budget.token_limit != usize::MAX)
        .then(|| budget.token_limit.saturating_sub(budget.tokens_consumed))
}

fn triage_profile(ctx: &ToolContext) -> Value {
    let Some(task) = ctx
        .session
        .as_ref()
        .and_then(|session| session.try_read().ok())
        .and_then(|session| session.task.as_ref().map(|task| task.description.clone()))
    else {
        return Value::Null;
    };

    crate::core::triage::TriageEngine::with_rules()
        .analyze(&crate::core::triage::TaskAnalysisInput {
            query: task,
            ..Default::default()
        })
        .ok()
        .and_then(|hypothesis| serde_json::to_value(hypothesis.profile).ok())
        .unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::server::tool_trait::McpTool;

    fn output(ctx: &ToolContext) -> Value {
        let tool = CtxPerfTool;
        let output = tool.handle(&Map::new(), ctx).unwrap();
        serde_json::from_str(&output.text).unwrap()
    }

    #[test]
    fn schema_accepts_no_arguments() {
        let schema = serde_json::to_value(CtxPerfTool.tool_def().input_schema).unwrap();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["required"], json!([]));
        assert_eq!(schema["properties"], json!({}));
    }

    #[test]
    fn output_contains_the_machine_readable_performance_fields() {
        let output = output(&ToolContext::default());
        let fields = output.as_object().unwrap();
        assert!(
            fields.len() >= 5,
            "expected at least 5 perf fields, got {}",
            fields.len()
        );
        assert!(fields["request_count"].is_u64());
        assert!(fields["tokens_saved_total"].is_u64());
        assert!(fields["avg_compression_ratio"].is_number());
        assert!(fields["turn_budget_remaining"].is_null());
        assert!(fields["triage_profile"].is_null());
    }

    #[test]
    fn output_reports_the_current_agents_remaining_budget() {
        let agent_id = format!("ctx_perf_test_{}", std::process::id());
        crate::core::agent_budget::set_limit(&agent_id, 1_000);
        crate::core::agent_budget::record_consumption(&agent_id, 250);
        let ctx = ToolContext {
            agent_id: Some(Arc::new(tokio::sync::RwLock::new(Some(agent_id.clone())))),
            ..Default::default()
        };

        assert_eq!(output(&ctx)["turn_budget_remaining"], 750);
        crate::core::agent_budget::remove(&agent_id);
    }
}
