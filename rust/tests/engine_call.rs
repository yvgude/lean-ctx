use serde_json::json;

#[tokio::test]
async fn context_engine_call_tool_text_reads_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file_path = dir.path().join("a.txt");
    std::fs::write(&file_path, "hello-engine\n").expect("write file");

    let engine = lean_ctx::engine::ContextEngine::with_project_root(dir.path());
    let out = engine
        .call_tool_text(
            "ctx_read",
            Some(json!({
                "path": file_path.to_string_lossy().to_string(),
                "mode": "full"
            })),
        )
        .await
        .expect("call tool");

    assert!(out.contains("hello-engine"));
}
