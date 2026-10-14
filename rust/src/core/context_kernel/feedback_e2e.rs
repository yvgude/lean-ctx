#[cfg(test)]
mod tests {
    use crate::core::context_kernel::{
        adaptive_bridge, adaptive_hook, ctx_read_dedup, evidence_wiring, health, health_api,
        kernel_config, response_evidence, startup,
    };
    use crate::tools::search_hook;

    fn isolated() -> std::sync::MutexGuard<'static, ()> {
        let guard = kernel_config::KERNEL_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        kernel_config::reset_features();
        crate::core::context_kernel::dedup_wiring::reset_dedup();
        crate::core::context_kernel::schema_wiring::reset_schema_state();
        crate::core::context_kernel::envelope_wiring::reset_evidence();
        crate::core::context_kernel::proxy_bridge::reset_state();
        crate::core::context_kernel::mcp_bridge::reset_mcp_state();
        startup::reset();
        ctx_read_dedup::reset();
        crate::core::context_kernel::list_tools_opt::reset();
        evidence_wiring::reset();
        adaptive_bridge::reset();
        crate::tools::search_kernel::reset();
        search_hook::reset();
        adaptive_hook::reset();
        response_evidence::reset();
        crate::core::context_kernel::usage_normalizer::reset_usage();
        crate::core::context_kernel::receipt_chain::reset_chain();
        guard
    }

    #[test]
    fn full_feedback_loop() {
        let _guard = isolated();
        startup::initialize();
        search_hook::on_search("query1", "regex", 10, 500);
        search_hook::on_search("query1", "regex", 10, 500);
        assert!(search_hook::maybe_warn_repeat("query1").is_some());
        for _ in 0..3 {
            evidence_wiring::record_from_tool_dispatch("ctx_search", 100, 50, 50);
        }
        response_evidence::record_response("ctx_read", 200, false);
        response_evidence::record_response("ctx_read", 50, true);

        let report = health::kernel_health();
        assert!(report.initialized);
        assert!(report.kernel_enabled);
        assert_eq!(response_evidence::response_summary().total_responses, 2);
        assert_eq!(search_hook::summary().searches_recorded, 2);
    }

    #[test]
    fn adaptive_from_bounce() {
        let _guard = isolated();
        startup::initialize();
        adaptive_bridge::update_bounce_signal(0.5);
        assert!(adaptive_hook::global_advice().should_reduce);
        adaptive_bridge::update_bounce_signal(0.01);
        assert!(!adaptive_hook::global_advice().should_reduce);
    }

    #[test]
    fn response_evidence_tracks() {
        let _guard = isolated();
        for index in 0..5 {
            response_evidence::record_response("tool", 100, index % 2 == 0);
        }
        let summary = response_evidence::response_summary();
        assert_eq!(summary.total_responses, 5);
        assert_eq!(summary.cached_responses, 3);
        assert!(summary.cache_hit_rate > 0.5);
    }

    #[test]
    fn health_dashboard_comprehensive() {
        let _guard = isolated();
        startup::initialize();
        let _ = ctx_read_dedup::try_dedup("a.rs", "content", false);
        let _ = ctx_read_dedup::try_dedup("a.rs", "content", false);
        search_hook::on_search("test", "regex", 5, 200);
        evidence_wiring::record_from_tool_dispatch("ctx_read", 100, 50, 50);
        response_evidence::record_response("ctx_read", 200, true);

        let dashboard = health_api::enhanced_dashboard();
        assert!(dashboard.kernel_health.initialized);
        assert!(dashboard.search.total_searches > 0);
        assert!(dashboard.evidence_dispatch.tool_dispatches > 0);
        assert!(
            !health_api::one_line_status().is_empty(),
            "`health_api::one_line_status()` is empty"
        );
    }

    #[test]
    fn disabled_kernel_all_noop() {
        let _guard = isolated();
        let mut features = kernel_config::features();
        features.enabled = false;
        kernel_config::update_features(features);
        search_hook::on_search("query", "regex", 10, 500);
        response_evidence::record_response("tool", 100, true);

        assert_eq!(search_hook::summary().searches_recorded, 0);
        assert_eq!(response_evidence::response_summary().total_responses, 0);
        assert!(search_hook::maybe_warn_repeat("query").is_none());
        assert!(!health::kernel_health().kernel_enabled);
    }

    #[test]
    fn search_repeat_detection() {
        let _guard = isolated();
        search_hook::on_search("unique1", "regex", 5, 100);
        search_hook::on_search("unique2", "semantic", 3, 200);
        search_hook::on_search("unique1", "regex", 5, 100);

        assert!(search_hook::maybe_warn_repeat("unique1").is_some());
        assert!(search_hook::maybe_warn_repeat("unique2").is_none());
    }
}
