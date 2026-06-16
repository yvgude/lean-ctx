//! Regression test for issue #249 — "semantic index keeps warming up, never
//! finishes, and there is no way to see what state it is in".
//!
//! Root cause: on a repo whose compressed BM25 index exceeded the (RAM-profile
//! derived) disk cap, `BM25Index::save` silently returned `Ok(())` without
//! writing, so `load` returned `None` on every call and the index rebuilt from
//! scratch forever — invisibly. This test drives the *real* orchestrator build
//! pipeline with a deliberately tiny cap and asserts that:
//!   1. the "could not persist (too large)" condition is now RECORDED, and
//!   2. it is OBSERVABLE via both `status_json` and `bm25_summary`, with an
//!      actionable remedy (so an operator/agent can fix it instead of guessing).

use std::time::{Duration, Instant};

use lean_ctx::core::index_orchestrator;

/// Poll the orchestrator until the BM25 component leaves the building/idle
/// state (Ready or Failed) or we time out.
fn wait_until_built(root: &str, timeout: Duration) -> index_orchestrator::Bm25Summary {
    let deadline = Instant::now() + timeout;
    loop {
        let summary = index_orchestrator::bm25_summary(root);
        if summary.state == "ready" || summary.state == "failed" {
            return summary;
        }
        if Instant::now() >= deadline {
            return summary;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

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

    index_orchestrator::ensure_all_background(&root);
    let summary = wait_until_built(&root, Duration::from_secs(30));

    // The build itself succeeds (index is usable in memory) ...
    assert_eq!(
        summary.state, "ready",
        "build should succeed in memory even when too large to persist; got {summary:?}"
    );

    // ... but the "not persisted" condition must be RECORDED (no silent success).
    let note = summary.note.clone().unwrap_or_default();
    assert!(
        note.contains("NOT persisted"),
        "too-large build must record a non-persistence note, got: {note:?}"
    );
    assert!(
        note.contains("LEAN_CTX_BM25_MAX_CACHE_MB") && note.contains("reindex"),
        "note must carry an actionable remedy, got: {note:?}"
    );

    // ... and it must be OBSERVABLE through the machine-readable status surface
    // that `ctx_index status` returns.
    let status = index_orchestrator::status_json(&root);
    let parsed: serde_json::Value = serde_json::from_str(&status).expect("status_json valid JSON");
    let bm25_note = parsed["bm25_index"]["note"].as_str().unwrap_or("");
    assert!(
        bm25_note.contains("NOT persisted"),
        "status_json must expose the non-persistence note, got: {status}"
    );

    unsafe { std::env::remove_var("LEAN_CTX_BM25_MAX_CACHE_MB") };
    unsafe { std::env::remove_var("LEAN_CTX_DATA_DIR") };
}
