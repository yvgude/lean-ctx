// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::core::a2a::relay::{test_origin_key, test_origin_public_key};
use axum::body::Body;
use axum::http::Request;
use futures::StreamExt;
use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};
use serde_json::json;
use tower::ServiceExt;

async fn read_first_sse_message(body: Body) -> String {
    let mut stream = body.into_data_stream();
    let mut buf: Vec<u8> = Vec::new();
    for _ in 0..32 {
        let next = tokio::time::timeout(Duration::from_secs(2), stream.next()).await;
        let Ok(Some(Ok(bytes))) = next else {
            break;
        };
        buf.extend_from_slice(&bytes);
        if buf.windows(2).any(|w| w == b"\n\n") {
            break;
        }
    }
    String::from_utf8_lossy(&buf).to_string()
}

#[tokio::test]
async fn agent_lifecycle_endpoints_reject_unknown_presence() {
    let _isolated_data_dir = crate::core::data_dir::isolated_data_dir();

    let heartbeat = v1_agents_heartbeat(Json(json!({"agent_id": "missing-agent"}))).await;
    assert_eq!(heartbeat.status(), StatusCode::NOT_FOUND);

    let deregister = v1_agents_deregister(Json(json!({"agent_id": "missing-agent"}))).await;
    assert_eq!(deregister.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a2a_task_lifecycle_is_project_scoped_through_router() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg = HttpServerConfig {
        project_root: dir.path().to_path_buf(),
        ..HttpServerConfig::default()
    };
    let app = build_app_router_with_auth(&cfg, false, None);
    let request = |body: Value| {
        Request::builder()
            .method("POST")
            .uri("/a2a")
            .header("Host", "localhost")
            .header("Content-Type", "application/json")
            .body(Body::from(body.to_string()))
            .expect("request")
    };
    let send = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tasks/send",
        "params": {
            "to": "server-a",
            "message": {
                "role": "agent",
                "parts": [{"type": "text", "text": "ship it"}]
            }
        }
    });
    let response = app.clone().oneshot(request(send)).await.expect("send");
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1_000_000)
        .await
        .expect("body");
    let sent: Value = serde_json::from_slice(&body).expect("json");
    let task_id = sent["result"]["id"].as_str().expect("task id");

    let cancel = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tasks/cancel",
        "params": {"id": task_id}
    });
    let response = app.oneshot(request(cancel)).await.expect("cancel");
    let body = axum::body::to_bytes(response.into_body(), 1_000_000)
        .await
        .expect("body");
    let canceled: Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(canceled["result"]["status"]["state"], "canceled");
    assert!(dir.path().join(".lean-ctx/a2a/tasks-v1.json").is_file());
}

