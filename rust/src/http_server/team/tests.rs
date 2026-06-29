use super::super::RateLimiter;
use super::*;
use futures::StreamExt;
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

fn cfg_two(tmp: &tempfile::TempDir) -> TeamServerConfig {
    let ws1 = tmp.path().join("ws1");
    let ws2 = tmp.path().join("ws2");
    std::fs::create_dir_all(&ws1).unwrap();
    std::fs::create_dir_all(&ws2).unwrap();
    std::fs::write(ws1.join("ws1_marker.txt"), "1").unwrap();
    std::fs::write(ws2.join("ws2_marker.txt"), "2").unwrap();

    TeamServerConfig {
        host: "127.0.0.1".to_string(),
        port: 0,
        default_workspace_id: "ws1".to_string(),
        workspaces: vec![
            TeamWorkspaceConfig {
                id: "ws1".to_string(),
                label: None,
                root: ws1,
            },
            TeamWorkspaceConfig {
                id: "ws2".to_string(),
                label: None,
                root: ws2,
            },
        ],
        tokens: vec![TeamTokenConfig {
            id: "t1".to_string(),
            sha256_hex: sha256_hex(b"secret"),
            scopes: vec![
                TeamScope::Search,
                TeamScope::Events,
                TeamScope::SessionMutations,
                TeamScope::Knowledge,
                TeamScope::Audit,
            ],
            role: None,
        }],
        audit_log_path: tmp.path().join("audit.jsonl"),
        disable_host_check: true,
        allowed_hosts: vec![],
        max_body_bytes: 2 * 1024 * 1024,
        max_concurrency: 4,
        max_rps: 100,
        rate_burst: 100,
        request_timeout_ms: 30_000,
        stateful_mode: false,
        json_response: true,
        storage_quota_bytes: None,
        roi_webhook_url: None,
        connectors: vec![],
    }
}

async fn build_app(cfg: TeamServerConfig) -> Router {
    let team_server = TeamCtxServer {
        default_workspace_id: cfg.default_workspace_id.clone(),
        roots: Arc::new(
            cfg.workspaces
                .iter()
                .map(|w| (w.id.clone(), w.root.to_string_lossy().to_string()))
                .collect(),
        ),
    };
    let engine = Arc::new(TeamContextEngine::new(team_server));
    let audit_file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&cfg.audit_log_path)
        .await
        .unwrap();
    let workspace_roots: Vec<(String, std::path::PathBuf)> = cfg
        .workspaces
        .iter()
        .map(|w| (w.id.clone(), w.root.clone()))
        .collect();
    let team = Arc::new(TeamState {
        auth: Arc::new(cfg.tokens.clone()),
        engine,
        audit: Arc::new(tokio::sync::Mutex::new(audit_file)),
        savings_store_dir: Arc::new(tokio::sync::Mutex::new(
            cfg.audit_log_path
                .parent()
                .unwrap_or(std::path::Path::new("."))
                .join("savings"),
        )),
        storage_roots: super::super::team_billing::storage_roots_from_config(
            &cfg.audit_log_path,
            &workspace_roots,
            cfg.storage_quota_bytes,
        ),
        storage_cache: Arc::new(tokio::sync::Mutex::new(
            super::super::team_billing::StorageCache::default(),
        )),
        connectors: Arc::new(cfg.connectors.clone()),
        connectors_state_dir: Arc::new(
            cfg.audit_log_path
                .parent()
                .unwrap_or(std::path::Path::new("."))
                .join("connectors"),
        ),
    });
    let state = TeamAppState {
        concurrency: Arc::new(tokio::sync::Semaphore::new(4)),
        rate: Arc::new(RateLimiter::new(100, 100)),
        timeout: Duration::from_secs(30),
        team,
        max_body_bytes: 2 * 1024 * 1024,
    };

    Router::new()
        .route("/v1/tools/call", axum::routing::post(v1_tool_call))
        .route("/v1/events", get(v1_events))
        .route(
            "/v1/savings/summary",
            get(super::super::savings_summary::v1_savings_summary),
        )
        .route(
            "/v1/savings/member/{signer}",
            get(super::super::savings_summary::v1_savings_member),
        )
        .route("/v1/storage", get(super::super::team_billing::v1_storage))
        .route("/v1/usage", get(super::super::team_billing::v1_usage))
        .route("/v1/connectors", get(super::connectors::v1_connectors))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            team_auth_middleware,
        ))
        .with_state(state)
}

