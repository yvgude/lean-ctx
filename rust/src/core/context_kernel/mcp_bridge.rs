//! Unified integration between MCP clients and the context kernel.

use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use super::client_wiring::OptimizationLevel;
use super::coverage_class::{self, CoverageClass};
use super::etpao_live::{EtpaoLive, RequestMetrics};
use super::identity::{CallerIdentity, CallerRole, IdentityLedger};

static MCP_ETPAO: OnceLock<Mutex<EtpaoLive>> = OnceLock::new();
static MCP_IDENTITY: OnceLock<Mutex<IdentityLedger>> = OnceLock::new();

const MCP_SESSION_ID: &str = "mcp-session";

/// Information about the MCP client, extracted from the initialize handshake.
#[derive(Debug, Clone, Default)]
pub struct McpClientInfo {
    /// Client application name (e.g. "cursor", "vscode", "zed").
    pub client_name: String,
    /// Whether the client declared roots capability.
    pub supports_roots: bool,
    /// Whether the client declared sampling capability.
    pub supports_sampling: bool,
    /// Number of tools visible to the client.
    pub tool_count: usize,
}

/// Data for a single MCP tool call, used to record factual request metrics.
#[derive(Debug, Clone, Default)]
pub struct McpCallData {
    /// Name of the tool called.
    pub tool_name: String,
    /// Estimated input tokens (tool arguments and context).
    pub input_tokens: usize,
    /// Output tokens in the tool result.
    pub output_tokens: usize,
    /// Whether this is a retry of a previous call.
    pub is_retry: bool,
    /// Sequential call number in the session.
    pub call_number: usize,
}

/// Result of kernel processing for an MCP client session.
#[derive(Debug, Clone)]
pub struct McpKernelResult {
    /// Integration coverage detected for the client.
    pub coverage: CoverageClass,
    /// Stable machine-readable label for the coverage class.
    pub coverage_label: &'static str,
    /// Whether the kernel can directly optimize client context.
    pub is_addressable: bool,
    /// Optimization strength appropriate for the client.
    pub optimization_level: OptimizationLevel,
    /// Maximum tokens available for MCP tool schemas.
    pub schema_budget: usize,
}

/// Aggregate MCP metrics summary.
#[derive(Debug, Clone, Default)]
pub struct McpSummary {
    /// Number of recorded MCP tool calls.
    pub total_calls: usize,
    /// Estimated input tokens across all calls.
    pub total_input_tokens: usize,
    /// Output tokens across all calls.
    pub total_output_tokens: usize,
    /// Calls with an explicit evaluated acceptance outcome.
    pub accepted_calls: usize,
    /// Effective tokens consumed per accepted outcome.
    pub etpao: f64,
    /// Stable coverage label used for recorded MCP calls.
    pub coverage_label: String,
}

/// Processes an MCP initialize handshake into kernel policy and budget data.
#[must_use]
pub fn process_mcp_context(info: &McpClientInfo) -> McpKernelResult {
    let coverage = super::mcp_coverage::detect_mcp_coverage(
        &info.client_name,
        info.supports_roots,
        info.supports_sampling,
    );
    let profile = super::mcp_coverage::mcp_client_profile(&info.client_name);

    McpKernelResult {
        coverage,
        coverage_label: coverage_class::coverage_label(coverage),
        is_addressable: coverage_class::is_addressable(coverage),
        optimization_level: optimization_level(coverage),
        schema_budget: profile.tool_budget.max_schema_tokens,
    }
}

/// Records factual token usage and session identity for an MCP call.
pub fn record_mcp_call(data: &McpCallData) {
    lock_etpao().record_request(RequestMetrics {
        input_tokens: data.input_tokens,
        output_tokens: data.output_tokens,
        reasoning_tokens: 0,
        schema_tokens: 0,
        cache_write_tokens: 0,
        retry_count: usize::from(data.is_retry),
        client_id: MCP_SESSION_ID.to_owned(),
        coverage_class: CoverageClass::ContextControlled,
    });

    lock_identity().record_usage(
        &mcp_identity(),
        data.input_tokens.saturating_add(data.output_tokens),
        0,
    );
}

/// Generate a ContextReceiptV1 from completed MCP tool call data.
#[must_use]
pub fn generate_mcp_receipt(
    plan_id: &str,
    tool_name: &str,
    _input_tokens: usize,
    output_tokens: usize,
    cache_hit: bool,
) -> super::types::ContextReceiptV1 {
    use std::collections::HashMap;

    use super::types::{ContextReceiptV1, ReceiptOutcome};

    ContextReceiptV1 {
        receipt_id: format!("mcp-{tool_name}-{}", uuid_v4_short()),
        plan_id: plan_id.to_owned(),
        task_id: crate::core::task_spine::TaskSpine::task_id(),
        delivered_tokens: output_tokens,
        cache_hits: usize::from(cache_hit),
        cache_misses: usize::from(!cache_hit),
        // A successful MCP delivery is an Engine/tool fact, not an explicit
        // host or evaluator judgment about task quality.
        outcome: ReceiptOutcome::Unknown,
        quality_signals: vec![],
        feedback_attribution: HashMap::new(),
    }
}

/// Returns the current MCP effective-tokens-per-accepted-outcome value.
#[must_use]
pub fn mcp_etpao() -> f64 {
    lock_etpao().current_etpao()
}

