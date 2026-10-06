// SPDX-License-Identifier: Apache-2.0

//! Production-router replay tests; not a full signed task lifecycle acceptance.
use super::relay_storage_id;
use crate::core::a2a::relay::{
    RELAY_PEER_HEADER, RelayPeerConfigV1, RelayPeerTableV1, RelayRecordV1, test_origin_key,
    test_origin_public_key,
};
use crate::core::a2a_transport::{AgentIdentityV1, TransportContentType, TransportEnvelopeV1};
use crate::http_server::{HttpServerConfig, build_app_router, remote_replay::RemoteReplayGuard};
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use lean_ctx_protocol::DataClassification;
use tower::ServiceExt;

fn peer(id: &str, recipient: &str, endpoint: &str) -> RelayPeerConfigV1 {
    RelayPeerConfigV1 {
        schema_version: 1,
        peer_id: id.into(),
        endpoint_url: endpoint.into(),
        bearer_token: format!("{id}-bearer"),
        channel_key: format!("{id}-key"),
        origin_public_key: test_origin_public_key(id),
        recipient_id: recipient.into(),
        allowed_tenant_ids: vec!["tenant".into()],
        allowed_project_ids: vec!["project".into()],
        allowed_content_types: vec![TransportContentType::A2ATask],
        allowed_classifications: vec![DataClassification::Internal],
        max_hops: 4,
        max_payload_bytes: 65536,
        retry_count: 0,
        retry_delay_ms: 0,
    }
}

fn fixture(
    root: &std::path::Path,
    local: &str,
    payload: &str,
) -> (HttpServerConfig, RelayRecordV1, TransportEnvelopeV1) {
    let now = chrono::Utc::now();
    let mut record = RelayRecordV1::new_signed(
        "response-replay",
        "origin",
        "recipient",
        "tenant",
        "project",
        TransportContentType::A2ATask,
        DataClassification::Internal,
        now + chrono::Duration::minutes(5),
        4,
        now,
        &test_origin_key("origin"),
        "origin-key",
        payload.as_bytes(),
    )
    .expect("signed record");
    if local != "recipient" {
        record
            .route_from_origin(local, now, &test_origin_public_key("origin"), "origin-key")
            .expect("route to relay");
    }
    let mut envelope = TransportEnvelopeV1::new(
        AgentIdentityV1::from_current("origin", "test"),
        Some("recipient"),
        TransportContentType::A2ATask,
        payload.into(),
    );
    envelope
        .metadata
        .insert("tenant_id".into(), "tenant".into());
    envelope
        .metadata
        .insert("project_id".into(), "project".into());
    envelope
        .attach_relay_record(&record)
        .expect("record attachment");
    envelope.sign(b"origin-key").expect("hop signature");
    let cfg = HttpServerConfig {
        project_root: root.into(),
        a2a_recipient_id: Some(local.into()),
        a2a_tenant_id: Some("tenant".into()),
        a2a_project_id: Some("project".into()),
        a2a_peers: RelayPeerTableV1 {
            schema_version: 1,
            peers: vec![peer("origin", local, "https://origin.example")],
        },
        ..HttpServerConfig::default()
    };
    (cfg, record, envelope)
}

async fn deliver(
    cfg: &HttpServerConfig,
    envelope: &TransportEnvelopeV1,
    bearer: &str,
) -> axum::response::Response {
    let request = Request::builder()
        .method("POST")
        .uri("/a2a/deliver")
        .header("Content-Type", "application/json")
        .header(RELAY_PEER_HEADER, "origin")
        .header("Authorization", format!("Bearer {bearer}"))
        .body(Body::from(
            serde_json::to_vec(envelope).expect("envelope JSON"),
        ))
        .expect("request");
    // Rebuild all receiver state each time: only the durable ledger survives.
    build_app_router(cfg, None)
        .oneshot(request)
        .await
        .expect("router response")
}

