#[cfg(test)]
mod tests {
    use std::sync::MutexGuard;

    use super::super::dedup_wiring::{self, DedupAction};
    use super::super::envelope_wiring;
    use super::super::kernel_config::{self, KernelFeatures};
    use super::super::live_dashboard;
    use super::super::mcp_bridge::{self, McpCallData};
    use super::super::proxy_bridge::{self, ProxyRequestData};
    use super::super::receipt_chain;
    use super::super::schema_wiring;
    use super::super::usage_normalizer;

    fn reset_all() -> MutexGuard<'static, ()> {
        let guard = crate::core::context_kernel::kernel_config::KERNEL_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        kernel_config::reset_features();
        proxy_bridge::reset_state();
        mcp_bridge::reset_mcp_state();
        usage_normalizer::reset_usage();
        receipt_chain::reset_chain();
        dedup_wiring::reset_dedup();
        schema_wiring::reset_schema_state();
        guard
    }

    fn proxy_request(index: usize) -> ProxyRequestData {
        ProxyRequestData {
            headers: vec![("x-user-id".to_owned(), format!("activation-{index}"))],
            input_tokens: 100 + index,
            output_tokens: 20,
            reasoning_tokens: 5,
            tokens_saved: 30,
            model: Some("gpt-5".to_owned()),
            provider: Some("openai".to_owned()),
            request_count: 1,
            ..ProxyRequestData::default()
        }
    }

    fn mcp_call(index: usize) -> McpCallData {
        McpCallData {
            tool_name: format!("ctx_tool_{index}"),
            input_tokens: 60 + index,
            output_tokens: 10,
            call_number: index + 1,
            ..McpCallData::default()
        }
    }

    fn process_proxy_requests(count: usize) {
        for index in 0..count {
            let data = proxy_request(index);
            let result = proxy_bridge::process_proxy_request(&data);
            envelope_wiring::process_proxy_evidence(&data, &result);
        }
    }

    fn mixed_requests(count: usize) {
        for index in 0..count {
            if index % 2 == 0 {
                let data = proxy_request(index);
                let result = proxy_bridge::process_proxy_request(&data);
                envelope_wiring::process_proxy_evidence(&data, &result);
            } else {
                envelope_wiring::process_mcp_evidence(&mcp_call(index));
            }
        }
    }

    fn schemas() -> Vec<(String, String, usize)> {
        (0..15)
            .map(|index| {
                (
                    format!("tool_{index}"),
                    format!(
                        "Tool {index} provides detailed contextual analysis. {}",
                        "This deliberately verbose schema description exercises budget reduction. "
                            .repeat(60)
                    ),
                    8,
                )
            })
            .collect()
    }

    #[test]
    fn full_kernel_activation_pipeline() {
        let _guard = reset_all();

        process_proxy_requests(3);
        for index in 0..2 {
            envelope_wiring::process_mcp_evidence(&mcp_call(index));
        }

        assert!(usage_normalizer::session_usage().total_requests >= 5);
        assert!(receipt_chain::chain_length() >= 5);
        assert!(live_dashboard::snapshot().usage.total_requests >= 5);
    }

    #[test]
    fn kernel_disabled_no_recording() {
        let _guard = reset_all();
        let mut features = KernelFeatures::default();
        features.enabled = false;
        kernel_config::update_features(features);

        process_proxy_requests(5);

        assert_eq!(usage_normalizer::session_usage().total_requests, 0);
        assert_eq!(receipt_chain::chain_length(), 0);
    }

    #[test]
    fn schema_optimization_pipeline() {
        let _guard = reset_all();
        let entries = schemas();

        let cursor = schema_wiring::optimize_tool_list(&entries, "cursor");
        let unknown = schema_wiring::optimize_tool_list(&entries, "unknown");

        assert!(cursor.tokens_after < cursor.tokens_before);
        assert!(unknown.tokens_after < unknown.tokens_before);
        assert!(cursor.tokens_after > unknown.tokens_after);
    }

    #[test]
    fn dedup_pipeline() {
        let _guard = reset_all();

        assert_eq!(
            dedup_wiring::check_content("file.rs", "fn main() {}"),
            DedupAction::DeliverFull
        );
        assert!(matches!(
            dedup_wiring::check_content("file.rs", "fn main() {}"),
            DedupAction::DeliverStub { .. }
        ));
        assert!(matches!(
            dedup_wiring::check_content("file.rs", "fn main() { work(); }"),
            DedupAction::DeliverModified | DedupAction::DeliverFull
        ));
        assert!(dedup_wiring::dedup_stats().hit_rate > 0.0);
    }

    #[test]
    fn selective_feature_disable() {
        let _guard = reset_all();
        let mut features = KernelFeatures::default();
        features.receipt_chain = false;
        kernel_config::update_features(features);

        process_proxy_requests(3);

        assert_eq!(usage_normalizer::session_usage().total_requests, 3);
        assert_eq!(receipt_chain::chain_length(), 0);
    }

    #[test]
    fn evidence_summary_matches_dashboard() {
        let _guard = reset_all();
        mixed_requests(10);

        let evidence = envelope_wiring::evidence_summary();
        let dashboard = live_dashboard::snapshot();
        assert_eq!(evidence.total_envelopes, dashboard.usage.total_requests);
    }

    #[test]
    fn config_max_budget_respected() {
        let _guard = reset_all();
        let mut features = KernelFeatures::default();
        features.max_kernel_budget = 50;
        kernel_config::update_features(features);

        assert_eq!(kernel_config::features().max_kernel_budget, 50);
    }

    #[test]
    fn dedup_disabled_always_full() {
        let _guard = reset_all();
        let mut features = KernelFeatures::default();
        features.content_dedup = false;
        kernel_config::update_features(features);

        for _ in 0..10 {
            assert_eq!(
                dedup_wiring::check_content("file.rs", "fn main() {}"),
                DedupAction::DeliverFull
            );
        }
        assert_eq!(dedup_wiring::dedup_stats().cache_hits, 0);
    }
}