/// Two-token config: an `owner` (audit scope) and a `member` (search only),
/// used to prove the savings-summary scope gate end-to-end.
fn cfg_savings(tmp: &tempfile::TempDir) -> TeamServerConfig {
    let ws1 = tmp.path().join("ws1");
    std::fs::create_dir_all(&ws1).unwrap();
    TeamServerConfig {
        host: "127.0.0.1".to_string(),
        port: 0,
        default_workspace_id: "ws1".to_string(),
        workspaces: vec![TeamWorkspaceConfig {
            id: "ws1".to_string(),
            label: None,
            root: ws1,
        }],
        tokens: vec![
            TeamTokenConfig {
                id: "owner".to_string(),
                sha256_hex: sha256_hex(b"owner-secret"),
                scopes: vec![TeamScope::Audit],
                role: None,
            },
            TeamTokenConfig {
                id: "member".to_string(),
                sha256_hex: sha256_hex(b"member-secret"),
                scopes: vec![TeamScope::Search],
                role: None,
            },
        ],
        audit_log_path: tmp.path().join("audit.jsonl"),
        disable_host_check: true,
        allowed_hosts: vec![],
        max_body_bytes: 2 * 1024 * 1024,
        max_concurrency: 4,
        max_rps: 100,
        rate_burst: 100,
        request_timeout_ms: 30_000,
        stateful_mode: false,
        json_response: true,
        storage_quota_bytes: None,
        roi_webhook_url: None,
        connectors: vec![],
    }
}

#[tokio::test]
async fn missing_bearer_token_is_401() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = cfg_two(&tmp);
    let app = build_app(cfg).await;

    let body = json!({"name":"ctx_tree","arguments":{"path":".","depth":1}}).to_string();
    let req = Request::builder()
        .method("POST")
        .uri("/v1/tools/call")
        .header("Host", "localhost")
        .header("Content-Type", "application/json")
        .body(Body::from(body))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
#[ignore = "requires full MCP server initialization via serve_directly"]
async fn workspace_header_routes_tool_call_and_audits() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = cfg_two(&tmp);
    let audit_path = cfg.audit_log_path.clone();
    let app = build_app(cfg).await;

    let body = json!({"name":"ctx_tree","arguments":{"path":".","depth":2}}).to_string();
    let req = Request::builder()
        .method("POST")
        .uri("/v1/tools/call")
        .header("Host", "localhost")
        .header("Content-Type", "application/json")
        .header("Authorization", "Bearer secret")
        .header("x-leanctx-workspace", "ws2")
        .body(Body::from(body))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = body::to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    let all = v.to_string();
    assert!(all.contains("ws2_marker.txt"));
    assert!(!all.contains("ws1_marker.txt"));

    let log = std::fs::read_to_string(&audit_path).unwrap_or_default();
    assert!(log.contains("\"tokenId\":\"t1\""));
    assert!(log.contains("\"workspaceId\":\"ws2\""));
    assert!(log.contains("\"tool\":\"ctx_tree\""));
}