#[test]
fn index_ensure_body_parses_root_and_optional_extra_roots() {
    // Wire contract for the #460 daemon delegation endpoint: camelCase
    // `extraRoots`, optional and defaulting to empty. daemon_client serializes
    // exactly this shape, so a drift here silently breaks delegation.
    let full: IndexEnsureBody =
        serde_json::from_str(r#"{"root":"/a","extraRoots":["/b","/c"]}"#).unwrap();
    assert_eq!(full.root, "/a");
    assert_eq!(full.extra_roots, vec!["/b".to_string(), "/c".to_string()]);

    let minimal: IndexEnsureBody = serde_json::from_str(r#"{"root":"/a"}"#).unwrap();
    assert_eq!(minimal.root, "/a");
    assert!(minimal.extra_roots.is_empty());
}

#[tokio::test]
async fn ipc_router_allows_local_tools_without_bearer_header() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg = HttpServerConfig {
        project_root: dir.path().to_path_buf(),
        auth_token: Some("secret".to_string()),
        ..HttpServerConfig::default()
    };
    let app = build_app_router_with_auth(&cfg, false, None);

    let body = json!({
        "name": "ctx_cache",
        "arguments": { "action": "stats" }
    })
    .to_string();
    let req = Request::builder()
        .method("POST")
        .uri("/v1/tools/call")
        .header("Host", "localhost")
        .header("Content-Type", "application/json")
        .body(Body::from(body))
        .expect("request");

    let resp = app.oneshot(req).await.expect("resp");
    assert_ne!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn auth_token_blocks_requests_without_bearer_header() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root_str = dir.path().to_string_lossy().to_string();
    let service_project_root = root_str.clone();
    let service_factory = move || -> Result<LeanCtxServer, std::io::Error> {
        Ok(LeanCtxServer::new_shared_with_context(
            &service_project_root,
            "default",
            "default",
        ))
    };
    let cfg = StreamableHttpServerConfig::default()
        .with_stateful_mode(false)
        .with_json_response(true);

    let mcp_http = StreamableHttpService::new(
        service_factory,
        Arc::new(
            rmcp::transport::streamable_http_server::session::local::LocalSessionManager::default(),
        ),
        cfg,
    );

    let state = AppState {
        token: Some("secret".to_string()),
        a2a_signing_key: None,
        a2a_recipient_id: None,
        a2a_tenant_id: None,
        a2a_project_id: None,
        a2a_peers: Arc::new(RelayPeerTableV1::default()),
        relay_quotas: None,
        a2a_task_authority: Arc::new(crate::core::a2a::task::TaskAuthorityConfigV1::default()),
        concurrency: Arc::new(tokio::sync::Semaphore::new(4)),
        rate: Arc::new(RateLimiter::new(50, 100)),
        remote_replays: Arc::new(remote_replay::RemoteReplayGuard::new(
            &root_str,
            handlers::REMOTE_REPLAY_RETENTION_SECONDS,
        )),
        project_root: root_str.clone(),
        timeout: Duration::from_secs(30),
        server: LeanCtxServer::new_shared_with_context(&root_str, "default", "default"),
    };

    let app = Router::new()
        .fallback_service(mcp_http)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ))
        .with_state(state);

    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/list",
        "params": {}
    })
    .to_string();

    let req = Request::builder()
        .method("POST")
        .uri("/")
        .header("Host", "localhost")
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
        .body(Body::from(body))
        .expect("request");

    let resp = app.clone().oneshot(req).await.expect("resp");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn remote_a2a_delivery_is_authenticated_and_idempotent() {
    use crate::core::a2a_transport::{AgentIdentityV1, TransportContentType, TransportEnvelopeV1};

    let dir = tempfile::tempdir().expect("tempdir");
    let cfg = HttpServerConfig {
        project_root: dir.path().to_path_buf(),
        auth_token: Some("secret".to_string()),
        a2a_signing_key: Some("signing-secret".to_string()),
        a2a_recipient_id: Some("recipient".to_string()),
        a2a_tenant_id: Some("tenant-a".to_string()),
        a2a_project_id: Some("project-a".to_string()),
        ..HttpServerConfig::default()
    };
    let app = build_app_router(&cfg, None);
    let mut envelope = TransportEnvelopeV1::new(
        AgentIdentityV1 {
            agent_id: "sender".to_string(),
            agent_type: "test".to_string(),
            daemon_fingerprint: "fingerprint".to_string(),
            capabilities: vec!["a2a_messaging".to_string()],
        },
        Some("recipient"),
        TransportContentType::A2AMessage,
        "{}".to_string(),
    );
    envelope
        .metadata
        .insert("tenant_id".to_string(), "tenant-a".to_string());
    envelope
        .metadata
        .insert("project_id".to_string(), "project-a".to_string());
    envelope.sign(b"signing-secret").expect("sign envelope");
    let body = serde_json::to_vec(&envelope).expect("serialize envelope");
    let mut uppercase_signature = envelope.clone();
    uppercase_signature.signature = uppercase_signature
        .signature
        .map(|value| value.to_uppercase());
    let uppercase_body =
        serde_json::to_vec(&uppercase_signature).expect("serialize uppercase signature");

    let request = |request_body: Vec<u8>| {
        Request::builder()
            .method("POST")
            .uri("/a2a/deliver")
            .header("Host", "localhost")
            .header("Content-Type", "application/json")
            .header("Authorization", "Bearer secret")
            .body(Body::from(request_body))
            .expect("request")
    };
    let first = app
        .clone()
        .oneshot(request(body))
        .await
        .expect("first delivery");
    assert_eq!(first.status(), StatusCode::OK);

    let duplicate = app
        .clone()
        .oneshot(request(uppercase_body))
        .await
        .expect("duplicate delivery");
    assert_eq!(duplicate.status(), StatusCode::OK);
    let duplicate_body = axum::body::to_bytes(duplicate.into_body(), 1_000_000)
        .await
        .expect("duplicate body");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&duplicate_body).expect("duplicate JSON")["status"],
        "already_received"
    );

    let mut refreshed_retry = envelope.clone();
    refreshed_retry.sent_at += chrono::Duration::seconds(1);
    refreshed_retry
        .sign(b"signing-secret")
        .expect("refresh retry signature");
    assert_ne!(refreshed_retry.signature, envelope.signature);
    let refreshed = app
        .clone()
        .oneshot(request(
            serde_json::to_vec(&refreshed_retry).expect("serialize refreshed retry"),
        ))
        .await
        .expect("refreshed retry");
    assert_eq!(refreshed.status(), StatusCode::OK);
    let refreshed_body = axum::body::to_bytes(refreshed.into_body(), 1_000_000)
        .await
        .expect("refreshed body");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&refreshed_body).expect("refreshed JSON")["status"],
        "already_received"
    );

    let ledger_path = dir.path().join(".lean-ctx/a2a/replay-v2.json");
    let mut ledger: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&ledger_path).expect("read replay ledger"))
            .expect("parse replay ledger");
    let entries = ledger["entries"].as_object_mut().expect("replay entries");
    let entry = entries.values_mut().next().expect("replay entry");
    entry["accepted_at"] = serde_json::json!(0);
    std::fs::write(
        &ledger_path,
        serde_json::to_vec(&ledger).expect("serialize replay ledger"),
    )
    .expect("expire replay entry");

    let retried = app
        .oneshot(request(
            serde_json::to_vec(&envelope).expect("serialize retry envelope"),
        ))
        .await
        .expect("retry after ledger expiry");
    assert_eq!(retried.status(), StatusCode::OK);
    let delivery_id = envelope.stable_delivery_id().expect("stable delivery id");
    let matching_events = crate::core::context_os::runtime()
        .bus
        .recent_by_kind(
            dir.path().to_string_lossy().as_ref(),
            "a2a",
            crate::core::context_os::ContextEventKindV1::SessionMutated.as_str(),
            100,
        )
        .into_iter()
        .filter(|event| event.payload["delivery_id"] == delivery_id)
        .count();
    assert_eq!(
        matching_events, 1,
        "retry must not duplicate bus side effect"
    );
}

