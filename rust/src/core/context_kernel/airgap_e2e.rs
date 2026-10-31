#[cfg(test)]
mod tests {
    use crate::core::context_kernel::{
        adaptive_bridge, ctx_read_dedup, envelope_wiring, evidence_hook, evidence_wiring, health,
        kernel_config, response_evidence, startup,
    };
    use crate::tools::search_kernel;

    fn isolated() -> std::sync::MutexGuard<'static, ()> {
        let guard = kernel_config::KERNEL_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        kernel_config::reset_features();
        crate::core::context_kernel::dedup_wiring::reset_dedup();
        crate::core::context_kernel::schema_wiring::reset_schema_state();
        envelope_wiring::reset_evidence();
        crate::core::context_kernel::proxy_bridge::reset_state();
        crate::core::context_kernel::mcp_bridge::reset_mcp_state();
        evidence_wiring::reset();
        adaptive_bridge::reset();
        search_kernel::reset();
        crate::core::context_kernel::usage_normalizer::reset_usage();
        crate::core::context_kernel::receipt_chain::reset_chain();
        response_evidence::reset();
        crate::tools::search_hook::reset();
        startup::reset();
        guard
    }

    #[test]
    fn kernel_init_offline() {
        let _guard = isolated();
        startup::initialize();
        assert!(startup::is_initialized());
    }

    #[test]
    fn dedup_works_offline() {
        let _guard = isolated();
        assert!(ctx_read_dedup::try_dedup("offline.rs", "local content", false).is_none());
        assert!(ctx_read_dedup::try_dedup("offline.rs", "local content", false).is_some());
    }

    #[test]
    fn search_evidence_offline() {
        let _guard = isolated();
        search_kernel::record_search("offline query", 3, 21);
        let summary = search_kernel::search_summary();
        assert_eq!(summary.total_searches, 1);
        assert_eq!(summary.unique_queries, 1);
        assert_eq!(summary.total_tokens, 21);
    }

    #[test]
    fn evidence_recording_offline() {
        let _guard = isolated();
        evidence_hook::record_tool_call("ctx_read", 13, 5);
        let report = evidence_hook::evidence_report();
        assert_eq!(report.tool_calls, 1);
        assert_eq!(report.total_input_tokens, 13);
        assert_eq!(report.total_output_tokens, 5);
    }

    #[test]
    fn health_report_offline() {
        let _guard = isolated();
        startup::initialize();
        let report = health::kernel_health();
        assert!(report.initialized);
        assert!(report.kernel_enabled);
        assert!(report.subsystem_count > 0);
        assert!(health::is_healthy());
        assert!(health::format_health().contains("Kernel: ON"));
    }

    #[test]
    fn adaptive_bridge_offline() {
        let _guard = isolated();
        adaptive_bridge::update_bounce_signal(0.5);
        let summary = adaptive_bridge::adaptive_summary();
        assert_eq!(summary.signals_received, 1);
        assert_eq!(
            adaptive_bridge::compression_advice(summary.current_bounce_rate),
            adaptive_bridge::KernelCompressionAdvice::Reduce
        );
    }

    #[test]
    fn config_loads_defaults() {
        let _guard = isolated();
        let features = kernel_config::features();
        assert!(features.enabled);
        assert!(features.content_dedup);
        assert!(features.schema_optimization);
        assert_eq!(features.max_kernel_budget, 150);
        assert_eq!(features.dedup_capacity, 1024);
    }

    #[test]
    fn envelope_wiring_offline() {
        let _guard = isolated();
        evidence_wiring::record_from_tool_dispatch("ctx_search", 17, 7, 10);
        let summary = evidence_wiring::dispatch_summary();
        assert_eq!(summary.tool_dispatches, 1);
        assert_eq!(summary.total_input_tokens, 17);
        assert_eq!(summary.total_output_tokens, 7);
        assert_eq!(summary.total_tokens_saved, 10);
    }

    #[test]
    fn response_evidence_offline() {
        let _guard = isolated();
        response_evidence::record_response("ctx_read", 8, true);
        let summary = response_evidence::response_summary();
        assert_eq!(summary.total_responses, 1);
        assert_eq!(summary.total_output_tokens, 8);
        assert_eq!(summary.cached_responses, 1);
        assert_eq!(summary.cache_hit_rate, 1.0);
    }
}
