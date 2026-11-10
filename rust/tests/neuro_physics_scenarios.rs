//! Comprehensive scenario tests for the Neuro-Physics Hardening implementation.
//!
//! Tests real-world usage patterns for:
//! 2. HNSW Dense Search Performance (ANN Theory)
//! 3. BM25 Score Array Optimization (Kolmogorov)
//! 4. Hebbian Cache + Boltzmann Eviction (Statistical Physics)
//! 5. Homeostasis Memory Guard (Biology)
//!
//! The shell allowlist scenarios live in crates/lean-ctx-shell-policy/tests.

// ═══════════════════════════════════════════════════════════════════════════════
// 2. HNSW + BRUTE-FORCE TOP-K — Performance & correctness
// ═══════════════════════════════════════════════════════════════════════════════

mod hnsw_performance {
    use lean_ctx::core::hnsw::{AnnIndex, FlatEmbeddings, brute_force_topk};

    fn deterministic_vec(dim: usize, seed: u64) -> Vec<f32> {
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
    fn scenario_topk_exact_on_small_set() {
        let dim = 384;
        let vectors: Vec<Vec<f32>> = (0..200).map(|i| deterministic_vec(dim, i)).collect();
        let query = deterministic_vec(dim, 9999);

        let top10 = brute_force_topk(&FlatEmbeddings::from_vecs(vectors.clone()), &query, 10);
        assert_eq!(top10.len(), 10);

        for w in top10.windows(2) {
            assert!(
                w[0].1 >= w[1].1,
                "Results not sorted: {} >= {} failed",
                w[0].1,
                w[1].1
            );
        }

        // Verify correctness by full sort comparison
        let mut all_sims: Vec<(usize, f32)> = vectors
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let dot: f32 = query.iter().zip(v).map(|(a, b)| a * b).sum();
                let na: f32 = query.iter().map(|x| x * x).sum::<f32>().sqrt();
                let nb: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
                let sim = if na * nb > 0.0 { dot / (na * nb) } else { 0.0 };
                (i, sim)
            })
            .collect();
        all_sims.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // Top-10 from brute_force_topk must match exhaustive top-10
        for k in 0..10 {
            assert_eq!(
                top10[k].0, all_sims[k].0,
                "Mismatch at rank {k}: got idx={}, expected idx={}",
                top10[k].0, all_sims[k].0
            );
        }
    }

    #[test]
    fn scenario_topk_handles_edge_cases() {
        // Empty vectors
        let result = brute_force_topk(&FlatEmbeddings::from_vecs(vec![]), &[1.0, 0.0], 5);
        assert_eq!(result, [] as [(usize, f32); 0]);

        // top_k > num vectors
        let vectors = vec![vec![1.0, 0.0], vec![0.0, 1.0]];
        let result = brute_force_topk(&FlatEmbeddings::from_vecs(vectors), &[1.0, 0.0], 10);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn scenario_ann_index_small_uses_brute_force() {
        let dim = 16;
        let vectors: Vec<Vec<f32>> = (0..100).map(|i| deterministic_vec(dim, i)).collect();
        let index = AnnIndex::build(FlatEmbeddings::from_vecs(vectors));

        let results = index.search(&deterministic_vec(dim, 5000), 5);
        assert_eq!(results.len(), 5);
        // Should still be sorted
        for w in results.windows(2) {
            assert!(w[0].1 >= w[1].1);
        }
    }

    #[test]
    fn scenario_performance_topk_vs_full_sort() {
        // Measure that topk is fast enough for real-world chunk counts
        let dim = 384;
        let n = 5000;
        let vectors: Vec<Vec<f32>> = (0..n).map(|i| deterministic_vec(dim, i as u64)).collect();
        let query = deterministic_vec(dim, 99999);

        let start = std::time::Instant::now();
        let results = brute_force_topk(&FlatEmbeddings::from_vecs(vectors), &query, 20);
        let elapsed = start.elapsed();
        assert_eq!(results.len(), 20);

        // No fixed wall-clock budget survives a loaded runner, so the timing
        // half is opt-in (`LEAN_CTX_PERF_GATE=1`, CI's quiet perf step).
        assert!(
            std::env::var("LEAN_CTX_PERF_GATE").as_deref() != Ok("1") || elapsed.as_millis() < 1000,
            "brute_force_topk took {}ms for {n} vectors — too slow",
            elapsed.as_millis()
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// 5. HOMEOSTASIS MEMORY GUARD
// ═══════════════════════════════════════════════════════════════════════════════

mod homeostasis {
    use lean_ctx::core::homeostasis::*;

    #[test]
    fn scenario_normal_operation_no_intervention() {
        let mut ctrl = HomeostasisController::new(100_000);

        let action = ctrl.evaluate(40_000);
        assert_eq!(action, HomeostasisAction::None);

        let action = ctrl.evaluate(60_000);
        assert_eq!(action, HomeostasisAction::None);
    }

    #[test]
    fn scenario_gradual_pressure_buildup() {
        let mut ctrl = HomeostasisController::new(100_000);

        let a1 = ctrl.evaluate(72_000);
        assert_eq!(a1, HomeostasisAction::TrimOutputs);

        ctrl.report_outcome(true);
        let a2 = ctrl.evaluate(65_000);
        assert_eq!(a2, HomeostasisAction::None);
    }

    #[test]
    fn scenario_rapid_pressure_spike() {
        let mut ctrl = HomeostasisController::new(100_000);

        let action = ctrl.evaluate(93_000);
        assert_eq!(action, HomeostasisAction::UnloadIndices);
    }

    #[test]
    fn scenario_sustained_pressure_escalates() {
        let mut ctrl = HomeostasisController::new(100_000);

        // Sustained high pressure without relief → should escalate
        for _ in 0..4 {
            ctrl.evaluate(92_000);
            ctrl.report_outcome(false);
        }

        let escalated = ctrl.evaluate(92_000);
        assert!(
            matches!(escalated, HomeostasisAction::EvictProtected { .. }),
            "Should escalate to EvictProtected after sustained ineffective actions, got {escalated:?}"
        );
    }

    #[test]
    fn scenario_recovery_resets_state() {
        let mut ctrl = HomeostasisController::new(100_000);

        ctrl.evaluate(92_000);
        ctrl.report_outcome(false);
        ctrl.evaluate(92_000);
        ctrl.report_outcome(false);

        let action = ctrl.evaluate(50_000);
        assert_eq!(action, HomeostasisAction::None);

        let action = ctrl.evaluate(73_000);
        assert_eq!(action, HomeostasisAction::TrimOutputs);
    }

    #[test]
    fn scenario_emergency_at_95_percent() {
        let mut ctrl = HomeostasisController::new(100_000);
        let action = ctrl.evaluate(96_000);
        assert_eq!(action, HomeostasisAction::EmergencyDrop);
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// 9. SESSION TOKEN SECURITY
// ═══════════════════════════════════════════════════════════════════════════════

mod session_token {
    use lean_ctx::core::session_token::generate_token;

    #[test]
    fn scenario_tokens_are_cryptographically_random() {
        let tokens: Vec<String> = (0..100).map(|_| generate_token()).collect();

        for t in &tokens {
            assert_eq!(t.len(), 64, "Token length should be 64, got {}", t.len());
            assert!(
                t.chars().all(|c| c.is_ascii_hexdigit()),
                "Token should be hex: {t}"
            );
        }

        let unique: std::collections::HashSet<&String> = tokens.iter().collect();
        assert_eq!(unique.len(), 100, "All 100 tokens should be unique");
    }
}
