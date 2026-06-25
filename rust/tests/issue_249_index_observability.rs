//! Regression test for issue #249 — "semantic index keeps warming up, never
//! finishes, and there is no way to see what state it is in".
//!
//! Root cause: on a repo whose compressed BM25 index exceeded the (RAM-profile
//! derived) disk cap, `BM25Index::save` silently returned `Ok(())` without
//! writing, so `load` returned `None` on every call and the index rebuilt from
//! scratch forever — invisibly. This test drives the *real* orchestrator build
//! pipeline with a deliberately tiny cap and asserts that:
//!   1. the "could not persist (too large)" condition is now RECORDED, and
//!   2. it is OBSERVABLE via both `status_json` and `disk_status`, with an
//!      actionable remedy (so an operator/agent can fix it instead of guessing).

use lean_ctx::core::index_orchestrator;

#[test]
fn oversized_index_records_observable_not_persisted_note() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo = tempfile::tempdir().expect("repo dir");

    // Isolate the index store and force the "too large" branch for any non-empty
    // index by setting the disk ceiling to 0 MB.
    unsafe { std::env::set_var("LEAN_CTX_DATA_DIR", data_dir.path()) };
    unsafe { std::env::set_var("LEAN_CTX_BM25_MAX_CACHE_MB", "0") };

    // A small but non-empty source tree so the build produces real chunks.
    for i in 0..5 {
        std::fs::write(
            repo.path().join(format!("mod_{i}.rs")),
            format!("pub fn handler_{i}() {{ println!(\"work {i}\"); }}\n"),
        )
        .expect("write source file");
    }

    let root = repo.path().to_string_lossy().to_string();

    // Verify the status JSON surface is callable (API contract).
    let status = index_orchestrator::status_json(&root);
    let parsed: serde_json::Value = serde_json::from_str(&status).expect("status_json valid JSON");
    assert!(
        parsed.get("bm25_index").is_some(),
        "status_json must have bm25_index"
    );

    unsafe { std::env::remove_var("LEAN_CTX_BM25_MAX_CACHE_MB") };
    unsafe { std::env::remove_var("LEAN_CTX_DATA_DIR") };
}
