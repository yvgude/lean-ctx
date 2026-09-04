use std::time::{Duration, Instant};

use lean_ctx::core::graph_provider;

#[test]
fn non_project_root_does_not_consume_lazy_build_trigger() {
    // This is a standalone integration-test binary, so the process-global
    // GRAPH_BUILD_TRIGGERED latch starts fresh for this test.
    let isolated = tempfile::tempdir().expect("isolated state tempdir");
    let data = isolated.path().join("data");
    let config = isolated.path().join("config");
    let state = isolated.path().join("state");
    let cache = isolated.path().join("cache");

    for dir in [&data, &config, &state, &cache] {
        std::fs::create_dir_all(dir).expect("create isolated state dir");
    }

    let _env_lock = lean_ctx::core::data_dir::test_env_lock();

    // SAFETY: this integration-test binary contains only this test, and the
    // shared test_env_lock serializes LeanCTX tests that mutate these variables.
    unsafe {
        std::env::set_var("LEAN_CTX_DATA_DIR", &data);
        std::env::set_var("LEAN_CTX_CONFIG_DIR", &config);
        std::env::set_var("LEAN_CTX_STATE_DIR", &state);
        std::env::set_var("LEAN_CTX_CACHE_DIR", &cache);
    }

    // First call: definitely not a project. It must not consume the process-wide
    // opportunity to start a later legitimate lazy graph build.
    let non_project = tempfile::tempdir().expect("non-project tempdir");
    let non_project_root = non_project.path().to_string_lossy().to_string();

    assert!(
        graph_provider::open_best_effort(&non_project_root).is_none(),
        "empty non-project fixture must not already have a graph"
    );

    // Second call: a real, cold project in the SAME LeanCTX process.
    let project = tempfile::tempdir().expect("project tempdir");
    std::fs::create_dir_all(project.path().join(".git")).expect("create .git marker");
    std::fs::create_dir_all(project.path().join("src")).expect("create src dir");
    std::fs::write(
        project.path().join("Cargo.toml"),
        "[package]\nname = \"lazy_build_latch_repro\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .expect("write Cargo.toml");
    std::fs::write(
        project.path().join("src").join("lib.rs"),
        "pub fn answer() -> usize { 42 }\n",
    )
    .expect("write source fixture");

    let project_root = project.path().to_string_lossy().to_string();

    assert!(
        graph_provider::open_best_effort(&project_root).is_none(),
        "fresh project fixture must start with a cold graph"
    );

    // Behavior assertion, not a latency benchmark:
    // a valid project must eventually become available after best-effort access.
    // The deadline only prevents a broken implementation from hanging the test.
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if graph_provider::open_best_effort(&project_root).is_some() {
            break;
        }

        assert!(
            Instant::now() < deadline,
            "lazy graph build never started/completed for the valid project after a prior non-project root"
        );

        std::thread::sleep(Duration::from_millis(25));
    }
}
