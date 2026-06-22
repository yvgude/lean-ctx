// Integration tests for Issue #244: Unable to clear context pressure.
// Tests ledger reset, eviction, session reset clearing ledger,
// and actionable eviction hints.

mod ledger_reset {
    use lean_ctx::core::context_ledger::{ContextLedger, PressureAction};

    #[test]
    fn reset_clears_all_entries_and_totals() {
        let mut ledger = ContextLedger::with_window_size(10000);
        ledger.record("a.rs", "full", 3000, 3000);
        ledger.record("b.rs", "full", 3000, 3000);
        ledger.record("c.rs", "full", 3500, 3500);
        // 9500/10000 = 95% → must be EvictLeastRelevant (>90%)
        assert_eq!(
            ledger.pressure().recommendation,
            PressureAction::EvictLeastRelevant
        );

        ledger.reset();

        assert_eq!(ledger.entries.len(), 0);
        assert_eq!(ledger.total_tokens_sent, 0);
        assert_eq!(ledger.total_tokens_saved, 0);
        assert_eq!(ledger.pressure().recommendation, PressureAction::NoAction);
        assert!((ledger.pressure().utilization - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn reset_preserves_window_size() {
        let mut ledger = ContextLedger::with_window_size(200_000);
        ledger.record("big.rs", "full", 100_000, 100_000);
        ledger.reset();
        assert_eq!(ledger.window_size, 200_000);
    }
}

mod ledger_evict {
    use lean_ctx::core::context_ledger::ContextLedger;

    #[test]
    fn evict_removes_specific_paths() {
        let mut ledger = ContextLedger::with_window_size(10000);
        ledger.record("keep.rs", "full", 1000, 1000);
        ledger.record("evict_me.rs", "full", 5000, 5000);
        ledger.record("also_evict.rs", "full", 3000, 3000);

        let removed = ledger.evict_paths(&["evict_me.rs", "also_evict.rs"]);

        assert_eq!(removed, 2);
        assert_eq!(ledger.entries.len(), 1);
        assert_eq!(ledger.entries[0].path, "keep.rs");
        assert_eq!(ledger.total_tokens_sent, 1000);
    }

    #[test]
    fn evict_reduces_pressure() {
        let mut ledger = ContextLedger::with_window_size(10000);
        ledger.record("a.rs", "full", 5000, 5000);
        ledger.record("b.rs", "full", 5000, 5000);
        // 10000/10000 = 100% → EvictLeastRelevant
        assert!(ledger.pressure().utilization > 0.9);

        ledger.evict_paths(&["a.rs"]);

        // 5000/10000 = 50% → SuggestCompression
        assert!(
            ledger.pressure().utilization <= 0.5 + 0.05,
            "pressure should drop to ~50% after eviction, got: {:.1}%",
            ledger.pressure().utilization * 100.0,
        );
    }

    #[test]
    fn evict_nonexistent_path_is_noop() {
        let mut ledger = ContextLedger::with_window_size(10000);
        ledger.record("exists.rs", "full", 1000, 1000);

        let removed = ledger.evict_paths(&["nonexistent.rs", "also_missing.rs"]);

        assert_eq!(removed, 0);
        assert_eq!(ledger.entries.len(), 1);
        assert_eq!(ledger.total_tokens_sent, 1000);
    }

    #[test]
    fn evict_with_mixed_existing_and_missing() {
        let mut ledger = ContextLedger::with_window_size(10000);
        ledger.record("a.rs", "full", 1000, 1000);
        ledger.record("b.rs", "full", 2000, 2000);

        let removed = ledger.evict_paths(&["a.rs", "missing.rs", "b.rs"]);

        assert_eq!(removed, 2);
        assert_eq!(ledger.entries.len(), 0);
        assert_eq!(ledger.total_tokens_sent, 0);
    }
}

mod session_reset_clears_ledger {
    use lean_ctx::core::context_ledger::{ContextLedger, PressureAction};

    #[test]
    fn fresh_ledger_has_zero_pressure() {
        let ledger = ContextLedger::new();
        let pressure = ledger.pressure();
        assert!((pressure.utilization - 0.0).abs() < f64::EPSILON);
        assert_eq!(pressure.recommendation, PressureAction::NoAction);
    }

    #[test]
    fn simulated_session_reset_clears_pressure() {
        let mut ledger = ContextLedger::with_window_size(10000);
        ledger.record("file1.json", "full", 4000, 4000);
        ledger.record("file2.json", "full", 4000, 4000);
        ledger.record("script.py", "full", 3000, 3000);
        assert!(ledger.pressure().utilization > 0.9);

        // Simulate what session reset now does
        ledger.reset();

        assert_eq!(ledger.entries.len(), 0);
        assert_eq!(ledger.total_tokens_sent, 0);
        assert!((ledger.pressure().utilization - 0.0).abs() < f64::EPSILON);
    }
}

mod actionable_hints {
    use lean_ctx::core::context_ledger::ContextLedger;
    use lean_ctx::core::context_overlay::OverlayStore;
    use lean_ctx::server::context_gate;

    #[test]
    fn eviction_hint_contains_ctx_ledger_command() {
        let mut ledger = ContextLedger::with_window_size(1000);
        ledger.record("a.rs", "full", 300, 300);
        std::thread::sleep(std::time::Duration::from_millis(10));
        ledger.record("b.rs", "full", 300, 300);
        std::thread::sleep(std::time::Duration::from_millis(10));
        ledger.record("c.rs", "full", 300, 300);
        std::thread::sleep(std::time::Duration::from_millis(10));
        ledger.record("d.rs", "full", 200, 200);

        let overlay = OverlayStore::new();
        let result = context_gate::post_dispatch_record_with_task(
            "e.rs",
            "full",
            100,
            100,
            &mut ledger,
            &overlay,
            None,
            None,
        );

        if let Some(hint) = result.eviction_hint {
            assert!(
                hint.contains("ctx_ledger"),
                "hint should reference ctx_ledger tool: {hint}"
            );
            assert!(
                hint.contains("evict"),
                "hint should contain evict action: {hint}"
            );
        }
    }

    #[test]
    fn no_hint_at_low_pressure() {
        let mut ledger = ContextLedger::with_window_size(100_000);
        ledger.record("small.rs", "full", 100, 100);

        let overlay = OverlayStore::new();
        let result = context_gate::post_dispatch_record_with_task(
            "another.rs",
            "full",
            50,
            50,
            &mut ledger,
            &overlay,
            None,
            None,
        );

        assert!(
            result.eviction_hint.is_none(),
            "should not hint at low pressure"
        );
    }
}

mod bug_reporter_scenario {
    use lean_ctx::core::context_ledger::{ContextLedger, PressureAction};

    #[test]
    fn exact_reporter_scenario_66_entries_high_pressure() {
        let mut ledger = ContextLedger::with_window_size(128_000);
        for i in 0..66 {
            let path = format!("project/file_{i}.json");
            let tokens = 15_000 + (i * 200);
            ledger.record(&path, "full", tokens, tokens);
        }
        assert!(ledger.total_tokens_sent > 128_000);
        assert!(ledger.pressure().utilization >= 1.0);
        assert_eq!(
            ledger.pressure().recommendation,
            PressureAction::EvictLeastRelevant
        );

        // The fix: ledger.reset() clears everything
        ledger.reset();
        assert_eq!(ledger.entries.len(), 0);
        assert_eq!(ledger.total_tokens_sent, 0);
        assert!((ledger.pressure().utilization - 0.0).abs() < f64::EPSILON);
        assert_eq!(ledger.pressure().recommendation, PressureAction::NoAction);
    }

    #[test]
    fn evict_suggested_files_reduces_pressure() {
        let mut ledger = ContextLedger::with_window_size(128_000);
        for i in 0..20 {
            let path = format!("src/module_{i}.rs");
            ledger.record(&path, "full", 7000, 7000);
        }
        // 140000/128000 > 100%
        assert!(ledger.pressure().utilization >= 0.9);

        // Evict the 3 least relevant as suggested by the hint
        let candidates = ledger.eviction_candidates_by_phi(3);
        let to_evict: Vec<&str> = candidates.iter().take(3).map(String::as_str).collect();
        let removed = ledger.evict_paths(&to_evict);

        assert_eq!(removed, 3);
        assert_eq!(ledger.entries.len(), 17);
        // Pressure should have dropped significantly (3*7000 = 21000 freed)
        let new_pressure = ledger.pressure().utilization;
        assert!(
            new_pressure < 1.0,
            "pressure should drop after eviction: {new_pressure}"
        );
    }

    #[test]
    fn session_reset_is_complete_fresh_start() {
        let mut ledger = ContextLedger::with_window_size(128_000);
        for i in 0..50 {
            ledger.record(&format!("file{i}.py"), "full", 3000, 3000);
        }
        assert!(ledger.pressure().utilization > 0.9);

        // Simulate session reset
        ledger.reset();

        // Verify completely clean state
        assert_eq!(ledger.entries.len(), 0);
        assert_eq!(ledger.total_tokens_sent, 0);
        assert_eq!(ledger.total_tokens_saved, 0);
        assert_eq!(ledger.window_size, 128_000); // preserved
        assert_eq!(ledger.pressure().recommendation, PressureAction::NoAction);

        // Verify we can record again normally
        ledger.record("new_file.rs", "full", 1000, 1000);
        assert_eq!(ledger.entries.len(), 1);
        assert_eq!(ledger.total_tokens_sent, 1000);
    }

    #[test]
    fn evict_then_re_record_same_file_works() {
        let mut ledger = ContextLedger::with_window_size(10000);
        ledger.record("big.rs", "full", 8000, 8000);
        assert!(ledger.pressure().utilization > 0.7);

        ledger.evict_paths(&["big.rs"]);
        assert_eq!(ledger.entries.len(), 0);
        assert_eq!(ledger.total_tokens_sent, 0);

        // Re-reading the file would re-add it (but with exclude overlay
        // in the real MCP tool, only signatures mode is allowed)
        ledger.record("big.rs", "signatures", 8000, 1600);
        assert_eq!(ledger.entries.len(), 1);
        assert_eq!(ledger.total_tokens_sent, 1600);
    }
}

mod file_locking {
    use lean_ctx::core::context_ledger::ContextLedger;

    #[test]
    fn save_and_load_roundtrip_with_locking() {
        let dir = std::env::temp_dir().join(format!("lean_ctx_test_lock_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        // TODO: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::set_var("LEAN_CTX_DATA_DIR", dir.to_str().unwrap()) };

        let mut ledger = ContextLedger::with_window_size(50000);
        ledger.record("test.rs", "full", 1000, 1000);
        ledger.record("other.rs", "map", 500, 100);
        ledger.save();

        let loaded = ContextLedger::load();
        assert_eq!(loaded.entries.len(), 2);
        assert_eq!(loaded.total_tokens_sent, 1100);

        // Clean up
        let _ = std::fs::remove_dir_all(&dir);
        // TODO: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::remove_var("LEAN_CTX_DATA_DIR") };
    }
}

mod double_booking_fix {
    use lean_ctx::core::context_ledger::ContextLedger;

    #[test]
    fn single_record_produces_correct_totals() {
        let mut ledger = ContextLedger::with_window_size(128_000);
        ledger.record("src/main.rs", "full", 5000, 3000);

        assert_eq!(ledger.entries.len(), 1);
        assert_eq!(ledger.total_tokens_sent, 3000);
        assert_eq!(ledger.total_tokens_saved, 2000);
    }

    #[test]
    fn upsert_same_path_does_not_double_count() {
        let mut ledger = ContextLedger::with_window_size(128_000);

        // First record (simulating dispatch)
        ledger.record("src/main.rs", "full", 5000, 5000);
        assert_eq!(ledger.total_tokens_sent, 5000);

        // Second record same path (simulating post_dispatch with different values)
        ledger.record("src/main.rs", "full", 5000, 3000);
        assert_eq!(ledger.entries.len(), 1);
        assert_eq!(ledger.total_tokens_sent, 3000);
        assert_eq!(ledger.total_tokens_saved, 2000);
    }

    #[test]
    fn remove_returns_bool_correctly() {
        let mut ledger = ContextLedger::with_window_size(10000);
        ledger.record("exists.rs", "full", 500, 500);

        assert!(ledger.remove("exists.rs"));
        assert!(!ledger.remove("exists.rs"));
        assert!(!ledger.remove("never_existed.rs"));
    }
}
