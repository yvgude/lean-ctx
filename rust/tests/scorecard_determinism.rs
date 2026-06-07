//! Reproducibility contract for the scorecard (#211).
//!
//! The quality metrics (compression savings + recall/MRR) must be identical
//! across runs. Latency is wall-clock and intentionally excluded from the
//! determinism digest.

use lean_ctx::core::scorecard::run_scorecard;

#[test]
fn scorecard_quality_metrics_are_reproducible() {
    let a = run_scorecard().expect("scorecard run a");
    let b = run_scorecard().expect("scorecard run b");
    assert_eq!(
        a.determinism_digest, b.determinism_digest,
        "scorecard quality metrics drifted between identical runs"
    );
    assert!(
        !a.determinism_digest.is_empty(),
        "digest must be populated and serialized into the artifact"
    );
}

#[test]
fn scorecard_has_all_scenarios_and_sane_metrics() {
    let sc = run_scorecard().expect("scorecard run");
    let names: Vec<&str> = sc.scenarios.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["small", "medium", "large"]);

    for s in &sc.scenarios {
        assert!(s.files > 0, "{} has no files", s.name);
        assert!(s.queries > 0, "{} has no queries", s.name);
        assert!(s.raw_tokens > 0, "{} measured no tokens", s.name);
        // Unique markers should make the right file easily retrievable.
        assert!(
            s.recall_at_10 >= 0.5,
            "{} recall@10 unexpectedly low: {}",
            s.name,
            s.recall_at_10
        );
        // Bounds sanity.
        assert!((0.0..=1.0).contains(&s.recall_at_5));
        assert!((0.0..=1.0).contains(&s.mrr));
        assert!(s.best_savings_pct >= 0.0);
    }

    assert!(sc.aggregate.avg_recall_at_10 >= 0.5);
}
