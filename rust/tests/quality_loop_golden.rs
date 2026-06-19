//! Golden test for the quality loop v1 (GL #494): an edit failure after a
//! compressed read must escalate the next auto read of that file to `full`,
//! and repeated failures must make the (ext × mode) pair risky so *other*
//! files of the same type stop being compressed with the failing mode.
//!
//! Single test in its own binary: it sets `LEAN_CTX_DATA_DIR` process-wide
//! before the edit-quality store's `OnceLock` is first touched.

use lean_ctx::core::auto_mode_resolver::{AutoModeContext, resolve};
use lean_ctx::tools::ctx_edit::{EditParams, record_outcome, run_io};

fn params_for(path: &str, old_string: &str) -> EditParams {
    EditParams {
        path: path.to_string(),
        old_string: old_string.to_string(),
        new_string: "fn replaced() {}".to_string(),
        replace_all: false,
        create: false,
        expected_md5: None,
        expected_size: None,
        expected_mtime_ms: None,
        backup: false,
        backup_path: None,
        evidence: false,
        diff_max_lines: 50,
        allow_lossy_utf8: false,
    }
}

#[test]
fn edit_fail_after_map_read_escalates_and_penalizes() {
    let tmp = tempfile::tempdir().unwrap();
    // TODO: Audit that the environment access only happens in single-threaded code.
    unsafe { std::env::set_var("LEAN_CTX_DATA_DIR", tmp.path()) };

    let file = tmp.path().join("golden.rs");
    std::fs::write(&file, "fn real_function() { 1 }\n".repeat(60)).unwrap();
    let path = file.to_string_lossy().to_string();

    // The agent quotes an old_string from a `map` rendering — the body was
    // never in context, so the string is not on disk and the edit misses.
    let params = params_for(&path, "fn imagined_from_map_view()");
    let (text, effect) = run_io(&params, "map");
    assert!(
        text.contains("old_string not found"),
        "expected a not-found failure, got: {text}"
    );
    record_outcome(&params, "map", &text, &effect);

    // Golden: the NEXT auto read of the same file resolves to full…
    let ctx = AutoModeContext {
        path: &path,
        token_count: 3000,
        task: None,
        cache: None,
    };
    let first = resolve(&ctx);
    assert_eq!(first.mode, "full");
    assert_eq!(first.source, "edit_fail_escalation");

    // …and the escalation is one-shot.
    let second = resolve(&ctx);
    assert_ne!(second.source, "edit_fail_escalation");

    // A second failure makes rs|map risky (2 fails, rate 1.0 >= 0.25):
    // a DIFFERENT .rs file that would normally resolve to map now gets full.
    let (text2, effect2) = run_io(&params, "map");
    record_outcome(&params, "map", &text2, &effect2);

    let other = tmp.path().join("other.rs");
    std::fs::write(&other, "fn unrelated() { 2 }\n".repeat(60)).unwrap();
    let other_path = other.to_string_lossy().to_string();
    let other_ctx = AutoModeContext {
        path: &other_path,
        // > 6000 tokens so the deterministic size heuristic resolves a code file
        // to `map` (#683 raised the map floor from 3000 → 6000). Only a non-full
        // base mode can be escalated to `full` by the risky (rs × map) penalty.
        token_count: 7000,
        task: None,
        cache: None,
    };
    let penalized = resolve(&other_ctx);
    assert_eq!(penalized.mode, "full");
    assert_eq!(penalized.source, "edit_quality_penalty");

    // Successful edits on the failing pair recover it (hysteresis: rate
    // must drop below 0.15 — 2 fails need 12+ successes).
    for _ in 0..12 {
        std::fs::write(&file, "fn real_function() { 1 }\n").unwrap();
        let p = params_for(&path, "fn real_function() { 1 }");
        let (t, e) = run_io(&p, "map");
        assert!(!t.starts_with("ERROR"), "expected success, got: {t}");
        record_outcome(&p, "map", &t, &e);
    }
    let recovered = resolve(&other_ctx);
    assert_ne!(
        recovered.source, "edit_quality_penalty",
        "12 successes must clear the risky flag (2/14 < 0.15)"
    );
}
