// SPDX-License-Identifier: Apache-2.0

#[tokio::test]
async fn relay_origin_policy_cannot_be_widened_by_forwarding_peer() {
    use crate::core::a2a::relay::{
        RELAY_PEER_HEADER, RelayPeerConfigV1, RelayPeerTableV1, RelayRecordV1,
    };
    use crate::core::a2a_transport::{AgentIdentityV1, TransportContentType, TransportEnvelopeV1};
    use lean_ctx_protocol::DataClassification;

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
        allowed_content_types: vec![TransportContentType::ContextPackage],
        allowed_classifications: vec![DataClassification::Internal],
        max_hops: 4,
        max_payload_bytes: 1024,
        retry_count: 0,
        retry_delay_ms: 0,
    };
    let now = chrono::Utc::now();
    let mut record = RelayRecordV1::new_signed(
        "policy-delivery",
        "origin",
        "recipient",
        "tenant-a",
        "project-a",
        TransportContentType::ContextPackage,
        DataClassification::Internal,
        now + chrono::Duration::minutes(1),
        4,
        now,
        &test_origin_key("origin"),
        "origin-key",
        b"{}",
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
    record
        .forward_from("relay", "recipient", now, "relay-key")
        .unwrap();
    let mut envelope = TransportEnvelopeV1::new(
        AgentIdentityV1 {
            agent_id: "relay".into(),
            agent_type: "test".into(),
            daemon_fingerprint: "fingerprint".into(),
            capabilities: vec![],
        },
        Some("recipient"),
        TransportContentType::ContextPackage,
        "{}".into(),
    );
    envelope
        .metadata
        .insert("tenant_id".into(), "tenant-a".into());
    envelope
        .metadata
        .insert("project_id".into(), "project-a".into());
    envelope.attach_relay_record(&record).unwrap();
    envelope.sign(b"relay-key").unwrap();
    let body = serde_json::to_vec(&envelope).unwrap();
    for denied in [
        "tenant",
        "project",
        "type",
        "classification",
        "size",
        "hops",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mut origin = peer("origin");
        match denied {
            "tenant" => origin.allowed_tenant_ids = vec!["other".into()],
            "project" => origin.allowed_project_ids = vec!["other".into()],
            "type" => origin.allowed_content_types = vec![TransportContentType::A2ATask],
            "classification" => {
                origin.allowed_classifications = vec![DataClassification::Public];
            }
            "size" => origin.max_payload_bytes = 1,
            "hops" => origin.max_hops = 1,
            _ => unreachable!(),
        }
        assert!(peer("relay").allows(&record, 2));
        origin.validate(false).unwrap();
        assert!(!origin.allows(&record, 2));
        let cfg = HttpServerConfig {
            project_root: dir.path().to_path_buf(),
            a2a_recipient_id: Some("recipient".into()),
            a2a_tenant_id: Some("tenant-a".into()),
            a2a_project_id: Some("project-a".into()),
            a2a_peers: RelayPeerTableV1 {
                schema_version: 1,
                peers: vec![origin, peer("relay")],
            },
            ..HttpServerConfig::default()
        };
        let response = build_app_router(&cfg, None)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/a2a/deliver")
                    .header("Host", "localhost")
                    .header("Content-Type", "application/json")
                    .header(
                        "Authorization",
                        format!(
                            "Bearer {}",
                            cfg.a2a_peers.peer("relay").unwrap().bearer_token
                        ),
                    )
                    .header(RELAY_PEER_HEADER, "relay")
                    .body(Body::from(body.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{denied}");
        let bytes = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        let result: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(result["error"], "relay_policy_denied", "{denied}");
    }
}

#[tokio::test]
async fn remote_transport_uses_distinct_credentials_over_real_http() {
    use crate::core::a2a::remote_transport::{RemoteTransport, RemoteTransportConfig};
    use crate::core::a2a_transport::{AgentIdentityV1, TransportContentType, TransportEnvelopeV1};

    let dir = tempfile::tempdir().expect("tempdir");
    let cfg = HttpServerConfig {
        project_root: dir.path().to_path_buf(),
        auth_token: Some("bearer-secret".to_string()),
        a2a_signing_key: Some("signing-secret".to_string()),
        a2a_recipient_id: Some("recipient".to_string()),
        a2a_tenant_id: Some("tenant-a".to_string()),
        a2a_project_id: Some("project-a".to_string()),
        ..HttpServerConfig::default()
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test server");
    let address = listener.local_addr().expect("test server address");
    let server = tokio::spawn(async move {
        axum::serve(listener, build_app_router(&cfg, None))
            .await
            .expect("serve test app");
    });
    let envelope = TransportEnvelopeV1::new(
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
    let transport = RemoteTransport::new(RemoteTransportConfig {
        endpoint_url: format!("http://{address}"),
        auth_token: Some("bearer-secret".to_string()),
        signing_key: Some("signing-secret".to_string()),
        recipient_id: Some("recipient".to_string()),
        tenant_id: Some("tenant-a".to_string()),
        project_id: Some("project-a".to_string()),
        retry_count: 0,
        ..RemoteTransportConfig::default()
    })
    .expect("valid remote transport");
    let receipt = transport.deliver(&envelope).await.expect("remote delivery");
    assert_eq!(receipt.remote_status, StatusCode::OK.as_u16());

    let wrong_signer = RemoteTransport::new(RemoteTransportConfig {
        endpoint_url: format!("http://{address}"),
        auth_token: Some("bearer-secret".to_string()),
        signing_key: Some("wrong-signing-secret".to_string()),
        recipient_id: Some("recipient".to_string()),
        tenant_id: Some("tenant-a".to_string()),
        project_id: Some("project-a".to_string()),
        retry_count: 0,
        ..RemoteTransportConfig::default()
    })
    .expect("valid transport with wrong remote key");
    assert!(matches!(
        wrong_signer.deliver(&envelope).await,
        Err(crate::core::a2a::remote_transport::TransportError::RemoteError(401, _))
    ));

    let wrong_scope = RemoteTransport::new(RemoteTransportConfig {
        endpoint_url: format!("http://{address}"),
        auth_token: Some("bearer-secret".to_string()),
        signing_key: Some("signing-secret".to_string()),
        recipient_id: Some("recipient".to_string()),
        tenant_id: Some("tenant-b".to_string()),
        project_id: Some("project-a".to_string()),
        retry_count: 0,
        ..RemoteTransportConfig::default()
    })
    .expect("valid transport with wrong remote scope");
    assert!(matches!(
        wrong_scope.deliver(&envelope).await,
        Err(crate::core::a2a::remote_transport::TransportError::RemoteError(401, _))
    ));
    server.abort();
}

#[tokio::test]
async fn mcp_service_factory_isolates_per_client_state() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root_str = dir.path().to_string_lossy().to_string();

    // Mirrors the serve() setup: service_factory must create a fresh server per MCP session.
    let service_project_root = root_str.clone();
    let service_factory = move || -> Result<LeanCtxServer, std::convert::Infallible> {
        Ok(LeanCtxServer::new_shared_with_context(
            &service_project_root,
            "default",
            "default",
        ))
    };

    let s1 = service_factory().expect("server 1");
    let s2 = service_factory().expect("server 2");

    // If the two servers accidentally share the same Arc-backed fields, these writes would
    // clobber each other. This test stays independent of rmcp's InitializeRequestParams API.
    *s1.client_name.write().await = "client-a".to_string();
    *s2.client_name.write().await = "client-b".to_string();

    let a = s1.client_name.read().await.clone();
    let b = s2.client_name.read().await.clone();
    assert_eq!(a, "client-a");
    assert_eq!(b, "client-b");
}

#[tokio::test]
async fn rate_limit_returns_429_when_exhausted() {
    let state = AppState {
        token: None,
        a2a_signing_key: None,
        a2a_recipient_id: None,
        a2a_tenant_id: None,
        a2a_project_id: None,
        a2a_peers: Arc::new(RelayPeerTableV1::default()),
        relay_quotas: None,
        a2a_task_authority: Arc::new(crate::core::a2a::task::TaskAuthorityConfigV1::default()),
        concurrency: Arc::new(tokio::sync::Semaphore::new(16)),
        rate: Arc::new(RateLimiter::new(1, 1)),
        remote_replays: Arc::new(remote_replay::RemoteReplayGuard::new(
            ".",
            handlers::REMOTE_REPLAY_RETENTION_SECONDS,
        )),
        project_root: ".".to_string(),
        timeout: Duration::from_secs(30),
        server: LeanCtxServer::new_shared_with_context(".", "default", "default"),
    };

    let app = Router::new()
        .route("/limited", get(|| async { (StatusCode::OK, "ok\n") }))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            rate_limit_middleware,
        ))
        .with_state(state);

    let req1 = Request::builder()
        .method("GET")
        .uri("/limited")
        .header("Host", "localhost")
        .body(Body::empty())
        .expect("req1");
    let resp1 = app.clone().oneshot(req1).await.expect("resp1");
    assert_eq!(resp1.status(), StatusCode::OK);

    let req2 = Request::builder()
        .method("GET")
        .uri("/limited")
        .header("Host", "localhost")
        .body(Body::empty())
        .expect("req2");
    let resp2 = app.clone().oneshot(req2).await.expect("resp2");
    assert_eq!(resp2.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn audit_events_endpoint_returns_json() {
    // The endpoint reads process-global audit paths. Isolate and serialize
    // the environment so concurrent tests cannot replace its data directory.
    let _isolated_data_dir = crate::core::data_dir::isolated_data_dir();
    let dir = tempfile::tempdir().expect("tempdir");
    let root_str = dir.path().to_string_lossy().to_string();

    let state = AppState {
        token: None,
        a2a_signing_key: None,
        a2a_recipient_id: None,
        a2a_tenant_id: None,
        a2a_project_id: None,
        a2a_peers: Arc::new(RelayPeerTableV1::default()),
        relay_quotas: None,
        a2a_task_authority: Arc::new(crate::core::a2a::task::TaskAuthorityConfigV1::default()),
        concurrency: Arc::new(tokio::sync::Semaphore::new(16)),
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
        .route("/v1/audit/events", get(v1_audit_events))
        .with_state(state);

    let req = Request::builder()
        .method("GET")
        .uri("/v1/audit/events?limit=10")
        .header("Host", "localhost")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), 1_000_000)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json.get("cross_project_events").unwrap().is_array());
    assert!(json.get("audit_trail").unwrap().is_array());
}

#[tokio::test]
async fn capabilities_endpoint_returns_contract() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root_str = dir.path().to_string_lossy().to_string();

    let state = AppState {
        token: None,
        a2a_signing_key: None,
        a2a_recipient_id: None,
        a2a_tenant_id: None,
        a2a_project_id: None,
        a2a_peers: Arc::new(RelayPeerTableV1::default()),
        relay_quotas: None,
        a2a_task_authority: Arc::new(crate::core::a2a::task::TaskAuthorityConfigV1::default()),
        concurrency: Arc::new(tokio::sync::Semaphore::new(16)),
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
        .route("/v1/capabilities", get(v1_capabilities))
        .with_state(state);

    let req = Request::builder()
        .method("GET")
        .uri("/v1/capabilities")
        .header("Host", "localhost")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), 1_000_000)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["contract_version"], json!(1));
    assert!(json["tools"]["total"].as_u64().unwrap() > 0);
    assert!(json["features"]["compression"].as_bool().unwrap());
    assert!(json["contracts"].is_object());
}

