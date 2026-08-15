//! Per-session value metrics for proxied completions.
//!
//! The proxy records prompt-compression measurements and a per-request input
//! cost estimate for each completion.

use std::sync::{Mutex, PoisonError};

/// Aggregate value metrics for the proxy process's current session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProxyValueMetrics {
    pub request_count: u64,
    pub total_tokens_pruned: u64,
    pub total_original_tokens: u64,
    pub last_task_class: Option<String>,
    pub cost_micros_estimate: u64,
    pub session_cpao_micros: Option<u64>,
}

static PROXY_VALUE_METRICS: Mutex<ProxyValueMetrics> = Mutex::new(ProxyValueMetrics {
    request_count: 0,
    total_tokens_pruned: 0,
    total_original_tokens: 0,
    last_task_class: None,
    cost_micros_estimate: 0,
    session_cpao_micros: None,
});

/// Records a completed proxy request in the current process session.
pub fn record_completion(tokens_pruned: usize, original_tokens: usize, task_class: &str) {
    let mut metrics = PROXY_VALUE_METRICS
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    metrics.request_count = metrics.request_count.saturating_add(1);
    metrics.total_tokens_pruned = metrics
        .total_tokens_pruned
        .saturating_add(tokens_pruned as u64);
    metrics.total_original_tokens = metrics
        .total_original_tokens
        .saturating_add(original_tokens as u64);
    metrics.last_task_class = Some(task_class.to_owned());
    let cost_micros = (original_tokens as u64).saturating_mul(3) / 1_000;
    metrics.cost_micros_estimate = metrics.cost_micros_estimate.saturating_add(cost_micros);
    metrics.session_cpao_micros = Some(metrics.cost_micros_estimate / metrics.request_count);
    tracing::debug!(
        task_class,
        tokens_pruned,
        original_tokens,
        "recorded proxy value metrics"
    );
}

/// Clears all value metrics for the current proxy session.
pub fn reset_session() {
    *PROXY_VALUE_METRICS
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = ProxyValueMetrics::default();
}

/// Returns a snapshot of value metrics accumulated for the current session.
pub fn session_metrics() -> ProxyValueMetrics {
    PROXY_VALUE_METRICS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

/// Returns the fraction of original tokens removed during the current session.
pub fn compression_ratio() -> f64 {
    let metrics = session_metrics();
    if metrics.total_original_tokens == 0 {
        return 0.0;
    }
    metrics.total_tokens_pruned as f64 / metrics.total_original_tokens as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn record_completion_accumulates_session_totals() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        reset_session();

        record_completion(20, 100, "coding_fix");
        record_completion(30, 200, "coding_new");

        let metrics = session_metrics();
        assert_eq!(metrics.request_count, 2);
        assert_eq!(metrics.total_tokens_pruned, 50);
        assert_eq!(metrics.total_original_tokens, 300);
        assert_eq!(metrics.last_task_class.as_deref(), Some("coding_new"));
        assert_eq!(metrics.cost_micros_estimate, 0);
        assert_eq!(metrics.session_cpao_micros, Some(0));
    }

    #[test]
    fn session_metrics_returns_a_clone() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        reset_session();
        record_completion(20, 100, "coding_fix");

        let mut snapshot = session_metrics();
        snapshot.request_count = 0;

        assert_eq!(snapshot.request_count, 0);
        assert_eq!(session_metrics().request_count, 1);
    }

    #[test]
    fn compression_ratio_is_zero_without_original_tokens() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        reset_session();

        assert_eq!(compression_ratio(), 0.0);
    }

    #[test]
    fn compression_ratio_uses_session_totals() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        reset_session();
        record_completion(25, 100, "coding_fix");
        record_completion(50, 200, "coding_new");

        assert_eq!(compression_ratio(), 0.25);
    }

    #[test]
    fn record_completion_estimates_session_cpao() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        reset_session();

        record_completion(0, 1_000, "coding_fix");
        record_completion(0, 2_000, "coding_new");

        let metrics = session_metrics();
        assert_eq!(metrics.cost_micros_estimate, 9);
        assert_eq!(metrics.session_cpao_micros, Some(4));
    }

    #[test]
    fn reset_session_clears_all_metrics() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        record_completion(20, 1_000, "coding_fix");

        reset_session();

        assert_eq!(session_metrics(), ProxyValueMetrics::default());
    }
}
