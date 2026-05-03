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

async fn call_tool_json(
    svc: &StreamableHttpService<lean_ctx::tools::LeanCtxServer, LocalSessionManager>,
    tool: &str,
    arguments: serde_json::Value,
) -> serde_json::Value {
    let req = Request::builder()
        .method("POST")
        .uri("/")
        .header("Host", "localhost")
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
        .body(Body::from(jsonrpc_call(tool, &arguments)))
        .expect("request");

    let resp = svc.handle(req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(Body::new(resp.into_body()), usize::MAX)
        .await
        .expect("read body");
    let v: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    assert!(
        v.get("error").is_none(),
        "expected jsonrpc result, got error: {v}"
    );
    v
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn memory_benchmark_suite_persists_core_artifacts() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let data_dir = tempfile::tempdir().expect("data dir");
    std::env::set_var("LEAN_CTX_DATA_DIR", data_dir.path());

    let dir = tempfile::tempdir().expect("project dir");
    // Ensure project-root detection can anchor on a marker.
    std::fs::create_dir_all(dir.path().join(".git")).expect("create .git marker");
    let file_path = dir.path().join("a.txt");
    std::fs::write(&file_path, "hello\n").expect("write file");

    let root = lean_ctx::core::pathutil::safe_canonicalize_or_self(dir.path());
    let root_str = root.to_string_lossy().to_string();
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

    // 1) Establish project context (tool pipeline).
    let _ = call_tool_json(
        &svc,
        "ctx_read",
        json!({
            "path": file_path.to_string_lossy().to_string(),
            "mode": "full"
        }),
    )
    .await;

    // 2) Knowledge: remember + feedback + relations.
    let _ = call_tool_json(
        &svc,
        "ctx_knowledge",
        json!({
            "action": "remember",
            "category": "arch",
            "key": "db",
            "value": "MySQL",
            "confidence": 0.9
        }),
    )
    .await;
    let _ = call_tool_json(
        &svc,
        "ctx_knowledge",
        json!({
            "action": "remember",
            "category": "arch",
            "key": "cache",
            "value": "Redis",
            "confidence": 0.9
        }),
    )
    .await;
    let _ = call_tool_json(
        &svc,
        "ctx_knowledge",
        json!({
            "action": "feedback",
            "category": "arch",
            "key": "db",
            "value": "up"
        }),
    )
    .await;
    let _ = call_tool_json(
        &svc,
        "ctx_knowledge",
        json!({
            "action": "relate",
            "category": "arch",
            "key": "db",
            "value": "depends_on",
            "query": "arch/cache"
        }),
    )
    .await;

    // 3) Episodic + procedural memory persistence.
    let _ = call_tool_json(
        &svc,
        "ctx_session",
        json!({
            "action": "episodes",
            "value": "record"
        }),
    )
    .await;
    let _ = call_tool_json(
        &svc,
        "ctx_session",
        json!({
            "action": "procedures",
            "value": "detect"
        }),
    )
    .await;

    // 4) Assert artifacts exist on disk.
    let knowledge = lean_ctx::core::knowledge::ProjectKnowledge::load_or_create(&root_str);
    let knowledge_dir = data_dir
        .path()
        .join("knowledge")
        .join(&knowledge.project_hash);

    assert!(
        knowledge_dir.join("knowledge.json").exists(),
        "expected knowledge.json to exist"
    );
    assert!(
        knowledge_dir.join("relations.json").exists(),
        "expected relations.json to exist"
    );

    let graph =
        lean_ctx::core::knowledge_relations::KnowledgeRelationGraph::load(&knowledge.project_hash)
            .expect("load relations graph");
    assert!(
        graph.edges.iter().any(|e| {
            e.from.category == "arch"
                && e.from.key == "db"
                && e.to.category == "arch"
                && e.to.key == "cache"
        }),
        "expected at least one relation edge"
    );

    let episodes_dir = data_dir.path().join("memory").join("episodes");
    let mut entries = std::fs::read_dir(&episodes_dir).expect("episodes dir exists");
    assert!(
        entries.next().is_some(),
        "expected at least one episodic store file"
    );

    let procs_dir = data_dir.path().join("memory").join("procedures");
    let mut entries = std::fs::read_dir(&procs_dir).expect("procedures dir exists");
    assert!(
        entries.next().is_some(),
        "expected at least one procedural store file"
    );

    std::env::remove_var("LEAN_CTX_DATA_DIR");
}
