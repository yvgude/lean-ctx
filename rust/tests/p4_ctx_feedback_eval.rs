use serde_json::json;

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn ctx_feedback_record_report_reset() {
    let _g = lean_ctx::core::data_dir::test_env_lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");

    std::env::set_var("LEAN_CTX_DATA_DIR", &data_dir);
    let engine = lean_ctx::engine::ContextEngine::with_project_root(dir.path());

    let _ = engine
        .call_tool_text("ctx_feedback", Some(json!({"action":"reset"})))
        .await
        .expect("reset");

    let _ = engine
        .call_tool_text(
            "ctx_feedback",
            Some(json!({
                "action":"record",
                "agent_id":"test-agent",
                "model":"test-model",
                "intent":"test-intent",
                "llm_input_tokens":100,
                "llm_output_tokens":250,
                "latency_ms":123
            })),
        )
        .await
        .expect("record");

    let report = engine
        .call_tool_text("ctx_feedback", Some(json!({"action":"report","limit":50})))
        .await
        .expect("report");
    assert!(report.contains("total_events: 1"), "report: {report}");

    let json_out = engine
        .call_tool_text("ctx_feedback", Some(json!({"action":"json","limit":50})))
        .await
        .expect("json");
    let v: serde_json::Value = serde_json::from_str(&json_out).expect("parse json");
    assert_eq!(
        v["summary"]["total_events"].as_u64().unwrap_or(0),
        1,
        "json: {json_out}"
    );

    let _ = engine
        .call_tool_text("ctx_feedback", Some(json!({"action":"reset"})))
        .await
        .expect("reset2");

    let report2 = engine
        .call_tool_text("ctx_feedback", Some(json!({"action":"report","limit":50})))
        .await
        .expect("report2");
    assert!(report2.contains("No LLM feedback"), "report2: {report2}");

    std::env::remove_var("LEAN_CTX_DATA_DIR");
}
