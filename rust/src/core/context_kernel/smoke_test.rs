#[cfg(test)]
mod tests {
    use std::sync::MutexGuard;

    use crate::core::context_kernel::{
        ctx_read_dedup, envelope_wiring, kernel_config, list_tools_opt, live_dashboard, mcp_bridge,
        proxy_bridge, startup,
    };

    fn isolated() -> MutexGuard<'static, ()> {
        let guard = kernel_config::KERNEL_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        kernel_config::reset_features();
        crate::core::context_kernel::dedup_wiring::reset_dedup();
        crate::core::context_kernel::schema_wiring::reset_schema_state();
        envelope_wiring::reset_evidence();
        proxy_bridge::reset_state();
        mcp_bridge::reset_mcp_state();
        startup::reset();
        crate::core::context_kernel::ctx_read_dedup::reset();
        list_tools_opt::reset();
        guard
    }

    fn long_content() -> String {
        "pub fn wired_context() -> &'static str { \"context\" }\n".repeat(100)
    }

    fn tools() -> Vec<(String, String, usize)> {
        (0..10)
            .map(|index| (format!("tool_{index}"), "d".repeat(4_000), 1_000))
            .collect()
    }

    fn proxy_data() -> proxy_bridge::ProxyRequestData {
        proxy_bridge::ProxyRequestData {
            input_tokens: 100,
            output_tokens: 20,
            tokens_saved: 40,
            request_count: 1,
            ..proxy_bridge::ProxyRequestData::default()
        }
    }

    fn mcp_data(call_number: usize) -> mcp_bridge::McpCallData {
        mcp_bridge::McpCallData {
            tool_name: "ctx_read".to_owned(),
            input_tokens: 80,
            output_tokens: 20,
            call_number,
            ..mcp_bridge::McpCallData::default()
        }
    }

    fn process_proxy() {
        let data = proxy_data();
        let result = proxy_bridge::process_proxy_request(&data);
        envelope_wiring::process_proxy_evidence(&data, &result);
    }

    fn process_mcp(call_number: usize) {
        let data = mcp_data(call_number);
        mcp_bridge::record_mcp_call(&data);
        envelope_wiring::process_mcp_evidence(&data);
    }

    #[test]
    fn startup_initializes_kernel() {
        let _guard = isolated();

        startup::initialize();

        assert!(startup::is_initialized());
        assert!(kernel_config::is_enabled());
    }

    #[test]
    fn dedup_hook_detects_repeated_content() {
        let _guard = isolated();
        let content = long_content();

        assert_eq!(ctx_read_dedup::try_dedup("a.rs", &content, false), None);
        let stub = ctx_read_dedup::try_dedup("a.rs", &content, false)
            .expect("repeated content should produce a deduplication stub");

        assert!(stub.len() < content.len());
    }

    #[test]
    fn schema_opt_works_for_cursor() {
        let _guard = isolated();
        let original = tools();
        let before: usize = original.iter().map(|tool| tool.1.len()).sum();

        let optimized = list_tools_opt::optimize_descriptions(original, "cursor");
        let after: usize = optimized.iter().map(|tool| tool.1.len()).sum();

        assert!(optimized.len() <= 10, "should not exceed original count");
        assert!(after < before);
        assert_eq!(
            list_tools_opt::schema_opt_summary().optimizations_applied,
            1
        );
    }

    #[test]
    fn kernel_disabled_all_hooks_noop() {
        let _guard = isolated();
        let features = kernel_config::KernelFeatures {
            enabled: false,
            ..kernel_config::KernelFeatures::default()
        };
        kernel_config::update_features(features);
        let content = long_content();

        assert_eq!(
            ctx_read_dedup::try_dedup("disabled.rs", &content, false),
            None
        );
        assert_eq!(
            ctx_read_dedup::try_dedup("disabled.rs", &content, false),
            None
        );
        assert!(!list_tools_opt::should_optimize_for_client("cursor"));
        process_proxy();
        process_mcp(1);
        assert_eq!(
            envelope_wiring::evidence_summary(),
            envelope_wiring::EvidenceSummary::default()
        );
    }

    #[test]
    fn evidence_records_after_activity() {
        let _guard = isolated();

        for _ in 0..3 {
            process_proxy();
        }
        for call_number in 1..=2 {
            process_mcp(call_number);
        }

        let summary = envelope_wiring::evidence_summary();
        assert_eq!(summary.proxy_requests, 3);
        assert_eq!(summary.mcp_calls, 2);
        assert_eq!(summary.total_envelopes, 5);
        assert_eq!(summary.chain_entries, 5);
    }

    #[test]
    fn full_lifecycle() {
        let _guard = isolated();
        startup::initialize();
        let content = long_content();

        assert_eq!(
            ctx_read_dedup::try_dedup("lifecycle.rs", &content, false),
            None
        );
        assert!(ctx_read_dedup::try_dedup("lifecycle.rs", &content, false).is_some());
        let optimized = list_tools_opt::optimize_descriptions(tools(), "cursor");
        process_proxy();
        process_mcp(1);

        let dashboard = live_dashboard::snapshot();
        assert!(optimized.len() <= 10, "should not exceed original count");
        assert!(dashboard.usage.total_requests > 0);
        assert!(dashboard.chain.total_entries > 0);
        assert!(
            !live_dashboard::format_summary().is_empty(),
            "`live_dashboard::format_summary()` is empty"
        );
    }
}
