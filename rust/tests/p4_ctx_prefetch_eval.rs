use serde_json::json;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn ctx_prefetch_does_not_hide_first_full_delivery() {
    let _g = lean_ctx::core::data_dir::test_env_lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    // SAFETY: serialized by `test_env_lock()`.
    unsafe { std::env::set_var("LEAN_CTX_DATA_DIR", &data_dir) };

    let project = tempfile::tempdir().expect("project");
    let file_a = project.path().join("a.rs");
    let file_b = project.path().join("b.rs");
    std::fs::write(&file_a, "mod b;\npub fn a() { b::b(); }\n").expect("write a");
    std::fs::write(&file_b, "pub fn b() {}\n").expect("write b");

    let engine = lean_ctx::engine::ContextEngine::with_project_root(project.path());

    let out = engine
        .call_tool_text(
            "ctx_prefetch",
            Some(json!({
                "root": project.path().to_string_lossy().to_string(),
                "changed_files": [file_a.to_string_lossy().to_string()],
                "budget_tokens": 500,
                "max_files": 2
            })),
        )
        .await
        .expect("prefetch");
    assert!(out.contains("prefetched"), "prefetch out: {out}");

    let full = engine
        .call_tool_text(
            "ctx_read",
            Some(json!({"path": file_a.to_string_lossy().to_string(), "mode":"full"})),
        )
        .await
        .expect("read full");
    // Warming is not delivery: the prefetch response listed files but did not
    // send their bodies. The first explicit read must still supply the source.
    assert!(
        full.contains("pub fn a() { b::b(); }"),
        "first delivery: {full}"
    );
    std::fs::write(&file_a, "pub fn changed_after_prefetch() {}\n").expect("change a");
    let changed = engine
        .call_tool_text(
            "ctx_read",
            Some(json!({"path": file_a.to_string_lossy().to_string(), "mode":"full"})),
        )
        .await
        .expect("read changed file");
    assert!(
        changed.contains("changed_after_prefetch"),
        "must not serve stale prefetch: {changed}"
    );

    // SAFETY: serialized by `test_env_lock()`.
    unsafe { std::env::remove_var("LEAN_CTX_DATA_DIR") };
}