#[tokio::test]
#[ignore = "requires full MCP server initialization via serve_directly"]
async fn events_endpoint_replays_tool_call_event_for_workspace_channel() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = cfg_two(&tmp);
    let app = build_app(cfg).await;

    // Trigger a tool call for ws1 + channelId=ch1.
    let body = json!({
        "name":"ctx_tree",
        "arguments":{"path":".","depth":1},
        "channelId":"ch1"
    })
    .to_string();
    let req = Request::builder()
        .method("POST")
        .uri("/v1/tools/call")
        .header("Host", "localhost")
        .header("Content-Type", "application/json")
        .header("Authorization", "Bearer secret")
        .header(WORKSPACE_HEADER, "ws1")
        .body(Body::from(body))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Replay via SSE.
    let req = Request::builder()
        .method("GET")
        .uri("/v1/events?since=0&limit=1&channelId=ch1")
        .header("Host", "localhost")
        .header("Accept", "text/event-stream")
        .header("Authorization", "Bearer secret")
        .header(WORKSPACE_HEADER, "ws1")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let msg = read_first_sse_message(resp.into_body()).await;
    assert!(msg.contains("event: tool_call_recorded"), "msg={msg:?}");
    assert!(msg.contains("\"workspaceId\":\"ws1\""), "msg={msg:?}");
    assert!(msg.contains("\"channelId\":\"ch1\""), "msg={msg:?}");
    assert!(msg.contains("\"tool\":\"ctx_tree\""), "msg={msg:?}");
}

/// End-to-end proof of the customer-facing team-savings surface through the real
/// auth middleware: no token → 401, non-audit token → 403, audit token → 200
/// with the honest aggregated roll-up read back from the savings store.
#[tokio::test]
async fn savings_summary_scope_gated_and_aggregated() {
    use crate::core::savings_ledger::SignedSavingsBatchV1;
    use crate::core::savings_ledger::signed_batch::BatchTotals;

    let tmp = tempfile::tempdir().unwrap();
    let cfg = cfg_savings(&tmp);

    // Seed the store the way ingest would: one signed snapshot per signer.
    let savings_dir = tmp.path().join("savings");
    std::fs::create_dir_all(&savings_dir).unwrap();
    let mk = |signer: &str, net: u64, usd: f64| SignedSavingsBatchV1 {
        schema_version: 1,
        kind: "lean-ctx.savings-batch".into(),
        created_at: "2026-06-08T00:00:00Z".into(),
        lean_ctx_version: "test".into(),
        agent_id: format!("agent-{signer}"),
        period: "all".into(),
        first_entry_hash: "genesis".into(),
        last_entry_hash: "head".into(),
        chain_valid: true,
        totals: BatchTotals {
            total_events: 1,
            saved_tokens: net,
            net_saved_tokens: net,
            saved_usd: usd,
            bounce_tokens: 0,
            bounce_events: 0,
            tokenizers: vec!["o200k_base".into()],
            by_model: vec![("claude-opus".into(), net, usd)],
            by_tool: vec![("ctx_read".into(), net)],
        },
        signer_public_key: Some(signer.into()),
        signature: Some("sig".into()),
    };
    for (signer, net, usd) in [
        ("aaaaaaaaaaaaaaaa", 4200u64, 0.042f64),
        ("bbbbbbbbbbbbbbbb", 1800u64, 0.018f64),
    ] {
        std::fs::write(
            savings_dir.join(format!("savings_{signer}.jsonl")),
            serde_json::to_string(&mk(signer, net, usd)).unwrap() + "\n",
        )
        .unwrap();
    }

    let app = build_app(cfg).await;

    // 1) No bearer token → 401.
    let req = Request::builder()
        .method("GET")
        .uri("/v1/savings/summary")
        .header("Host", "localhost")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app.clone().oneshot(req).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );

    // 2) Valid token WITHOUT audit scope → 403 (sensitive team data).
    let req = Request::builder()
        .method("GET")
        .uri("/v1/savings/summary")
        .header("Host", "localhost")
        .header("Authorization", "Bearer member-secret")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app.clone().oneshot(req).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );

    // 3) Audit token → 200 with the honest cross-member roll-up.
    let req = Request::builder()
        .method("GET")
        .uri("/v1/savings/summary")
        .header("Host", "localhost")
        .header("Authorization", "Bearer owner-secret")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = body::to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["schema_version"], 2);
    assert_eq!(v["member_count"], 2);
    assert_eq!(v["totals"]["net_saved_tokens"], 6000); // 4200 + 1800
    assert_eq!(v["totals"]["total_events"], 2); // 1 + 1 (latest per signer)
    assert_eq!(v["by_member"][0]["net_saved_tokens"], 4200); // sorted desc
    assert_eq!(v["by_model"][0]["model"], "claude-opus");
    assert_eq!(v["by_model"][0]["saved_tokens"], 6000);
    // Tool breakdown is now surfaced (previously unused in the response).
    assert_eq!(v["by_tool"][0]["tool"], "ctx_read");
    assert_eq!(v["by_tool"][0]["saved_tokens"], 6000);
    // The cumulative daily series is present (geometry is unit-tested separately).
    assert!(v["series"].is_array());
}

