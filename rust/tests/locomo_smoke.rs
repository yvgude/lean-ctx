//! Smoke + gate test for the `LoCoMo` memory benchmark (#291).
//!
//! Runs the bundled reference suite through real lean-ctx memory recall in an
//! isolated data dir and asserts the harness produces sane, deterministic numbers.

use lean_ctx::core::locomo::{self, dataset};

#[test]
fn reference_suite_recall_and_savings() {
    let _g = lean_ctx::core::data_dir::test_env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    // TODO: Audit that the environment access only happens in single-threaded code.
    unsafe { std::env::set_var("LEAN_CTX_DATA_DIR", &data_dir) };
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();

    let samples = dataset::reference_samples();
    assert!(!samples.is_empty(), "reference suite must have samples");

    let report = locomo::run("reference-suite", &samples, &ws, 5);

    // Every question is answerable from the conversation, so a healthy retrieval
    // layer should surface the answer-bearing memory for (almost) all of them.
    assert!(
        report.overall.containment_rate >= 0.9,
        "containment too low: {}",
        report.overall.containment_rate
    );
    // Recalling top-k memories must cost fewer tokens than dumping the transcript.
    assert!(
        report.token_reduction_pct > 0.0,
        "expected positive token reduction, got {}",
        report.token_reduction_pct
    );
    let expected_questions: usize = samples.iter().map(|s| s.qa.len()).sum();
    assert_eq!(report.questions, expected_questions);
    assert!(!report.by_category.is_empty());

    // TODO: Audit that the environment access only happens in single-threaded code.
    unsafe { std::env::remove_var("LEAN_CTX_DATA_DIR") };
}

#[test]
fn determinism_same_numbers_twice() {
    let _g = lean_ctx::core::data_dir::test_env_lock();
    let tmp = tempfile::tempdir().unwrap();
    // TODO: Audit that the environment access only happens in single-threaded code.
    unsafe { std::env::set_var("LEAN_CTX_DATA_DIR", tmp.path().join("data")) };
    let samples = dataset::reference_samples();

    let ws1 = tmp.path().join("ws1");
    let ws2 = tmp.path().join("ws2");
    std::fs::create_dir_all(&ws1).unwrap();
    std::fs::create_dir_all(&ws2).unwrap();

    let a = locomo::run("s", &samples, &ws1, 5);
    let b = locomo::run("s", &samples, &ws2, 5);
    assert_eq!(a.overall.containment_rate, b.overall.containment_rate);
    assert_eq!(a.overall.mean_f1, b.overall.mean_f1);

    // TODO: Audit that the environment access only happens in single-threaded code.
    unsafe { std::env::remove_var("LEAN_CTX_DATA_DIR") };
}
