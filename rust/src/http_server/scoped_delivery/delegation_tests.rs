// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::core::a2a::task::{
    CapabilityGrantRefV1,
    delivery_authority::{DELIVERY_CHECK, DeliveryAuthorityV1},
};
use crate::core::work_graph::{BoundedWorkGraph, WorkNodeBudget};
use crate::core::work_graph_store::{ClaimNodeExecution, WorkGraphStore};
use axum::{body::Body, http::Request};
use tower::ServiceExt;

#[tokio::test(flavor = "current_thread")]
async fn delegated_http_requires_current_attempt_and_certificate_bound_signature() {
    // Store paths are environment-selected and also read by spawn_blocking.
    // Hold the shared test env lock across the entire request lifecycle.
    let _isolated = crate::core::data_dir::isolated_data_dir();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_str().unwrap();
    let context_path = dir.path().join("delegated-context");
    std::fs::write(&context_path, "context").unwrap();
    let (mut certificate, policy, host_key) =
        crate::core::a2a::task::delivery_delegation::tests::fixture();
    let budget = || WorkNodeBudget {
        tokens_allocated: 100,
        tokens_consumed: 0,
        cost_micros_allocated: 100,
        cost_micros_consumed: 0,
    };
    let fence = WorkGraphStore::mutate(root, |store| {
        let mut graph = BoundedWorkGraph::default();
        graph
            .add_root(
                "root".into(),
                "host".into(),
                "root-capsule".into(),
                budget(),
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
        let now = u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap();
        let ClaimNodeExecution::Claimed(node) =
            store.claim_node_execution("graph", "node", now, now + 600_000)?
        else {
            return Err("unexpected recovery".into());
        };
        let fence = node.execution_fence.unwrap();
        store
            .graph_mut("graph")?
            .begin_delivery_attempt("node", &fence, 1)
            .map_err(|e| e.to_string())?;
        Ok(fence)
    })
    .unwrap();
    certificate.execution.fence = fence.clone();
    certificate.project_root = dir.path().canonicalize().unwrap();
    let task = crate::core::work_graph_executor::execution_key(dir.path(), "graph", "node", &fence)
        .unwrap();
    certificate.execution.task_id = format!("{task}:attempt-1");
    certificate.sign(&host_key);
    let cfg = crate::http_server::HttpServerConfig {
        project_root: dir.path().to_owned(),
        a2a_signing_key: Some("test-channel".into()),
        a2a_recipient_id: Some("daemon".into()),
        a2a_tenant_id: Some("account".into()),
        a2a_project_id: Some("project".into()),
        a2a_task_authority: policy,
        ..Default::default()
    };
    cfg.validate().unwrap();
    let child_key = ed25519_dalek::SigningKey::from_bytes(&[32; 32]);
    let make_request = |nonce: &str| {
        let mut request = SignedDeliveryRequest {
            authority: DeliveryAuthorityV1 {
                schema_version: 1,
                sender: "child".into(),
                recipient: "daemon".into(),
                tenant_id: "account".into(),
                project_id: "project".into(),
                action: DELIVERY_CHECK.into(),
                request_digest: String::new(),
                issued_at: certificate.issued_at,
                expires_at: certificate.issued_at + chrono::Duration::minutes(1),
                nonce: nonce.into(),
                grant_ref: CapabilityGrantRefV1 {
                    schema_version: 1,
                    grant_id: "read".into(),
                },
                key_id: "child-key".into(),
                signature: String::new(),
            },
            request: DeliveryOperation::Check {
                path: context_path.to_str().unwrap().into(),
                blake3: [4; 12],
                conversation_id: None,
            },
            delegation: Some(Box::new(certificate.clone())),
        };
        request.authority.request_digest = request.bound_request_digest();
        request.authority.sign(&child_key);
        request
    };
    let send = |request: SignedDeliveryRequest| {
        let app = crate::http_server::build_app_router_with_auth(&cfg, false, None);
        async move {
            app.oneshot(
                Request::post("/ocla/v1/delivery/scoped")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&request).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
        }
    };
    assert_eq!(send(make_request("valid")).await, StatusCode::OK);
    let resign = |request: &mut SignedDeliveryRequest| {
        request.authority.request_digest = request.bound_request_digest();
        request.authority.sign(&child_key);
    };
    let mut stripped = make_request("stripped");
    stripped.delegation = None;
    assert_eq!(send(stripped).await, StatusCode::FORBIDDEN);
    let mut plain_digest = make_request("plain-digest");
    plain_digest.authority.request_digest = plain_digest.request.request_digest();
    plain_digest.authority.sign(&child_key);
    assert_eq!(send(plain_digest).await, StatusCode::FORBIDDEN);
    let outside = tempfile::NamedTempFile::new().unwrap();
    let mut escaped = make_request("escaped");
    if let DeliveryOperation::Check { path, .. } = &mut escaped.request {
        *path = outside.path().to_str().unwrap().into();
    }
    resign(&mut escaped);
    assert_eq!(send(escaped).await, StatusCode::FORBIDDEN);
    let mut wrong_root = make_request("wrong-root");
    let cert = wrong_root.delegation.as_mut().unwrap();
    cert.project_root = outside.path().parent().unwrap().canonicalize().unwrap();
    cert.sign(&host_key);
    resign(&mut wrong_root);
    assert_eq!(send(wrong_root).await, StatusCode::FORBIDDEN);
    let record = |nonce: &str, privacy, allow_write| {
        let mut request = make_request(nonce);
        if allow_write {
            let cert = request.delegation.as_mut().unwrap();
            cert.write_grant_id = Some("write".into());
            cert.sign(&host_key);
        }
        request.authority.action =
            crate::core::a2a::task::delivery_authority::DELIVERY_RECORD.into();
        request.authority.grant_ref.grant_id = "write".into();
        request.request = DeliveryOperation::Record {
            entry: crate::core::ocla::types::DeliveryEntry {
                access: Some(lean_ctx_ocla::delivery_scope::DeliveryAccessV1 {
                    scope: DeliveryScopeV1::new("account".into(), "project".into()).unwrap(),
                    privacy,
                }),
                blake3: [4; 12],
                path: context_path.to_str().unwrap().into(),
                line_count: 1,
                token_count: 1,
                agent_id: "child".into(),
                conversation_id: "delegated-write".into(),
                mtime: 0,
                relay_content: None,
                relay_mode: None,
            },
        };
        resign(&mut request);
        request
    };
    use lean_ctx_ocla::delivery_scope::DeliveryPrivacyV1::{Private, Project};
    assert_eq!(
        send(record("no-write", Private, false)).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(record("privacy-escalation", Project, true)).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(record("authorized-write", Private, true)).await,
        StatusCode::OK
    );
    let mut transplanted = make_request("transplanted");
    let cert = transplanted.delegation.as_mut().unwrap();
    cert.expires_at -= chrono::Duration::seconds(1);
    cert.sign(&host_key);
    assert_eq!(send(transplanted).await, StatusCode::FORBIDDEN);
    WorkGraphStore::mutate(root, |store| {
        store
            .graph_mut("graph")?
            .begin_delivery_attempt("node", &fence, 2)
            .map_err(|e| e.to_string())
    })
    .unwrap();
    assert_eq!(
        send(make_request("stale-attempt")).await,
        StatusCode::FORBIDDEN
    );
    let second_request = |nonce: &str| {
        let mut request = make_request(nonce);
        let certificate = request.delegation.as_mut().unwrap();
        certificate.execution.attempt = 2;
        certificate.execution.task_id = format!("{task}:attempt-2");
        certificate.expires_at = certificate.issued_at + chrono::Duration::minutes(20);
        certificate.sign(&host_key);
        request.authority.request_digest = request.bound_request_digest();
        request.authority.sign(&child_key);
        request
    };
    assert_eq!(send(second_request("second-valid")).await, StatusCode::OK);
    let probe = second_request("lease-probe");
    let certificate = probe.delegation.as_ref().unwrap();
    let expired_lease = WorkGraphStore::load(root)
        .unwrap()
        .child_delivery_authority(
            certificate,
            &cfg.a2a_task_authority,
            &crate::core::a2a::task::TaskAuthorityExpectationV1 {
                sender: "child",
                recipient: "daemon",
                tenant_id: "account",
                project_id: "project",
                now: certificate.issued_at + chrono::Duration::minutes(11),
            },
        );
    assert_eq!(
        expired_lease.unwrap_err(),
        "delegated execution is not live"
    );
    WorkGraphStore::mutate(root, |store| {
        store
            .graph_mut("graph")?
            .stop("node", crate::core::work_graph::StopReason::ManualStop)
            .map_err(|error| error.to_string())
    })
    .unwrap();
    assert_eq!(
        send(second_request("after-cancel")).await,
        StatusCode::FORBIDDEN
    );
}
