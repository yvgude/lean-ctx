#[cfg(test)]
mod tests {
    use std::sync::MutexGuard;

    use super::super::{
        adaptive_bridge, ctx_read_dedup, dedup_wiring, envelope_wiring, evidence_wiring, health,
        kernel_config, list_tools_opt, startup,
    };
    use crate::tools::search_kernel;

    fn isolated() -> MutexGuard<'static, ()> {
        let guard = kernel_config::KERNEL_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        kernel_config::reset_features();
        dedup_wiring::reset_dedup();
        super::super::schema_wiring::reset_schema_state();
        envelope_wiring::reset_evidence();
        super::super::proxy_bridge::reset_state();
        super::super::mcp_bridge::reset_mcp_state();
        startup::reset();
        ctx_read_dedup::reset();
        list_tools_opt::reset();
        evidence_wiring::reset();
        adaptive_bridge::reset();
        search_kernel::reset();
        super::super::usage_normalizer::reset_usage();
        super::super::receipt_chain::reset_chain();
        guard
    }

    fn tools(count: usize) -> Vec<(String, String, usize)> {
        (0..count)
            .map(|index| (format!("tool_{index}"), "description that is very detailed and thorough and explains every aspect of the tool functionality ".repeat(50), 3))
            .collect()
    }

    #[test]
    fn full_wiring_lifecycle() {
        let _guard = isolated();
        startup::initialize();

        assert_eq!(ctx_read_dedup::try_dedup("a.rs", "alpha", false), None);
        assert_eq!(ctx_read_dedup::try_dedup("b.rs", "beta", false), None);
        assert!(ctx_read_dedup::try_dedup("a.rs", "alpha", false).is_some());
        assert!(ctx_read_dedup::try_dedup("b.rs", "beta", false).is_some());
        let _ = list_tools_opt::optimize_descriptions(tools(10), "cursor");
        for index in 0..3 {
            evidence_wiring::record_from_tool_dispatch(&format!("tool_{index}"), 100, 40, 60);
        }
        for _ in 0..2 {
            evidence_wiring::record_from_proxy_dispatch(
                200,
                80,
                120,
                Some("gpt-5"),
                Some("openai"),
            );
        }

        let report = health::kernel_health();
        assert!(report.initialized);
        assert!(report.evidence_total_envelopes >= 5);
        assert!(dedup_wiring::dedup_stats().cache_hits >= 1);
    }

    #[test]
    fn evidence_records_all_sources() {
        let _guard = isolated();
        for index in 0..3 {
            evidence_wiring::record_from_tool_dispatch(&format!("tool_{index}"), 100, 40, 60);
        }
        for _ in 0..2 {
            evidence_wiring::record_from_proxy_dispatch(200, 80, 120, None, None);
        }
        search_kernel::record_search("query1", 10, 500);
        search_kernel::record_search("query2", 8, 400);

        let dispatch = evidence_wiring::dispatch_summary();
        assert_eq!(dispatch.tool_dispatches, 3);
        assert_eq!(dispatch.proxy_dispatches, 2);
        assert_eq!(search_kernel::search_summary().total_searches, 2);
    }

    #[test]
    fn adaptive_responds_to_bounce_rate() {
        let _guard = isolated();
        adaptive_bridge::update_bounce_signal(0.5);
        assert_eq!(
            adaptive_bridge::compression_advice(0.5),
            adaptive_bridge::KernelCompressionAdvice::Reduce
        );
        adaptive_bridge::update_bounce_signal(0.01);
        assert_eq!(
            adaptive_bridge::compression_advice(0.01),
            adaptive_bridge::KernelCompressionAdvice::Increase
        );
    }

    #[test]
    fn health_report_comprehensive() {
        let _guard = isolated();
        startup::initialize();
        assert_eq!(
            ctx_read_dedup::try_dedup("health.rs", "content", false),
            None
        );
        assert!(ctx_read_dedup::try_dedup("health.rs", "content", false).is_some());
        let _ = list_tools_opt::optimize_descriptions(tools(10), "cursor");
        evidence_wiring::record_from_tool_dispatch("ctx_read", 100, 40, 60);

        let report = health::kernel_health();
        assert!(report.kernel_enabled);
        assert!(report.initialized);
        assert!(report.evidence_total_envelopes > 0);
        assert!(report.dedup_total_checks >= 2);
        assert!(report.dedup_hit_rate > 0.0);
        assert!(report.schema_optimizations > 0);
        assert!(report.schema_tokens_saved > 0);
        assert!(report.evidence_chain_entries > 0);
        assert!(
            !report.config_source.is_empty(),
            "`report.config_source` is empty"
        );
        assert!(report.subsystem_count >= 6);
        assert!(health::is_healthy());
    }

    #[test]
    fn disabled_kernel_safe() {
        let _guard = isolated();
        startup::initialize();
        let features = kernel_config::KernelFeatures {
            enabled: false,
            ..kernel_config::KernelFeatures::default()
        };
        kernel_config::update_features(features);
        let original = tools(10);

        // Initialize first (loads config), then disable kernel.
        assert_eq!(
            ctx_read_dedup::try_dedup("disabled.rs", "content", false),
            None
        );
        assert_eq!(
            list_tools_opt::optimize_descriptions(original.clone(), "cursor"),
            original
        );
        evidence_wiring::record_from_tool_dispatch("ctx_read", 100, 40, 60);
        evidence_wiring::record_from_proxy_dispatch(200, 80, 120, None, None);
        search_kernel::record_search("disabled", 10, 500);

        assert_eq!(
            super::super::evidence_hook::evidence_report(),
            super::super::evidence_hook::EvidenceReport::default()
        );
        assert_eq!(search_kernel::search_summary().total_searches, 0);
        assert_eq!(
            adaptive_bridge::compression_advice(0.5),
            adaptive_bridge::KernelCompressionAdvice::Maintain
        );
        assert!(!health::kernel_health().kernel_enabled);
    }

    #[test]
    fn search_dedup_detects_repeats() {
        let _guard = isolated();
        search_kernel::record_search("query1", 10, 500);
        search_kernel::record_search("query1", 10, 500);

        assert!(search_kernel::is_repeated_query("query1"));
        assert!(!search_kernel::is_repeated_query("query2"));
    }
}
