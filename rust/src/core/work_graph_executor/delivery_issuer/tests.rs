// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::core::work_graph::{BoundedWorkGraph, WorkNodeBudget};
use crate::core::work_graph_store::{ClaimNodeExecution, WorkGraphStore};

#[test]
fn issuance_requires_host_grant_and_current_persisted_attempt() {
    let _isolated = crate::core::data_dir::isolated_data_dir();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_str().unwrap();
    let (_, mut policy, _) = crate::core::a2a::task::delivery_delegation::tests::fixture();
    let host_key = crate::core::agent_identity::get_or_create_keypair("host").unwrap();
    policy.peers[0].public_key =
        crate::core::agent_identity::hex_encode(host_key.verifying_key().as_bytes());
    let authority_file = dir.path().join("authority.json");
    let budget = || WorkNodeBudget {
        tokens_allocated: 100,
        tokens_consumed: 0,
        cost_micros_allocated: 100,
        cost_micros_consumed: 0,
    };
    let mut node = WorkGraphStore::mutate(root, |store| {
        let mut graph = BoundedWorkGraph::default();
        graph
            .add_root(
                "root".into(),
                "host".into(),
                "root-capsule".into(),
                WorkNodeBudget {
                    tokens_allocated: 200,
                    cost_micros_allocated: 200,
                    ..budget()
                },
            )
            .map_err(|e| e.to_string())?;
        graph
            .queue_child(
                "root",
                "node".into(),
                "child".into(),
                "child-capsule".into(),
                budget(),
            )
            .map_err(|e| e.to_string())?;
        store.create("graph", graph)?;
        let now = chrono::Utc::now().timestamp_millis() as u64;
        let ClaimNodeExecution::Claimed(node) =
            store.claim_node_execution("graph", "node", now, now + 600_000)?
        else {
            return Err("unexpected recovery".into());
        };
        store
            .graph_mut("graph")?
            .begin_delivery_attempt("node", node.execution_fence.as_deref().unwrap(), 1)
            .map_err(|e| e.to_string())?;
        Ok(*node)
    })
    .unwrap();
    node.task_ref = "task:child".into();
    node.policy_ref = "policy:test".into();
    let plan = NodeExecutionPlan {
        graph_id: "graph".into(),
        node,
        root_agent_id: "host".into(),
        parent_capsule_ref: None,
        project_root: dir.path().to_owned(),
        timeout_ms: 60_000,
        model: None,
        max_attempts: 2,
    };
    let mut config = DeliveryIssuerConfig {
        schema_version: 1,
        project_root: dir.path().to_owned(),
        authority_file: authority_file.clone(),
        host_agent: "host".into(),
        host_key_id: "host-key".into(),
        host_grant_id: "delegate".into(),
        recipient: "daemon".into(),
        tenant_id: "account".into(),
        project_id: "project".into(),
        privacy: DeliveryPrivacyV1::Private,
        allow_write: false,
    };
    let fence = plan.node.execution_fence.as_deref().unwrap();
    let base = super::super::execution_key(dir.path(), "graph", "node", fence).unwrap();
    let task = format!("{base}:attempt-1");
    policy.grants[0].revoked = true;
    std::fs::write(&authority_file, serde_json::to_vec(&policy).unwrap()).unwrap();
    assert!(
        config
            .issue(&plan, 1, &task, CapsuleSensitivityV1::Internal)
            .is_err()
    );
    assert!(crate::core::agent_identity::get_stored_public_key("child").is_err());
    policy.grants[0].revoked = false;
    std::fs::write(&authority_file, serde_json::to_vec(&policy).unwrap()).unwrap();
    let profile = config
        .issue(&plan, 1, &task, CapsuleSensitivityV1::Internal)
        .unwrap();
    assert_eq!(profile.task_id, task);
    assert_eq!(profile.profile.write_grant_id, "no-write-grant");
    assert!(
        profile
            .profile
            .delegation
            .as_ref()
            .unwrap()
            .write_grant_id
            .is_none()
    );
    assert_eq!(
        profile
            .profile
            .delegation
            .as_ref()
            .unwrap()
            .execution
            .attempt,
        1
    );
    assert!(
        config
            .issue(&plan, 1, "wrong-task", CapsuleSensitivityV1::Internal)
            .is_err()
    );
    WorkGraphStore::mutate(root, |store| {
        store
            .graph_mut("graph")?
            .begin_delivery_attempt("node", fence, 2)
            .map_err(|e| e.to_string())
    })
    .unwrap();
    assert!(
        config
            .issue(&plan, 1, &task, CapsuleSensitivityV1::Internal)
            .is_err()
    );
    let second = config
        .issue(
            &plan,
            2,
            &format!("{base}:attempt-2"),
            CapsuleSensitivityV1::Internal,
        )
        .unwrap();
    assert_eq!(
        second
            .profile
            .delegation
            .as_ref()
            .unwrap()
            .execution
            .attempt,
        2
    );
    config.privacy = DeliveryPrivacyV1::Project;
    assert!(
        config
            .issue(
                &plan,
                2,
                &format!("{base}:attempt-2"),
                CapsuleSensitivityV1::Restricted
            )
            .is_err()
    );
    config.privacy = DeliveryPrivacyV1::Private;
    config.allow_write = true;
    let writable = config
        .issue(
            &plan,
            2,
            &format!("{base}:attempt-2"),
            CapsuleSensitivityV1::Restricted,
        )
        .unwrap();
    assert_eq!(
        writable
            .profile
            .delegation
            .as_ref()
            .unwrap()
            .write_grant_id
            .as_deref(),
        Some("delegated-write")
    );
    config.allow_write = false;
    assert_eq!(writable.profile.write_grant_id, "delegated-write");
    let other = tempfile::tempdir().unwrap();
    config.project_root = other.path().to_owned();
    assert!(
        config
            .issue(
                &plan,
                2,
                &format!("{base}:attempt-2"),
                CapsuleSensitivityV1::Internal
            )
            .is_err()
    );
    config.project_root = dir.path().to_owned();
    WorkGraphStore::mutate(root, |store| {
        store
            .graph_mut("graph")?
            .stop("node", crate::core::work_graph::StopReason::ManualStop)
            .map_err(|error| error.to_string())
    })
    .unwrap();
    assert!(
        config
            .issue(
                &plan,
                2,
                &format!("{base}:attempt-2"),
                CapsuleSensitivityV1::Internal
            )
            .is_err()
    );
    let node = WorkGraphStore::mutate(root, |store| {
        store
            .graph_mut("graph")?
            .queue_child(
                "root",
                "dispatch".into(),
                "dispatched-child".into(),
                "pending-capsule".into(),
                budget(),
            )
            .map_err(|e| e.to_string())?;
        let now = u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap();
        let ClaimNodeExecution::Claimed(node) =
            store.claim_node_execution("graph", "dispatch", now, now + 600_000)?
        else {
            return Err("unexpected recovery".into());
        };
        Ok(*node)
    })
    .unwrap();
    let mut dispatch_plan = plan.clone();
    dispatch_plan.node = node;
    let mut capsule = super::super::tests::capsule();
    capsule.agent_id = "host".into();
    capsule.chain.owner_agent_id = "host".into();
    capsule.chain.chain_id = "graph:graph".into();
    capsule.chain.parent_capsule_ref = None;
    capsule.allowed_agent_ids = vec!["dispatched-child".into()];
    capsule.budget.cost_micros_remaining = 100;
    capsule.budget.latency_ms_remaining = 120_000;
    capsule.assign_capsule_id().unwrap();
    dispatch_plan.node.capsule_ref = capsule.capsule_id.clone();
    dispatch_plan.node.task_ref = capsule.task_ref.clone();
    dispatch_plan.node.policy_ref = capsule.policy_ref.clone();
    dispatch_plan.node.expected_outcome_ref = capsule.expected_outcome_ref.clone();
    let signed =
        crate::core::context_capsule::SignedContextCapsuleV1::sign(&capsule, &host_key).unwrap();
    config.privacy = DeliveryPrivacyV1::Project;
    crate::test_env::set_var(
        "LEAN_CTX_DELIVERY_ISSUER_PROFILE",
        serde_json::to_string(&config).unwrap(),
    );
    let connector = InspectProfileConnector::default();
    let result = super::super::execute_claimed_node(
        &dispatch_plan,
        &signed,
        &host_key.verifying_key(),
        &connector,
    );
    crate::test_env::remove_var("LEAN_CTX_DELIVERY_ISSUER_PROFILE");
    assert!(result.unwrap_err().message.contains("inspection complete"));
    let request = connector.0.lock().unwrap().take().unwrap();
    let profile = request.delivery_profile.unwrap();
    assert_eq!(profile.task_id, request.id);
    assert_eq!(profile.profile.agent_id, "dispatched-child");
    assert_eq!(
        profile
            .profile
            .delegation
            .as_ref()
            .unwrap()
            .execution
            .attempt,
        1
    );
    assert_eq!(
        WorkGraphStore::load(root)
            .unwrap()
            .graph("graph")
            .unwrap()
            .get_node("dispatch")
            .unwrap()
            .delivery_attempt,
        Some(1)
    );
}