/// Returns aggregate metrics for all MCP calls recorded in this process.
#[must_use]
pub fn mcp_summary() -> McpSummary {
    let etpao = lock_etpao();
    let etpao_summary = etpao.summary();
    let total_calls = etpao.request_count();
    let (total_input_tokens, total_output_tokens) = etpao.request_token_totals();

    McpSummary {
        total_calls,
        total_input_tokens,
        total_output_tokens,
        accepted_calls: etpao_summary.accepted_outcomes,
        etpao: etpao_summary.etpao,
        coverage_label: coverage_class::coverage_label(CoverageClass::ContextControlled).to_owned(),
    }
}

/// Clears process-wide MCP metrics and identity state.
pub fn reset_mcp_state() {
    *lock_etpao() = EtpaoLive::new();
    *lock_identity() = IdentityLedger::new();
}

fn optimization_level(coverage: CoverageClass) -> OptimizationLevel {
    match coverage {
        CoverageClass::FullInline => OptimizationLevel::Full,
        CoverageClass::ContextControlled => OptimizationLevel::Partial,
        CoverageClass::ObserveOnly => OptimizationLevel::ObserveOnly,
        CoverageClass::Unmanaged => OptimizationLevel::None,
    }
}

fn mcp_identity() -> CallerIdentity {
    CallerIdentity {
        user_id: Some(MCP_SESSION_ID.to_owned()),
        role: CallerRole::Agent,
        session_id: Some(MCP_SESSION_ID.to_owned()),
        ..CallerIdentity::default()
    }
}

fn uuid_v4_short() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .to_string()
}

fn lock_etpao() -> MutexGuard<'static, EtpaoLive> {
    MCP_ETPAO
        .get_or_init(|| Mutex::new(EtpaoLive::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn lock_identity() -> MutexGuard<'static, IdentityLedger> {
    MCP_IDENTITY
        .get_or_init(|| Mutex::new(IdentityLedger::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
pub mod tests {
    use std::sync::{Mutex, MutexGuard};

    use super::{
        McpCallData, McpClientInfo, generate_mcp_receipt, mcp_etpao, mcp_summary,
        process_mcp_context, record_mcp_call, reset_mcp_state,
    };
    use crate::core::context_kernel::coverage_class::CoverageClass;
    use crate::core::context_kernel::types::ReceiptOutcome;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn test_guard() -> MutexGuard<'static, ()> {
        TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn call(number: usize, retry: bool) -> McpCallData {
        McpCallData {
            tool_name: "ctx_read".to_owned(),
            input_tokens: 100,
            output_tokens: 20,
            is_retry: retry,
            call_number: number,
        }
    }

    #[test]
    fn process_cursor_context() {
        let _guard = test_guard();
        let result = process_mcp_context(&McpClientInfo {
            client_name: "cursor".to_owned(),
            ..McpClientInfo::default()
        });
        assert_eq!(result.coverage, CoverageClass::FullInline);
    }

    #[test]
    fn process_vscode_context() {
        let _guard = test_guard();
        let result = process_mcp_context(&McpClientInfo {
            client_name: "vscode".to_owned(),
            ..McpClientInfo::default()
        });
        assert_eq!(result.coverage, CoverageClass::ContextControlled);
    }

    #[test]
    fn process_unknown_context() {
        let _guard = test_guard();
        let result = process_mcp_context(&McpClientInfo {
            client_name: "unknown".to_owned(),
            ..McpClientInfo::default()
        });
        assert_eq!(result.coverage, CoverageClass::ObserveOnly);
    }

    #[test]
    fn record_call_preserves_usage_without_inventing_outcome() {
        let _guard = test_guard();
        reset_mcp_state();
        record_mcp_call(&call(1, false));
        let summary = mcp_summary();
        assert_eq!(mcp_etpao(), 0.0);
        assert_eq!(summary.accepted_calls, 0);
        assert_eq!(summary.total_input_tokens, 100);
        assert_eq!(summary.total_output_tokens, 20);
    }

    #[test]
    fn record_retry_does_not_invent_rejection_or_acceptance() {
        let _guard = test_guard();
        reset_mcp_state();
        record_mcp_call(&call(2, true));
        assert_eq!(mcp_summary().accepted_calls, 0);
    }

    #[test]
    fn summary_aggregates() {
        let _guard = test_guard();
        reset_mcp_state();
        for number in 1..=5 {
            record_mcp_call(&call(number, false));
        }
        let summary = mcp_summary();
        assert_eq!(summary.total_calls, 5);
        assert_eq!(summary.total_input_tokens, 500);
        assert_eq!(summary.total_output_tokens, 100);
    }

    #[test]
    fn reset_clears() {
        let _guard = test_guard();
        reset_mcp_state();
        record_mcp_call(&call(1, false));
        reset_mcp_state();
        assert_eq!(mcp_etpao(), 0.0);
        assert_eq!(mcp_summary().total_calls, 0);
    }

    #[test]
    fn test_generate_mcp_receipt_fields() {
        let receipt = generate_mcp_receipt("plan-42", "ctx_read", 100, 20, false);

        assert_eq!(receipt.plan_id, "plan-42");
        assert!(receipt.receipt_id.starts_with("mcp-ctx_read-"));
        assert_eq!(receipt.delivered_tokens, 20);
        assert_eq!(receipt.outcome, ReceiptOutcome::Unknown);
    }

    #[test]
    fn test_receipt_cache_hit_counting() {
        let receipt = generate_mcp_receipt("plan-42", "ctx_read", 100, 20, true);

        assert_eq!(receipt.cache_hits, 1);
        assert_eq!(receipt.cache_misses, 0);
    }

    #[test]
    fn test_receipt_cache_miss_counting() {
        let receipt = generate_mcp_receipt("plan-42", "ctx_read", 100, 20, false);

        assert_eq!(receipt.cache_hits, 0);
        assert_eq!(receipt.cache_misses, 1);
    }
}
