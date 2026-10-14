use super::reinjection::downgrade_mode;
use super::*;
use crate::core::context_field::ContextState;

#[test]
fn new_ledger_is_empty() {
    let ledger = ContextLedger::new();
    assert_eq!(ledger.total_tokens_sent, 0);
    assert_eq!(ledger.entries.len(), 0);
    assert_eq!(ledger.pressure().recommendation, PressureAction::NoAction);
}

#[test]
fn record_tracks_tokens() {
    let mut ledger = ContextLedger::with_window_size(10000);
    ledger.record("src/main.rs", "full", 500, 500);
    ledger.record("src/lib.rs", "signatures", 1000, 200);
    assert_eq!(ledger.total_tokens_sent, 700);
    assert_eq!(ledger.total_tokens_saved, 800);
    assert_eq!(ledger.entries.len(), 2);
}

#[test]
fn ignition_broadcasts_high_salience_outlier() {
    // #6: an item far above the mean salience ignites and is pinned.
    let mut ledger = ContextLedger::with_window_size(100_000);
    for i in 0..5 {
        ledger.record(&format!("low{i}.rs"), "map", 100, 100);
    }
    ledger.record("hot.rs", "full", 100, 100);
    for e in &mut ledger.entries {
        e.phi = Some(if e.path == "hot.rs" { 0.95 } else { 0.1 });
    }
    let ignited = ledger.ignite_high_salience();
    assert_eq!(ignited, vec!["hot.rs".to_string()]);
    let hot = ledger.entries.iter().find(|e| e.path == "hot.rs").unwrap();
    assert_eq!(hot.state, Some(ContextState::Pinned));
}

#[test]
fn ignition_skips_small_ledger() {
    // Below GWT_MIN_ENTRIES the distribution is too small — no ignition.
    let mut ledger = ContextLedger::with_window_size(100_000);
    ledger.record("a.rs", "full", 100, 100);
    ledger.entries[0].phi = Some(0.99);
    {
        let found = &ledger.ignite_high_salience();
        assert!(found.is_empty(), "{found:?}");
    }
}

#[test]
fn ignition_is_deterministic() {
    // Determinism contract (#498): same Phi distribution → same ignitions.
    let build = || {
        let mut l = ContextLedger::with_window_size(100_000);
        for i in 0..5 {
            l.record(&format!("f{i}.rs"), "map", 100, 100);
        }
        for (i, e) in l.entries.iter_mut().enumerate() {
            e.phi = Some(if i == 0 { 0.95 } else { 0.1 });
        }
        l
    };
    let mut a = build();
    let mut b = build();
    assert_eq!(a.ignite_high_salience(), b.ignite_high_salience());
}

#[test]
fn record_updates_existing_entry() {
    let mut ledger = ContextLedger::with_window_size(10000);
    ledger.record("src/main.rs", "full", 500, 500);
    ledger.record("src/main.rs", "signatures", 500, 100);
    assert_eq!(ledger.entries.len(), 1);
    assert_eq!(ledger.total_tokens_sent, 100);
    assert_eq!(ledger.total_tokens_saved, 400);
}

#[test]
fn access_count_tracks_rereads() {
    let mut ledger = ContextLedger::with_window_size(10000);
    ledger.record("src/main.rs", "full", 500, 500);
    assert_eq!(ledger.entries[0].access_count, 1);
    ledger.record("src/main.rs", "signatures", 500, 100);
    ledger.record("src/main.rs", "map", 500, 50);
    assert_eq!(ledger.entries[0].access_count, 3);
    // A different file starts its own count.
    ledger.record("src/other.rs", "full", 200, 200);
    let other = ledger.entries.iter().find(|e| e.path == "src/other.rs");
    assert_eq!(other.map(|e| e.access_count), Some(1));
}

#[test]
fn pressure_escalates() {
    let mut ledger = ContextLedger::with_window_size(1000);
    ledger.record("a.rs", "full", 600, 600);
    assert_eq!(
        ledger.pressure().recommendation,
        PressureAction::SuggestCompression
    );
    ledger.record("b.rs", "full", 200, 200);
    assert_eq!(
        ledger.pressure().recommendation,
        PressureAction::ForceCompression
    );
    ledger.record("c.rs", "full", 150, 150);
    assert_eq!(
        ledger.pressure().recommendation,
        PressureAction::EvictLeastRelevant
    );
}