#[tokio::test]
async fn refreshed_dlq_retry_honors_pre_upgrade_signature_ledger_entry() {
    use crate::core::a2a_transport::{
        AgentIdentityV1, LEGACY_DELIVERY_ID_METADATA, LEGACY_DELIVERY_SENT_AT_METADATA,
        TransportContentType, TransportEnvelopeV1,
    };

    let dir = tempfile::tempdir().expect("tempdir");
    let cfg = HttpServerConfig {
        project_root: dir.path().to_path_buf(),
        auth_token: Some("secret".to_string()),
        a2a_signing_key: Some("signing-secret".to_string()),
        a2a_recipient_id: Some("recipient".to_string()),
        a2a_tenant_id: Some("tenant-a".to_string()),
        a2a_project_id: Some("project-a".to_string()),
        ..HttpServerConfig::default()
    };
    let mut original = TransportEnvelopeV1::new(
        AgentIdentityV1 {
            agent_id: "sender".to_string(),
            agent_type: "test".to_string(),
            daemon_fingerprint: "fingerprint".to_string(),
            capabilities: vec!["a2a_messaging".to_string()],
        },
        Some("recipient"),
        TransportContentType::A2AMessage,
        "{}".to_string(),
    );
    original
        .metadata
        .insert("tenant_id".to_string(), "tenant-a".to_string());
    original
        .metadata
        .insert("project_id".to_string(), "project-a".to_string());
    original.sign(b"signing-secret").expect("sign original");
    let legacy_id = original.signature.clone().expect("legacy signature");
    let legacy_sent_at = original.sent_at.to_rfc3339();

    let guard = remote_replay::RemoteReplayGuard::new(
        dir.path().to_str().expect("root string"),
        handlers::REMOTE_REPLAY_RETENTION_SECONDS,
    );
    let remote_replay::Reservation::Reserved(generation) = guard
        .reserve(&legacy_id, chrono::Utc::now())
        .await
        .expect("reserve legacy")
    else {
        panic!("legacy id must reserve");
    };
    guard
        .complete(&legacy_id, generation)
        .await
        .expect("complete legacy");

    let mut retry = original;
    retry
        .metadata
        .insert(LEGACY_DELIVERY_ID_METADATA.to_string(), legacy_id);
    retry
        .metadata
        .insert(LEGACY_DELIVERY_SENT_AT_METADATA.to_string(), legacy_sent_at);
    retry.sent_at = chrono::Utc::now();
    retry.sign(b"signing-secret").expect("sign refreshed retry");
    let request = Request::builder()
        .method("POST")
        .uri("/a2a/deliver")
        .header("Host", "localhost")
        .header("Content-Type", "application/json")
        .header("Authorization", "Bearer secret")
        .body(Body::from(
            serde_json::to_vec(&retry).expect("retry envelope JSON"),
        ))
        .expect("request");
    let response = build_app_router(&cfg, None)
        .oneshot(request)
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1_000_000)
        .await
        .expect("body");
    assert_eq!(
        serde_json::from_slice::<Value>(&body).expect("JSON")["status"],
        "already_received"
    );
}

