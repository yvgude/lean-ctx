//! Bridges canonical provider usage into kernel evidence and aggregate statistics.

use std::cmp::Reverse;
use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, OnceLock};

use serde::Serialize;

use super::token_envelope::{ProviderKind, TokenEnvelope};
use super::{evidence_wiring, kernel_config};

/// Aggregate request statistics for one provider.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ProviderStat {
    /// Provider represented by this aggregate.
    pub provider: ProviderKind,
    /// Number of recorded requests.
    pub request_count: usize,
    /// Total input tokens across requests.
    pub total_input: usize,
    /// Total output tokens across requests.
    pub total_output: usize,
    /// Total cache-read tokens across requests.
    pub total_cache_read: usize,
    /// Mean input tokens per request.
    pub avg_input: usize,
}

#[derive(Debug, Default)]
struct ProviderAccum {
    count: usize,
    sum_input: usize,
    sum_output: usize,
    sum_cache_read: usize,
}

static PROVIDER_STATS: OnceLock<Mutex<HashMap<ProviderKind, ProviderAccum>>> = OnceLock::new();

fn stats_guard() -> MutexGuard<'static, HashMap<ProviderKind, ProviderAccum>> {
    PROVIDER_STATS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn record_stats(envelope: &TokenEnvelope) {
    let mut stats = stats_guard();
    let accum = stats.entry(envelope.provider).or_default();
    accum.count = accum.count.saturating_add(1);
    accum.sum_input = accum.sum_input.saturating_add(envelope.input_tokens);
    accum.sum_output = accum.sum_output.saturating_add(envelope.output_tokens);
    accum.sum_cache_read = accum
        .sum_cache_read
        .saturating_add(envelope.cache_read_tokens);
}

/// Records one measured proxy envelope in the dispatch counters and provider statistics.
///
/// The request itself enters the usage normalizer, the receipt chain, the identity
/// ledger and ETPAO exactly once, from the forward path's Context IR stage
/// (`envelope_wiring::process_proxy_evidence`); recording it here again counted
/// every proxied request two or three times.
pub fn record_proxy_envelope(envelope: &TokenEnvelope) {
    if kernel_config::is_enabled() {
        evidence_wiring::record_from_proxy_dispatch(
            envelope.input_tokens,
            envelope.output_tokens,
            envelope.tokens_saved,
        );
    }
    record_stats(envelope);
}

/// Records one MCP envelope in enabled kernel pipelines and provider statistics.
///
/// `record_from_tool_dispatch` already records the call in the usage normalizer.
pub fn record_mcp_envelope(tool_name: &str, envelope: &TokenEnvelope) {
    if kernel_config::is_enabled() {
        evidence_wiring::record_from_tool_dispatch(
            tool_name,
            envelope.input_tokens,
            envelope.output_tokens,
            envelope.tokens_saved,
        );
    }
    record_stats(envelope);
}

/// Returns provider aggregates sorted by descending request count.
#[must_use]
pub fn provider_stats() -> Vec<ProviderStat> {
    let mut stats = stats_guard()
        .iter()
        .map(|(provider, accum)| ProviderStat {
            provider: *provider,
            request_count: accum.count,
            total_input: accum.sum_input,
            total_output: accum.sum_output,
            total_cache_read: accum.sum_cache_read,
            avg_input: accum.sum_input.checked_div(accum.count).unwrap_or_default(),
        })
        .collect::<Vec<_>>();
    stats.sort_unstable_by_key(|stat| (Reverse(stat.request_count), stat.provider as u8));
    stats
}

/// Clears all provider statistics.
pub fn reset() {
    stats_guard().clear();
}

#[cfg(test)]
pub mod tests {
    use super::{provider_stats, record_mcp_envelope, record_proxy_envelope, reset};
    use crate::core::context_kernel::token_envelope::{ProviderKind, TokenEnvelope};
    use crate::core::context_kernel::{evidence_wiring, kernel_config, usage_normalizer};

    fn isolated() -> std::sync::MutexGuard<'static, ()> {
        let guard = kernel_config::KERNEL_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        kernel_config::reset_features();
        evidence_wiring::reset();
        usage_normalizer::reset_usage();
        reset();
        guard
    }

    fn envelope(provider: ProviderKind, input: usize) -> TokenEnvelope {
        TokenEnvelope {
            model: "test-model".to_owned(),
            provider,
            input_tokens: input,
            output_tokens: 20,
            cache_read_tokens: 5,
            ..TokenEnvelope::default()
        }
    }

    #[test]
    fn record_proxy_updates_stats() {
        let _guard = isolated();
        for _ in 0..3 {
            record_proxy_envelope(&envelope(ProviderKind::OpenAi, 100));
        }
        assert_eq!(provider_stats()[0].request_count, 3);
    }

    #[test]
    fn record_mcp_updates_stats() {
        let _guard = isolated();
        record_mcp_envelope("ctx_read", &envelope(ProviderKind::Gemini, 80));
        let stats = provider_stats();
        assert_eq!((stats[0].total_input, stats[0].total_output), (80, 20));
        assert_eq!(stats[0].total_cache_read, 5);
    }

    #[test]
    fn multi_provider_tracked() {
        let _guard = isolated();
        record_proxy_envelope(&envelope(ProviderKind::OpenAi, 100));
        record_proxy_envelope(&envelope(ProviderKind::Anthropic, 200));
        assert_eq!(provider_stats().len(), 2);
    }

    #[test]
    fn reset_clears() {
        let _guard = isolated();
        record_proxy_envelope(&envelope(ProviderKind::OpenAi, 100));
        reset();
        assert!(provider_stats().is_empty());
    }

    #[test]
    fn empty_stats_safe() {
        let _guard = isolated();
        assert!(provider_stats().is_empty());
    }

    #[test]
    fn avg_input_correct() {
        let _guard = isolated();
        for input in [100, 200, 300] {
            record_proxy_envelope(&envelope(ProviderKind::OpenAi, input));
        }
        assert_eq!(provider_stats()[0].avg_input, 200);
    }
}