#[tokio::test]
async fn cached_task_bytes_require_authentication_and_survive_router_restart() {
    use crate::core::a2a::task::{
        TASK_ACTION_GET, TaskAuthorityConfigV1, TaskControlDescriptorV1, TaskPeerTrustV1,
        TaskScopeV1, TaskState, TaskStatusV1,
    };
    use crate::core::a2a::task_response::SignedTaskStatusV1;
    use ed25519_dalek::SigningKey;
    let now = chrono::Utc::now();
    let server_key = SigningKey::from_bytes(&[9; 32]);
    let mut request = TaskControlDescriptorV1::new(
        "origin",
        "recipient",
        "tenant",
        "project",
        TASK_ACTION_GET,
        "task-1",
        &format!("sha256:{}", "a".repeat(64)),
        now,
        now + chrono::Duration::minutes(5),
        "nonce-1",
        None,
        "grant",
        "origin-key",
    );
    request.sign(&test_origin_key("origin"));
    let reply = SignedTaskStatusV1::sign_status(
        &request,
        TaskStatusV1 {
            schema_version: 1,
            state: TaskState::Created,
            timestamp: now,
        },
        "server-key",
        &server_key,
        now,
    )
    .expect("signed response");
    let policy = TaskAuthorityConfigV1 {
        schema_version: 1,
        peers: vec![TaskPeerTrustV1 {
            schema_version: 1,
            key_id: "server-key".into(),
            agent_id: "recipient".into(),
            public_key: hex::encode(server_key.verifying_key().as_bytes()),
            allowed_actions: vec![TASK_ACTION_GET.into()],
            allowed_scopes: vec![TaskScopeV1 {
                schema_version: 1,
                tenant_id: "tenant".into(),
                project_id: "project".into(),
            }],
            not_before: now - chrono::Duration::minutes(1),
            expires_at: now + chrono::Duration::hours(1),
            revoked: false,
        }],
        grants: vec![],
    };
    let root = tempfile::tempdir().expect("root");
    let payload = serde_json::to_string(&request).expect("signed request JSON");
    let (cfg, record, envelope) = fixture(root.path(), "recipient", &payload);
    let id = relay_storage_id(&record);
    let guard = RemoteReplayGuard::new(
        root.path().to_str().expect("UTF8 path"),
        crate::http_server::relay_replay::MIN_RETENTION_SECONDS,
    );
    let fingerprint = record.origin_fingerprint().expect("fingerprint");
    let crate::http_server::relay_replay::Reservation::Reserved(lease) = guard
        .reserve_relay(&id, &record, &fingerprint, chrono::Utc::now())
        .await
        .expect("reserve")
    else {
        panic!("new reservation required")
    };
    let raw = serde_json::to_string_pretty(&reply).expect("signed response JSON");
    guard
        .complete_relay_with_response(&id, &lease, raw.clone())
        .await
        .expect("cache response");
    drop(guard);
    assert_eq!(
        deliver(&cfg, &envelope, "wrong").await.status(),
        StatusCode::UNAUTHORIZED
    );
    for _ in 0..2 {
        let response = deliver(&cfg, &envelope, "origin-bearer").await;
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 65536)
            .await
            .expect("response bytes");
        assert_eq!(bytes.as_ref(), raw.as_bytes());
        let restored =
            SignedTaskStatusV1::from_json(std::str::from_utf8(&bytes).expect("UTF8 response"))
                .expect("response schema");
        assert_eq!(restored.verify(&request, &policy, now), Ok(()));
        assert!(
            restored
                .verify(&request, &policy, reply.expires_at)
                .is_err()
        );
        let mut revoked = policy.clone();
        revoked.peers[0].revoked = true;
        assert!(restored.verify(&request, &revoked, now).is_err());
        let mut other_poll = request.clone();
        other_poll.nonce = "different-poll".into();
        other_poll.sign(&test_origin_key("origin"));
        assert!(restored.verify(&other_poll, &policy, now).is_err());
    }
    assert!(
        !root.path().join(".lean-ctx/handoffs").exists(),
        "completed delivery must bypass execution"
    );
}

#[tokio::test]
async fn old_completed_task_without_response_is_not_reexecuted() {
    let root = tempfile::tempdir().expect("root");
    let (cfg, record, envelope) = fixture(root.path(), "recipient", "{}");
    let id = relay_storage_id(&record);
    let guard = RemoteReplayGuard::new(
        root.path().to_str().expect("UTF8 path"),
        crate::http_server::relay_replay::MIN_RETENTION_SECONDS,
    );
    let fingerprint = record.origin_fingerprint().expect("fingerprint");
    let crate::http_server::relay_replay::Reservation::Reserved(lease) = guard
        .reserve_relay(&id, &record, &fingerprint, chrono::Utc::now())
        .await
        .expect("reserve")
    else {
        panic!("new reservation required")
    };
    guard
        .complete_relay(&id, &lease)
        .await
        .expect("old completion");
    drop(guard);
    for _ in 0..2 {
        let response = deliver(&cfg, &envelope, "origin-bearer").await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let bytes = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .expect("error body");
        let error: serde_json::Value = serde_json::from_slice(&bytes).expect("error JSON");
        assert_eq!(error["error"], "task_response_unavailable");
    }
    assert!(!root.path().join(".lean-ctx/handoffs").exists());
}

