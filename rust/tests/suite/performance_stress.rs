//! Performance stress tests — verifies that critical paths meet latency bounds.
//!
//! These are complexity-regression guards, not benchmarks: limits sit ~5-10x
//! above typical wall-clock so hosted-runner CPU contention (observed 2x+
//! slowdowns on otherwise green runs) never flakes the gate, while a real
//! algorithmic regression still lands far above the bound.
//!
//! That slack is not enough once the suite itself runs multi-threaded: the
//! runner is no longer quiet, and 500-chunk attention assembly measured 733ms
//! against a 350ms bound purely from CPU contention. The *timing* half of each
//! guard is therefore opt-in — CI's perf-gate step sets `LEAN_CTX_PERF_GATE=1`
//! and runs these serialized. The functional assertions run always, everywhere.

use std::time::Instant;

fn timing_enforced() -> bool {
    std::env::var("LEAN_CTX_PERF_GATE").as_deref() == Ok("1")
}

mod bm25_performance {
    use super::*;
    use lean_ctx::core::bm25_index::BM25Index;

    #[test]
    fn stress_bm25_large_corpus() {
        // Build a temp dir with many files to stress-test BM25
        let dir = tempfile::tempdir().unwrap();
        for i in 0..500 {
            let content = format!(
                "pub fn handler_{i}() {{ let x = process_request(); validate(x); }}\n\
                 pub fn helper_{i}() {{ compute_hash(); transform(); }}\n"
            );
            std::fs::write(dir.path().join(format!("module_{i}.rs")), content).unwrap();
        }

        let index = BM25Index::build_from_directory(dir.path());

        let start = Instant::now();
        let results = index.search("process_request validate", 20);
        let elapsed = start.elapsed();

        assert!(!results.is_empty());
        assert!(
            !timing_enforced() || elapsed.as_millis() < 100,
            "BM25 search over 500-file corpus took {}ms — must be <100ms",
            elapsed.as_millis()
        );
    }

    #[test]
    fn stress_bm25_repeated_searches() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..100 {
            let content = format!(
                "pub fn api_endpoint_{i}() {{ authenticate(); authorize(); respond(); }}\n"
            );
            std::fs::write(dir.path().join(format!("route_{i}.rs")), content).unwrap();
        }

        let index = BM25Index::build_from_directory(dir.path());

        let start = Instant::now();
        for _ in 0..100 {
            let _ = index.search("authenticate authorize", 10);
        }
        let elapsed = start.elapsed();

        assert!(
            !timing_enforced() || elapsed.as_millis() < 500,
            "100 BM25 searches took {}ms — must be <500ms",
            elapsed.as_millis()
        );
    }
}

mod hnsw_stress {
    use super::*;
    use lean_ctx::core::hnsw::FlatEmbeddings;
    use lean_ctx::core::hnsw::brute_force_topk;

    fn random_vec(dim: usize, seed: u64) -> Vec<f32> {
        let mut v = Vec::with_capacity(dim);
        let mut s = seed;
        for _ in 0..dim {
            s = s
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            v.push((s as f64 / u64::MAX as f64 * 2.0 - 1.0) as f32);
        }
        v
    }

    #[test]
    fn stress_topk_10k_vectors() {
        let dim = 384;
        let n = 10_000;
        let vectors: Vec<Vec<f32>> = (0..n).map(|i| random_vec(dim, i as u64)).collect();
        let query = random_vec(dim, 99999);

        let start = Instant::now();
        let results = brute_force_topk(&FlatEmbeddings::from_vecs(vectors), &query, 20);
        let elapsed = start.elapsed();

        assert_eq!(results.len(), 20);
        assert!(
            !timing_enforced() || elapsed.as_millis() < 1000,
            "Top-20 from 10K 384d vectors took {}ms — must be <1000ms",
            elapsed.as_millis()
        );
    }

    #[test]
    fn stress_topk_maintains_ordering_under_load() {
        let dim = 128;
        let n = 50_000;
        let vectors: Vec<Vec<f32>> = (0..n).map(|i| random_vec(dim, i as u64)).collect();
        let query = random_vec(dim, 12345);

        let results = brute_force_topk(&FlatEmbeddings::from_vecs(vectors), &query, 50);
        assert_eq!(results.len(), 50);

        // Verify strict descending order
        for w in results.windows(2) {
            assert!(
                w[0].1 >= w[1].1,
                "Ordering violation: {} < {}",
                w[0].1,
                w[1].1
            );
        }
    }
}

mod homeostasis_stress {
    use super::*;
    use lean_ctx::core::homeostasis::*;

    #[test]
    fn stress_rapid_pressure_oscillations() {
        let mut ctrl = HomeostasisController::new(100_000);

        // Simulate 1000 rapid oscillations between normal and critical
        let start = Instant::now();
        for i in 0..1000 {
            let usage = if i % 2 == 0 { 40_000 } else { 92_000 };
            let action = ctrl.evaluate(usage);
            if matches!(action, HomeostasisAction::None) {
                // Normal pressure
            } else {
                ctrl.report_outcome(true);
            }
        }
        let elapsed = start.elapsed();

        assert!(
            !timing_enforced() || elapsed.as_micros() < 50_000,
            "1000 homeostasis evaluations took {}µs — must be <50000µs",
            elapsed.as_micros()
        );
    }

    #[test]
    fn stress_escalation_ladder_is_bounded() {
        let mut ctrl = HomeostasisController::new(100_000);

        // Keep reporting failure — escalation should not panic or overflow
        for _ in 0..100 {
            ctrl.evaluate(92_000);
            ctrl.report_outcome(false);
        }

        // Should still produce valid actions
        let action = ctrl.evaluate(92_000);
        assert!(
            matches!(
                action,
                HomeostasisAction::EvictProtected { .. } | HomeostasisAction::EmergencyDrop
            ),
            "After 100 failures, should be at max escalation level, got {action:?}"
        );
    }
}