#[tokio::test]
async fn signed_remote_task_is_authorized_persisted_and_truthfully_replayed() {
    use crate::core::a2a::task::{
        TASK_ACTION_SEND, TASK_AUTHORITY_CONFIG_VERSION, TaskAuthorityConfigV1,
        TaskCapabilityGrantV1, TaskDescriptorV1, TaskPeerTrustV1, TaskScopeV1, TaskStore,
    };
    use crate::core::a2a_transport::{AgentIdentityV1, TransportContentType, TransportEnvelopeV1};
    use chrono::{Duration as ChronoDuration, Utc};
    use ed25519_dalek::SigningKey;

    let dir = tempfile::tempdir().expect("tempdir");
    let key = SigningKey::from_bytes(&[17; 32]);
    let now = Utc::now();
    let policy = TaskAuthorityConfigV1 {
        schema_version: TASK_AUTHORITY_CONFIG_VERSION,
        peers: vec![TaskPeerTrustV1 {
            schema_version: TASK_AUTHORITY_CONFIG_VERSION,
            key_id: "key-a".to_string(),
            agent_id: "sender".to_string(),
            public_key: crate::core::agent_identity::hex_encode(key.verifying_key().as_bytes()),
            allowed_actions: vec![TASK_ACTION_SEND.to_string()],
            allowed_scopes: vec![TaskScopeV1 {
                schema_version: TASK_AUTHORITY_CONFIG_VERSION,
                tenant_id: "tenant-a".to_string(),
                project_id: "project-a".to_string(),
            }],
            not_before: now - ChronoDuration::hours(1),
            expires_at: now + ChronoDuration::hours(1),
            revoked: false,
        }],
        grants: vec![TaskCapabilityGrantV1 {
            schema_version: TASK_AUTHORITY_CONFIG_VERSION,
            grant_id: "grant-a".to_string(),
            key_id: "key-a".to_string(),
            action: TASK_ACTION_SEND.to_string(),
            tenant_id: "tenant-a".to_string(),
            project_id: "project-a".to_string(),
            not_before: now - ChronoDuration::hours(1),
            expires_at: now + ChronoDuration::hours(1),
            revoked: false,
        }],
    };
    let cfg = HttpServerConfig {
        project_root: dir.path().to_path_buf(),
        auth_token: Some("bearer-secret".to_string()),
        a2a_signing_key: Some("channel-secret".to_string()),
        a2a_recipient_id: Some("recipient".to_string()),
        a2a_tenant_id: Some("tenant-a".to_string()),
        a2a_project_id: Some("project-a".to_string()),
        a2a_task_authority: policy,
        ..HttpServerConfig::default()
    };
    assert!(cfg.validate().is_ok());
    let app = build_app_router(&cfg, None);

    let make_descriptor = |description: &str| {
        let mut descriptor = TaskDescriptorV1::new(
            "sender",
            "recipient",
            "tenant-a",
            "project-a",
            TASK_ACTION_SEND,
            now,
            now + ChronoDuration::minutes(30),
            "idem-a",
            description,
            Vec::new(),
            "grant-a",
            "key-a",
        );
        descriptor.sign(&key);
        descriptor
    };
    let make_body = |descriptor: &TaskDescriptorV1, attempt: &str| {
        let mut envelope = TransportEnvelopeV1::new(
            AgentIdentityV1 {
                agent_id: "sender".to_string(),
                agent_type: "test".to_string(),
                daemon_fingerprint: "fingerprint".to_string(),
                capabilities: vec!["a2a_messaging".to_string()],
            },
            Some("recipient"),
            TransportContentType::A2ATask,
            serde_json::to_string(descriptor).expect("descriptor JSON"),
        );
        envelope
            .metadata
            .insert("tenant_id".to_string(), "tenant-a".to_string());
        envelope
            .metadata
            .insert("project_id".to_string(), "project-a".to_string());
        envelope
            .metadata
            .insert("attempt".to_string(), attempt.to_string());
        envelope.sign(b"channel-secret").expect("sign envelope");
        serde_json::to_vec(&envelope).expect("envelope JSON")
    };
    let request = |body| {
        Request::builder()
            .method("POST")
            .uri("/a2a/deliver")
            .header("Host", "localhost")
            .header("Content-Type", "application/json")
            .header("Authorization", "Bearer bearer-secret")
            .body(Body::from(body))
            .expect("request")
    };

    let descriptor = make_descriptor("authorized task");
    let accepted = app
        .clone()
        .oneshot(request(make_body(&descriptor, "1")))
        .await
        .expect("accepted response");
    assert_eq!(accepted.status(), StatusCode::OK);
    let store_path =
        TaskStore::scoped_path(dir.path().to_str().expect("root string")).expect("store path");
    assert_eq!(
        TaskStore::read_locked(&store_path, |store| store.tasks.len()).expect("stored task"),
        1
    );

    let duplicate = app
        .clone()
        .oneshot(request(make_body(&descriptor, "2")))
        .await
        .expect("duplicate response");
    assert_eq!(duplicate.status(), StatusCode::OK);
    let duplicate_body = axum::body::to_bytes(duplicate.into_body(), 1_000_000)
        .await
        .expect("duplicate body");
    assert_eq!(
        serde_json::from_slice::<Value>(&duplicate_body).expect("duplicate JSON")["status"],
        "duplicate"
    );

    let mut rejected = descriptor.clone();
    rejected.description = "tampered after signing".to_string();
    let rejected_body = make_body(&rejected, "3");
    for _ in 0..2 {
        let response = app
            .clone()
            .oneshot(request(rejected_body.clone()))
            .await
            .expect("rejected response");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    let conflict = make_descriptor("different task with reused idempotency key");
    let response = app
        .oneshot(request(make_body(&conflict, "4")))
        .await
        .expect("conflict response");
    assert_eq!(response.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn relay_two_servers_deliver_and_replay_signed_evidence() {
    use crate::core::a2a::{
        relay::{RelayPeerConfigV1, RelayPeerTableV1, RelayRecordV1},
        remote_transport::RemoteTransport,
    };
    use crate::core::a2a_transport::{AgentIdentityV1, TransportContentType, TransportEnvelopeV1};
    use crate::core::context_kernel::evidence_bundle::EvidenceBundle;
    use lean_ctx_protocol::DataClassification;

    struct Server(tokio::task::JoinHandle<()>);
    impl Drop for Server {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let relay_root = tempfile::tempdir().unwrap();
    let recipient_root = tempfile::tempdir().unwrap();
    let relay_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let recipient_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let relay_url = format!("http://{}", relay_listener.local_addr().unwrap());
    let recipient_url = format!("http://{}", recipient_listener.local_addr().unwrap());
    let peer = |id: &str, recipient: &str, endpoint: &str, credential: &str| RelayPeerConfigV1 {
        schema_version: 1,
        peer_id: id.into(),
        endpoint_url: endpoint.into(),
        bearer_token: format!("{credential}-bearer"),
        channel_key: format!("{credential}-key"),
        origin_public_key: test_origin_public_key(id),
        recipient_id: recipient.into(),
        allowed_tenant_ids: vec!["tenant-a".into()],
        allowed_project_ids: vec!["project-a".into()],
        allowed_content_types: vec![TransportContentType::EvidenceBundle],
        allowed_classifications: vec![DataClassification::Internal],
        max_hops: 4,
        max_payload_bytes: 65536,
        retry_count: 0,
        retry_delay_ms: 0,
    };
    let table = |peers| RelayPeerTableV1 {
        schema_version: 1,
        peers,
    };
    let config = |root: &std::path::Path, recipient: &str, peers| HttpServerConfig {
        project_root: root.to_path_buf(),
        auth_token: Some("admin-only".into()),
        a2a_recipient_id: Some(recipient.into()),
        a2a_tenant_id: Some("tenant-a".into()),
        a2a_project_id: Some("project-a".into()),
        a2a_peers: table(peers),
        request_timeout_ms: 5000,
        ..HttpServerConfig::default()
    };
    let relay_cfg = config(
        relay_root.path(),
        "relay",
        vec![
            peer("origin", "relay", "https://origin.example", "origin"),
            peer("recipient", "recipient", &recipient_url, "relay"),
        ],
    );
    let recipient_cfg = config(
        recipient_root.path(),
        "recipient",
        vec![
            peer("origin", "recipient", "https://origin.example", "origin"),
            peer("relay", "recipient", &relay_url, "relay"),
        ],
    );
    let relay_app = build_app_router(&relay_cfg, None);
    let recipient_app = build_app_router(&recipient_cfg, None);
    let _relay_server = Server(tokio::spawn(async move {
        axum::serve(relay_listener, relay_app).await.unwrap();
    }));
    let _recipient_server = Server(tokio::spawn(async move {
        axum::serve(recipient_listener, recipient_app)
            .await
            .unwrap();
    }));
    let transport = RemoteTransport::for_peer_table(
        table(vec![peer("relay", "relay", &relay_url, "origin")]),
        "origin",
        Duration::from_secs(5),
        true,
    )
    .unwrap();
    let mut bundle = EvidenceBundle::new("task-relayed".into());
    bundle.finalize();
    let payload = serde_json::to_string(&bundle).unwrap();
    let now = chrono::Utc::now();
    let delivery_id = format!("two-server-{}", uuid::Uuid::new_v4());
    let mut record = RelayRecordV1::new_signed(
        &delivery_id,
        "origin",
        "recipient",
        "tenant-a",
        "project-a",
        TransportContentType::EvidenceBundle,
        DataClassification::Internal,
        now + chrono::Duration::minutes(1),
        4,
        now,
        &test_origin_key("origin"),
        "origin-key",
        payload.as_bytes(),
    )
    .unwrap();
    record
        .route_from_origin(
            "relay",
            now,
            &test_origin_public_key("origin"),
            "origin-key",
        )
        .unwrap();
    let mut envelope = TransportEnvelopeV1::new(
        AgentIdentityV1::from_current("origin", "test"),
        Some("recipient"),
        TransportContentType::EvidenceBundle,
        payload.clone(),
    );
    envelope
        .metadata
        .insert("tenant_id".into(), "tenant-a".into());
    envelope
        .metadata
        .insert("project_id".into(), "project-a".into());
    envelope.attach_relay_record(&record).unwrap();
    // Force a real recipient persistence failure, not a mock response.
    let mut invalid_hop = envelope.clone();
    let mut tampered_record = record.clone();
    tampered_record.hop_signature = "0".repeat(64);
    invalid_hop
        .metadata
        .remove(crate::core::a2a::relay::RELAY_METADATA_KEY);
    invalid_hop.attach_relay_record(&tampered_record).unwrap();
    assert!(matches!(transport.deliver_to("relay", &invalid_hop).await,
            Err(crate::core::a2a::remote_transport::TransportError::SerializationError(error))
                if error == "invalid relay signature"));
    // Repeating the same delivery must fail again, never return a false ACK.
    let handoffs = recipient_root.path().join(".lean-ctx/handoffs");
    std::fs::create_dir_all(&handoffs).unwrap();
    let obstruction = handoffs.join("evidence");
    std::fs::write(&obstruction, "test-only storage obstruction").unwrap();
    let mut signed = envelope.clone();
    signed.sign(b"origin-key").unwrap();
    let client = reqwest::Client::new();
    for _ in 0..2 {
        let response = client
            .post(format!("{relay_url}/a2a/deliver"))
            .header(crate::core::a2a::relay::RELAY_PEER_HEADER, "origin")
            .bearer_auth("origin-bearer")
            .timeout(Duration::from_secs(10))
            .json(&signed)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let result: serde_json::Value = response.json().await.unwrap();
        assert_eq!(result["error"], "downstream_delivery_failed");
        assert!(result.get("detail").is_none());
        assert!(obstruction.is_file());
    }
    std::fs::remove_file(&obstruction).unwrap();
    let scope = crate::core::a2a::dlq::DlqScope::new("tenant-a", "project-a").unwrap();
    let letters = crate::core::ocla::health::dead_letter_queue()
        .peek(&scope)
        .unwrap()
        .into_iter()
        .filter(|letter| letter.delivery_id == delivery_id && letter.peer_id == "recipient")
        .collect::<Vec<_>>();
    assert!(
        !letters.is_empty(),
        "failed forwarding must persist a retryable dead letter"
    );
    let retry_transport = RemoteTransport::for_peer_table(
        relay_cfg.a2a_peers.clone(),
        "relay",
        Duration::from_secs(5),
        true,
    )
    .unwrap();
    for (tenant_id, project_id) in [("wrong-tenant", "project-a"), ("tenant-a", "wrong-project")] {
        let mut mismatched = letters[0].clone();
        mismatched.tenant_id = tenant_id.into();
        mismatched.project_id = project_id.into();
        assert!(matches!(
            retry_transport.retry_dead_letter(&mismatched).await,
            Err(crate::core::a2a::remote_transport::TransportError::SerializationError(message))
                if message == "dead letter scope does not match relay record"
        ));
        assert!(
            !recipient_root
                .path()
                .join(".lean-ctx/handoffs/evidence")
                .exists()
        );
    }
    for letter in &letters {
        assert_eq!(
            letter.delivery,
            crate::core::a2a::dlq::DeadLetterDelivery::RemoteHttp {
                endpoint_url: recipient_url.clone(),
            }
        );
        let receipt = retry_transport.retry_dead_letter(letter).await.unwrap();
        assert_eq!(receipt.envelope_id, delivery_id);
        assert_eq!(receipt.remote_status, 200);
    }
    for _ in 0..2 {
        let receipt = transport.deliver_to("relay", &envelope).await.unwrap();
        assert_eq!(receipt.remote_status, 200);
        let files = std::fs::read_dir(recipient_root.path().join(".lean-ctx/handoffs/evidence"))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(std::fs::read_to_string(files[0].path()).unwrap(), payload);
        assert!(
            !relay_root
                .path()
                .join(".lean-ctx/handoffs/evidence")
                .exists()
        );
    }
}

#[tokio::test]
async fn relay_http_replay_separates_origins_and_rejects_changed_content() {
    use crate::core::a2a::relay::{
        RELAY_PEER_HEADER, RelayPeerConfigV1, RelayPeerTableV1, RelayRecordV1,
    };
    use crate::core::a2a_transport::{AgentIdentityV1, TransportContentType, TransportEnvelopeV1};
    use crate::core::context_kernel::evidence_bundle::EvidenceBundle;
    use lean_ctx_protocol::DataClassification;

    let root = tempfile::tempdir().unwrap();
    let peer = |id: &str| RelayPeerConfigV1 {
        schema_version: 1,
        peer_id: id.into(),
        endpoint_url: format!("https://{id}.example"),
        bearer_token: format!("{id}-bearer"),
        channel_key: format!("{id}-key"),
        origin_public_key: test_origin_public_key(id),
        recipient_id: "recipient".into(),
        allowed_tenant_ids: vec!["tenant-a".into()],
        allowed_project_ids: vec!["project-a".into()],
        allowed_content_types: vec![TransportContentType::EvidenceBundle],
        allowed_classifications: vec![DataClassification::Internal],
        max_hops: 4,
        max_payload_bytes: 65536,
        retry_count: 0,
        retry_delay_ms: 0,
    };
    let cfg = HttpServerConfig {
        project_root: root.path().to_path_buf(),
        a2a_recipient_id: Some("recipient".into()),
        a2a_tenant_id: Some("tenant-a".into()),
        a2a_project_id: Some("project-a".into()),
        a2a_peers: RelayPeerTableV1 {
            schema_version: 1,
            peers: vec![peer("origin-a"), peer("origin-b"), peer("relay")],
        },
        ..HttpServerConfig::default()
    };
    cfg.validate().unwrap();
    let now = chrono::Utc::now();
    let message = |origin: &str, task: &str| {
        let mut bundle = EvidenceBundle::new(task.into());
        bundle.finalize();
        let payload = serde_json::to_string(&bundle).unwrap();
        let key = format!("{origin}-key");
        let mut record = RelayRecordV1::new_signed(
            "shared-delivery-id",
            origin,
            "recipient",
            "tenant-a",
            "project-a",
            TransportContentType::EvidenceBundle,
            DataClassification::Internal,
            now + chrono::Duration::minutes(5),
            4,
            now,
            &test_origin_key(origin),
            &key,
            payload.as_bytes(),
        )
        .unwrap();
        record
            .route_from_origin("relay", now, &test_origin_public_key(origin), &key)
            .unwrap();
        record
            .forward_from("relay", "recipient", now, "relay-key")
            .unwrap();
        let mut envelope = TransportEnvelopeV1::new(
            AgentIdentityV1::from_current("relay", "test"),
            Some("recipient"),
            TransportContentType::EvidenceBundle,
            payload,
        );
        envelope
            .metadata
            .insert("tenant_id".into(), "tenant-a".into());
        envelope
            .metadata
            .insert("project_id".into(), "project-a".into());
        envelope.attach_relay_record(&record).unwrap();
        envelope.sign(b"relay-key").unwrap();
        serde_json::to_vec(&envelope).unwrap()
    };
    for (index, (body, expected)) in [
        (message("origin-a", "task-a"), StatusCode::OK),
        (message("origin-b", "task-b"), StatusCode::OK),
        (message("origin-a", "task-a"), StatusCode::OK),
        (message("origin-a", "changed-task"), StatusCode::CONFLICT),
        (
            message("origin-a", "task-a"),
            StatusCode::SERVICE_UNAVAILABLE,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let database = root.path().join(".lean-ctx/a2a/relay-replay-v1.sqlite");
        if index == 4 {
            // Deleting only the established database must not reset proofs.
            std::fs::remove_file(&database).unwrap();
        }
        let request = Request::builder()
            .method("POST")
            .uri("/a2a/deliver")
            .header("Content-Type", "application/json")
            .header(RELAY_PEER_HEADER, "relay")
            .header(
                "Authorization",
                format!(
                    "Bearer {}",
                    cfg.a2a_peers.peer("relay").unwrap().bearer_token
                ),
            )
            .body(Body::from(body))
            .unwrap();
        // Recreate the complete receiver state for every delivery. Replay
        // success must come from durable state, never an in-memory cache.
        let response = build_app_router(&cfg, None).oneshot(request).await.unwrap();
        assert_eq!(response.status(), expected);
        if index == 2 {
            let body = axum::body::to_bytes(response.into_body(), 1024)
                .await
                .unwrap();
            let body: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(body["status"], "already_received");
        }
        if index == 4 {
            assert!(!database.exists());
        }
    }
    let stored = std::fs::read_dir(root.path().join(".lean-ctx/handoffs/evidence"))
        .unwrap()
        .map(|file| {
            let bytes = std::fs::read(file.unwrap().path()).unwrap();
            serde_json::from_slice::<EvidenceBundle>(&bytes)
                .unwrap()
                .task_id
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        stored,
        std::collections::BTreeSet::from(["task-a".to_string(), "task-b".to_string()])
    );
}

#[tokio::test]
async fn relay_http_quota_returns_retry_after_without_charging_failed_auth() {
    use crate::core::a2a::relay::{RELAY_PEER_HEADER, RelayPeerConfigV1, RelayRecordV1};
    use crate::core::a2a_transport::{AgentIdentityV1, TransportContentType, TransportEnvelopeV1};
    use crate::core::context_kernel::evidence_bundle::EvidenceBundle;
    use lean_ctx_protocol::DataClassification;
    let root = tempfile::tempdir().unwrap();
    let root_str = root.path().to_str().unwrap().to_string();
    let peer = RelayPeerConfigV1 {
        schema_version: 1,
        peer_id: "origin".into(),
        endpoint_url: "https://origin.example".into(),
        bearer_token: "origin-bearer".into(),
        channel_key: "origin-key".into(),
        origin_public_key: test_origin_public_key("origin"),
        recipient_id: "recipient".into(),
        allowed_tenant_ids: vec!["tenant".into()],
        allowed_project_ids: vec!["project".into()],
        allowed_content_types: vec![TransportContentType::EvidenceBundle],
        allowed_classifications: vec![DataClassification::Internal],
        max_hops: 4,
        max_payload_bytes: 65536,
        retry_count: 0,
        retry_delay_ms: 0,
    };
    let quota_config = RelayQuotaConfig {
        peer: RelayQuota {
            requests_per_second: 1,
            burst: 1,
        },
        ..RelayQuotaConfig::default()
    };
    // Pin refill beyond this test so scheduler delays cannot refill its
    // single token; exercise the real HTTP handler, not a canned response.
    let quotas = relay_rate::RelayQuotaState::new(
        quota_config,
        &["origin".into()],
        "tenant",
        "project",
        std::time::Instant::now() + Duration::from_hours(1),
    )
    .unwrap();
    let state = AppState {
        token: None,
        a2a_signing_key: None,
        a2a_recipient_id: Some("recipient".into()),
        a2a_tenant_id: Some("tenant".into()),
        a2a_project_id: Some("project".into()),
        a2a_peers: Arc::new(RelayPeerTableV1 {
            schema_version: 1,
            peers: vec![peer],
        }),
        relay_quotas: Some(Arc::new(tokio::sync::Mutex::new(quotas))),
        a2a_task_authority: Arc::new(crate::core::a2a::task::TaskAuthorityConfigV1::default()),
        concurrency: Arc::new(tokio::sync::Semaphore::new(4)),
        rate: Arc::new(RateLimiter::new(50, 100)),
        remote_replays: Arc::new(remote_replay::RemoteReplayGuard::new(
            &root_str,
            handlers::REMOTE_REPLAY_RETENTION_SECONDS,
        )),
        project_root: root_str.clone(),
        timeout: Duration::from_secs(5),
        server: LeanCtxServer::new_shared_with_context(&root_str, "default", "default"),
    };
    let app = Router::new()
        .route("/a2a/deliver", axum::routing::post(a2a_deliver))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            concurrency_middleware,
        ))
        .with_state(state.clone());
    let mut bundle = EvidenceBundle::new("quota-task".into());
    bundle.finalize();
    let payload = serde_json::to_string(&bundle).unwrap();
    let now = chrono::Utc::now();
    let record = RelayRecordV1::new_signed(
        "quota-delivery",
        "origin",
        "recipient",
        "tenant",
        "project",
        TransportContentType::EvidenceBundle,
        DataClassification::Internal,
        now + chrono::Duration::minutes(5),
        4,
        now,
        &test_origin_key("origin"),
        "origin-key",
        payload.as_bytes(),
    )
    .unwrap();
    let mut envelope = TransportEnvelopeV1::new(
        AgentIdentityV1::from_current("origin", "test"),
        Some("recipient"),
        TransportContentType::EvidenceBundle,
        payload,
    );
    envelope
        .metadata
        .insert("tenant_id".into(), "tenant".into());
    envelope
        .metadata
        .insert("project_id".into(), "project".into());
    envelope.attach_relay_record(&record).unwrap();
    envelope.sign(b"origin-key").unwrap();
    let body = serde_json::to_vec(&envelope).unwrap();
    for (bearer, expected) in [
        ("wrong", StatusCode::UNAUTHORIZED),
        ("origin-bearer", StatusCode::OK),
        ("origin-bearer", StatusCode::TOO_MANY_REQUESTS),
    ] {
        let request = Request::builder()
            .method("POST")
            .uri("/a2a/deliver")
            .header("Content-Type", "application/json")
            .header(RELAY_PEER_HEADER, "origin")
            .header("Authorization", format!("Bearer {bearer}"))
            .body(Body::from(body.clone()))
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), expected);
        if expected == StatusCode::TOO_MANY_REQUESTS {
            assert_eq!(response.headers()[header::RETRY_AFTER], "1");
            let bytes = axum::body::to_bytes(response.into_body(), 1024)
                .await
                .unwrap();
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
                serde_json::json!({"error": "relay_rate_limited"})
            );
        }
    }
    let mut unavailable_state = state;
    unavailable_state.relay_quotas = None;
    let unavailable_app = Router::new()
        .route("/a2a/deliver", axum::routing::post(a2a_deliver))
        .layer(middleware::from_fn_with_state(
            unavailable_state.clone(),
            concurrency_middleware,
        ))
        .with_state(unavailable_state);
    let request = Request::builder()
        .method("POST")
        .uri("/a2a/deliver")
        .header("Content-Type", "application/json")
        .header(RELAY_PEER_HEADER, "origin")
        .header("Authorization", "Bearer origin-bearer")
        .body(Body::from(body))
        .unwrap();
    let response = unavailable_app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(!response.headers().contains_key(header::RETRY_AFTER));
    let bytes = axum::body::to_bytes(response.into_body(), 1024)
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
        serde_json::json!({"error": "relay_quota_unavailable"})
    );
    assert_eq!(
        std::fs::read_dir(root.path().join(".lean-ctx/handoffs/evidence"))
            .unwrap()
            .count(),
        1
    );
}

