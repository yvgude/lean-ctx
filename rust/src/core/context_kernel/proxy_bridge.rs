//! Unified bridge between raw proxy requests and context-kernel services.

use std::sync::{Mutex, MutexGuard, OnceLock};

use super::client_wiring::{self, OptimizationLevel};
use super::context_broker::BrokerBudget;
use super::coverage_class::{self, CoverageClass};
use super::etpao_live::{EtpaoLive, EtpaoSummary, RequestMetrics};
use super::identity::{CallerIdentity, IdentityLedger, IdentityLedgerSummary};
use super::outcome_signal::{InferredOutcome, OutcomeSignal};
use super::types::ReceiptOutcome;

static IDENTITY_LEDGER: OnceLock<Mutex<IdentityLedger>> = OnceLock::new();
static ETPAO_TRACKER: OnceLock<Mutex<EtpaoLive>> = OnceLock::new();

/// Raw data extracted from a proxy request; proxy-side types remain in the proxy.
#[derive(Debug, Clone, Default)]
pub struct ProxyRequestData {
    /// Request headers used for identity and client-profile detection.
    pub headers: Vec<(String, String)>,
    /// Tokens supplied as model input.
    pub input_tokens: usize,
    /// Tokens produced by the model.
    pub output_tokens: usize,
    /// Tokens consumed by model reasoning.
    pub reasoning_tokens: usize,
    /// Tokens avoided through context optimization.
    pub tokens_saved: usize,
    /// Requested model name, when available.
    pub model: Option<String>,
    /// Upstream provider name, when available.
    pub provider: Option<String>,
    /// Whether this request retries an earlier attempt.
    pub is_retry: bool,
    /// Attempt number retained for factual retry accounting.
    pub request_count: usize,
}

/// Result of kernel processing for a single proxy request.
#[derive(Debug, Clone)]
pub struct ProxyKernelResult {
    /// Caller identity resolved from request headers.
    pub identity: CallerIdentity,
    /// Effective coverage for the inline proxy path.
    pub coverage: CoverageClass,
    /// Stable machine-readable coverage label.
    pub coverage_label: &'static str,
    /// Whether the kernel can modify context on this path.
    pub is_addressable: bool,
    /// Optimization level available for the request.
    pub optimization_level: OptimizationLevel,
    /// Broker-computed token allocation.
    pub kernel_budget: BrokerBudget,
    /// Compatibility outcome; unknown until an explicit evaluator reports one.
    pub outcome_signal: InferredOutcome,
}

fn identity_ledger() -> &'static Mutex<IdentityLedger> {
    IDENTITY_LEDGER.get_or_init(|| Mutex::new(IdentityLedger::new()))
}

fn etpao_tracker() -> &'static Mutex<EtpaoLive> {
    ETPAO_TRACKER.get_or_init(|| Mutex::new(EtpaoLive::new()))
}

