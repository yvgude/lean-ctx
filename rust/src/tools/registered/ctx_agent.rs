use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_bool, get_str};
use crate::tool_defs::tool_def;

pub struct CtxAgentTool;

impl McpTool for CtxAgentTool {
    fn name(&self) -> &'static str {
        "ctx_agent"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_agent",
            "Multi-agent coordination — shared message bus, persistent diaries, stigmergic scent field. \
Actions: register (agent_type+role), post (message+category), read (poll), \
status (active|idle|finished), handoff (task+summary), sync (agents+messages+scent), \
claim/release (file/task claim), brief (sub-agent briefing), \
return (distill report into knowledge), diary, recall_diary, list, info. \
Use when orchestrating multiple LLM agents across a shared workspace.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["register", "list", "post", "read", "status", "info", "handoff", "sync", "claim", "release", "brief", "return", "diary", "recall_diary", "diaries", "share_knowledge", "receive_knowledge"],
                        "description": "register|list|post|read|status|info|handoff|sync|claim|release|brief|return|diary|recall_diary|diaries|share_knowledge|receive_knowledge"
                    },
                    "agent_type": {
                        "type": "string",
                        "description": "cursor|claude|codex|gemini|crush|subagent"
                    },
                    "role": {
                        "type": "string",
                        "description": "dev|review|test|plan"
                    },
                    "message": {
                        "type": "string",
                        "description": "Post text or status detail"
                    },
                    "category": {
                        "type": "string",
                        "description": "finding|warning|request|status"
                    },
                    "to_agent": {
                        "type": "string",
                        "description": "Target agent ID"
                    },
                    "status": {
                        "type": "string",
                        "enum": ["active", "idle", "finished"],
                        "description": "active|idle|finished"
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
        let agent_type = get_str(args, "agent_type");
        let role = get_str(args, "role");
        let message = get_str(args, "message");
        let category = get_str(args, "category");
        let to_agent = get_str(args, "to_agent");
        let status = get_str(args, "status");
        let privacy = get_str(args, "privacy");
        let priority = get_str(args, "priority");
        let ttl_hours: Option<u64> = args.get("ttl_hours").and_then(serde_json::Value::as_u64);
        let format = get_str(args, "format");
        let write = get_bool(args, "write").unwrap_or(false);
        let filename = get_str(args, "filename");

        let project_root = ctx.project_root.clone();

        let agent_id_handle = ctx.agent_id.as_ref();
        let current_agent_id = agent_id_handle
            .map(|a| a.blocking_read().clone())
            .unwrap_or_default();

        let result = crate::tools::ctx_agent::handle(
            &action,
            agent_type.as_deref(),
            role.as_deref(),
            &project_root,
            current_agent_id.as_deref(),
            message.as_deref(),
            category.as_deref(),
            to_agent.as_deref(),
            status.as_deref(),
            privacy.as_deref(),
            priority.as_deref(),
            ttl_hours,
            format.as_deref(),
            write,
            filename.as_deref(),
        );

        if action == "register" {
            if let Some(id) = result.split(':').nth(1) {
                let id = id.split_whitespace().next().unwrap_or("").to_string();
                if !id.is_empty()
                    && let Some(handle) = agent_id_handle
                {
                    let mut guard = handle.blocking_write();
                    *guard = Some(id);
                }
            }

            let agent_role =
                crate::core::agents::AgentRole::from_str_loose(role.as_deref().unwrap_or("coder"));
            let depth = crate::core::agents::ContextDepthConfig::for_role(agent_role);
            let depth_hint = format!(
                "\n[context] role={:?} preferred_mode={} max_full={} max_sig={} budget_ratio={:.0}%",
                agent_role,
                depth.preferred_mode,
                depth.max_files_full,
                depth.max_files_signatures,
                depth.context_budget_ratio * 100.0,
            );
            return Ok(ToolOutput {
                text: format!("{result}{depth_hint}"),
                original_tokens: 0,
                saved_tokens: 0,
                mode: Some(action),
                path: None,
                changed: false,
                shell_outcome: None,
            });
        }

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