/// Regression: a session where many entries are Pinned (e.g. via GWT
/// ignition over a long session) must not report `remaining_tokens: 0`
/// nor 100% utilization when actual token usage is moderate. The pinned
/// nudge must stay bounded, and `remaining_tokens` must track real usage.
#[test]
fn pinned_pressure_does_not_zero_out_remaining_tokens() {
    let mut ledger = ContextLedger::with_window_size(200_000);
    // ~59% raw utilization, matching the real-world repro.
    ledger.record("hot.rs", "full", 118_762, 118_762);
    for i in 0..32 {
        let path = format!("pinned_{i}.rs");
        ledger.record(&path, "full", 100, 100);
        ledger.set_state(&path, ContextState::Pinned);
    }

    let pressure = ledger.pressure();
    assert!(
        pressure.utilization < 1.0,
        "32 pinned entries alone must not saturate utilization to 100%, got {}",
        pressure.utilization
    );
    assert!(
        pressure.remaining_tokens > 0,
        "real token headroom must not be reported as zero"
    );
    // total_tokens_sent = 118762 + 32*100 = 121962; window 200000.
    assert_eq!(pressure.remaining_tokens, 200_000 - 121_962);
}

#[test]
fn compression_ratio_accurate() {
    let mut ledger = ContextLedger::with_window_size(10000);
    ledger.record("a.rs", "full", 1000, 1000);
    ledger.record("b.rs", "signatures", 1000, 200);
    let ratio = ledger.compression_ratio();
    assert!((ratio - 0.6).abs() < 0.01);
}

#[test]
fn eviction_returns_oldest() {
    let mut ledger = ContextLedger::with_window_size(10000);
    ledger.record("old.rs", "full", 100, 100);
    std::thread::sleep(std::time::Duration::from_millis(10));
    ledger.record("new.rs", "full", 100, 100);
    let candidates = ledger.eviction_candidates(1);
    assert_eq!(candidates, vec!["old.rs"]);
}

#[test]
fn remove_updates_totals() {
    let mut ledger = ContextLedger::with_window_size(10000);
    ledger.record("a.rs", "full", 500, 500);
    ledger.record("b.rs", "full", 300, 300);
    assert!(ledger.remove("a.rs"));
    assert_eq!(ledger.total_tokens_sent, 300);
    assert_eq!(ledger.entries.len(), 1);
    assert!(!ledger.remove("nonexistent.rs"));
}

#[test]
fn reset_clears_everything() {
    let mut ledger = ContextLedger::with_window_size(10000);
    ledger.record("a.rs", "full", 500, 500);
    ledger.record("b.rs", "full", 300, 300);
    ledger.reset();
    assert_eq!(ledger.entries.len(), 0);
    assert_eq!(ledger.total_tokens_sent, 0);
    assert_eq!(ledger.total_tokens_saved, 0);
    assert_eq!(ledger.pressure().recommendation, PressureAction::NoAction);
}

#[test]
fn evict_paths_removes_matching() {
    let mut ledger = ContextLedger::with_window_size(10000);
    ledger.record("a.rs", "full", 500, 500);
    ledger.record("b.rs", "full", 300, 300);
    ledger.record("c.rs", "full", 200, 200);
    let removed = ledger.evict_paths(&["a.rs", "c.rs", "nonexistent.rs"]);
    assert_eq!(removed, 2);
    assert_eq!(ledger.entries.len(), 1);
    assert_eq!(ledger.entries[0].path, "b.rs");
    assert_eq!(ledger.total_tokens_sent, 300);
}

#[test]
fn mode_distribution_counts() {
    let mut ledger = ContextLedger::new();
    ledger.record("a.rs", "full", 100, 100);
    ledger.record("b.rs", "signatures", 100, 50);
    ledger.record("c.rs", "full", 100, 100);
    let dist = ledger.mode_distribution();
    assert_eq!(dist.get("full"), Some(&2));
    assert_eq!(dist.get("signatures"), Some(&1));
}

#[test]
fn format_summary_includes_key_info() {
    let mut ledger = ContextLedger::with_window_size(10000);
    ledger.record("a.rs", "full", 500, 500);
    let summary = ledger.format_summary();
    assert!(summary.contains("500/10000"));
    assert!(summary.contains("1 files"));
}