#[derive(Default)]
struct InspectProfileConnector(
    std::sync::Mutex<Option<crate::core::agent_connector::traits::TaskRequest>>,
);

#[test]
fn issuer_environment_requires_bounded_complete_json() {
    let _guard = crate::core::data_dir::test_env_lock();
    let variable = "LEAN_CTX_DELIVERY_ISSUER_PROFILE";
    let previous = std::env::var_os(variable);
    let rejected: Vec<_> = [
        String::new(),
        "{}".into(),
        "null".into(),
        "x".repeat(16_385),
    ]
    .into_iter()
    .map(|raw| {
        crate::test_env::set_var(variable, raw);
        DeliveryIssuerConfig::from_environment().is_err()
    })
    .collect();
    crate::test_env::remove_var(variable);
    let absent = DeliveryIssuerConfig::from_environment().unwrap().is_none();
    if let Some(previous) = previous {
        crate::test_env::set_var(variable, previous);
    }
    assert!(rejected.into_iter().all(|rejected| rejected));
    assert!(absent);
}

impl crate::core::agent_connector::traits::AgentConnector for InspectProfileConnector {
    fn name(&self) -> &'static str {
        "codex"
    }
    fn info(&self) -> crate::core::agent_connector::traits::AgentInfo {
        crate::core::agent_connector::traits::AgentInfo {
            name: "codex".into(),
            version: None,
            path: "codex".into(),
            capabilities: Vec::new(),
            available: true,
        }
    }
    fn health_check_with_timeout(&self, _: u64) -> anyhow::Result<bool> {
        Ok(true)
    }
    fn execute(
        &self,
        request: &crate::core::agent_connector::traits::TaskRequest,
    ) -> anyhow::Result<crate::core::agent_connector::traits::TaskResult> {
        let runtime = tokio::runtime::Runtime::new()?;
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        listener.set_nonblocking(true)?;
        let source = request.working_dir.join("authority.json").canonicalize()?;
        crate::core::ocla::registry::OclaRegistry::global()
            .delivery_registry
            .record_delivery(crate::core::ocla::types::DeliveryEntry {
                access: Some(lean_ctx_ocla::delivery_scope::DeliveryAccessV1 {
                    scope: lean_ctx_ocla::delivery_scope::DeliveryScopeV1::new(
                        "account".into(),
                        "project".into(),
                    )
                    .unwrap(),
                    privacy: DeliveryPrivacyV1::Project,
                }),
                blake3: [0; 12],
                path: source.to_string_lossy().into_owned(),
                line_count: 1,
                token_count: 12,
                agent_id: "original-context-writer".into(),
                conversation_id: "issuer-http-fixture".into(),
                mtime: 1,
                relay_content: Some("issuer-cross-process-shared-context".into()),
                relay_mode: Some("map:v2".into()),
            });
        let config = crate::http_server::HttpServerConfig {
            project_root: request.working_dir.clone(),
            a2a_recipient_id: Some("daemon".into()),
            a2a_tenant_id: Some("account".into()),
            a2a_project_id: Some("project".into()),
            a2a_task_authority: TaskAuthorityConfigV1::from_file(
                &request.working_dir.join("authority.json"),
            )
            .map_err(anyhow::Error::msg)?,
            ..Default::default()
        };
        let server = runtime.spawn(async move {
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            axum::serve(
                listener,
                crate::http_server::build_delivery_test_router(&config),
            )
            .await
            .unwrap();
        });
        let mut command = std::process::Command::new(std::env::current_exe()?);
        command
            .args([
                "--exact",
                "core::work_graph_executor::delivery_issuer::tests::issued_profile_child_process",
                "--nocapture",
            ])
            .env("LEAN_CTX_TEST_ISSUED_TASK", &request.id)
            .env("LEAN_CTX_TEST_ISSUED_ADDRESS", address.to_string())
            .env("LEAN_CTX_DELIVERY_PROFILE", "parent-authority")
            .env("LEAN_CTX_DELIVERY_ISSUER_PROFILE", "parent-issuer");
        crate::core::agent_connector::traits::apply_profile_environment(&mut command, request);
        let observed =
            crate::core::agent_connector::timeout::run_with_timeout(&mut command, 15_000)?;
        server.abort();
        anyhow::ensure!(
            !observed.timed_out && !observed.cancelled && observed.output.status.success(),
            "child profile inspection failed: {} {}",
            String::from_utf8_lossy(&observed.output.stdout),
            String::from_utf8_lossy(&observed.output.stderr)
        );
        *self.0.lock().unwrap() = Some(request.clone());
        anyhow::bail!("inspection complete; no provider process launched")
    }
}

