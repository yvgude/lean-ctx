use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};
use serde_json::json;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streamable_http_stateless_json_tool_call_works() {
    let dir = tempfile::tempdir().expect("tempdir");
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
        Arc::new(
            rmcp::transport::streamable_http_server::session::local::LocalSessionManager::default(),
        ),
        cfg,
    );

    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "ctx_read",
            "arguments": {
                "path": file_path.to_string_lossy().to_string(),
                "mode": "full"
            }
        }
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

    let resp = svc.handle(req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(Body::new(resp.into_body()), usize::MAX)
        .await
        .expect("read body");
    let v: serde_json::Value = serde_json::from_slice(&bytes).expect("json");

    let text = v["result"]["content"][0]["text"].as_str().unwrap_or("");
    assert!(text.contains("hello"));
}
