use lean_ctx::core::intent_protocol;

#[test]
fn ctx_intent_knowledge_fact_routes_to_project_knowledge() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::env::set_var(
        "LEAN_CTX_DATA_DIR",
        tmp.path().to_string_lossy().to_string(),
    );

    let project_root = tmp.path().join("proj");
    std::fs::create_dir_all(&project_root).expect("mkdir");
    let project_root_str = project_root.to_string_lossy().to_string();

    let query = r#"{"intent_type":"knowledge_fact","category":"decision","key":"k1","value":"v1","confidence":0.9}"#;
    let intent = intent_protocol::intent_from_query(query, Some(&project_root_str));
    intent_protocol::apply_side_effects(&intent, Some(&project_root_str), "s1")
        .expect("apply_side_effects");

    let knowledge = lean_ctx::core::knowledge::ProjectKnowledge::load(&project_root_str)
        .expect("knowledge should exist");
    assert!(knowledge
        .facts
        .iter()
        .any(|f| f.is_current() && f.category == "decision" && f.key == "k1" && f.value == "v1"));

    std::env::remove_var("LEAN_CTX_DATA_DIR");
}
