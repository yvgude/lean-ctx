//! Aggregated live metrics for dashboard consumers.

use std::panic::{AssertUnwindSafe, catch_unwind};

use super::{coverage_class, mcp_bridge, proxy_bridge, receipt_chain, usage_normalizer};

/// Complete kernel metrics snapshot for dashboard rendering.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct DashboardSnapshot {
    /// Current ETPAO (proxy).
    pub proxy_etpao: f64,
    /// Current ETPAO (MCP).
    pub mcp_etpao: f64,
    /// Identity summary.
    pub identity: IdentityView,
    /// Usage overview.
    pub usage: UsageView,
    /// Receipt chain summary.
    pub chain: ChainView,
    /// Coverage breakdown.
    pub coverage: CoverageView,
}

/// Identity attribution totals visible to the dashboard.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct IdentityView {
    /// Number of distinct users observed.
    pub total_users: usize,
    /// Tokens attributed to users.
    pub total_tokens: usize,
    /// Tokens saved for users.
    pub total_saved: usize,
    /// Fraction of attributed requests that were accepted.
    pub acceptance_rate: f64,
}

/// Provider-normalized usage totals visible to the dashboard.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct UsageView {
    /// Number of normalized provider requests.
    pub total_requests: usize,
    /// Total normalized tokens consumed.
    pub total_tokens: usize,
    /// Total tokens saved by context optimization.
    pub total_saved: usize,
    /// Ratio of delivered tokens to original tokens.
    pub compression_ratio: f64,
    /// Most frequently observed model, when any usage exists.
    pub top_model: Option<String>,
}

/// Request-to-outcome evidence-chain totals.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ChainView {
    /// Number of complete or partial chain entries.
    pub total_entries: usize,
    /// Entries with accepted outcomes.
    pub accepted: usize,
    /// Entries with rejected outcomes.
    pub rejected: usize,
    /// Fraction of entries whose plans used the kernel.
    pub kernel_hit_rate: f64,
    /// Average phantom savings percentage across receipts.
    pub phantom_savings_pct: f64,
}

/// Coverage labels for the proxy and MCP integration paths.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct CoverageView {
    /// Stable proxy coverage label.
    pub proxy_coverage: String,
    /// Stable MCP coverage label.
    pub mcp_coverage: String,
    /// Whether both paths permit direct context management.
    pub is_fully_addressable: bool,
}

/// Returns a point-in-time view of all live kernel metrics.
#[must_use]
pub fn snapshot() -> DashboardSnapshot {
    catch_unwind(AssertUnwindSafe(build_snapshot)).unwrap_or_default()
}

/// Returns the current dashboard snapshot as JSON.
#[must_use]
pub fn snapshot_json() -> String {
    serde_json::to_string(&snapshot()).unwrap_or_else(|_| "{}".to_owned())
}

/// Returns a compact human-readable dashboard summary.
#[must_use]
pub fn format_summary() -> String {
    let value = snapshot();
    format!(
        "ETPAO proxy={:.2} mcp={:.2}; requests={}; compression={:.2}; chain={}/{} accepted",
        value.proxy_etpao,
        value.mcp_etpao,
        value.usage.total_requests,
        value.usage.compression_ratio,
        value.chain.accepted,
        value.chain.total_entries,
    )
}

fn build_snapshot() -> DashboardSnapshot {
    let identity = proxy_bridge::identity_summary();
    let proxy_etpao = proxy_bridge::etpao_summary();
    let usage = usage_normalizer::session_usage();
    let compression = usage_normalizer::compression_overview();
    let chain = receipt_chain::chain_summary();
    let proxy_coverage = coverage_class::CoverageClass::FullInline;
    let mcp_coverage = coverage_class::CoverageClass::ContextControlled;

    DashboardSnapshot {
        proxy_etpao: proxy_etpao.etpao,
        mcp_etpao: mcp_bridge::mcp_etpao(),
        identity: IdentityView {
            total_users: identity.total_users,
            total_tokens: identity.total_tokens,
            total_saved: identity.total_savings,
            acceptance_rate: proxy_etpao.first_pass_rate,
        },
        usage: UsageView {
            total_requests: usage.total_requests,
            total_tokens: usage.total_tokens,
            total_saved: usage.total_saved,
            compression_ratio: compression.avg_compression_ratio,
            top_model: compression.best_model,
        },
        chain: ChainView {
            total_entries: chain.total_entries,
            accepted: chain.accepted,
            rejected: chain.rejected,
            kernel_hit_rate: receipt_chain::kernel_hit_rate(),
            phantom_savings_pct: average(chain.total_phantom_savings_pct, chain.total_entries),
        },
        coverage: CoverageView {
            proxy_coverage: coverage_class::coverage_label(proxy_coverage).to_owned(),
            mcp_coverage: coverage_class::coverage_label(mcp_coverage).to_owned(),
            is_fully_addressable: coverage_class::is_addressable(proxy_coverage)
                && coverage_class::is_addressable(mcp_coverage),
        },
    }
}

fn average(total: f64, count: usize) -> f64 {
    if count == 0 {
        0.0
    } else {
        total / count as f64
    }
}

#[cfg(test)]
pub mod tests {
    use std::sync::{Mutex, MutexGuard};

    use super::{DashboardSnapshot, format_summary, snapshot, snapshot_json};
    use crate::core::context_kernel::mcp_bridge::{self, McpCallData};
    use crate::core::context_kernel::proxy_bridge::{self, ProxyRequestData};

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn isolated_test() -> MutexGuard<'static, ()> {
        let guard = TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        proxy_bridge::reset_state();
        mcp_bridge::reset_mcp_state();
        crate::core::context_kernel::usage_normalizer::reset_usage();
        crate::core::context_kernel::receipt_chain::reset_chain();
        guard
    }

    #[test]
    fn empty_snapshot() {
        let _guard = isolated_test();
        let value = snapshot();

        assert_eq!(value.proxy_etpao, 0.0);
        assert_eq!(value.mcp_etpao, 0.0);
        assert_eq!(value.identity.total_tokens, 0);
        assert_eq!(value.usage.total_requests, 0);
        assert_eq!(value.chain.total_entries, 0);
    }

    #[test]
    fn snapshot_serializes() {
        let _guard = isolated_test();
        let decoded: DashboardSnapshot = serde_json::from_str(&snapshot_json()).unwrap();

        assert_eq!(decoded.proxy_etpao, 0.0);
    }

    #[test]
    fn format_summary_non_empty() {
        let _guard = isolated_test();
        let summary = format_summary();

        assert!(summary.contains("ETPAO"));
        assert!(summary.contains("compression"));
    }

    #[test]
    fn snapshot_includes_coverage() {
        let _guard = isolated_test();
        let coverage = snapshot().coverage;

        assert_eq!(coverage.proxy_coverage, "full_inline");
        assert_eq!(coverage.mcp_coverage, "context_controlled");
        assert!(coverage.is_fully_addressable);
    }

    #[test]
    fn snapshot_after_activity() {
        let _guard = isolated_test();
        let _ = proxy_bridge::process_proxy_request(&ProxyRequestData {
            input_tokens: 80,
            output_tokens: 20,
            request_count: 1,
            ..ProxyRequestData::default()
        });
        mcp_bridge::record_mcp_call(&McpCallData {
            input_tokens: 40,
            output_tokens: 10,
            call_number: 1,
            ..McpCallData::default()
        });

        let value = snapshot();
        assert_eq!(value.proxy_etpao, 0.0);
        assert_eq!(value.mcp_etpao, 0.0);
        assert!(value.identity.total_tokens > 0);
    }
}