/// End-to-end proof of the member drilldown (GL #389) through the real auth
/// middleware: audit-gated like the summary, 404 for unknown signers, 400 for
/// ids that could never be a signer (path-traversal defense), and the member
/// payload carries its own series + breakdowns.
#[tokio::test]
async fn savings_member_drilldown_scope_gated() {
    use crate::core::savings_ledger::SignedSavingsBatchV1;
    use crate::core::savings_ledger::signed_batch::BatchTotals;

    let tmp = tempfile::tempdir().unwrap();
    let cfg = cfg_savings(&tmp);

    let savings_dir = tmp.path().join("savings");
    std::fs::create_dir_all(&savings_dir).unwrap();
    let batch = SignedSavingsBatchV1 {
        schema_version: 1,
        kind: "lean-ctx.savings-batch".into(),
        created_at: "2026-06-08T00:00:00Z".into(),
        lean_ctx_version: "test".into(),
        agent_id: "agent-a".into(),
        period: "all".into(),
        first_entry_hash: "genesis".into(),
        last_entry_hash: "head".into(),
        chain_valid: true,
        totals: BatchTotals {
            total_events: 7,
            saved_tokens: 4200,
            net_saved_tokens: 4200,
            saved_usd: 0.042,
            bounce_tokens: 0,
            bounce_events: 0,
            tokenizers: vec!["o200k_base".into()],
            by_model: vec![("claude-opus".into(), 4200, 0.042)],
            by_tool: vec![("ctx_read".into(), 4200)],
        },
        signer_public_key: Some("aaaaaaaaaaaaaaaa".into()),
        signature: Some("sig".into()),
    };
    std::fs::write(
        savings_dir.join("savings_aaaaaaaaaaaaaaaa.jsonl"),
        serde_json::to_string(&batch).unwrap() + "\n",
    )
    .unwrap();

    let app = build_app(cfg).await;

    // Non-audit token → 403.
    let req = Request::builder()
        .method("GET")
        .uri("/v1/savings/member/aaaaaaaaaaaaaaaa")
        .header("Host", "localhost")
        .header("Authorization", "Bearer member-secret")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app.clone().oneshot(req).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );

    // Audit token + known signer → 200 with member-scoped payload.
    let req = Request::builder()
        .method("GET")
        .uri("/v1/savings/member/aaaaaaaaaaaaaaaa")
        .header("Host", "localhost")
        .header("Authorization", "Bearer owner-secret")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = body::to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["signer"], "aaaaaaaaaaaaaaaa");
    assert_eq!(v["agent_id"], "agent-a");
    assert_eq!(v["totals"]["net_saved_tokens"], 4200);
    assert_eq!(v["totals"]["total_events"], 7);
    assert_eq!(v["by_model"][0]["model"], "claude-opus");
    assert_eq!(v["by_tool"][0]["tool"], "ctx_read");
    assert!(v["series"].is_array());
    assert_eq!(v["window_days"], 90);

    // Unknown signer → 404.
    let req = Request::builder()
        .method("GET")
        .uri("/v1/savings/member/ffffffffffffffff")
        .header("Host", "localhost")
        .header("Authorization", "Bearer owner-secret")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app.clone().oneshot(req).await.unwrap().status(),
        StatusCode::NOT_FOUND
    );

    // Path-traversal shaped id → 400 before touching the filesystem.
    let req = Request::builder()
        .method("GET")
        .uri("/v1/savings/member/..%2F..%2Fetc%2Fpasswd")
        .header("Host", "localhost")
        .header("Authorization", "Bearer owner-secret")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app.oneshot(req).await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );
}

