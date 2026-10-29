// SPDX-License-Identifier: Apache-2.0
use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_str};
use crate::tool_defs::tool_def;

pub struct CtxWorkGraphTool;

impl McpTool for CtxWorkGraphTool {
    fn name(&self) -> &'static str {
        "ctx_work_graph"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_work_graph",
            "Free persistent project-scoped bounded local Work Graph for multi-agent coordination. The host launches agents; start binds a host-launched child to its node (verified context capsule, leases, launch env with the child's delivery profile), receipt or cancel ends it and a stop revokes its delivery. Actions: create|delegate|claim|start|consume|complete|receipt|accept|fuse|attribution|value_report|cancel|get|observe|list. Observe returns compact state without context payloads or execution fences. Node path claims share one lease namespace with ctx_agent path leases: an agent holding src blocks starting a node that claims src/x.",
            json!({
                "type": "object",
                "properties": {
                    "action": {"type":"string","enum":["create","delegate","claim","start","consume","complete","receipt","accept","fuse","attribution","value_report","cancel","get","observe","list"]},
                    "graph_id": {"type":"string","minLength":1,"maxLength":128},
                    "if_revision": {"type":"string","pattern":"^[0-9a-f]{64}$","description":"For observe: return only unchanged=true and revision if state matches. For delegate|claim|start|consume|complete|receipt|accept|cancel: the write_revision from observe; the write fails with a revision conflict and changes nothing if the graph changed since. Refused on other actions. Authorization is still enforced."},
                    "node_id": {"type":"string","minLength":1,"maxLength":128},
                    "parent_node_id": {"type":"string","minLength":1,"maxLength":128},
                    "to_agent": {"type":"string","minLength":1,"maxLength":128},
                    "capsule_ref": {"type":"string","minLength":1,"maxLength":4096},
                    "outcome_ref": {"type":"string","minLength":1,"maxLength":4096},
                    "tokens": {"type":"integer","minimum":0},
                    "cost_micros": {"type":"integer","minimum":0}
                    ,"max_concurrency": {"type":"integer","minimum":1,"maximum":16}
                    ,"receipt_id": {"type":"string","minLength":1,"maxLength":128}
                    ,"execution_fence": {"type":"string","pattern":"^fence:[0-9a-f]{64}$"}
                    ,"outcome": {"type":"string","enum":["accepted","rejected","partial"]}
                    ,"claims": {"type":"array","maxItems":64,"items":{"type":"object","properties":{"key":{"type":"string","minLength":1,"maxLength":128},"value":{"type":"string","maxLength":4096}},"required":["key","value"],"additionalProperties":false}}
                    ,"accepted_nodes": {"type":"array","minItems":1,"maxItems":256,"items":{"type":"string","minLength":1,"maxLength":128}}
                    ,"stop_reason": {"type":"string","enum":["manual_stop","stale","redundant","policy_denied","lease_lost","duplicate","low_value","execution_failed"]}
                    ,"timeout_ms": {"type":"integer","minimum":1,"maximum":1800000,"description":"Lease window for claim/start in ms (start: at most 30 min, default 5 min)."}
                    ,"path_claims": {"type":"array","maxItems":64,"items":{"type":"string","pattern":"^path:[^/\\\\](?:[^\\r\\n]*[^/\\\\])?$","maxLength":517}}
                    ,"task_ref": {"type":"string","minLength":1,"maxLength":128}
                    ,"policy_ref": {"type":"string","minLength":1,"maxLength":128}
                    ,"expected_outcome_ref": {"type":"string","minLength":1,"maxLength":128}
                },
                "required": ["action"],
                "additionalProperties": false
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
        // Free local reference behavior has no billing dependency. Registration,
        // project scope, ownership, bounded execution and policy still apply.
        let graph_id = get_str(args, "graph_id").unwrap_or_default();
        let agent_id = ctx
            .agent_id
            .as_ref()
            .map(|agent| agent.blocking_read().clone())
            .unwrap_or_default();
        let changed = matches!(
            action.as_str(),
            "create"
                | "delegate"
                | "claim"
                | "start"
                | "consume"
                | "complete"
                | "receipt"
                | "accept"
                | "cancel"
        );
        let result = crate::tools::ctx_work_graph::handle(crate::tools::ctx_work_graph::Request {
            action: &action,
            project_root: &ctx.project_root,
            agent_id: agent_id.as_deref().unwrap_or_default(),
            graph_id: &graph_id,
            if_revision: get_str(args, "if_revision").as_deref(),
            node_id: get_str(args, "node_id").as_deref(),
            parent_node_id: get_str(args, "parent_node_id").as_deref(),
            to_agent: get_str(args, "to_agent").as_deref(),
            capsule_ref: get_str(args, "capsule_ref").as_deref(),
            outcome_ref: get_str(args, "outcome_ref").as_deref(),
            tokens: args.get("tokens").and_then(Value::as_u64),
            cost_micros: args.get("cost_micros").and_then(Value::as_u64),
            max_concurrency: args
                .get("max_concurrency")
                .and_then(Value::as_u64)
                .and_then(|value| usize::try_from(value).ok()),
            receipt_id: get_str(args, "receipt_id").as_deref(),
            execution_fence: get_str(args, "execution_fence").as_deref(),
            outcome: get_str(args, "outcome").as_deref(),
            claims: args.get("claims"),
            accepted_nodes: args.get("accepted_nodes"),
            stop_reason: get_str(args, "stop_reason").as_deref(),
            timeout_ms: args.get("timeout_ms").and_then(Value::as_u64),
            path_claims: args.get("path_claims"),
            task_ref: get_str(args, "task_ref").as_deref(),
            policy_ref: get_str(args, "policy_ref").as_deref(),
            expected_outcome_ref: get_str(args, "expected_outcome_ref").as_deref(),
        })
        .map_err(|error| ErrorData::invalid_params(error, None))?;
        Ok(ToolOutput {
            text: result,
            original_tokens: 0,
            saved_tokens: 0,
            mode: Some(action),
            path: None,
            changed,
            shell_outcome: None,
            content_blocks: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn work_graph_is_free_and_keeps_registration_isolation_and_policy() {
        let isolated = crate::core::data_dir::isolated_data_dir();
        let args = Map::from_iter([("action".into(), Value::String("list".into()))]);
        let context = ToolContext {
            project_root: isolated.path().to_string_lossy().into_owned(),
            ..ToolContext::default()
        };

        let Err(error) = CtxWorkGraphTool.handle(&args, &context) else {
            panic!("the dispatcher must enforce agent registration");
        };
        assert_eq!(
            error.message,
            "agent must be registered first via ctx_agent"
        );

        let mut registry = crate::core::agents::AgentRegistry::new();
        let agent = registry
            .register(
                "codex",
                Some("signed graph test"),
                &context.project_root,
                None,
            )
            .unwrap();
        registry.save().unwrap();
        let context = ToolContext {
            agent_id: Some(std::sync::Arc::new(tokio::sync::RwLock::new(Some(agent)))),
            ..context
        };
        let create = json!({
            "action": "create", "graph_id": "paid-graph", "node_id": "root",
            "capsule_ref": "capsule:root", "task_ref": "task:root",
            "policy_ref": "policy:root", "expected_outcome_ref": "outcome:root",
            "tokens": 100, "cost_micros": 50
        });
        let created = CtxWorkGraphTool
            .handle(create.as_object().unwrap(), &context)
            .unwrap();
        assert!(created.changed);
        let listed = CtxWorkGraphTool.handle(&args, &context).unwrap();
        assert!(!listed.changed);
        let listed: Value = serde_json::from_str(&listed.text).unwrap();
        assert_eq!(listed["graph_ids"], json!(["paid-graph"]));
        let get = json!({"action": "get", "graph_id": "paid-graph"});
        let before = CtxWorkGraphTool
            .handle(get.as_object().unwrap(), &context)
            .unwrap();
        let observe = json!({"action": "observe", "graph_id": "paid-graph"});
        let observed = CtxWorkGraphTool
            .handle(observe.as_object().unwrap(), &context)
            .unwrap();
        assert!(!observed.changed);
        let observation: Value = serde_json::from_str(&observed.text).unwrap();
        assert!(observation.get("nodes").is_some());
        assert!(!observed.text.contains("capsule:root"));
        let conditional = json!({
            "action": "observe", "graph_id": "paid-graph",
            "if_revision": observation["revision"]
        });
        let unchanged = CtxWorkGraphTool
            .handle(conditional.as_object().unwrap(), &context)
            .unwrap();
        assert!(!unchanged.changed);
        let unchanged: Value = serde_json::from_str(&unchanged.text).unwrap();
        assert_eq!(unchanged["unchanged"], true);
        assert!(unchanged.get("nodes").is_none());
        let wrong_project = ToolContext {
            project_root: isolated
                .path()
                .join("other-project")
                .to_string_lossy()
                .into_owned(),
            agent_id: context.agent_id.clone(),
            ..ToolContext::default()
        };
        assert!(
            CtxWorkGraphTool
                .handle(conditional.as_object().unwrap(), &wrong_project)
                .is_err()
        );
        assert_eq!(
            before.text,
            CtxWorkGraphTool
                .handle(get.as_object().unwrap(), &context)
                .unwrap()
                .text
        );
        let policy_dir = std::path::Path::new(&context.project_root).join(".lean-ctx");
        std::fs::create_dir_all(&policy_dir).unwrap();
        std::fs::write(policy_dir.join("policy.toml"),
            "name = \"deny-execution\"\nversion = \"1.0.0\"\ndescription = \"test\"\n[context]\ndeny_tools = [\"ctx_work_graph\"]\n").unwrap();
        let start = json!({
            "action": "start", "graph_id": "paid-graph", "node_id": "root"
        });
        let error = CtxWorkGraphTool
            .handle(start.as_object().unwrap(), &context)
            .err()
            .expect("project policy must deny execution");
        assert_eq!(error.message, "project policy denies Work Graph execution");
        let after = CtxWorkGraphTool
            .handle(get.as_object().unwrap(), &context)
            .unwrap();
        assert_eq!(
            before.text, after.text,
            "denial must not claim or mutate the graph"
        );
        std::fs::remove_file(policy_dir.join("policy.toml")).unwrap();
        let mut free_create = create.clone();
        free_create["graph_id"] = json!("free-graph");
        assert!(
            CtxWorkGraphTool
                .handle(free_create.as_object().unwrap(), &context)
                .expect("registered agents can create local graphs without a paid account")
                .changed
        );
        assert!(
            CtxWorkGraphTool
                .handle(conditional.as_object().unwrap(), &context)
                .is_ok()
        );
        let cancel = json!({
            "action": "cancel", "graph_id": "paid-graph", "node_id": "root",
            "stop_reason": "manual_stop"
        });
        let unregistered = ToolContext {
            project_root: context.project_root.clone(),
            ..ToolContext::default()
        };
        let error = CtxWorkGraphTool
            .handle(cancel.as_object().unwrap(), &unregistered)
            .err()
            .expect("cancel still requires registration");
        assert_eq!(
            error.message,
            "agent must be registered first via ctx_agent"
        );
        let other_project = ToolContext {
            project_root: isolated
                .path()
                .join("other-project")
                .to_string_lossy()
                .into_owned(),
            agent_id: context.agent_id.clone(),
            ..ToolContext::default()
        };
        let error = CtxWorkGraphTool
            .handle(cancel.as_object().unwrap(), &other_project)
            .err()
            .expect("cancel must preserve project isolation");
        assert!(
            error
                .message
                .contains("agent is not active in this project")
        );
        assert!(
            CtxWorkGraphTool
                .handle(cancel.as_object().unwrap(), &context)
                .unwrap()
                .changed
        );
    }

    #[test]
    fn forged_legacy_plan_files_never_bypass_agent_registration() {
        let isolated = crate::core::data_dir::isolated_data_dir();
        let cloud = isolated.path().join("cloud");
        std::fs::create_dir_all(&cloud).unwrap();
        std::fs::write(cloud.join("plan.txt"), "enterprise").unwrap();
        std::fs::write(
            cloud.join("plan.json"),
            br#"{"plan":"enterprise","verified_at":9007199254740991}"#,
        )
        .unwrap();
        let args = Map::from_iter([("action".into(), Value::String("list".into()))]);
        let context = ToolContext {
            project_root: isolated.path().to_string_lossy().into_owned(),
            ..ToolContext::default()
        };

        let Err(error) = CtxWorkGraphTool.handle(&args, &context) else {
            panic!("forged plan files must not bypass agent registration");
        };

        assert_eq!(
            error.message,
            "agent must be registered first via ctx_agent"
        );
    }
}