#[test]
fn reinjection_no_action_when_low_pressure() {
    use crate::core::intent_engine::StructuredIntent;

    let mut ledger = ContextLedger::with_window_size(10000);
    ledger.record("a.rs", "full", 100, 100);
    let intent = StructuredIntent::from_query("fix bug in a.rs");
    let plan = ledger.reinjection_plan(&intent, 0.7);
    assert!(plan.actions.is_empty());
    assert_eq!(plan.total_tokens_freed, 0);
}

#[test]
fn reinjection_downgrades_non_target_files() {
    use crate::core::intent_engine::StructuredIntent;

    let mut ledger = ContextLedger::with_window_size(1000);
    ledger.record("src/target.rs", "full", 400, 400);
    std::thread::sleep(std::time::Duration::from_millis(10));
    ledger.record("src/other.rs", "full", 400, 400);
    std::thread::sleep(std::time::Duration::from_millis(10));
    ledger.record("src/utils.rs", "full", 200, 200);

    let intent = StructuredIntent::from_query("fix bug in target.rs");
    let plan = ledger.reinjection_plan(&intent, 0.5);

    assert!(!plan.actions.is_empty());
    assert!(
        plan.actions.iter().all(|a| !a.path.contains("target")),
        "should not downgrade target file"
    );
    assert!(plan.total_tokens_freed > 0);
}

#[test]
fn reinjection_preserves_targets() {
    use crate::core::intent_engine::StructuredIntent;

    let mut ledger = ContextLedger::with_window_size(1000);
    ledger.record("src/auth.rs", "full", 900, 900);
    let intent = StructuredIntent::from_query("fix bug in auth.rs");
    let plan = ledger.reinjection_plan(&intent, 0.5);
    assert!(
        plan.actions.is_empty(),
        "should not downgrade target files even under pressure"
    );
}

#[test]
fn downgrade_mode_chain() {
    assert_eq!(
        downgrade_mode("full", 1000),
        Some(("signatures".to_string(), 200))
    );
    assert_eq!(
        downgrade_mode("signatures", 200),
        Some(("map".to_string(), 100))
    );
    assert_eq!(
        downgrade_mode("map", 100),
        Some(("reference".to_string(), 25))
    );
    assert_eq!(downgrade_mode("reference", 25), None);
}

#[test]
fn record_assigns_item_id() {
    let mut ledger = ContextLedger::new();
    ledger.record("src/main.rs", "full", 500, 500);
    let entry = &ledger.entries[0];
    assert!(entry.id.is_some());
    assert_eq!(entry.id.as_ref().unwrap().as_str(), "file:src/main.rs");
}

#[test]
fn record_sets_state_to_included() {
    let mut ledger = ContextLedger::new();
    ledger.record("src/main.rs", "full", 500, 500);
    assert_eq!(
        ledger.entries[0].state,
        Some(crate::core::context_field::ContextState::Included)
    );
}

#[test]
fn record_generates_view_costs() {
    let mut ledger = ContextLedger::new();
    ledger.record("src/main.rs", "full", 5000, 5000);
    let vc = ledger.entries[0].view_costs.as_ref().unwrap();
    assert_eq!(vc.get(&crate::core::context_field::ViewKind::Full), 5000);
    assert_eq!(
        vc.get(&crate::core::context_field::ViewKind::Signatures),
        1000
    );
}

#[test]
fn update_phi_works() {
    let mut ledger = ContextLedger::new();
    ledger.record("a.rs", "full", 100, 100);
    ledger.update_phi("a.rs", 0.85);
    assert_eq!(ledger.entries[0].phi, Some(0.85));
}

#[test]
fn set_state_works() {
    let mut ledger = ContextLedger::new();
    ledger.record("a.rs", "full", 100, 100);
    ledger.set_state("a.rs", crate::core::context_field::ContextState::Pinned);
    assert_eq!(
        ledger.entries[0].state,
        Some(crate::core::context_field::ContextState::Pinned)
    );
}

#[test]
fn items_by_state_filters() {
    let mut ledger = ContextLedger::new();
    ledger.record("a.rs", "full", 100, 100);
    ledger.record("b.rs", "full", 100, 100);
    ledger.set_state("b.rs", crate::core::context_field::ContextState::Excluded);
    let included = ledger.items_by_state(crate::core::context_field::ContextState::Included);
    assert_eq!(included.len(), 1);
    assert_eq!(included[0].path, "a.rs");
}

