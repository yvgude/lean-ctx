// SPDX-License-Identifier: Apache-2.0
//! End-to-end validation for the synthetic dual-arm comparison corpus.

use std::collections::BTreeMap;
use std::path::PathBuf;

use lean_ctx::core::ocla::reference_adapters::{
    EXPECTED_FIXTURE_COUNT, QualityCheck, generate_from_fixtures, load_fixtures,
};

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../_archive/benchmarks/capability-comparison/rtk-v1")
}

#[test]
fn all_capability_comparison_fixtures_are_valid_and_comparable() {
    let fixtures = load_fixtures(fixture_root()).expect("fixture corpus should load");
    assert_eq!(fixtures.len(), EXPECTED_FIXTURE_COUNT);

    let mut category_counts = BTreeMap::new();
    for fixture in &fixtures {
        assert!(!fixture.metadata.command.trim().is_empty());
        assert!(!fixture.metadata.working_dir.trim().is_empty());
        assert!(fixture.metadata.expected_tokens > 0);
        assert!(!fixture.input.trim().is_empty());
        assert!(fixture.input.lines().count() >= 2);
        assert!(fixture.input.chars().any(char::is_alphanumeric));
        assert!(!fixture.input.contains("[placeholder]"));
        *category_counts
            .entry(fixture.metadata.category.as_str())
            .or_insert(0usize) += 1;
    }

    assert_eq!(category_counts.get("git"), Some(&10));
    assert_eq!(category_counts.get("test"), Some(&10));
    assert_eq!(category_counts.get("structured"), Some(&10));

    let report = generate_from_fixtures(&fixtures);
    assert_eq!(report.fixture_count, EXPECTED_FIXTURE_COUNT);
    assert_eq!(report.workloads.len(), EXPECTED_FIXTURE_COUNT);
    assert!(!report.external_preferred_workloads.is_empty());
    assert!(!report.native_preferred_workloads.is_empty());
    assert!(report.workloads.iter().any(|workload| {
        matches!(
            workload.receipt.quality_check,
            QualityCheck::QualityFloorFailed
        )
    }));
    assert!(report.aggregate.native_tokens > 0);
    assert!(report.aggregate.external_tokens > 0);

    let json = report.to_json().expect("report JSON should serialize");
    let value: serde_json::Value = serde_json::from_str(&json).expect("report JSON is valid");
    assert_eq!(value["fixture_count"], EXPECTED_FIXTURE_COUNT);
    assert!(value["categories"]["git"].is_object());

    let text = report.to_text();
    assert!(text.contains("RTK wins"));
    assert!(text.contains("RTK-preferred workloads"));
    assert!(text.contains("Native-preferred workloads"));
    assert!(text.contains("Aggregate token accounting"));
}
