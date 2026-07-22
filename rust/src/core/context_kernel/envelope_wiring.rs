//! Activation wiring for the kernel evidence pipeline.

use super::{
    accounting_fix, kernel_config, outcome_signal, receipt_chain, token_envelope, usage_normalizer,
};

const PROXY_SOURCE: &str = "proxy";
const MCP_SOURCE: &str = "mcp";

/// Aggregated evidence recorded by the active kernel pipeline.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EvidenceSummary {
    /// Proxy requests represented in the receipt chain.
    pub proxy_requests: usize,
    /// MCP calls represented in the receipt chain.
    pub mcp_calls: usize,
    /// Canonical token envelopes recorded by the usage normalizer.
    pub total_envelopes: usize,
    /// Delivery lifecycles recorded in the receipt chain.
    pub chain_entries: usize,
    /// Aggregate saved-token ratio across recorded envelopes.
    pub compression_ratio: f64,
    /// Fraction of receipt entries supplemented by the kernel.
    pub kernel_hit_rate: f64,
}

/// Records proxy usage and delivery evidence when the kernel is enabled.
pub fn process_proxy_evidence(
    data: &super::proxy_bridge::ProxyRequestData,
    result: &super::proxy_bridge::ProxyKernelResult,
) {
    if !kernel_config::is_enabled() {
        return;
    }

    let envelope = token_envelope::from_proxy_data(data);
    if kernel_config::is_feature_enabled("usage_tracking") {
        usage_normalizer::record_envelope(&envelope);
    }

    let accounting = accounting_fix::compute_honest_accounting(
        data.input_tokens.saturating_add(data.tokens_saved),
        data.input_tokens,
        0,
        0,
    );
    if kernel_config::is_feature_enabled("receipt_chain") {
        receipt_chain::record_chain_entry(
            PROXY_SOURCE,
            envelope,
            accounting,
            result.outcome_signal.outcome,
            false,
            0,
        );
    }

    tracing::trace!(source = PROXY_SOURCE, "processed kernel evidence");
}

/// Records MCP usage and delivery evidence when the kernel is enabled.
pub fn process_mcp_evidence(data: &super::mcp_bridge::McpCallData) {
    if !kernel_config::is_enabled() {
        return;
    }

    let envelope = token_envelope::from_mcp_call(data);
    if kernel_config::is_feature_enabled("usage_tracking") {
        usage_normalizer::record_envelope(&envelope);
    }

    let accounting =
        accounting_fix::compute_honest_accounting(data.input_tokens, data.output_tokens, 0, 0);
    if kernel_config::is_feature_enabled("receipt_chain") {
        let call_number = if data.is_retry {
            data.call_number.max(2)
        } else {
            data.call_number
        };
        let outcome = outcome_signal::infer_outcome(call_number, data.is_retry, data.output_tokens);
        receipt_chain::record_chain_entry(
            MCP_SOURCE,
            envelope,
            accounting,
            outcome.outcome,
            false,
            0,
        );
    }

    tracing::trace!(source = MCP_SOURCE, "processed kernel evidence");
}

/// Returns aggregate usage and receipt evidence for the current process.
#[must_use]
pub fn evidence_summary() -> EvidenceSummary {
    let usage = usage_normalizer::session_usage();
    let compression = usage_normalizer::compression_overview();
    let chain = receipt_chain::chain_summary();
    let entries = receipt_chain::chain_entries();

    EvidenceSummary {
        proxy_requests: entries
            .iter()
            .filter(|entry| entry.source == PROXY_SOURCE)
            .count(),
        mcp_calls: entries
            .iter()
            .filter(|entry| entry.source == MCP_SOURCE)
            .count(),
        total_envelopes: usage.total_requests,
        chain_entries: chain.total_entries,
        compression_ratio: compression.avg_compression_ratio,
        kernel_hit_rate: receipt_chain::kernel_hit_rate(),
    }
}

/// Clears all evidence and restores the kernel feature defaults.
pub fn reset_evidence() {
    usage_normalizer::reset_usage();
    receipt_chain::reset_chain();
    kernel_config::reset_features();
}

#[cfg(test)]
mod tests {
    use std::sync::MutexGuard;

    use super::{
        EvidenceSummary, evidence_summary, process_mcp_evidence, process_proxy_evidence,
        reset_evidence,
    };
    use crate::core::context_kernel::kernel_config::{self, KernelFeatures};
    use crate::core::context_kernel::mcp_bridge::McpCallData;
    use crate::core::context_kernel::proxy_bridge::{self, ProxyRequestData};
    use crate::core::context_kernel::{receipt_chain, usage_normalizer};

    fn isolated() -> MutexGuard<'static, ()> {
        let guard = crate::core::context_kernel::kernel_config::KERNEL_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_evidence();
        crate::core::context_kernel::proxy_bridge::reset_state();
        crate::core::context_kernel::mcp_bridge::reset_mcp_state();
        guard
    }

    fn proxy_data() -> ProxyRequestData {
        ProxyRequestData {
            input_tokens: 100,
            output_tokens: 20,
            tokens_saved: 50,
            request_count: 1,
            ..ProxyRequestData::default()
        }
    }

    fn process_proxy() {
        let data = proxy_data();
        let result = proxy_bridge::process_proxy_request(&data);
        process_proxy_evidence(&data, &result);
    }

    fn mcp_data(number: usize) -> McpCallData {
        McpCallData {
            tool_name: "ctx_read".to_owned(),
            input_tokens: 80,
            output_tokens: 20,
            call_number: number,
            ..McpCallData::default()
        }
    }

    #[test]
    fn proxy_evidence_records_envelope() {
        let _guard = isolated();
        process_proxy();
        assert_eq!(usage_normalizer::session_usage().total_requests, 1);
    }

    #[test]
    fn proxy_evidence_records_chain() {
        let _guard = isolated();
        process_proxy();
        assert!(receipt_chain::chain_length() > 0);
    }

    #[test]
    fn mcp_evidence_records() {
        let _guard = isolated();
        process_mcp_evidence(&mcp_data(1));
        assert_eq!(usage_normalizer::session_usage().total_requests, 1);
    }

    #[test]
    fn disabled_kernel_skips() {
        let _guard = isolated();
        let mut features = KernelFeatures::default();
        features.enabled = false;
        kernel_config::update_features(features);
        process_proxy();
        assert_eq!(evidence_summary(), EvidenceSummary::default());
    }

    #[test]
    fn evidence_summary_aggregates() {
        let _guard = isolated();
        for _ in 0..3 {
            process_proxy();
        }
        for number in 1..=2 {
            process_mcp_evidence(&mcp_data(number));
        }
        let summary = evidence_summary();
        assert_eq!(summary.proxy_requests, 3);
        assert_eq!(summary.mcp_calls, 2);
        assert_eq!(summary.total_envelopes, 5);
        assert_eq!(summary.chain_entries, 5);
    }

    #[test]
    fn reset_clears_all() {
        let _guard = isolated();
        process_proxy();
        process_mcp_evidence(&mcp_data(1));
        reset_evidence();
        crate::core::context_kernel::proxy_bridge::reset_state();
        crate::core::context_kernel::mcp_bridge::reset_mcp_state();
        assert_eq!(evidence_summary(), EvidenceSummary::default());
        assert!(kernel_config::is_enabled());
    }
}
