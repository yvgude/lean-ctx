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

/// Poll `cond` up to ~10s. The memory stores are persisted on a background
/// thread (the consolidation pipeline), so asserting immediately after a tool
/// call returns raced the writer and made this test flaky under full-suite load
/// (#215). Polling keeps the assertion strict (it still fails if the artifact
/// never lands) while tolerating scheduling latency.
async fn wait_until(mut cond: impl FnMut() -> bool) -> bool {
    for _ in 0..200 {
        if cond() {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    cond()
}

/// True once `dir` exists and contains at least one entry. Tolerates the dir not
/// existing yet (the background writer may not have created it).
fn dir_has_entry(dir: &std::path::Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|mut it| it.next().is_some())
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn memory_benchmark_suite_persists_core_artifacts() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let data_dir = tempfile::tempdir().expect("data dir");
    unsafe { std::env::set_var("LEAN_CTX_DATA_DIR", data_dir.path()) };

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
    let relate_resp = call_tool_json(
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

    // 4) Assert artifacts exist on disk. Persistence runs on a background thread,
    //    so poll with a bounded timeout rather than asserting immediately (#215).
    let knowledge = lean_ctx::core::knowledge::ProjectKnowledge::load_or_create(&root_str);
    let knowledge_dir = data_dir
        .path()
        .join("knowledge")
        .join(&knowledge.project_hash);

    assert!(
        wait_until(|| knowledge_dir.join("knowledge.json").exists()).await,
        "expected knowledge.json to exist"
    );
    assert!(
        wait_until(|| knowledge_dir.join("relations.json").exists()).await,
        "expected relations.json to exist (relate said: {relate_resp})"
    );

    let edge_present = wait_until(|| {
        lean_ctx::core::knowledge_relations::KnowledgeRelationGraph::load(&knowledge.project_hash)
            .is_some_and(|graph| {
                graph.edges.iter().any(|e| {
                    e.from.category == "arch"
                        && e.from.key == "db"
                        && e.to.category == "arch"
                        && e.to.key == "cache"
                })
            })
    })
    .await;
    assert!(edge_present, "expected at least one relation edge");

    let episodes_dir = data_dir.path().join("memory").join("episodes");
    assert!(
        wait_until(|| dir_has_entry(&episodes_dir)).await,
        "expected at least one episodic store file"
    );

    let procs_dir = data_dir.path().join("memory").join("procedures");
    assert!(
        wait_until(|| dir_has_entry(&procs_dir)).await,
        "expected at least one procedural store file"
    );

    unsafe { std::env::remove_var("LEAN_CTX_DATA_DIR") };
}