#[tokio::test]
async fn relay_evidence_rejects_invalid_bundles_before_storage() {
    use crate::core::a2a_transport::{AgentIdentityV1, TransportContentType, TransportEnvelopeV1};
    use crate::core::context_kernel::evidence_bundle::EvidenceBundle;

    let dir = tempfile::tempdir().unwrap();
    let cfg = HttpServerConfig {
        project_root: dir.path().to_path_buf(),
        auth_token: Some("evidence-bearer".into()),
        a2a_signing_key: Some("evidence-key".into()),
        a2a_recipient_id: Some("recipient".into()),
        a2a_tenant_id: Some("tenant-a".into()),
        a2a_project_id: Some("project-a".into()),
        ..HttpServerConfig::default()
    };
    let app = build_app_router(&cfg, None);
    let mut bundle = EvidenceBundle::new("task-1".into());
    let unfinalized = serde_json::to_string(&bundle).unwrap();
    bundle.finalize();
    let valid = serde_json::to_string(&bundle).unwrap();
    bundle.bundle_hash.push('0');
    let tampered = serde_json::to_string(&bundle).unwrap();
    for (payload, expected) in [
        ("{}".to_string(), StatusCode::BAD_REQUEST),
        ("[]".to_string(), StatusCode::BAD_REQUEST),
        (unfinalized, StatusCode::BAD_REQUEST),
        (tampered, StatusCode::BAD_REQUEST),
        (valid, StatusCode::OK),
    ] {
        let mut envelope = TransportEnvelopeV1::new(
            AgentIdentityV1::from_current("sender", "test"),
            Some("recipient"),
            TransportContentType::EvidenceBundle,
            payload,
        );
        envelope
            .metadata
            .insert("tenant_id".into(), "tenant-a".into());
        envelope
            .metadata
            .insert("project_id".into(), "project-a".into());
        envelope.sign(b"evidence-key").unwrap();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/a2a/deliver")
                    .header("Host", "localhost")
                    .header("Content-Type", "application/json")
                    .header(
                        "Authorization",
                        format!("Bearer {}", cfg.auth_token.as_deref().unwrap()),
                    )
                    .body(Body::from(serde_json::to_vec(&envelope).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
        let evidence_dir = dir.path().join(".lean-ctx/handoffs/evidence");
        if expected == StatusCode::BAD_REQUEST {
            assert!(
                !evidence_dir.exists(),
                "rejected bundle must not reach storage"
            );
        } else {
            assert_eq!(std::fs::read_dir(evidence_dir).unwrap().count(), 1);
        }
    }
}

#[tokio::test]
async fn relay_context_packages_validate_before_storage() {
    use crate::core::a2a_transport::{AgentIdentityV1, TransportContentType, TransportEnvelopeV1};
    use crate::core::context_package::{PackageBuilder, signing::sign_package};

    let mut session = crate::core::session::SessionState::new();
    session.set_task("portable context task", None);
    let (mut manifest, content) = PackageBuilder::new("relay-context", "1.0.0")
        .add_session(&session)
        .build()
        .unwrap();
    let serialize = |manifest: &crate::core::context_package::manifest::PackageManifest| {
        // Preserve writer field order: integrity hashes the compacted raw content.
        format!(
            "{{\"manifest\":{},\"content\":{}}}",
            serde_json::to_string(manifest).unwrap(),
            serde_json::to_string_pretty(&content).unwrap()
        )
    };
    let unsigned = serialize(&manifest);
    sign_package(&mut manifest, &content, &test_origin_key("package-author"));
    let signed = serialize(&manifest);
    let tampered = signed.replace("portable context task", "changed context task");
    let mut bad_signature = manifest.clone();
    bad_signature.signature.as_mut().unwrap().value = "0".repeat(128);
    let mut addon = manifest.clone();
    addon.kind = crate::core::context_package::manifest::PackageKind::Addon;

    let root = tempfile::tempdir().unwrap();
    let bearer = "context-test-bearer";
    let cfg = HttpServerConfig {
        project_root: root.path().to_path_buf(),
        auth_token: Some(bearer.into()),
        a2a_signing_key: Some("context-test-channel".into()),
        a2a_recipient_id: Some("recipient".into()),
        a2a_tenant_id: Some("tenant".into()),
        a2a_project_id: Some("project".into()),
        ..HttpServerConfig::default()
    };
    cfg.validate().unwrap();
    let app = build_app_router(&cfg, None);
    let packages = root.path().join(".lean-ctx/handoffs/packages");
    let mut accepted = 0;
    for (payload, expected) in [
        ("not json".to_string(), StatusCode::BAD_REQUEST),
        ("{}".to_string(), StatusCode::BAD_REQUEST),
        ("[]".to_string(), StatusCode::BAD_REQUEST),
        (tampered, StatusCode::BAD_REQUEST),
        (serialize(&bad_signature), StatusCode::BAD_REQUEST),
        (serialize(&addon), StatusCode::BAD_REQUEST),
        (unsigned, StatusCode::OK),
        (signed, StatusCode::OK),
    ] {
        let mut envelope = TransportEnvelopeV1::new(
            AgentIdentityV1::from_current("sender", "test"),
            Some("recipient"),
            TransportContentType::ContextPackage,
            payload.clone(),
        );
        envelope
            .metadata
            .insert("tenant_id".into(), "tenant".into());
        envelope
            .metadata
            .insert("project_id".into(), "project".into());
        envelope.sign(b"context-test-channel").unwrap();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/a2a/deliver")
                    .header("Host", "localhost")
                    .header("Content-Type", "application/json")
                    .header("Authorization", format!("Bearer {bearer}"))
                    .body(Body::from(serde_json::to_vec(&envelope).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
        if expected == StatusCode::BAD_REQUEST {
            assert!(
                !packages.exists(),
                "invalid package must not create storage"
            );
        } else {
            accepted += 1;
            let files = std::fs::read_dir(&packages)
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            assert_eq!(files.len(), accepted);
            assert!(
                files
                    .iter()
                    .any(|entry| std::fs::read_to_string(entry.path()).unwrap() == payload)
            );
        }
    }
}

include!("main_http_tests_tail.rs");