#[test]
fn eviction_by_phi_prefers_low_phi() {
    let mut ledger = ContextLedger::with_window_size(10000);
    ledger.record("high.rs", "full", 100, 100);
    ledger.update_phi("high.rs", 0.9);
    ledger.record("low.rs", "full", 100, 100);
    ledger.update_phi("low.rs", 0.1);
    let candidates = ledger.eviction_candidates_by_phi(1);
    assert_eq!(candidates, vec!["low.rs"]);
}

#[test]
fn eviction_by_phi_skips_pinned() {
    let mut ledger = ContextLedger::with_window_size(10000);
    ledger.record("pinned.rs", "full", 100, 100);
    ledger.update_phi("pinned.rs", 0.01);
    ledger.set_state(
        "pinned.rs",
        crate::core::context_field::ContextState::Pinned,
    );
    ledger.record("normal.rs", "full", 100, 100);
    ledger.update_phi("normal.rs", 0.5);
    let candidates = ledger.eviction_candidates_by_phi(1);
    assert_eq!(candidates, vec!["normal.rs"]);
}

#[test]
fn mark_stale_by_hash_detects_change() {
    let mut ledger = ContextLedger::new();
    ledger.record("a.rs", "full", 100, 100);
    ledger.entries[0].source_hash = Some("hash_v1".to_string());
    ledger.mark_stale_by_hash("a.rs", "hash_v2");
    assert_eq!(
        ledger.entries[0].state,
        Some(crate::core::context_field::ContextState::Stale)
    );
}

#[test]
fn find_by_id_works() {
    let mut ledger = ContextLedger::new();
    ledger.record("src/lib.rs", "full", 100, 100);
    let id = crate::core::context_field::ContextItemId::from_file("src/lib.rs");
    assert!(ledger.find_by_id(&id).is_some());
}

#[test]
fn phi_recomputed_on_reread_not_sticky() {
    // #2: Phi must track time-variant salience, not freeze on first read.
    let _env = crate::core::data_dir::test_env_lock();
    let dir = tempfile::tempdir().unwrap();
    crate::test_env::set_var("LEAN_CTX_DATA_DIR", dir.path());

    let mut ledger = ContextLedger::with_window_size(100_000);
    // First read carries a task whose keyword matches the path → relevance up.
    ledger.record_with_task(
        "src/authentication.rs",
        "full",
        2000,
        2000,
        Some("fix authentication login flow"),
    );
    let phi_with_task = ledger.entries[0].phi.unwrap();
    // Re-read with no task context → relevance collapses, so the blended Phi
    // must move. Before the fix this stayed frozen at the first value.
    ledger.record_with_task("src/authentication.rs", "full", 2000, 2000, None);
    let phi_after = ledger.entries[0].phi.unwrap();
    assert_ne!(
        phi_with_task, phi_after,
        "Phi must be recomputed on re-read (#2)"
    );
    assert!(
        phi_after < phi_with_task,
        "dropping task relevance should lower Phi ({phi_with_task} -> {phi_after})"
    );
}

// ── #715: target resolution (exact → root-relative → unique suffix) ──

#[test]
fn resolve_entry_matches_basename_suffix_uniquely() {
    let mut ledger = ContextLedger::with_window_size(10000);
    ledger.record("/home/user/proj/src/context_ledger.rs", "full", 500, 500);
    ledger.record("/home/user/proj/src/other.rs", "full", 300, 300);

    // The exact repro from #715: basename against absolute entries.
    assert_eq!(
        ledger.resolve_entry("context_ledger.rs", None),
        LedgerResolution::Unique(0)
    );
    // Partial relative path.
    assert_eq!(
        ledger.resolve_entry("src/other.rs", None),
        LedgerResolution::Unique(1)
    );
    // Component boundary: `edger.rs` must NOT suffix-match `…ledger.rs`.
    assert_eq!(
        ledger.resolve_entry("edger.rs", None),
        LedgerResolution::NotFound
    );
}

