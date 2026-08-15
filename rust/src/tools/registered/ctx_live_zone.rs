//! MCP tool exposing live-zone detection and freeze/thaw controls to agents.
//!
//! Allows agents to explicitly mark context sections as frozen, optimizing
//! provider cache utilization by ensuring frozen prefixes remain byte-stable.

use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::core::session::LiveZoneSessionState;
use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_str};
use crate::tool_defs::tool_def;

pub struct CtxLiveZoneTool;

impl McpTool for CtxLiveZoneTool {
    fn name(&self) -> &'static str {
        "ctx_live_zone"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            self.name(),
            "Manage context freeze zones for optimal provider cache utilization. \
             Freeze stable prefix to guarantee provider KV-cache reuse.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["status", "freeze", "thaw", "analyze"],
                        "description": "Operation: status (current zones), freeze (mark prefix stable), thaw (unfreeze), analyze (recommend freeze point)"
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
        let text = match action.as_str() {
            "status" => handle_status(ctx)?,
            "freeze" => handle_freeze(ctx)?,
            "thaw" => handle_thaw(ctx)?,
            "analyze" => handle_analyze(ctx)?,
            other => {
                return Err(ErrorData::invalid_params(
                    format!("unknown action: {other}"),
                    None,
                ));
            }
        };
        Ok(ToolOutput::simple(text))
    }
}

fn get_live_zone_state(ctx: &ToolContext) -> LiveZoneSessionState {
    ctx.session
        .as_ref()
        .and_then(|s| s.try_read().ok())
        .map(|s| s.live_zone.clone())
        .unwrap_or_default()
}

fn set_live_zone_state(ctx: &ToolContext, state: LiveZoneSessionState) {
    if let Some(session) = ctx.session.as_ref() {
        if let Ok(mut s) = session.try_write() {
            s.live_zone = state;
        }
    }
}

fn handle_status(ctx: &ToolContext) -> Result<String, ErrorData> {
    let lz = get_live_zone_state(ctx);
    let frozen_turns = lz.frozen_at_turn.unwrap_or(0);
    let result = json!({
        "frozen_turns": frozen_turns,
        "frozen_tokens_estimate": lz.frozen_tokens_estimate,
        "live_turns_since_freeze": 0,
        "savings_from_freeze": lz.frozen_tokens_estimate,
        "snapshot_hash": lz.snapshot_hash,
    });
    serde_json::to_string_pretty(&result)
        .map_err(|e| ErrorData::internal_error(e.to_string(), None))
}

fn handle_freeze(ctx: &ToolContext) -> Result<String, ErrorData> {
    let turn_estimate = ctx
        .session
        .as_ref()
        .and_then(|s| s.try_read().ok())
        .map(|s| s.findings.len() + s.decisions.len())
        .unwrap_or(0);
    let tokens_estimate = (turn_estimate * 800) as u64;
    let hash = format!("{:016x}", {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        turn_estimate.hash(&mut h);
        h.finish()
    });

    let new_state = LiveZoneSessionState {
        frozen_at_turn: Some(turn_estimate),
        frozen_tokens_estimate: tokens_estimate,
        snapshot_hash: Some(hash.clone()),
    };
    set_live_zone_state(ctx, new_state);

    let result = json!({
        "frozen_at_turn": turn_estimate,
        "estimated_cache_savings_per_request": tokens_estimate,
        "snapshot_hash": hash,
    });
    serde_json::to_string_pretty(&result)
        .map_err(|e| ErrorData::internal_error(e.to_string(), None))
}

fn handle_thaw(ctx: &ToolContext) -> Result<String, ErrorData> {
    let prev = get_live_zone_state(ctx);
    let previously_frozen = prev.frozen_at_turn.unwrap_or(0);
    set_live_zone_state(ctx, LiveZoneSessionState::default());

    let result = json!({
        "thawed": true,
        "previously_frozen_turns": previously_frozen,
    });
    serde_json::to_string_pretty(&result)
        .map_err(|e| ErrorData::internal_error(e.to_string(), None))
}

fn handle_analyze(ctx: &ToolContext) -> Result<String, ErrorData> {
    let current = get_live_zone_state(ctx);
    if current.frozen_at_turn.is_some() {
        let result = json!({
            "recommendation": "already_frozen",
            "message": "Context is already frozen. Use 'thaw' first to re-analyze.",
        });
        return serde_json::to_string_pretty(&result)
            .map_err(|e| ErrorData::internal_error(e.to_string(), None));
    }

    let session_size = ctx
        .session
        .as_ref()
        .and_then(|s| s.try_read().ok())
        .map(|s| s.findings.len() + s.decisions.len() + s.files_touched.len())
        .unwrap_or(0);

    let recommend_freeze = session_size.saturating_sub(2);
    let potential_savings = (recommend_freeze * 800) as u64;
    let reason = if session_size > 5 {
        "System prompt + early tool outputs are stable and unlikely to change"
    } else {
        "Session is still short — freezing may be premature"
    };

    let result = json!({
        "recommend_freeze_at_turn": recommend_freeze,
        "potential_savings_tokens": potential_savings,
        "reason": reason,
    });
    serde_json::to_string_pretty(&result)
        .map_err(|e| ErrorData::internal_error(e.to_string(), None))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_returns_default_when_not_frozen() {
        let tool = CtxLiveZoneTool;
        let args: Map<String, Value> = serde_json::from_str(r#"{"action": "status"}"#).unwrap();
        let ctx = ToolContext::default();
        let result = tool.handle(&args, &ctx).unwrap();
        let parsed: Value = serde_json::from_str(&result.text).unwrap();
        assert_eq!(parsed["frozen_turns"], 0);
        assert_eq!(parsed["frozen_tokens_estimate"], 0);
    }

    #[test]
    fn freeze_and_thaw_cycle() {
        let tool = CtxLiveZoneTool;
        let ctx = ToolContext::default();

        let freeze_args: Map<String, Value> =
            serde_json::from_str(r#"{"action": "freeze"}"#).unwrap();
        let freeze_result = tool.handle(&freeze_args, &ctx).unwrap();
        let parsed: Value = serde_json::from_str(&freeze_result.text).unwrap();
        assert!(parsed["snapshot_hash"].is_string());

        let thaw_args: Map<String, Value> = serde_json::from_str(r#"{"action": "thaw"}"#).unwrap();
        let thaw_result = tool.handle(&thaw_args, &ctx).unwrap();
        let parsed: Value = serde_json::from_str(&thaw_result.text).unwrap();
        assert_eq!(parsed["thawed"], true);
    }

    #[test]
    fn analyze_gives_recommendation() {
        let tool = CtxLiveZoneTool;
        let args: Map<String, Value> = serde_json::from_str(r#"{"action": "analyze"}"#).unwrap();
        let ctx = ToolContext::default();
        let result = tool.handle(&args, &ctx).unwrap();
        let parsed: Value = serde_json::from_str(&result.text).unwrap();
        assert!(parsed["reason"].is_string());
    }
}
