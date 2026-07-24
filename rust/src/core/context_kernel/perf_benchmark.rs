#[cfg(test)]
mod tests {
    use std::sync::MutexGuard;
    use std::time::{Duration, Instant};

    use crate::core::context_kernel::{
        adaptive_bridge, adaptive_hook, ctx_read_dedup, evidence_hook, evidence_wiring, health,
        kernel_config, list_tools_opt, response_evidence, startup, token_envelope::TokenEnvelope,
    };
    use crate::tools::{search_hook, search_kernel};

    /// Wall-clock budgets below only hold on a quiet, single-threaded runner.
    /// The suite runs multi-threaded, so they are opt-in: CI's perf-gate step
    /// sets `LEAN_CTX_PERF_GATE=1` and runs them serialized. The work itself
    /// (and every correctness assertion) still runs on every invocation.
    fn timing_enforced() -> bool {
        std::env::var("LEAN_CTX_PERF_GATE").as_deref() == Ok("1")
    }

    fn isolated() -> MutexGuard<'static, ()> {
        let guard = kernel_config::KERNEL_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        kernel_config::reset_features();
        crate::core::context_kernel::dedup_wiring::reset_dedup();
        crate::core::context_kernel::schema_wiring::reset_schema_state();
        crate::core::context_kernel::envelope_wiring::reset_evidence();
        crate::core::context_kernel::proxy_bridge::reset_state();
        crate::core::context_kernel::mcp_bridge::reset_mcp_state();
        evidence_wiring::reset();
        adaptive_bridge::reset();
        search_kernel::reset();
        crate::core::context_kernel::usage_normalizer::reset_usage();
        crate::core::context_kernel::receipt_chain::reset_chain();
        response_evidence::reset();
        search_hook::reset();
        adaptive_hook::reset();
        startup::reset();
        ctx_read_dedup::reset();
        list_tools_opt::reset();
        guard
    }

    #[test]
    fn dedup_throughput() {
        let _guard = isolated();
        let started = Instant::now();
        for index in 0..1_000 {
            let content = format!("unique content {index}");
            assert!(ctx_read_dedup::try_dedup("bench.rs", &content).is_none());
        }
        assert!(!timing_enforced() || started.elapsed() < Duration::from_millis(500));
    }

    #[test]
    fn evidence_throughput() {
        let _guard = isolated();
        let started = Instant::now();
        for _ in 0..10_000 {
            evidence_hook::record_tool_call("ctx_read", 100, 25);
        }
        assert!(!timing_enforced() || started.elapsed() < Duration::from_millis(200));
        assert_eq!(evidence_hook::evidence_report().tool_calls, 10_000);
    }

    #[test]
    fn health_latency() {
        let _guard = isolated();
        let started = Instant::now();
        for _ in 0..100 {
            std::hint::black_box(health::kernel_health());
        }
        assert!(!timing_enforced() || started.elapsed() < Duration::from_millis(50));
    }

    #[test]
    fn search_dedup_scales() {
        let _guard = isolated();
        let started = Instant::now();
        for index in 0..500 {
            search_kernel::record_search(&format!("query-{index}"), 5, 100);
        }
        for index in 0..100 {
            search_kernel::record_search(&format!("query-{index}"), 5, 100);
        }
        let summary = search_kernel::search_summary();
        assert!(!timing_enforced() || started.elapsed() < Duration::from_millis(200));
        assert_eq!(summary.total_searches, 600);
        assert_eq!(summary.unique_queries, 500);
        assert_eq!(summary.repeated_queries, 100);
    }

    #[test]
    fn envelope_creation_speed() {
        let _guard = isolated();
        let started = Instant::now();
        let mut token_total = 0;
        for index in 0..5_000 {
            let envelope = TokenEnvelope {
                model: "benchmark-model".to_owned(),
                input_tokens: index,
                output_tokens: 10,
                cache_read_tokens: 5,
                ..TokenEnvelope::default()
            };
            token_total += std::hint::black_box(envelope).input_tokens;
        }
        assert!(!timing_enforced() || started.elapsed() < Duration::from_millis(50));
        assert_eq!(token_total, 12_497_500);
    }

    #[test]
    fn concurrent_evidence_safe() {
        let _guard = isolated();
        let threads: Vec<_> = (0..4)
            .map(|_| {
                std::thread::spawn(|| {
                    for _ in 0..1_000 {
                        evidence_hook::record_tool_call("ctx_shell", 20, 10);
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().expect("evidence worker panicked");
        }
        assert_eq!(evidence_hook::evidence_report().tool_calls, 4_000);
    }

    #[test]
    fn schema_opt_throughput() {
        let _guard = isolated();
        let tools: Vec<_> = (0..20)
            .map(|index| {
                (
                    format!("tool-{index}"),
                    "Read files and return complete file contents with metadata".to_owned(),
                    0_usize,
                )
            })
            .collect();
        let started = Instant::now();
        for _ in 0..100 {
            std::hint::black_box(list_tools_opt::optimize_descriptions(
                tools.clone(),
                "cursor",
            ));
        }
        assert!(!timing_enforced() || started.elapsed() < Duration::from_millis(500));
    }
}
