// SPDX-License-Identifier: Apache-2.0

use super::*;

fn request<'a>(action: &'a str, project: &'a str, agent: &'a str) -> Request<'a> {
    Request {
        action,
        project_root: project,
        agent_id: agent,
        graph_id: "graph",
        node_id: Some("child"),
        if_revision: None,
        parent_node_id: None,
        to_agent: None,
        capsule_ref: None,
        outcome_ref: None,
        tokens: None,
        cost_micros: None,
        max_concurrency: None,
        receipt_id: None,
        execution_fence: None,
        outcome: None,
        claims: None,
        accepted_nodes: None,
        stop_reason: None,
        timeout_ms: None,
        path_claims: None,
        task_ref: None,
        policy_ref: None,
        expected_outcome_ref: None,
    }
}

fn create_graph(project: &str) {
    let budget = WorkNodeBudget {
        tokens_allocated: 100,
        tokens_consumed: 0,
        cost_micros_allocated: 100,
        cost_micros_consumed: 0,
    };
    let mut graph = BoundedWorkGraph::default();
    graph
        .add_root(
            "root".into(),
            "lead".into(),
            "capsule:root".into(),
            budget.clone(),
        )
        .unwrap();
    graph
        .queue_child(
            "root",
            "child".into(),
            "worker".into(),
            "capsule:child".into(),
            WorkNodeBudget {
                tokens_allocated: 40,
                cost_micros_allocated: 40,
                ..budget
            },
        )
        .unwrap();
    WorkGraphStore::mutate(project, |store| store.create("graph", graph)).unwrap();
}

fn node(project: &str) -> crate::core::work_graph::WorkNode {
    WorkGraphStore::load(project)
        .unwrap()
        .graph("graph")
        .unwrap()
        .get_node("child")
        .unwrap()
        .clone()
}