#[tokio::test]
async fn cache_stats_endpoint_returns_live_shape() {
    let app = Router::new().route("/v1/cache/stats", get(v1_cache_stats));
    let request = Request::builder()
        .method("GET")
        .uri("/v1/cache/stats")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1_000_000)
        .await
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(value["l1"]["entries"].is_u64());
    assert!(value["l2"]["hit_rate"].is_number());
    assert!(value["l3"]["bytes"].is_u64());
    assert!(value["delivery"]["references_served"].is_u64());
    assert!(value["by_kind"]["shell_command"].is_object());
}

#[tokio::test]
async fn openapi_endpoint_returns_spec() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root_str = dir.path().to_string_lossy().to_string();

    let state = AppState {
        token: None,
        a2a_signing_key: None,
        a2a_recipient_id: None,
        a2a_tenant_id: None,
        a2a_project_id: None,
        a2a_peers: Arc::new(RelayPeerTableV1::default()),
        relay_quotas: None,
        a2a_task_authority: Arc::new(crate::core::a2a::task::TaskAuthorityConfigV1::default()),
        concurrency: Arc::new(tokio::sync::Semaphore::new(16)),
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
        .route("/v1/openapi.json", get(v1_openapi))
        .with_state(state);

    let req = Request::builder()
        .method("GET")
        .uri("/v1/openapi.json")
        .header("Host", "localhost")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), 1_000_000)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["openapi"], json!("3.0.3"));
    assert!(json["paths"]["/v1/capabilities"]["get"].is_object());
    assert!(json["paths"]["/v1/openapi.json"]["get"].is_object());
}

