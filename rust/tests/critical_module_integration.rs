// Integration tests for modules identified as critical during the architecture audit.
// Covers: pathjail, degradation_policy, gotcha_tracker/learn, cache CCR.

// ---------------------------------------------------------------------------
// pathjail: security-critical path containment
// ---------------------------------------------------------------------------
#[cfg(not(feature = "no-jail"))]
#[test]
fn pathjail_blocks_traversal() {
    use lean_ctx::core::pathjail;

    let jail = std::env::current_dir().unwrap();
    let escaped = jail.join("../../etc/passwd");

    let result = pathjail::jail_path(&escaped, &jail);
    assert!(
        result.is_err(),
        "traversal path must be rejected by jail_path"
    );
}

#[test]
fn pathjail_allows_safe_path() {
    use lean_ctx::core::pathjail;

    let jail = std::env::current_dir().unwrap();
    let safe = jail.join("src/main.rs");

    let result = pathjail::jail_path(&safe, &jail);
    assert!(result.is_ok(), "path inside jail must be allowed");
}

// ---------------------------------------------------------------------------
// degradation_policy: must evaluate without panic
// ---------------------------------------------------------------------------
#[test]
fn degradation_policy_evaluates_for_known_tool() {
    use lean_ctx::core::degradation_policy::evaluate_v1_for_tool;

    let policy = evaluate_v1_for_tool("ctx_read", Some("2025-01-01T00:00:00Z"));
    assert_eq!(policy.schema_version, 1);
    assert_eq!(policy.tool, "ctx_read");
    assert!(!policy.decision.reason.is_empty());
}

#[test]
fn degradation_policy_evaluates_for_unknown_tool() {
    let policy = lean_ctx::core::degradation_policy::evaluate_v1_for_tool(
        "nonexistent_tool",
        Some("2025-01-01T00:00:00Z"),
    );
    assert_eq!(policy.tool, "nonexistent_tool");
}

// ---------------------------------------------------------------------------
// gotcha_tracker/learn: extract learnings from resolved gotchas
// ---------------------------------------------------------------------------
#[test]
fn learn_extracts_high_confidence_gotchas() {
    use lean_ctx::core::gotcha_tracker::learn::extract_learnings;
    use lean_ctx::core::gotcha_tracker::{
        Gotcha, GotchaCategory, GotchaSeverity, GotchaSource, GotchaStats, GotchaStore,
    };

    let mut g = Gotcha::new(
        GotchaCategory::Build,
        GotchaSeverity::Warning,
        "cargo build fails with missing feature",
        "Add --all-features flag",
        GotchaSource::AutoDetected {
            command: "cargo build".into(),
            exit_code: 1,
        },
        "sess-1",
    );
    g.confidence = 0.8;
    g.occurrences = 3;
    g.session_ids = vec!["a".into(), "b".into(), "c".into()];

    let store = GotchaStore {
        project_hash: "test".into(),
        gotchas: vec![g],
        error_log: vec![],
        stats: GotchaStats::default(),
        updated_at: chrono::Utc::now(),
        pending_errors: vec![],
    };

    let learnings = extract_learnings(&store);
    assert_eq!(learnings.len(), 1);
    assert!(learnings[0].resolution.contains("--all-features"));
}

#[test]
fn learn_filters_low_confidence() {
    use lean_ctx::core::gotcha_tracker::learn::extract_learnings;
    use lean_ctx::core::gotcha_tracker::*;

    let mut g = Gotcha::new(
        GotchaCategory::Build,
        GotchaSeverity::Info,
        "some flaky thing",
        "retry",
        GotchaSource::AutoDetected {
            command: "make".into(),
            exit_code: 1,
        },
        "sess-1",
    );
    g.confidence = 0.3;
    g.occurrences = 1;

    let store = GotchaStore {
        project_hash: "test".into(),
        gotchas: vec![g],
        error_log: vec![],
        stats: GotchaStats::default(),
        updated_at: chrono::Utc::now(),
        pending_errors: vec![],
    };

    let learnings = extract_learnings(&store);
    assert!(
        learnings.is_empty(),
        "low confidence gotchas should be filtered"
    );
}

#[test]
fn learn_format_agents_section_has_markers() {
    use lean_ctx::core::gotcha_tracker::learn::{Learning, format_agents_section};

    let learnings = vec![Learning {
        category: "build".into(),
        trigger: "missing dep".into(),
        resolution: "add to Cargo.toml".into(),
        confidence: 0.9,
        occurrences: 5,
        sessions: 3,
    }];

    let section = format_agents_section(&learnings);
    assert!(section.contains("lean-ctx-learn-start"));
    assert!(section.contains("lean-ctx-learn-end"));
    assert!(section.contains("missing dep"));
}

#[test]
fn learn_format_empty_returns_empty() {
    use lean_ctx::core::gotcha_tracker::learn::format_agents_section;

    let section = format_agents_section(&[]);
    assert!(section.is_empty());
}

// ---------------------------------------------------------------------------
// cache CCR: get_full_content
// ---------------------------------------------------------------------------
#[test]
fn cache_get_full_content_returns_stored_content() {
    use lean_ctx::core::cache::SessionCache;

    let mut cache = SessionCache::new();
    cache.store("/test/file.rs", "fn main() {}");
    let content = cache.get_full_content("/test/file.rs");
    assert!(content.is_some());
    assert!(content.unwrap().contains("fn main"));
}

#[test]
fn cache_get_full_content_returns_none_for_missing() {
    use lean_ctx::core::cache::SessionCache;

    let cache = SessionCache::new();
    let missing = cache.get_full_content("/nonexistent.rs");
    assert!(missing.is_none());
}