#[test]
fn issued_profile_child_process() {
    let Ok(task_id) = std::env::var("LEAN_CTX_TEST_ISSUED_TASK") else {
        return;
    };
    assert!(std::env::var_os("LEAN_CTX_DELIVERY_ISSUER_PROFILE").is_none());
    let profile = DeliverySigningProfileV1::from_environment()
        .unwrap()
        .expect("explicit child profile");
    assert_eq!(profile.agent_id, "dispatched-child");
    assert_eq!(profile.write_grant_id, "no-write-grant");
    let certificate = profile.delegation.as_ref().expect("host delegation");
    assert_eq!(certificate.execution.task_id, task_id);
    assert_eq!(certificate.execution.attempt, 1);
    let key = crate::core::agent_identity::get_stored_signing_key(&profile.agent_id)
        .expect("issuer-provisioned child key accessible across processes");
    assert_eq!(
        crate::core::agent_identity::hex_encode(key.verifying_key().as_bytes()),
        certificate.child_public_key
    );
    use std::io::{Read, Write};
    let signed = profile
        .sign_request(
            crate::core::a2a::task::delivery_authority::DeliveryOperation::Check {
                path: profile
                    .project_root
                    .join("authority.json")
                    .to_string_lossy()
                    .into_owned(),
                blake3: [0; 12],
                conversation_id: None,
            },
        )
        .unwrap();
    let body = serde_json::to_vec(&signed).unwrap();
    let address: std::net::SocketAddr = std::env::var("LEAN_CTX_TEST_ISSUED_ADDRESS")
        .unwrap()
        .parse()
        .unwrap();
    assert!(address.ip().is_loopback());
    let mut stream =
        std::net::TcpStream::connect_timeout(&address, std::time::Duration::from_secs(3)).unwrap();
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(3)))
        .unwrap();
    stream
        .set_write_timeout(Some(std::time::Duration::from_secs(3)))
        .unwrap();
    write!(stream, "POST /ocla/v1/delivery/scoped HTTP/1.1\r\nHost: {address}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
    stream.write_all(&body).unwrap();
    let mut response = String::new();
    stream.take(16_384).read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.contains("\"hit\":true"), "{response}");
    assert!(
        response.contains("issuer-cross-process-shared-context"),
        "{response}"
    );
    assert!(response.contains("original-context-writer"), "{response}");
}