/// End-to-end proof of the billing-plane surface (GL #463) through the real
/// auth middleware: `/v1/storage` and `/v1/usage` are audit-gated and report
/// the shapes `lean-ctx-cloud`'s metering job/proxy parse (`usedBytes`
/// camelCase on storage; snake_case `storage.used_bytes` inside usage).
#[tokio::test]
async fn storage_and_usage_scope_gated_with_metering_shapes() {
    use crate::core::savings_ledger::SignedSavingsBatchV1;
    use crate::core::savings_ledger::signed_batch::BatchTotals;

    let tmp = tempfile::tempdir().unwrap();
    let cfg = cfg_savings(&tmp);

    // Real on-disk footprint: a savings snapshot (under the data root) and
    // workspace `.lean-ctx` state.
    let savings_dir = tmp.path().join("savings");
    std::fs::create_dir_all(&savings_dir).unwrap();
    let batch = SignedSavingsBatchV1 {
        schema_version: 1,
        kind: "lean-ctx.savings-batch".into(),
        created_at: "2026-06-08T00:00:00Z".into(),
        lean_ctx_version: "test".into(),
        agent_id: "agent-a".into(),
        period: "all".into(),
        first_entry_hash: "genesis".into(),
        last_entry_hash: "head".into(),
        chain_valid: true,
        totals: BatchTotals {
            total_events: 7,
            saved_tokens: 4200,
            net_saved_tokens: 4000,
            saved_usd: 0.042,
            bounce_tokens: 200,
            bounce_events: 1,
            tokenizers: vec!["o200k_base".into()],
            by_model: vec![("claude-opus".into(), 4200, 0.042)],
            by_tool: vec![("ctx_read".into(), 4200)],
        },
        signer_public_key: Some("aaaaaaaaaaaaaaaa".into()),
        signature: Some("sig".into()),
    };
    std::fs::write(
        savings_dir.join("savings_aaaaaaaaaaaaaaaa.jsonl"),
        serde_json::to_string(&batch).unwrap() + "\n",
    )
    .unwrap();
    let ws_state = tmp.path().join("ws1").join(".lean-ctx");
    std::fs::create_dir_all(&ws_state).unwrap();
    std::fs::write(ws_state.join("events.jsonl"), vec![b'e'; 8_192]).unwrap();

    let app = build_app(cfg).await;

    for path in ["/v1/storage", "/v1/usage"] {
        // 1) No bearer token → 401.
        let req = Request::builder()
            .method("GET")
            .uri(path)
            .header("Host", "localhost")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            app.clone().oneshot(req).await.unwrap().status(),
            StatusCode::UNAUTHORIZED,
            "{path} without token"
        );

        // 2) Valid token WITHOUT audit scope → 403.
        let req = Request::builder()
            .method("GET")
            .uri(path)
            .header("Host", "localhost")
            .header("Authorization", "Bearer member-secret")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            app.clone().oneshot(req).await.unwrap().status(),
            StatusCode::FORBIDDEN,
            "{path} without audit scope"
        );
    }

    // 3) Audit token → 200, camelCase report covering the real footprint.
    let req = Request::builder()
        .method("GET")
        .uri("/v1/storage")
        .header("Host", "localhost")
        .header("Authorization", "Bearer owner-secret")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = body::to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["schemaVersion"], 1);
    let used = v["usedBytes"].as_u64().expect("usedBytes");
    // Savings snapshot + audit log live under the data root; the workspace
    // state dir is nested inside the tempdir too, so everything is covered by
    // the server-data component — and the 8 KiB events file must be visible.
    assert!(used >= 8_192, "usedBytes {used} misses workspace state");
    assert!(v["components"].as_array().is_some_and(|c| !c.is_empty()));
    assert!(v["measuredAt"].is_string());
    // No storageQuotaBytes in team.json + no env override ⇒ the Team-tier
    // default applies (#282) and the quota is always concrete.
    assert_eq!(
        v["quotaBytes"].as_u64(),
        Some(super::super::team_billing::DEFAULT_TEAM_STORAGE_QUOTA_BYTES)
    );

    // 4) /v1/usage carries the savings roll-up + snake_case storage block.
    let req = Request::builder()
        .method("GET")
        .uri("/v1/usage")
        .header("Host", "localhost")
        .header("Authorization", "Bearer owner-secret")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = body::to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["schemaVersion"], 1);
    assert_eq!(v["savings"]["memberCount"], 1);
    assert_eq!(v["savings"]["netSavedTokens"], 4000);
    assert_eq!(v["toolCalls"], 7);
    let storage_used = v["storage"]["used_bytes"].as_u64().expect("used_bytes");
    assert_eq!(
        storage_used, used,
        "usage storage block must match /v1/storage"
    );
    assert_eq!(
        v["storage"]["quota_bytes"].as_u64(),
        Some(super::super::team_billing::DEFAULT_TEAM_STORAGE_QUOTA_BYTES),
        "usage storage block must carry the resolved quota"
    );
}

