use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};
use serde_json::json;

type LocalSessionManager =
    rmcp::transport::streamable_http_server::session::local::LocalSessionManager;

fn jsonrpc_call(tool: &str, arguments: &serde_json::Value) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": tool,
            "arguments": arguments
        }
    })
    .to_string()
}

async fn call_tool(
    svc: &StreamableHttpService<lean_ctx::tools::LeanCtxServer, LocalSessionManager>,
    body: String,
) {
    let req = Request::builder()
        .method("POST")
        .uri("/")
        .header("Host", "localhost")
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
        .body(Body::from(body))
        .expect("request");

    let resp = svc.handle(req).await;
    assert_eq!(resp.status(), StatusCode::OK);
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn anomaly_detector_is_persisted_from_tool_call_pipeline() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let data_dir = tempfile::tempdir().expect("data dir");
    std::env::set_var("LEAN_CTX_DATA_DIR", data_dir.path());

    let dir = tempfile::tempdir().expect("project dir");
    let file_path = dir.path().join("a.txt");
    std::fs::write(&file_path, "hello\n").expect("write file");

    let root_str = dir.path().to_string_lossy().to_string();
    let base = lean_ctx::tools::LeanCtxServer::new_with_project_root(Some(&root_str));

    let service_factory = move || Ok(base.clone());
    let cfg = StreamableHttpServerConfig::default()
        .with_stateful_mode(false)
        .with_json_response(true);
    let svc = StreamableHttpService::new(
        service_factory,
        Arc::new(LocalSessionManager::default()),
        cfg,
    );

    call_tool(
        &svc,
        jsonrpc_call(
            "ctx_read",
            &json!({
                "path": file_path.to_string_lossy().to_string(),
                "mode": "full"
            }),
        ),
    )
    .await;

    assert!(
        data_dir.path().join("anomaly_detector.json").exists(),
        "expected anomaly_detector.json to be persisted"
    );

    std::env::remove_var("LEAN_CTX_DATA_DIR");
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn episodic_and_procedural_memory_persist_via_ctx_session_actions() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let data_dir = tempfile::tempdir().expect("data dir");
    std::env::set_var("LEAN_CTX_DATA_DIR", data_dir.path());

    let dir = tempfile::tempdir().expect("project dir");
    let file_path = dir.path().join("a.txt");
    std::fs::write(&file_path, "hello\n").expect("write file");

    let root_str = dir.path().to_string_lossy().to_string();
    let base = lean_ctx::tools::LeanCtxServer::new_with_project_root(Some(&root_str));

    let service_factory = move || Ok(base.clone());
    let cfg = StreamableHttpServerConfig::default()
        .with_stateful_mode(false)
        .with_json_response(true);
    let svc = StreamableHttpService::new(
        service_factory,
        Arc::new(LocalSessionManager::default()),
        cfg,
    );

    call_tool(
        &svc,
        jsonrpc_call(
            "ctx_read",
            &json!({
                "path": file_path.to_string_lossy().to_string(),
                "mode": "full"
            }),
        ),
    )
    .await;

    call_tool(
        &svc,
        jsonrpc_call(
            "ctx_session",
            &json!({
                "action": "episodes",
                "value": "record"
            }),
        ),
    )
    .await;

    let episodes_dir = data_dir.path().join("memory").join("episodes");
    let mut entries = std::fs::read_dir(&episodes_dir).expect("episodes dir exists");
    assert!(
        entries.next().is_some(),
        "expected at least one episodic store file"
    );

    call_tool(
        &svc,
        jsonrpc_call(
            "ctx_session",
            &json!({
                "action": "procedures",
                "value": "detect"
            }),
        ),
    )
    .await;

    let procs_dir = data_dir.path().join("memory").join("procedures");
    let mut entries = std::fs::read_dir(&procs_dir).expect("procedures dir exists");
    assert!(
        entries.next().is_some(),
        "expected at least one procedural store file"
    );

    std::env::remove_var("LEAN_CTX_DATA_DIR");
}
