use serde_json::json;

fn extract_path(s: &str) -> String {
    for line in s.lines() {
        if let Some(rest) = line.strip_prefix(" path: ") {
            return rest.trim().to_string();
        }
    }
    panic!("no path in output: {s}");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn ctx_handoff_create_show_list_pull_clear() {
    let _g = lean_ctx::core::data_dir::test_env_lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    std::env::set_var("LEAN_CTX_DATA_DIR", &data_dir);

    let project = tempfile::tempdir().expect("project");
    let file = project.path().join("a.rs");
    std::fs::write(&file, "pub fn a() {}\n").expect("write file");

    let engine = lean_ctx::engine::ContextEngine::with_project_root(project.path());

    // Establish project_root in session + create some context.
    let _ = engine
        .call_tool_text(
            "ctx_read",
            Some(json!({"path": file.to_string_lossy().to_string(), "mode":"signatures"})),
        )
        .await
        .expect("ctx_read");

    let _ = engine
        .call_tool_text(
            "ctx_knowledge",
            Some(json!({"action":"remember","category":"handoff","key":"k1","value":"v1","confidence":0.9})),
        )
        .await
        .expect("remember");

    let _ = engine
        .call_tool_text(
            "ctx_workflow",
            Some(json!({"action":"start","name":"plan_code_test"})),
        )
        .await
        .expect("workflow start");

    let created = engine
        .call_tool_text(
            "ctx_handoff",
            Some(json!({"action":"create","paths":[file.to_string_lossy().to_string()]})),
        )
        .await
        .expect("handoff create");
    let ledger_path = extract_path(&created);

    let listed = engine
        .call_tool_text("ctx_handoff", Some(json!({"action":"list"})))
        .await
        .expect("handoff list");
    assert!(listed.contains(&ledger_path), "list: {listed}");

    let shown = engine
        .call_tool_text(
            "ctx_handoff",
            Some(json!({"action":"show","path":ledger_path})),
        )
        .await
        .expect("handoff show");
    assert!(shown.contains("\"manifest_md5\""), "show: {shown}");

    // Pull into a new engine instance (fresh in-memory state).
    let engine2 = lean_ctx::engine::ContextEngine::with_project_root(project.path());
    let pulled = engine2
        .call_tool_text(
            "ctx_handoff",
            Some(json!({"action":"pull","path": extract_path(&created)})),
        )
        .await
        .expect("handoff pull");
    assert!(pulled.contains("imported_knowledge: 1"), "pull: {pulled}");

    let cleared = engine
        .call_tool_text("ctx_handoff", Some(json!({"action":"clear"})))
        .await
        .expect("handoff clear");
    assert!(cleared.contains("removed:"), "clear: {cleared}");

    std::env::remove_var("LEAN_CTX_DATA_DIR");
}