#[tokio::test]
async fn forwarded_task_response_is_cached_byte_exactly_across_router_restart() {
    use crate::core::a2a::task::{
        TASK_ACTION_GET, TaskControlDescriptorV1, TaskState, TaskStatusV1,
    };
    use crate::core::a2a::task_response::SignedTaskStatusV1;
    use ed25519_dalek::SigningKey;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    let now = chrono::Utc::now();
    let mut request = TaskControlDescriptorV1::new(
        "origin",
        "recipient",
        "tenant",
        "project",
        TASK_ACTION_GET,
        "task-forwarded",
        &format!("sha256:{}", "b".repeat(64)),
        now,
        now + chrono::Duration::minutes(5),
        "nonce-forwarded",
        None,
        "grant",
        "origin-key",
    );
    request.sign(&test_origin_key("origin"));
    let response = SignedTaskStatusV1::sign_status(
        &request,
        TaskStatusV1 {
            schema_version: 1,
            state: TaskState::Created,
            timestamp: now,
        },
        "server-key",
        &SigningKey::from_bytes(&[7; 32]),
        now,
    )
    .expect("signed response");
    let raw = serde_json::to_string_pretty(&response).expect("response JSON");
    let expected = raw.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let app = axum::Router::new().route(
        "/a2a/deliver",
        axum::routing::post(move || {
            let observed = observed.clone();
            let raw = raw.clone();
            async move {
                observed.fetch_add(1, Ordering::SeqCst);
                (StatusCode::OK, raw)
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener");
    let endpoint = format!("http://{}", listener.local_addr().expect("address"));
    let server = tokio::spawn(async move { axum::serve(listener, app).await.expect("server") });
    struct Stop(tokio::task::JoinHandle<()>);
    impl Drop for Stop {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let _server = Stop(server);
    let root = tempfile::tempdir().expect("root");
    let payload = serde_json::to_string(&request).expect("request JSON");
    let (mut cfg, _, envelope) = fixture(root.path(), "relay", &payload);
    cfg.a2a_peers
        .peers
        .push(peer("recipient", "recipient", &endpoint));

    for _ in 0..2 {
        let response = deliver(&cfg, &envelope, "origin-bearer").await;
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 65536)
            .await
            .expect("response bytes");
        assert_eq!(bytes.as_ref(), expected.as_bytes());
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "router restart must replay cached bytes without redelivery"
    );
}

#[tokio::test]
async fn task_downstream_empty_success_keeps_pending_across_router_restart() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let app = axum::Router::new().route(
        "/a2a/deliver",
        axum::routing::post(move || {
            let observed = observed.clone();
            async move {
                observed.fetch_add(1, Ordering::SeqCst);
                StatusCode::NO_CONTENT
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener");
    let endpoint = format!("http://{}", listener.local_addr().expect("address"));
    let server = tokio::spawn(async move { axum::serve(listener, app).await.expect("server") });
    struct Stop(tokio::task::JoinHandle<()>);
    impl Drop for Stop {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let _server = Stop(server);
    let root = tempfile::tempdir().expect("root");
    let (mut cfg, record, envelope) = fixture(root.path(), "relay", "{}");
    cfg.a2a_peers
        .peers
        .push(peer("recipient", "recipient", &endpoint));
    assert_eq!(
        deliver(&cfg, &envelope, "origin-bearer").await.status(),
        StatusCode::BAD_GATEWAY
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        deliver(&cfg, &envelope, "origin-bearer").await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "retry must not forward a second task"
    );
    let scope = crate::core::a2a::dlq::DlqScope::new("tenant", "project").expect("DLQ scope");
    assert!(
        !crate::core::ocla::health::dead_letter_queue()
            .peek(&scope)
            .expect("DLQ readable")
            .iter()
            .any(|letter| letter.delivery_id == record.delivery_id),
        "acknowledged task delivery must not be recorded as a permanent non-delivery"
    );
}