#[test]
fn durable_stop_revokes_and_frees_the_slot_only_after_it_persists() {
    let isolated = crate::core::data_dir::isolated_data_dir();
    let project = isolated.path().to_str().unwrap();
    create_graph(project);
    WorkGraphStore::mutate(project, |store| {
        let graph = store.graph_mut("graph")?;
        graph
            .queue_child(
                "root",
                "sibling".into(),
                "peer".into(),
                "capsule:sibling".into(),
                WorkNodeBudget {
                    tokens_allocated: 10,
                    tokens_consumed: 0,
                    cost_micros_allocated: 10,
                    cost_micros_consumed: 0,
                },
            )
            .map_err(|error| error.to_string())?;
        for id in ["child", "sibling"] {
            graph
                .set_path_claims(id, vec!["path:src/lib.rs".into()])
                .map_err(|error| error.to_string())?;
        }
        store.claim_node_execution("graph", "child", 1, 1_000)?;
        Ok(())
    })
    .unwrap();
    let cancel_request = request("cancel", project, "worker");
    let error =
        crate::core::work_graph_store::with_save_failure(|| cancel(&cancel_request)).unwrap_err();
    assert!(error.contains("injected work graph persistence failure"));
    assert_eq!(
        node(project).status,
        crate::core::work_graph::NodeStatus::Active
    );
    // The delegating host may stop its child; a stranger may not.
    assert!(cancel(&request("cancel", project, "stranger")).is_err());
    cancel(&request("cancel", project, "lead")).unwrap();
    let store = WorkGraphStore::load(project).unwrap();
    let graph = store.graph("graph").unwrap();
    // Only the root stays active; the stopped child no longer holds a slot.
    assert_eq!(graph.active_count(), 1);
    assert_eq!(
        graph.observation()["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["node_id"] == "child")
            .unwrap()["process_exit"],
        "confirmed"
    );
    // The stopped child may still be running: its paths stay exclusive
    // until its window ends.
    let claim_sibling = |now| {
        WorkGraphStore::mutate(project, |store| {
            store
                .claim_node_execution("graph", "sibling", now, 5_000)
                .map(|_| ())
        })
    };
    assert!(
        claim_sibling(999)
            .unwrap_err()
            .contains("path claim conflicts")
    );
    claim_sibling(1_000).unwrap();
}

/// AP07-G1 across tools: a node's path claim and a `ctx_agent` lease of
/// the same file are one lease resource, and a claim canonicalized for
/// another project never leases a path of this one.
#[test]
fn path_claims_and_agent_leases_name_one_resource() {
    let project = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let root = project.path().to_str().unwrap();
    let claim = canonical_path_claim(root, "path:src").unwrap();
    assert_eq!(
        crate::core::work_graph_delegation::path_lease_resource(root, &claim).unwrap(),
        crate::core::agent_lease::project_path_target_resource(root, "src").unwrap()
    );
    let foreign = canonical_path_claim(other.path().to_str().unwrap(), "path:src").unwrap();
    assert!(crate::core::work_graph_delegation::path_lease_resource(root, &foreign).is_err());
}

/// AP07-G6: the store's compare-and-mutate was unreachable from the tool,
/// so a write planned on an observed state silently overwrote a newer one.
#[test]
fn guarded_write_refuses_a_graph_changed_since_observe() {
    let isolated = crate::core::data_dir::isolated_data_dir();
    let project = isolated.path().to_str().unwrap();
    create_graph(project);
    let write_revision = || -> String {
        let observed: serde_json::Value =
            serde_json::from_str(&observe(&request("observe", project, "lead")).unwrap()).unwrap();
        observed["write_revision"].as_str().unwrap().to_string()
    };
    let observed = write_revision();
    WorkGraphStore::mutate(project, |store| {
        store
            .graph_mut("graph")?
            .set_path_claims("child", vec!["path:src/lib.rs".into()])
            .map_err(|error| error.to_string())
    })
    .unwrap();
    let stale = Request {
        if_revision: Some(&observed),
        ..request("cancel", project, "lead")
    };
    let before = node(project).status;
    assert!(cancel(&stale).unwrap_err().contains("revision conflict"));
    assert_eq!(node(project).status, before);
    let fresh = write_revision();
    cancel(&Request {
        if_revision: Some(&fresh),
        ..request("cancel", project, "lead")
    })
    .unwrap();
    assert_eq!(
        node(project).status,
        crate::core::work_graph::NodeStatus::Stopped
    );
}

#[test]
fn launcher_action_and_unbounded_windows_are_refused_before_any_claim() {
    let isolated = crate::core::data_dir::isolated_data_dir();
    let project = isolated.path().to_str().unwrap();
    create_graph(project);
    let before = WorkGraphStore::load(project)
        .unwrap()
        .graph_revision("graph");
    let execute = request("execute", project, "worker");
    assert_eq!(handle(execute).unwrap_err(), EXECUTE_REMOVED);
    for timeout in [0, 1_800_001] {
        let invalid = Request {
            timeout_ms: Some(timeout),
            ..request("start", project, "worker")
        };
        assert!(start(&invalid).unwrap_err().contains("timeout_ms"));
    }
    assert_eq!(
        before,
        WorkGraphStore::load(project)
            .unwrap()
            .graph_revision("graph")
    );
    // Every missing field of an action is named in one answer.
    let create = Request {
        capsule_ref: Some(""),
        tokens: Some(10),
        node_id: None,
        ..request("create", project, "lead")
    };
    assert_eq!(
        check_required_fields(&create).unwrap_err(),
        "create requires node_id, capsule_ref, task_ref, policy_ref, \
             expected_outcome_ref, tokens, cost_micros; missing: node_id, capsule_ref, task_ref, \
             policy_ref, expected_outcome_ref, cost_micros"
    );
}

/// Regression: a claimed child carries a fence, and the receipt path used to
/// demand a canonical measured receipt for task `graph:node` that nothing
/// produces, so claimed children could never finish.
#[test]
fn claimed_child_finishes_with_a_fenced_reported_receipt() {
    let isolated = crate::core::data_dir::isolated_data_dir();
    let project = isolated.path().to_str().unwrap();
    create_graph(project);
    claim(&request("claim", project, "worker")).unwrap();
    let fence = node(project).execution_fence.unwrap();
    let receipt_request = |agent: &'static str, fence: Option<&'static str>| Request {
        receipt_id: Some("receipt-child"),
        outcome_ref: Some("outcome:child"),
        outcome: Some("accepted"),
        tokens: Some(12),
        cost_micros: Some(3),
        execution_fence: fence,
        ..request("receipt", project, agent)
    };
    assert!(
        receipt(&receipt_request("worker", None))
            .unwrap_err()
            .contains("execution_fence")
    );
    let fence: &'static str = Box::leak(fence.into_boxed_str());
    assert!(receipt(&receipt_request("stranger", Some(fence))).is_err());
    receipt(&receipt_request("lead", Some(fence))).unwrap();
    let finished = node(project);
    assert_eq!(
        finished.status,
        crate::core::work_graph::NodeStatus::Completed
    );
    assert_eq!(finished.budget.tokens_consumed, 12);
    let graph: serde_json::Value =
        serde_json::from_str(&get(&request("get", project, "lead")).unwrap()).unwrap();
    assert_eq!(
        graph["receipts"]["receipt-child"]["usage_source"],
        "reported"
    );
    // Reported figures never free the allocation: the root's 100 minus the
    // child's full 40 leaves 60, not 100 - 12.
    let queue_next = |tokens: u64| {
        WorkGraphStore::mutate(project, |store| {
            store
                .graph_mut("graph")?
                .queue_child(
                    "root",
                    format!("next-{tokens}"),
                    "worker".into(),
                    format!("capsule:next-{tokens}"),
                    WorkNodeBudget {
                        tokens_allocated: tokens,
                        tokens_consumed: 0,
                        cost_micros_allocated: 1,
                        cost_micros_consumed: 0,
                    },
                )
                .map(|_| ())
                .map_err(|error| error.to_string())
        })
    };
    assert!(queue_next(61).is_err());
    queue_next(60).unwrap();
}

/// Regression: budget exhaustion stopped a started child without ending its
/// incarnation, so it held a concurrency slot and its paths forever.
#[test]
fn budget_exhaustion_ends_the_started_incarnation() {
    let isolated = crate::core::data_dir::isolated_data_dir();
    let project = isolated.path().to_str().unwrap();
    create_graph(project);
    WorkGraphStore::mutate(project, |store| {
        store.claim_node_execution("graph", "child", 1, u64::MAX)?;
        Ok(())
    })
    .unwrap();
    consume(&Request {
        tokens: Some(40),
        cost_micros: Some(40),
        ..request("consume", project, "worker")
    })
    .unwrap();
    let store = WorkGraphStore::load(project).unwrap();
    let graph = store.graph("graph").unwrap();
    assert_eq!(
        graph.get_node("child").unwrap().stop_reason,
        Some(StopReason::BudgetExhausted)
    );
    assert_eq!(graph.active_count(), 1);
}

const E2E_CHILD: &str = "e2e-host-start-child";

/// The host starts the child; LeanCTX binds it to the node and hands the
/// child a delivery profile that works across processes, exactly like the
/// removed launcher did, and a receipt ends the incarnation.
#[test]
fn host_started_child_gets_scoped_delivery_until_the_receipt() {
    use crate::core::agent_lease::{
        AGENT_LEASE_SCHEMA_VERSION, AgentLeaseAcquireV1, AgentLeaseRequestV1,
        AgentLeaseResourceKindV1, acquire_shared,
    };
    use crate::core::work_graph_delegation as delegation;
    let _isolated = crate::core::data_dir::isolated_data_dir();
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().to_str().unwrap();
    let (_, mut policy, _) = crate::core::a2a::task::delivery_delegation::tests::fixture();
    let host_key = crate::core::agent_identity::get_or_create_keypair("host").unwrap();
    policy.peers[0].public_key =
        crate::core::agent_identity::hex_encode(host_key.verifying_key().as_bytes());
    let authority_file = dir.path().join("authority.json");
    std::fs::write(&authority_file, serde_json::to_vec(&policy).unwrap()).unwrap();

    let budget = |amount| WorkNodeBudget {
        tokens_allocated: amount,
        tokens_consumed: 0,
        cost_micros_allocated: amount,
        cost_micros_consumed: 0,
    };
    let mut capsule = delegation::tests::capsule();
    capsule.agent_id = "host".into();
    capsule.chain.owner_agent_id = "host".into();
    capsule.chain.chain_id = "graph:graph".into();
    capsule.chain.parent_capsule_ref = Some("capsule:root".into());
    capsule.allowed_agent_ids = vec![E2E_CHILD.into()];
    capsule.budget.tokens_remaining = 40;
    capsule.budget.cost_micros_remaining = 40;
    capsule.budget.latency_ms_remaining = 120_000;
    capsule.assign_capsule_id().unwrap();
    let mut graph = BoundedWorkGraph::default();
    graph
        .add_root(
            "root".into(),
            "host".into(),
            "capsule:root".into(),
            budget(100),
        )
        .unwrap();
    graph
        .configure_root_context("root", "task:root", "policy:workspace", "outcome:root")
        .unwrap();
    for (node, capsule_ref) in [
        ("child", capsule.capsule_id.as_str()),
        ("unfed", "capsule:unfed"),
    ] {
        graph
            .queue_child(
                "root",
                node.into(),
                E2E_CHILD.into(),
                capsule_ref.into(),
                budget(40),
            )
            .unwrap();
        graph
            .configure_child_context(node, "task:child", "outcome:tests-pass")
            .unwrap();
    }
    WorkGraphStore::mutate(project, |store| store.create("graph", graph)).unwrap();
    let issuer = serde_json::json!({
        "schema_version": 1,
        "project_root": dir.path(),
        "authority_file": authority_file,
        "host_agent": "host",
        "host_key_id": "host-key",
        "host_grant_id": "delegate",
        "recipient": "daemon",
        "tenant_id": "account",
        "project_id": "project",
        "privacy": "project",
        "allow_write": false,
    });
    crate::test_env::set_var(delegation::ISSUER_ENV, issuer.to_string());

    // The host's issuer configuration in another agent's process (its host
    // ignored unset_env) is refused before any claim.
    let leaked = Request {
        timeout_ms: Some(60_000),
        ..request("start", project, E2E_CHILD)
    };
    assert!(start(&leaked).unwrap_err().contains("must not reach"));
    assert_eq!(
        node(project).status,
        crate::core::work_graph::NodeStatus::Pending
    );

    // Without a handed-over capsule the start is refused and fully undone.
    let unfed = Request {
        node_id: Some("unfed"),
        timeout_ms: Some(60_000),
        ..request("start", project, "host")
    };
    assert!(
        start(&unfed)
            .unwrap_err()
            .contains("no unreserved signed context capsule")
    );
    let unfed_node = WorkGraphStore::load(project).unwrap();
    let unfed_node = unfed_node
        .graph("graph")
        .unwrap()
        .get_node("unfed")
        .unwrap();
    assert_eq!(
        unfed_node.status,
        crate::core::work_graph::NodeStatus::Stopped
    );
    assert_eq!(unfed_node.stop_reason, Some(StopReason::ExecutionFailed));

    let signed =
        crate::core::context_capsule::SignedContextCapsuleV1::sign(&capsule, &host_key).unwrap();
    crate::core::a2a::health::local_transport()
        .deliver(&signed, E2E_CHILD)
        .unwrap();
    let started = start(&Request {
        timeout_ms: Some(60_000),
        ..request("start", project, "host")
    });
    crate::test_env::remove_var(delegation::ISSUER_ENV);
    let started: serde_json::Value = serde_json::from_str(&started.unwrap()).unwrap();
    let launch = &started["launch"];
    assert_eq!(launch["delegated_delivery"], true);
    assert_eq!(launch["unset_env"][0], delegation::ISSUER_ENV);
    let profile_value = launch["env"][delegation::CHILD_DELIVERY_ENV]
        .as_str()
        .unwrap()
        .to_string();
    let task_id = launch["task_id"].as_str().unwrap().to_string();
    let fence = launch["execution_fence"].as_str().unwrap().to_string();

    // A second start of the live incarnation is refused.
    assert!(start(&request("start", project, "host")).is_err());

    let runtime = tokio::runtime::Runtime::new().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let source = authority_file.canonicalize().unwrap();
    crate::core::ocla::registry::OclaRegistry::global()
        .delivery_registry
        .record_delivery(crate::core::ocla::types::DeliveryEntry {
            access: Some(lean_ctx_ocla::delivery_scope::DeliveryAccessV1 {
                scope: lean_ctx_ocla::delivery_scope::DeliveryScopeV1::new(
                    "account".into(),
                    "project".into(),
                )
                .unwrap(),
                privacy: lean_ctx_ocla::delivery_scope::DeliveryPrivacyV1::Project,
            }),
            blake3: [0; 12],
            path: source.to_string_lossy().into_owned(),
            line_count: 1,
            token_count: 12,
            agent_id: "original-context-writer".into(),
            conversation_id: "host-start-fixture".into(),
            mtime: 1,
            relay_content: Some("host-start-cross-process-shared-context".into()),
            relay_mode: Some("map:v2".into()),
        });
    let config = crate::http_server::HttpServerConfig {
        project_root: dir.path().to_path_buf(),
        a2a_recipient_id: Some("daemon".into()),
        a2a_tenant_id: Some("account".into()),
        a2a_project_id: Some("project".into()),
        a2a_task_authority: policy.clone(),
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
    // Launch the child the way a host does: apply `env`, drop `unset_env`.
    let mut child = std::process::Command::new(std::env::current_exe().unwrap());
    child
        .args([
            "--exact",
            "tools::ctx_work_graph::lifecycle_tests::host_started_child_process",
            "--nocapture",
        ])
        .env(delegation::ISSUER_ENV, "parent-issuer")
        .env(delegation::CHILD_DELIVERY_ENV, &profile_value)
        .env_remove(delegation::ISSUER_ENV)
        .env("LEAN_CTX_TEST_HOST_TASK", &task_id)
        .env("LEAN_CTX_TEST_HOST_ADDRESS", address.to_string());
    let mut child = child.spawn().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            panic!("host-started child did not finish");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    server.abort();
    assert!(
        status.success(),
        "host-started child failed scoped delivery"
    );

    receipt(&Request {
        receipt_id: Some("receipt-child"),
        outcome_ref: Some("outcome:child"),
        outcome: Some("partial"),
        tokens: Some(7),
        cost_micros: Some(2),
        execution_fence: Some(&fence),
        ..request("receipt", project, "host")
    })
    .unwrap();
    // The receipt ended the incarnation: delivery is revoked and the node
    // lease is free again.
    let profile: crate::core::a2a::task::delivery_authority::DeliverySigningProfileV1 =
        serde_json::from_str(&profile_value).unwrap();
    let certificate = profile.delegation.unwrap();
    assert!(
        WorkGraphStore::load(project)
            .unwrap()
            .child_delivery_authority(
                &certificate,
                &policy,
                &crate::core::a2a::task::TaskAuthorityExpectationV1 {
                    sender: E2E_CHILD,
                    recipient: "daemon",
                    tenant_id: "account",
                    project_id: "project",
                    now: chrono::Utc::now(),
                },
            )
            .is_err()
    );
    assert!(matches!(
        acquire_shared(AgentLeaseRequestV1 {
            schema_version: AGENT_LEASE_SCHEMA_VERSION,
            lease_request_ref: "fence:next-incarnation".into(),
            resource_kind: AgentLeaseResourceKindV1::WorkGraphNode,
            resource_ref: delegation::node_lease_resource(project, "graph", "child").unwrap(),
            owner_agent_id: "other-agent".into(),
            duration_ms: 1_000,
        })
        .unwrap(),
        AgentLeaseAcquireV1::Granted(_)
    ));
}

#[test]
fn host_started_child_process() {
    let Ok(task_id) = std::env::var("LEAN_CTX_TEST_HOST_TASK") else {
        return;
    };
    use std::io::{Read, Write};
    assert!(
        std::env::var_os(crate::core::work_graph_delegation::ISSUER_ENV).is_none(),
        "the parent's issuer configuration must not reach the child"
    );
    let profile =
        crate::core::a2a::task::delivery_authority::DeliverySigningProfileV1::from_environment()
            .unwrap()
            .expect("explicit child profile");
    assert_eq!(profile.agent_id, E2E_CHILD);
    let certificate = profile.delegation.as_ref().expect("host delegation");
    assert_eq!(certificate.execution.task_id, task_id);
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
    let address: std::net::SocketAddr = std::env::var("LEAN_CTX_TEST_HOST_ADDRESS")
        .unwrap()
        .parse()
        .unwrap();
    assert!(address.ip().is_loopback());
    let mut stream =
        std::net::TcpStream::connect_timeout(&address, std::time::Duration::from_secs(3)).unwrap();
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(3)))
        .unwrap();
    write!(stream, "POST /ocla/v1/delivery/scoped HTTP/1.1\r\nHost: {address}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
    stream.write_all(&body).unwrap();
    let mut response = String::new();
    stream.take(16_384).read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(
        response.contains("host-start-cross-process-shared-context"),
        "{response}"
    );
}
