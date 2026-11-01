//! Dispatch-aware evidence recording for MCP tools and proxy requests.

use std::sync::atomic::{AtomicUsize, Ordering};

static TOOL_DISPATCHES: AtomicUsize = AtomicUsize::new(0);
static PROXY_DISPATCHES: AtomicUsize = AtomicUsize::new(0);
static TOTAL_TOKENS_SAVED: AtomicUsize = AtomicUsize::new(0);
static TOTAL_INPUT_TOKENS: AtomicUsize = AtomicUsize::new(0);
static TOTAL_OUTPUT_TOKENS: AtomicUsize = AtomicUsize::new(0);

/// Cumulative evidence totals observed at dispatch boundaries.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct DispatchSummary {
    /// Number of dispatched MCP tool calls.
    pub tool_dispatches: usize,
    /// Number of dispatched proxy requests.
    pub proxy_dispatches: usize,
    /// Tokens saved across all dispatches.
    pub total_tokens_saved: usize,
    /// Input tokens across all dispatches.
    pub total_input_tokens: usize,
    /// Output tokens across all dispatches.
    pub total_output_tokens: usize,
}

fn record_dispatch(input_tokens: usize, output_tokens: usize, tokens_saved: usize) {
    TOTAL_INPUT_TOKENS.fetch_add(input_tokens, Ordering::Relaxed);
    TOTAL_OUTPUT_TOKENS.fetch_add(output_tokens, Ordering::Relaxed);
    TOTAL_TOKENS_SAVED.fetch_add(tokens_saved, Ordering::Relaxed);
}

/// Records evidence for one dispatched MCP tool call.
pub fn record_from_tool_dispatch(
    tool_name: &str,
    input_tokens: usize,
    output_tokens: usize,
    tokens_saved: usize,
) {
    TOOL_DISPATCHES.fetch_add(1, Ordering::Relaxed);
    record_dispatch(input_tokens, output_tokens, tokens_saved);
    super::evidence_hook::record_tool_call(tool_name, input_tokens, output_tokens);
}

/// Counts one proxy-forwarded request at the dispatch boundary.
///
/// Only the dispatch counters move here. The forward path's Context IR stage
/// records the request once in the kernel evidence pipelines.
pub fn record_from_proxy_dispatch(input_tokens: usize, output_tokens: usize, tokens_saved: usize) {
    PROXY_DISPATCHES.fetch_add(1, Ordering::Relaxed);
    record_dispatch(input_tokens, output_tokens, tokens_saved);
}

/// Returns cumulative dispatch-boundary evidence totals.
#[must_use]
pub fn dispatch_summary() -> DispatchSummary {
    DispatchSummary {
        tool_dispatches: TOOL_DISPATCHES.load(Ordering::Relaxed),
        proxy_dispatches: PROXY_DISPATCHES.load(Ordering::Relaxed),
        total_tokens_saved: TOTAL_TOKENS_SAVED.load(Ordering::Relaxed),
        total_input_tokens: TOTAL_INPUT_TOKENS.load(Ordering::Relaxed),
        total_output_tokens: TOTAL_OUTPUT_TOKENS.load(Ordering::Relaxed),
    }
}

/// Clears dispatch counters and delegated evidence state.
pub fn reset() {
    TOOL_DISPATCHES.store(0, Ordering::Relaxed);
    PROXY_DISPATCHES.store(0, Ordering::Relaxed);
    TOTAL_TOKENS_SAVED.store(0, Ordering::Relaxed);
    TOTAL_INPUT_TOKENS.store(0, Ordering::Relaxed);
    TOTAL_OUTPUT_TOKENS.store(0, Ordering::Relaxed);
    super::evidence_hook::reset();
}

#[cfg(test)]
pub mod tests {
    use super::{dispatch_summary, record_from_proxy_dispatch, record_from_tool_dispatch, reset};
    use crate::core::context_kernel::{evidence_hook, kernel_config};

    fn isolated() -> std::sync::MutexGuard<'static, ()> {
        let guard = kernel_config::KERNEL_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        kernel_config::reset_features();
        reset();
        guard
    }

    #[test]
    fn tool_dispatch_records() {
        let _guard = isolated();
        for _ in 0..3 {
            record_from_tool_dispatch("ctx_read", 10, 4, 6);
        }
        let summary = dispatch_summary();
        assert_eq!(summary.tool_dispatches, 3);
        assert_eq!(summary.total_tokens_saved, 18);
    }

    #[test]
    fn proxy_dispatch_records() {
        let _guard = isolated();
        for _ in 0..2 {
            record_from_proxy_dispatch(20, 5, 8);
        }
        let summary = dispatch_summary();
        assert_eq!(summary.proxy_dispatches, 2);
        assert_eq!(summary.total_input_tokens, 40);
        // The Context IR stage owns the per-request evidence; a dispatch adds none.
        assert_eq!(evidence_hook::evidence_report().proxy_calls, 0);
        assert_eq!(
            crate::core::context_kernel::usage_normalizer::session_usage().total_requests,
            0
        );
    }

    #[test]
    fn disabled_kernel_no_records() {
        let _guard = isolated();
        let mut features = kernel_config::features();
        features.enabled = false;
        kernel_config::update_features(features);
        record_from_tool_dispatch("ctx_read", 10, 4, 6);
        record_from_proxy_dispatch(20, 5, 8);
        assert_eq!(evidence_hook::evidence_report().tool_calls, 0);
        assert_eq!(evidence_hook::evidence_report().proxy_calls, 0);
        assert_eq!(dispatch_summary().tool_dispatches, 1);
        assert_eq!(dispatch_summary().proxy_dispatches, 1);
    }

    #[test]
    fn reset_clears_all() {
        let _guard = isolated();
        record_from_tool_dispatch("ctx_read", 10, 4, 6);
        record_from_proxy_dispatch(20, 5, 8);
        reset();
        let summary = dispatch_summary();
        assert_eq!(summary.tool_dispatches, 0);
        assert_eq!(summary.proxy_dispatches, 0);
        assert_eq!(summary.total_tokens_saved, 0);
        assert_eq!(summary.total_input_tokens, 0);
        assert_eq!(summary.total_output_tokens, 0);
    }
}
