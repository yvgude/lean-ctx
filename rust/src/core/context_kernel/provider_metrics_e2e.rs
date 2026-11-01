#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::super::envelope_bridge;
    use super::super::provider_parity::{detect_provider, envelope_from_usage};
    use super::super::token_envelope::ProviderKind;
    use super::super::{evidence_wiring, kernel_config, usage_normalizer};

    fn isolated() -> std::sync::MutexGuard<'static, ()> {
        let guard = kernel_config::KERNEL_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        kernel_config::reset_features();
        evidence_wiring::reset();
        usage_normalizer::reset_usage();
        envelope_bridge::reset();
        guard
    }

    #[allow(clippy::needless_pass_by_value)]
    fn record(url: &str, expected: ProviderKind, model: &str, usage: Value) {
        let provider = detect_provider(url);
        assert_eq!(provider, expected);
        let envelope = envelope_from_usage(provider, model, &usage);
        envelope_bridge::record_proxy_envelope(&envelope);
    }

    macro_rules! provider_e2e {
        ($name:ident, $url:expr, $kind:expr, $model:expr, $usage:expr, $totals:expr) => {
            #[test]
            fn $name() {
                let _guard = isolated();
                record($url, $kind, $model, $usage);
                let stats = envelope_bridge::provider_stats();
                assert_eq!(stats.len(), 1);
                assert_eq!(stats[0].provider, $kind);
                assert_eq!(stats[0].request_count, 1);
                assert_eq!((stats[0].total_input, stats[0].total_output), $totals);
            }
        };
    }

    provider_e2e!(
        openai_e2e,
        "https://api.openai.com",
        ProviderKind::OpenAi,
        "gpt-5",
        json!({"prompt_tokens": 100, "completion_tokens": 25}),
        (100, 25)
    );
    provider_e2e!(
        anthropic_e2e,
        "https://api.anthropic.com",
        ProviderKind::Anthropic,
        "claude-sonnet",
        json!({"input_tokens": 80, "output_tokens": 20}),
        (80, 20)
    );
    provider_e2e!(
        bedrock_e2e,
        "https://bedrock-runtime.us-east-1.amazonaws.com",
        ProviderKind::Bedrock,
        "amazon.nova-pro",
        json!({"inputTokens": 70, "outputTokens": 15}),
        (70, 15)
    );
    provider_e2e!(
        azure_e2e,
        "https://tenant.openai.azure.com",
        ProviderKind::Azure,
        "gpt-4.1",
        json!({"prompt_tokens": 60, "completion_tokens": 10}),
        (60, 10)
    );

    #[test]
    fn multi_provider_e2e() {
        let _guard = isolated();
        for _ in 0..3 {
            record(
                "https://api.openai.com",
                ProviderKind::OpenAi,
                "gpt-5",
                json!({"prompt_tokens": 10, "completion_tokens": 2}),
            );
        }
        for _ in 0..2 {
            record(
                "https://api.anthropic.com",
                ProviderKind::Anthropic,
                "claude-sonnet",
                json!({"input_tokens": 20, "output_tokens": 4}),
            );
        }
        let stats = envelope_bridge::provider_stats();
        assert_eq!(stats.len(), 2);
        assert_eq!(
            (stats[0].provider, stats[0].request_count),
            (ProviderKind::OpenAi, 3)
        );
        assert_eq!(
            (stats[1].provider, stats[1].request_count),
            (ProviderKind::Anthropic, 2)
        );
    }

    #[test]
    fn envelope_bridge_records_evidence() {
        let _guard = isolated();
        record(
            "https://api.openai.com",
            ProviderKind::OpenAi,
            "gpt-5",
            json!({
                "prompt_tokens": 40,
                "completion_tokens": 12,
                "completion_tokens_details": {"reasoning_tokens": 7}
            }),
        );
        let summary = evidence_wiring::dispatch_summary();
        assert!(summary.proxy_dispatches >= 1);
        assert!(summary.total_input_tokens >= 40);
        assert!(summary.total_output_tokens >= 12);
        // tokens_saved comes from envelope.tokens_saved, not reasoning
    }

    #[test]
    fn envelope_bridge_records_measured_usage_without_recounting_the_request() {
        let _guard = isolated();
        record(
            "https://api.anthropic.com",
            ProviderKind::Anthropic,
            "claude-sonnet",
            json!({
                "input_tokens": 30,
                "output_tokens": 9,
                "cache_read_input_tokens": 6
            }),
        );
        let stats = envelope_bridge::provider_stats();
        assert_eq!(
            (
                stats[0].total_input,
                stats[0].total_output,
                stats[0].total_cache_read
            ),
            (30, 9, 6)
        );
        // The forward path's Context IR stage already counted this request.
        assert_eq!(usage_normalizer::session_usage().total_requests, 0);
        assert_eq!(super::super::receipt_chain::chain_length(), 0);
        assert_eq!(super::super::proxy_bridge::etpao_summary().total_tokens, 0);
        assert_eq!(
            super::super::proxy_bridge::identity_summary().total_tokens,
            0
        );
    }

    #[test]
    fn reset_isolates() {
        let _guard = isolated();
        record(
            "https://api.openai.com",
            ProviderKind::OpenAi,
            "gpt-5",
            json!({"prompt_tokens": 10, "completion_tokens": 2}),
        );
        envelope_bridge::reset();
        assert!(envelope_bridge::provider_stats().is_empty());
    }
}
