//! End-to-end conformance tests for the R25 evidence pipeline.

#[cfg(test)]
mod tests {
    use std::sync::MutexGuard;

    use super::super::accounting_fix;
    use super::super::live_dashboard;
    use super::super::mcp_bridge::{self, McpCallData};
    use super::super::proxy_bridge::{self, ProxyRequestData};
    use super::super::receipt_chain;
    use super::super::token_envelope::{self, ProviderKind, TokenEnvelope};
    use super::super::types::ReceiptOutcome;
    use super::super::usage_normalizer;

    fn isolated_test() -> MutexGuard<'static, ()> {
        let guard = crate::core::context_kernel::kernel_config::KERNEL_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        proxy_bridge::reset_state();
        mcp_bridge::reset_mcp_state();
        usage_normalizer::reset_usage();
        receipt_chain::reset_chain();
        crate::core::context_kernel::kernel_config::reset_features();
        guard
    }

    fn proxy_request(model: &str, input_tokens: usize, tokens_saved: usize) -> ProxyRequestData {
        ProxyRequestData {
            headers: vec![("x-user-id".to_owned(), "e2e-user".to_owned())],
            input_tokens,
            output_tokens: 100,
            reasoning_tokens: 25,
            tokens_saved,
            model: Some(model.to_owned()),
            provider: Some("OpenAI".to_owned()),
            request_count: 1,
            ..ProxyRequestData::default()
        }
    }

    fn mcp_call(tool_name: &str) -> McpCallData {
        McpCallData {
            tool_name: tool_name.to_owned(),
            input_tokens: 400,
            output_tokens: 80,
            is_retry: false,
            call_number: 1,
        }
    }

    fn accounting() -> accounting_fix::PostDeliveryAccounting {
        accounting_fix::compute_honest_accounting(1_000, 700, 50, 20)
    }

    fn record_receipt(envelope: &TokenEnvelope, outcome: ReceiptOutcome, kernel_hit: bool) {
        receipt_chain::record_chain_entry(
            "e2e",
            envelope.clone(),
            accounting(),
            outcome,
            kernel_hit,
            usize::from(kernel_hit) * 50,
        );
    }

    #[test]
    fn proxy_to_envelope_to_normalizer() {
        let _guard = isolated_test();
        let request = proxy_request("gpt-5", 1_000, 300);

        let envelope = token_envelope::from_proxy_data(&request);
        assert_eq!(envelope.provider, ProviderKind::OpenAi);

        usage_normalizer::record_envelope(&envelope);
        assert_eq!(usage_normalizer::session_usage().total_requests, 1);
        assert!(
            usage_normalizer::model_breakdown()
                .iter()
                .any(|(model, _)| model == "gpt-5")
        );
    }

    #[test]
    fn mcp_to_envelope_pipeline() {
        let _guard = isolated_test();
        let call = mcp_call("ctx_read");

        let envelope = token_envelope::from_mcp_call(&call);
        assert_eq!(envelope.provider, ProviderKind::Unknown);
        assert!(envelope.model.is_empty(), "MCP calls have no model");

        usage_normalizer::record_envelope(&envelope);
        assert_eq!(usage_normalizer::session_usage().total_requests, 1);
    }

    #[test]
    fn full_receipt_chain() {
        let _guard = isolated_test();
        for index in 0..5 {
            let envelope = token_envelope::from_proxy_data(&proxy_request("gpt-5", 1_000, 300));
            let accepted = index < 3;
            let outcome = if accepted {
                ReceiptOutcome::Accepted
            } else {
                ReceiptOutcome::Rejected
            };
            record_receipt(&envelope, outcome, accepted);
        }

        let summary = receipt_chain::chain_summary();
        assert_eq!(summary.total_entries, 5);
        assert_eq!(summary.accepted, 3);
        assert_eq!(summary.rejected, 2);
        let kernel_hit_rate = summary.kernel_supplemented as f64 / summary.total_entries as f64;
        assert!((kernel_hit_rate - 0.6).abs() < f64::EPSILON);
    }

    #[test]
    fn dashboard_aggregates_all() {
        let _guard = isolated_test();
        for _ in 0..3 {
            let request = proxy_request("gpt-5", 1_000, 300);
            let _ = proxy_bridge::process_proxy_request(&request);
            let envelope = token_envelope::from_proxy_data(&request);
            usage_normalizer::record_envelope(&envelope);
            record_receipt(&envelope, ReceiptOutcome::Accepted, true);
        }
        for tool in ["ctx_read", "ctx_search"] {
            let call = mcp_call(tool);
            mcp_bridge::record_mcp_call(&call);
            let envelope = token_envelope::from_mcp_call(&call);
            usage_normalizer::record_envelope(&envelope);
            record_receipt(&envelope, ReceiptOutcome::Accepted, true);
        }

        let snapshot = live_dashboard::snapshot();
        assert!(snapshot.proxy_etpao > 0.0);
        assert_eq!(snapshot.usage.total_requests, 5);
        assert_eq!(snapshot.chain.total_entries, 5);
    }

    #[test]
    fn compression_overview_accurate() {
        let _guard = isolated_test();
        for _ in 0..5 {
            usage_normalizer::record_envelope(&token_envelope::from_proxy_data(&proxy_request(
                "gpt-5-mini",
                1_000,
                100,
            )));
            usage_normalizer::record_envelope(&token_envelope::from_proxy_data(&proxy_request(
                "gpt-5", 1_000, 500,
            )));
        }

        let overview = usage_normalizer::compression_overview();
        let expected = 3_000.0 / 13_000.0;
        assert!((overview.avg_compression_ratio - expected).abs() < 1.0e-9);
        assert_eq!(overview.best_model.as_deref(), Some("gpt-5"));
    }

    #[test]
    fn receipt_chain_honest_accounting() {
        let _guard = isolated_test();
        let envelope = token_envelope::from_proxy_data(&proxy_request("gpt-5", 1_000, 300));
        let accounting = accounting_fix::compute_honest_accounting(1_000, 600, 250, 100);
        receipt_chain::record_chain_entry(
            "proxy",
            envelope,
            accounting,
            ReceiptOutcome::Accepted,
            true,
            250,
        );

        assert!(receipt_chain::chain_summary().total_phantom_savings_pct > 0.0);
    }

    #[test]
    fn end_to_end_evidence_pipeline() {
        let _guard = isolated_test();
        let request = proxy_request("gpt-5", 1_000, 300);
        let _ = proxy_bridge::process_proxy_request(&request);
        let envelope = token_envelope::from_proxy_data(&request);
        usage_normalizer::record_envelope(&envelope);
        record_receipt(&envelope, ReceiptOutcome::Accepted, true);

        let snapshot = live_dashboard::snapshot();
        assert_eq!(snapshot.usage.total_requests, 1);
        assert_eq!(snapshot.usage.total_tokens, envelope.total_tokens());
        assert_eq!(snapshot.chain.total_entries, 1);
        assert_eq!(snapshot.chain.accepted, 1);
        assert!(snapshot.proxy_etpao > 0.0);
    }

    #[test]
    fn envelope_merge_is_additive() {
        let _guard = isolated_test();
        let envelopes = [
            token_envelope::from_proxy_data(&proxy_request("gpt-5", 100, 10)),
            token_envelope::from_proxy_data(&proxy_request("gpt-5", 200, 20)),
            token_envelope::from_proxy_data(&proxy_request("gpt-5", 300, 30)),
        ];
        let expected = envelopes
            .iter()
            .map(TokenEnvelope::total_tokens)
            .sum::<usize>();

        let merged = TokenEnvelope::merge(&envelopes);
        assert_eq!(merged.total_tokens(), expected);
    }
}
