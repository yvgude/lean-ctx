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
            "Free persistent project-scoped bounded local Work Graph for multi-agent execution. Actions: create|delegate|claim|execute|consume|complete|receipt|accept|fuse|attribution|value_report|cancel|get|observe|list. Observe returns compact state without context payloads or execution fences.",
            json!({
                "type": "object",
                "properties": {
                    "action": {"type":"string","enum":["create","delegate","claim","execute","consume","complete","receipt","accept","fuse","attribution","value_report","cancel","get","observe","list"]},
                    "graph_id": {"type":"string","minLength":1,"maxLength":128},
                    "if_revision": {"type":"string","pattern":"^[0-9a-f]{64}$","description":"For observe: return only unchanged=true and revision if state matches. Authorization is still enforced."},
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
                    ,"connector": {"type":"string","enum":["codex"]}
                    ,"model": {"type":"string","minLength":1,"maxLength":128}
                    ,"timeout_ms": {"type":"integer","minimum":1,"maximum":1800000}
                    ,"max_attempts": {"type":"integer","minimum":1,"maximum":2}
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
                | "execute"
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
            connector: get_str(args, "connector").as_deref(),
            model: get_str(args, "model").as_deref(),
            timeout_ms: args.get("timeout_ms").and_then(Value::as_u64),
            max_attempts: args
                .get("max_attempts")
                .and_then(Value::as_u64)
                .and_then(|value| u8::try_from(value).ok()),
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
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use ed25519_dalek::{Signer as _, SigningKey};
    use lean_ctx_protocol::{
        EntitlementDeploymentV1, EntitlementEnvelopeV1, EntitlementKindV1, EntitlementPlanV1,
        EntitlementSignerV1, Sha256Digest,
    };
    use sha2::{Digest as _, Sha256};

    #[test]
    fn work_graph_stays_free_after_entitlement_removal_and_preserves_security() {
        let isolated = crate::core::data_dir::isolated_data_dir();
        let cloud = isolated.path().join("cloud");
        std::fs::create_dir_all(&cloud).unwrap();
        let trust_path = isolated.path().join("entitlement-trust.json");
        let credentials_path = cloud.join("credentials.json");
        let cache_path = cloud.join("entitlement-v1.json");
        let key = SigningKey::from_bytes(&[47; 32]);
        let public_key = key.verifying_key().to_bytes();
        let digest = crate::core::agent_identity::hex_encode(&Sha256::digest(public_key));
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let mut envelope = EntitlementEnvelopeV1 {
            schema_version: 1,
            entitlement_id: "handler-entitlement".into(),
            kind: EntitlementKindV1::OnlineSubscription,
            account_id: Some("handler-account".into()),
            plan: EntitlementPlanV1::Pro,
            seats: 1,
            capabilities: vec![crate::core::billing::PRO_LOCAL_WORK_GRAPH.into()],
            issued_at: now - 10,
            not_before: now - 10,
            expires_at: now + 600,
            grace_until: now + 1_200,
            allowed_deployments: vec![EntitlementDeploymentV1::Hosted],
            deployment_id: None,
            org_id: None,
            workspace_id: None,
            signer: EntitlementSignerV1 {
                algorithm: "ed25519".into(),
                key_id: "handler-key".into(),
                public_key_digest: Sha256Digest::new(format!("sha256:{digest}")).unwrap(),
            },
            signature: String::new(),
        };
        envelope.signature =
            STANDARD.encode(key.sign(&envelope.signing_bytes().unwrap()).to_bytes());
        std::fs::write(
            &trust_path,
            serde_json::to_vec(&json!({
                "schema_version": 1,
                "keys": [{
                    "key_id": "handler-key",
                    "public_key_base64": STANDARD.encode(public_key)
                }],
                "revoked_entitlement_ids": [],
                "deployment": "hosted"
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            &credentials_path,
            br#"{"api_key":"handler-secret","user_id":"handler-account","email":"handler@example.invalid"}"#,
        )
        .unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(
            &credentials_path,
            std::os::unix::fs::PermissionsExt::from_mode(0o600),
        )
        .unwrap();
        let envelope_bytes = envelope.canonical_bytes().unwrap();
        std::fs::write(&cache_path, &envelope_bytes).unwrap();
        let _guard = crate::cloud_client::install_test_signed_entitlement_paths(
            trust_path,
            credentials_path,
            cache_path,
            vec![("handler-key".into(), public_key)],
        );
        crate::cloud_client::accept_test_signed_entitlement("handler-account", &envelope_bytes);
        let args = Map::from_iter([("action".into(), Value::String("list".into()))]);
        let context = ToolContext {
            project_root: isolated.path().to_string_lossy().into_owned(),
            ..ToolContext::default()
        };

        let Err(error) = CtxWorkGraphTool.handle(&args, &context) else {
            panic!("the dispatcher must enforce agent registration after the paid gate");
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
        let execute = json!({
            "action": "execute", "graph_id": "paid-graph", "node_id": "root",
            "connector": "codex"
        });
        let error = CtxWorkGraphTool
            .handle(execute.as_object().unwrap(), &context)
            .err()
            .expect("project policy must deny execution even with a valid paid entitlement");
        assert_eq!(error.message, "project policy denies Work Graph execution");
        let after = CtxWorkGraphTool
            .handle(get.as_object().unwrap(), &context)
            .unwrap();
        assert_eq!(
            before.text, after.text,
            "denial must not claim or mutate the graph"
        );
        std::fs::write(policy_dir.join("policy.toml"),
            "name = \"model-ceiling\"\nversion = \"1.0.0\"\ndescription = \"test\"\n[routing]\nallowed_models = [\"claude-*\"]\nmodel_ceiling_groups = [[\"*-opus-*\"]]\n").unwrap();
        for model in [None, Some(""), Some("gpt-5"), Some("claude-sonnet-5")] {
            let mut request = execute.clone();
            if let Some(model) = model {
                request["model"] = json!(model);
            }
            let error = CtxWorkGraphTool
                .handle(request.as_object().unwrap(), &context)
                .err()
                .expect("unproven or forbidden model must be denied before dispatch");
            assert_eq!(
                error.message,
                "project policy denies Work Graph model selection"
            );
            let unchanged = CtxWorkGraphTool
                .handle(get.as_object().unwrap(), &context)
                .unwrap();
            assert_eq!(before.text, unchanged.text);
        }
        drop(_guard);
        let _denied = crate::cloud_client::install_test_signed_entitlement_paths(
            isolated.path().join("missing-trust.json"),
            cloud.join("missing-credentials.json"),
            cloud.join("missing-entitlement.json"),
            Vec::new(),
        );
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
        let _guard = crate::cloud_client::install_test_signed_entitlement_paths(
            isolated.path().join("missing-trust.json"),
            cloud.join("missing-credentials.json"),
            cloud.join("missing-entitlement.json"),
            Vec::new(),
        );
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
