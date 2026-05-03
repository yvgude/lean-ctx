use lean_ctx::core::knowledge::ProjectKnowledge;
use lean_ctx::core::memory_policy::MemoryPolicy;

#[test]
fn recall_rehydrates_from_archive_when_active_set_empty() {
    let _g = lean_ctx::core::data_dir::test_env_lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("mkdir");
    std::env::set_var("LEAN_CTX_DATA_DIR", data_dir.to_string_lossy().to_string());

    let project_root = tmp.path().join("proj");
    std::fs::create_dir_all(&project_root).expect("mkdir proj");
    let project_root_str = project_root.to_string_lossy().to_string();

    // Create a fact that will be archived by lifecycle (low confidence).
    let policy = MemoryPolicy::default();
    let mut k = ProjectKnowledge::load_or_create(&project_root_str);
    k.remember("architecture", "db", "PostgreSQL", "s1", 0.1, &policy);
    let _ = k.run_memory_lifecycle(&policy);
    k.save().expect("save");

    // Now the active set should be empty (fact archived), so recall should rehydrate it.
    let out = lean_ctx::tools::ctx_knowledge::handle(
        &project_root_str,
        "recall",
        None,
        None,
        None,
        Some("db postgres"),
        "s1",
        None,
        None,
        None,
        None,
    );

    assert!(
        out.contains("architecture/db") || out.contains("PostgreSQL"),
        "expected rehydrated recall result, got: {out}"
    );

    std::env::remove_var("LEAN_CTX_DATA_DIR");
}
