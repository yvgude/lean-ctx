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
        .layer(middleware::from_fn_with_state(
            state.clone(),
            team_auth_middleware,
        ))
        .with_state(state)
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
