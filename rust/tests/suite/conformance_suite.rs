//! CI gate for the conformance & reproducibility scorecard (EPIC 12.17).
//!
//! Fails the build if this instance violates any of its own contracts, the
//! discovery documents are non-deterministic, or a registered extension breaks
//! an invariant. See `core::conformance`.

use lean_ctx::core::conformance;

#[test]
fn conformance_scorecard_all_pass() {
    let card = conformance::run();
    assert!(
        card.all_passed(),
        "conformance scorecard has {} failure(s): {:#?}",
        card.failures().len(),
        card.failures()
    );
}

#[test]
fn scorecard_covers_all_categories() {
    let card = conformance::run();
    let categories: std::collections::BTreeSet<&str> =
        card.checks.iter().map(|c| c.category.as_str()).collect();
    for expected in ["contracts", "reproducibility", "extensions"] {
        assert!(
            categories.contains(expected),
            "scorecard missing category '{expected}'"
        );
    }
}
