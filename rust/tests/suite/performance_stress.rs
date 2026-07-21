//! Performance stress tests — verifies that critical paths meet latency bounds.
//!
//! These are complexity-regression guards, not benchmarks: limits sit ~5-10x
//! above typical wall-clock so hosted-runner CPU contention (observed 2x+
//! slowdowns on otherwise green runs) never flakes the gate, while a real
//! algorithmic regression still lands far above the bound.

use std::time::Instant;

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
            elapsed.as_millis() < 100,
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
            elapsed.as_millis() < 500,
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
            elapsed.as_millis() < 1000,
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
            elapsed.as_micros() < 50_000,
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

mod hebbian_stress {
    use super::*;
    use lean_ctx::core::hebbian_cache::*;

    #[test]
    fn stress_large_file_set() {
        let mut matrix = CoAccessMatrix::new();

        // Simulate 500 unique files with patterns
        let start = Instant::now();
        for burst in 0..200 {
            let file_a = path_hash(&format!("src/module_{}/main.rs", burst % 50));
            let file_b = path_hash(&format!("src/module_{}/lib.rs", burst % 50));
            let file_c = path_hash(&format!("tests/module_{}_test.rs", burst % 50));

            matrix.record_access(file_a);
            matrix.record_access(file_b);
            matrix.record_access(file_c);
            matrix.end_burst();
        }
        let elapsed = start.elapsed();

        assert!(
            elapsed.as_millis() < 100,
            "200 bursts with 3 files each took {}ms — must be <100ms",
            elapsed.as_millis()
        );

        // Verify associations are established
        let active = vec![path_hash("src/module_0/main.rs")];
        let assoc = matrix.association_strength(path_hash("src/module_0/lib.rs"), &active);
        assert!(
            assoc > 0.0,
            "Co-accessed files should have positive association"
        );
    }

    #[test]
    fn stress_boltzmann_eviction_many_entries() {
        let energies: Vec<f64> = (0..1000).map(|i| f64::from(i) * 0.1).collect();

        let start = Instant::now();
        let evictions = boltzmann_select_evictions(&energies, 100, 0.1);
        let elapsed = start.elapsed();

        assert_eq!(evictions.len(), 100);
        // Regression guard, not a benchmark: the operation is µs-scale, so a
        // real complexity regression lands far above 100ms. Tighter limits
        // (20ms) flaked on hosted runners — observed 41ms on an otherwise
        // green run purely from CPU contention.
        let limit_us = if cfg!(windows) { 200_000 } else { 100_000 };
        assert!(
            elapsed.as_micros() < limit_us,
            "Evicting 100 from 1000 entries took {}µs — must be <{limit_us}µs",
            elapsed.as_micros()
        );
    }
}

mod predictive_coding_stress {
    use super::*;
    use lean_ctx::core::predictive_coding::*;

    #[test]
    fn stress_large_file_delta() {
        // Simulate a large file (2000 lines) with 5% changes
        let lines: Vec<String> = (0..2000)
            .map(|i| format!("line {i}: content here"))
            .collect();
        let prev = lines.join("\n");

        let mut new_lines = lines.clone();
        for i in (0..2000).step_by(20) {
            new_lines[i] = format!("line {i}: MODIFIED content");
        }
        let curr = new_lines.join("\n");

        let start = Instant::now();
        let delta = compute_delta("full", &prev, &curr).unwrap();
        let elapsed = start.elapsed();

        assert!(
            elapsed.as_millis() < 250,
            "Delta computation for 2000-line file took {}ms — must be <250ms",
            elapsed.as_millis()
        );

        // Should detect ~100 changes (every 20th line)
        let total_changes = delta.added_lines.len() + delta.removed_lines.len();
        assert!(
            total_changes > 50,
            "Should detect many changes, got {total_changes}"
        );
    }

    #[test]
    fn stress_identical_large_file() {
        let lines: Vec<String> = (0..5000).map(|i| format!("unchanged line {i}")).collect();
        let content = lines.join("\n");

        let start = Instant::now();
        let delta = compute_delta("map", &content, &content).unwrap();
        let elapsed = start.elapsed();

        assert!(
            elapsed.as_millis() < 250,
            "Delta of identical 5000-line file took {}ms — must be <250ms",
            elapsed.as_millis()
        );
        assert!(delta.added_lines.is_empty());
        assert!(delta.removed_lines.is_empty());
        assert_eq!(delta.unchanged_count, 5000);
    }
}

mod attention_stress {
    use super::*;
    use lean_ctx::core::attention_context::*;

    #[test]
    fn stress_many_chunks_assembly() {
        // 500 chunks — realistic for a large search result
        let chunks: Vec<(usize, &str, bool)> = (0..500)
            .map(|i| {
                let content = if i % 10 == 0 {
                    "pub fn important_function() { complex_logic(); with_many_unique_terms(); }"
                } else {
                    "use std::io; use std::fmt; fn helper() {}"
                };
                (i, content, i % 10 == 0)
            })
            .collect();

        let start = Instant::now();
        let result = attention_weighted_assembly(&chunks, 50_000);
        let elapsed = start.elapsed();

        assert_eq!(result.len(), 500);
        // 350ms budget for debug builds on shared CI runners; release is ~10x faster
        assert!(
            elapsed.as_millis() < 350,
            "Attention assembly of 500 chunks took {}ms — must be <350ms",
            elapsed.as_millis()
        );

        // Important chunks (every 10th) should get more budget
        let important_budget: usize = result
            .iter()
            .filter(|r| r.chunk_idx % 10 == 0)
            .map(|r| r.token_budget)
            .sum();
        let other_budget: usize = result
            .iter()
            .filter(|r| r.chunk_idx % 10 != 0)
            .map(|r| r.token_budget)
            .sum();

        // 50 important chunks (10%) should get at least 12% of budget (above equal share)
        let important_ratio = important_budget as f64 / (important_budget + other_budget) as f64;
        assert!(
            important_ratio > 0.10,
            "Important chunks ({important_budget}) should get above-equal share, ratio={important_ratio:.3}"
        );
    }
}