#[tokio::test]
async fn events_endpoint_replays_tool_call_event() {
    use crate::core::context_os::{self, ContextEventKindV1};

    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join(".git")).expect("git marker");
    std::fs::write(dir.path().join("a.txt"), "ok").expect("file");
    let root_str = dir.path().to_string_lossy().to_string();
    let workspace = format!("ws-events-{}", std::process::id());
    let channel = format!("ch-events-{}", std::process::id());

    let state = AppState {
        token: None,
        a2a_signing_key: None,
        a2a_recipient_id: None,
        a2a_tenant_id: None,
        a2a_project_id: None,
        a2a_peers: Arc::new(RelayPeerTableV1::default()),
        relay_quotas: None,
        a2a_task_authority: Arc::new(crate::core::a2a::task::TaskAuthorityConfigV1::default()),
        concurrency: Arc::new(tokio::sync::Semaphore::new(16)),
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
        .route("/v1/events", get(v1_events))
        .with_state(state);

    // Directly append an event to the bus — no fire-and-forget timing dependency.
    let rt = context_os::runtime();
    rt.bus.append(
        &workspace,
        &channel,
        &ContextEventKindV1::ToolCallRecorded,
        Some("test-agent"),
        json!({"tool": "ctx_session", "action": "status"}),
    );

    let req = Request::builder()
        .method("GET")
        .uri(format!(
            "/v1/events?workspaceId={workspace}&channelId={channel}&since=0&limit=1"
        ))
        .header("Host", "localhost")
        .header("Accept", "text/event-stream")
        .body(Body::empty())
        .expect("req");
    let resp = app.clone().oneshot(req).await.expect("events");
    assert_eq!(resp.status(), StatusCode::OK);

    let msg = read_first_sse_message(resp.into_body()).await;
    assert!(msg.contains("event: tool_call_recorded"), "msg={msg:?}");
    assert!(msg.contains(&format!("\"{workspace}\"")), "msg={msg:?}");
    assert!(msg.contains(&format!("\"{channel}\"")), "msg={msg:?}");
}