fn lock_identity_ledger() -> MutexGuard<'static, IdentityLedger> {
    match identity_ledger().lock() {
        Ok(ledger) => ledger,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn lock_etpao_tracker() -> MutexGuard<'static, EtpaoLive> {
    match etpao_tracker().lock() {
        Ok(tracker) => tracker,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Resolves kernel context and records factual identity and request metrics.
#[must_use]
pub fn process_proxy_request(data: &ProxyRequestData) -> ProxyKernelResult {
    let context = client_wiring::build_request_context(&data.headers, true, false, false);
    let outcome = InferredOutcome {
        outcome: ReceiptOutcome::Unknown,
        confidence: 0.0,
        signal: OutcomeSignal::Ambiguous,
    };
    {
        let mut tracker = lock_etpao_tracker();
        tracker.record_request(RequestMetrics {
            input_tokens: data.input_tokens,
            output_tokens: data.output_tokens,
            reasoning_tokens: data.reasoning_tokens,
            schema_tokens: 0,
            cache_write_tokens: 0,
            retry_count: usize::from(data.is_retry),
            client_id: context.profile.client_id.clone(),
            coverage_class: context.coverage,
        });
    }

    let consumed = data
        .input_tokens
        .saturating_add(data.output_tokens)
        .saturating_add(data.reasoning_tokens);
    lock_identity_ledger().record_usage(&context.identity, consumed, data.tokens_saved);

    let coverage = context.coverage;
    let optimization_level = client_wiring::optimization_level(&context);
    let kernel_budget = context.broker_budget;

    ProxyKernelResult {
        identity: context.identity,
        coverage,
        coverage_label: coverage_class::coverage_label(coverage),
        is_addressable: coverage_class::is_addressable(coverage),
        optimization_level,
        kernel_budget,
        outcome_signal: outcome,
    }
}

/// Returns aggregate identity attribution recorded by the proxy bridge.
#[must_use]
pub fn identity_summary() -> IdentityLedgerSummary {
    lock_identity_ledger().summary()
}

/// Returns aggregate live ETPAO metrics recorded by the proxy bridge.
#[must_use]
pub fn etpao_summary() -> EtpaoSummary {
    lock_etpao_tracker().summary()
}

/// Returns current effective tokens per accepted outcome.
#[must_use]
pub fn current_etpao() -> f64 {
    lock_etpao_tracker().current_etpao()
}

/// Clears all process-wide proxy bridge metrics.
pub fn reset_state() {
    *lock_identity_ledger() = IdentityLedger::new();
    *lock_etpao_tracker() = EtpaoLive::new();
}

#[cfg(test)]
pub mod tests {
    use std::sync::{Mutex, MutexGuard};

    use super::{
        ProxyRequestData, current_etpao, etpao_summary, identity_summary, process_proxy_request,
        reset_state,
    };
    use crate::core::context_kernel::client_wiring::OptimizationLevel;
    use crate::core::context_kernel::coverage_class::CoverageClass;
    use crate::core::context_kernel::types::ReceiptOutcome;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn isolated_test() -> MutexGuard<'static, ()> {
        let guard = match TEST_LOCK.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        reset_state();
        guard
    }

    fn request() -> ProxyRequestData {
        ProxyRequestData {
            headers: vec![("x-user-id".to_owned(), "bridge-user".to_owned())],
            input_tokens: 100,
            output_tokens: 20,
            request_count: 1,
            ..ProxyRequestData::default()
        }
    }

    #[test]
    fn process_empty_request() {
        let _guard = isolated_test();
        let result = process_proxy_request(&ProxyRequestData::default());

        assert_eq!(result.coverage, CoverageClass::FullInline);
        assert!(result.kernel_budget.context_tokens > 0);
    }

    #[test]
    fn process_records_identity() {
        let _guard = isolated_test();
        let _ = process_proxy_request(&request());

        let summary = identity_summary();
        assert_eq!(summary.total_users, 1);
        assert_eq!(summary.total_tokens, 120);
    }

    #[test]
    fn process_records_usage_without_inventing_outcome() {
        let _guard = isolated_test();
        let _ = process_proxy_request(&request());

        let summary = etpao_summary();
        assert_eq!(current_etpao(), 0.0);
        assert_eq!(summary.total_tokens, 120);
        assert_eq!(summary.accepted_outcomes, 0);
    }

    #[test]
    fn coverage_is_full_inline() {
        let _guard = isolated_test();
        let result = process_proxy_request(&request());

        assert_eq!(result.coverage, CoverageClass::FullInline);
        assert_eq!(result.coverage_label, "full_inline");
        assert!(result.is_addressable);
        assert_eq!(result.optimization_level, OptimizationLevel::Full);
    }

    #[test]
    fn first_pass_outcome_remains_unknown() {
        let _guard = isolated_test();
        let result = process_proxy_request(&request());

        assert_eq!(result.outcome_signal.outcome, ReceiptOutcome::Unknown);
        assert_eq!(result.outcome_signal.confidence, 0.0);
    }

    #[test]
    fn retry_outcome_remains_unknown() {
        let _guard = isolated_test();
        let mut data = request();
        data.is_retry = true;
        data.request_count = 2;

        let result = process_proxy_request(&data);
        assert_eq!(result.outcome_signal.outcome, ReceiptOutcome::Unknown);
        assert_eq!(result.outcome_signal.confidence, 0.0);
    }

    #[test]
    fn reset_clears_state() {
        let _guard = isolated_test();
        let _ = process_proxy_request(&request());
        reset_state();

        assert_eq!(current_etpao(), 0.0);
        assert_eq!(identity_summary().total_users, 0);
    }
}