#[test]
fn resolve_entry_reports_ambiguous_suffix() {
    let mut ledger = ContextLedger::with_window_size(10000);
    ledger.record("/proj/a/mod.rs", "full", 100, 100);
    ledger.record("/proj/b/mod.rs", "full", 100, 100);

    match ledger.resolve_entry("mod.rs", None) {
        LedgerResolution::Ambiguous(candidates) => {
            assert_eq!(candidates.len(), 2);
            assert!(candidates.contains(&"/proj/a/mod.rs".to_string()));
        }
        other => panic!("expected Ambiguous, got {other:?}"),
    }
    // A longer suffix disambiguates.
    assert_eq!(
        ledger.resolve_entry("a/mod.rs", None),
        LedgerResolution::Unique(0)
    );
}

#[test]
fn resolve_entry_prefers_project_root_relative() {
    let mut ledger = ContextLedger::with_window_size(10000);
    ledger.record("/work/proj/src/lib.rs", "full", 100, 100);
    ledger.record("/elsewhere/src/lib.rs", "full", 100, 100);

    // Suffix alone is ambiguous; the project root resolves it.
    assert!(matches!(
        ledger.resolve_entry("src/lib.rs", None),
        LedgerResolution::Ambiguous(_)
    ));
    assert_eq!(
        ledger.resolve_entry("src/lib.rs", Some("/work/proj")),
        LedgerResolution::Unique(0)
    );
}

#[test]
fn resolve_entry_handles_windows_separators() {
    let mut ledger = ContextLedger::with_window_size(10000);
    // Simulates a migrated ledger: forward-slash canonical entries.
    ledger.entries.push(LedgerEntry {
        path: "C:/Users/dev/proj/src/main.rs".to_string(),
        mode: "full".to_string(),
        original_tokens: 100,
        sent_tokens: 100,
        timestamp: chrono::Utc::now().timestamp(),
        id: None,
        kind: None,
        source_hash: None,
        state: None,
        phi: None,
        view_costs: None,
        active_view: None,
        provenance: None,
        access_count: 1,
    });
    ledger.total_tokens_sent = 100;

    // Backslash target (Windows UI / dashboard) resolves lexically.
    assert_eq!(
        ledger.resolve_entry("src\\main.rs", None),
        LedgerResolution::Unique(0)
    );
    assert_eq!(ledger.evict_paths(&["src\\main.rs"]), 1);
    assert!(ledger.entries.is_empty());
}

#[test]
fn evict_paths_resolved_reports_outcomes() {
    let mut ledger = ContextLedger::with_window_size(10000);
    ledger.record("/proj/src/gate.rs", "full", 500, 500);
    ledger.record("/proj/a/dup.rs", "full", 100, 100);
    ledger.record("/proj/b/dup.rs", "full", 100, 100);

    let outcomes = ledger.evict_paths_resolved(&["gate.rs", "dup.rs", "missing.rs"], Some("/proj"));
    assert_eq!(
        outcomes[0].resolved.as_deref(),
        Some("/proj/src/gate.rs"),
        "basename must resolve and evict"
    );
    assert!(outcomes[1].resolved.is_none());
    assert_eq!(outcomes[1].ambiguous.len(), 2, "ambiguity is diagnosed");
    assert!(outcomes[2].resolved.is_none());
    {
        let found = &outcomes[2].ambiguous;
        assert!(found.is_empty(), "{found:?}");
    }
    assert_eq!(ledger.entries.len(), 2, "only the unique match is removed");
    assert_eq!(ledger.total_tokens_sent, 200);
}

#[test]
fn set_state_resolves_partial_paths() {
    let mut ledger = ContextLedger::new();
    ledger.record("/proj/src/deep/file.rs", "full", 100, 100);
    ledger.set_state("file.rs", crate::core::context_field::ContextState::Pinned);
    assert_eq!(
        ledger.entries[0].state,
        Some(crate::core::context_field::ContextState::Pinned)
    );
}

#[test]
fn upsert_sets_source_hash_and_kind() {
    let mut ledger = ContextLedger::new();
    ledger.upsert(
        "src/main.rs",
        "full",
        500,
        500,
        Some("sha256_abc"),
        crate::core::context_field::ContextKind::File,
        None,
    );
    let entry = &ledger.entries[0];
    assert_eq!(entry.source_hash.as_deref(), Some("sha256_abc"));
    assert_eq!(
        entry.kind,
        Some(crate::core::context_field::ContextKind::File)
    );
}