#[tokio::test]
async fn events_endpoint_scopes_directed_events_to_agent_identity() {
    use crate::core::context_os::{self, ContextEventKindV1};

    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join(".git")).expect("git marker");
    let root_str = dir.path().to_string_lossy().to_string();
    let workspace = format!("ws-events-directed-{}", std::process::id());
    let channel = format!("ch-events-directed-{}", std::process::id());

    let state = AppState {
        token: None,
        a2a_signing_key: None,
        a2a_recipient_id: None,
        a2a_tenant_id: None,
        a2a_project_id: None,
        a2a_peers: Arc::new(RelayPeerTableV1::default()),
        relay_quotas: None,
        a2a_task_authority: Arc::new(crate::core::a2a::task::TaskAuthorityConfigV1::default()),
        concurrency: Arc::new(tokio::sync::Semaphore::new(16)),
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
        .route("/v1/events", get(v1_events))
        .with_state(state);
    let rt = context_os::runtime();
    let directed = rt
        .bus
        .append_directed(
            &workspace,
            &channel,
            &ContextEventKindV1::SessionMutated,
            Some("directed-agent"),
            json!({"secret":"directed"}),
            vec!["sse-agent".to_string()],
        )
        .expect("directed event");
    let _broadcast = rt
        .bus
        .append(
            &workspace,
            &channel,
            &ContextEventKindV1::ToolCallRecorded,
            Some("broadcast-agent"),
            json!({"marker":"broadcast"}),
        )
        .expect("broadcast event");

    let request = |query: String| {
        Request::builder()
            .method("GET")
            .uri(format!("/v1/events?{query}"))
            .header("Host", "localhost")
            .header("Accept", "text/event-stream")
            .body(Body::empty())
            .expect("events request")
    };

    let public_response = app
        .clone()
        .oneshot(request(format!(
            "workspaceId={workspace}&channelId={channel}&since=0&limit=10"
        )))
        .await
        .expect("public events");
    assert_eq!(public_response.status(), StatusCode::OK);
    let public_msg = read_first_sse_message(public_response.into_body()).await;
    assert!(public_msg.contains("broadcast-agent"), "msg={public_msg:?}");
    assert!(!public_msg.contains("directed-agent"), "msg={public_msg:?}");

    let scoped_response = app
        .oneshot(request(format!(
            "workspaceId={workspace}&channelId={channel}&since=0&limit=10&agentId=sse-agent",
        )))
        .await
        .expect("scoped events");
    assert_eq!(scoped_response.status(), StatusCode::OK);
    let scoped_msg = read_first_sse_message(scoped_response.into_body()).await;
    assert!(scoped_msg.contains("directed-agent"), "msg={scoped_msg:?}");
    assert!(
        scoped_msg.contains(&directed.id.to_string()),
        "msg={scoped_msg:?}"
    );
}