/// `/v1/connectors` (#281) is audit-gated like the other billing-plane reads,
/// and its roster is secret-free: the configured credential must never appear
/// in the response (the secret lives only in the private team.json).
#[tokio::test]
async fn connectors_roster_is_audit_gated_and_secret_free() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = cfg_savings(&tmp);
    let secret = "super-secret-token-value";
    cfg.connectors.push(connectors::ConnectorConfig {
        id: "gl-issues".into(),
        provider: "gitlab".into(),
        display_name: Some("GitLab Issues".into()),
        workspace_id: None,
        resource: "issues".into(),
        project: Some("group/proj".into()),
        host: None,
        state: Some("opened".into()),
        limit: None,
        interval_secs: 3_600,
        secret: Some(secret.into()),
        enabled: true,
    });
    let app = build_app(cfg).await;

    // No token → 401.
    let req = Request::builder()
        .method("GET")
        .uri("/v1/connectors")
        .header("Host", "localhost")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app.clone().oneshot(req).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );

    // Member token (no audit scope) → 403.
    let req = Request::builder()
        .method("GET")
        .uri("/v1/connectors")
        .header("Host", "localhost")
        .header("Authorization", "Bearer member-secret")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app.clone().oneshot(req).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );

    // Audit token → 200 with a secret-free roster.
    let req = Request::builder()
        .method("GET")
        .uri("/v1/connectors")
        .header("Host", "localhost")
        .header("Authorization", "Bearer owner-secret")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = body::to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
    let raw = String::from_utf8_lossy(&bytes);
    assert!(
        !raw.contains(secret),
        "connector secret leaked into /v1/connectors response"
    );
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["connector_count"], 1);
    assert_eq!(v["connectors"][0]["id"], "gl-issues");
    assert_eq!(v["connectors"][0]["provider"], "gitlab");
    assert_eq!(v["connectors"][0]["hasSecret"], true);
    // No sync has run yet, so the status is the default (no last status).
    assert!(v["connectors"][0]["status"]["lastStatus"].is_null());
}
