// SPDX-License-Identifier: Apache-2.0
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
            "Local multi-agent coordination helper for explicit opt-in sessions. It is not a hosted workflow service.\n\
             Active presence uses a bounded lease; read or status=active renews it.\n\
             Actions: register, list, post, read, status, info, handoff, sync, poll_events, export,\n\
              claim/release, lease_acquire/lease_renew/lease_release, brief/return, diary/recall_diary/diaries,\n\
              share_knowledge/receive_knowledge, control_status. Directed events require an agent filter when read.\n\
             Leases (message=path or symbol:<name>; renew/release take category=lease_ref) are machine-wide:\n\
              every lean-ctx process sharing the data dir sees the same holder.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": crate::tools::ctx_agent::ACTIONS,
                        "description": crate::tools::ctx_agent::ACTIONS.join("|")
                    },
                    "agent_type": {
                        "type": "string",
                        "description": "cursor|claude|codex|gemini|crush|subagent"
                    },
                    "role": {
                        "type": "string",
                        "description": "dev|review|test|plan"
                    },
                    "durable_identity_id": {
                        "type": "string",
                        "description": "Explicit durable identity ID to bind on register; never inferred"
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
                    },
                    "privacy": {
                        "type": "string",
                        "description": "public|team|private; poll_events accepts comma-separated event kinds"
                    },
                    "priority": {
                        "type": "string",
                        "description": "low|normal|high|critical; brief also accepts a numeric token budget"
                    },
                    "ttl_hours": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "message TTL in hours; lease_acquire/lease_renew: 0 = 10 min (default), 1 = 1 h"
                    },
                    "format": {
                        "type": "string",
                        "enum": ["json", "text"],
                        "description": "export output format"
                    },
                    "write": {
                        "type": "boolean",
                        "description": "export a proof snapshot under the project proofs directory"
                    },
                    "filename": {
                        "type": "string",
                        "description": "plain proof filename for export (no directories)"
                    }
                },
                "allOf": [
                    { "if": { "properties": { "action": { "const": "post" } }, "required": ["action"] }, "then": { "required": ["action", "message"] } },
                    { "if": { "properties": { "action": { "const": "status" } }, "required": ["action"] }, "then": { "required": ["action", "status"] } },
                    { "if": { "properties": { "action": { "const": "handoff" } }, "required": ["action"] }, "then": { "required": ["action", "to_agent"] } },
                    { "if": { "properties": { "action": { "const": "claim" } }, "required": ["action"] }, "then": { "required": ["action", "message"] } },
                    { "if": { "properties": { "action": { "const": "release" } }, "required": ["action"] }, "then": { "required": ["action", "message"] } },
                    { "if": { "properties": { "action": { "const": "brief" } }, "required": ["action"] }, "then": { "required": ["action", "message"] } },
                    { "if": { "properties": { "action": { "const": "return" } }, "required": ["action"] }, "then": { "required": ["action", "message"] } },
                    { "if": { "properties": { "action": { "const": "diary" } }, "required": ["action"] }, "then": { "required": ["action", "message"] } },
                    { "if": { "properties": { "action": { "const": "share_knowledge" } }, "required": ["action"] }, "then": { "required": ["action", "message"] } },
                    { "if": { "properties": { "action": { "const": "lease_acquire" } }, "required": ["action"] }, "then": { "required": ["action", "message"] } },
                    { "if": { "properties": { "action": { "const": "lease_renew" } }, "required": ["action"] }, "then": { "required": ["action", "message", "category"] } },
                    { "if": { "properties": { "action": { "const": "lease_release" } }, "required": ["action"] }, "then": { "required": ["action", "message", "category"] } }
                ],
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
        let durable_identity_id = if action == "register" {
            get_str(args, "durable_identity_id")
        } else {
            None
        };
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
            durable_identity_id.as_deref(),
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
                content_blocks: None,
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
            content_blocks: None,
        })
    }
}

#[cfg(test)]
mod schema_tests {
    use super::*;
    use crate::server::tool_trait::McpTool;
    use crate::tools::ctx_agent::ACTIONS;

    #[test]
    fn schema_advertises_optional_durable_identity_binding() {
        let schema = serde_json::to_string(&CtxAgentTool.tool_def()).expect("tool schema");
        assert!(schema.contains("durable_identity_id"));
        assert!(schema.contains("Explicit durable identity ID"));
    }

    #[test]
    fn schema_exposes_each_bus_action_without_combined_enum_values() {
        let schema = serde_json::to_value(CtxAgentTool.tool_def()).expect("tool schema");
        let action_enum = schema["inputSchema"]["properties"]["action"]["enum"]
            .as_array()
            .expect("action enum");
        for action in [
            "poll_events",
            "export",
            "control_status",
            "lease_acquire",
            "lease_renew",
            "lease_release",
        ] {
            assert!(
                action_enum.iter().any(|value| value == action),
                "missing action {action}"
            );
        }
        assert!(
            !action_enum
                .iter()
                .any(|value| value.as_str().is_some_and(|value| value.contains('|')))
        );
        assert!(schema["inputSchema"]["properties"]["privacy"].is_object());
        assert!(schema["inputSchema"]["properties"]["ttl_hours"].is_object());
    }

    /// #1913: a merged `a|b|c` entry advertised an action no client could send.
    #[test]
    fn action_enum_lists_each_dispatched_action_once() {
        let tool = CtxAgentTool.tool_def();
        let schema = serde_json::Value::Object((*tool.input_schema).clone());
        let listed: Vec<&str> = schema["properties"]["action"]["enum"]
            .as_array()
            .expect("action enum")
            .iter()
            .map(|v| v.as_str().expect("string entry"))
            .collect();
        assert_eq!(listed, ACTIONS);
        let mut unique = listed.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), listed.len(), "no duplicate entries");
        for action in &listed {
            assert!(
                action.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'),
                "enum entry {action:?} must be a single action name"
            );
            let arm = format!("\"{action}\"");
            assert!(
                DISPATCH_SOURCE.lines().any(|line| {
                    let line = line.trim_start();
                    line.starts_with(&arm) && line.contains("=>")
                }),
                "advertised action {action:?} is not dispatched"
            );
        }
    }

    const DISPATCH_SOURCE: &str = include_str!("../ctx_agent.rs");
}
