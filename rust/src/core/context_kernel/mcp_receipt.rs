//! Receipt recording and honest accounting for MCP tool calls.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, OnceLock};

use super::accounting_fix::{PostDeliveryAccounting, compute_honest_accounting};

/// Receipt for a single MCP tool call.
#[derive(Debug, Clone)]
pub struct McpReceipt {
    /// Tool name.
    pub tool: String,
    /// Input tokens, including arguments and context sent to the tool.
    pub tokens_in: usize,
    /// Output tokens in the tool result.
    pub tokens_out: usize,
    /// Kernel overhead tokens added through enrichment and hints.
    pub kernel_overhead: usize,
    /// Whether the outcome was accepted.
    pub accepted: bool,
}

/// Per-tool savings summary.
#[derive(Debug, Clone, Default)]
pub struct ToolSavings {
    /// Tool name.
    pub tool: String,
    /// Number of recorded calls.
    pub calls: usize,
    /// Total input tokens.
    pub tokens_in: usize,
    /// Total output tokens.
    pub tokens_out: usize,
    /// Total kernel overhead tokens.
    pub kernel_overhead: usize,
    /// Savings after output and kernel overhead, as a percentage of input.
    pub honest_savings_pct: f64,
}

#[derive(Default)]
struct McpReceiptStore {
    receipts: Vec<McpReceipt>,
    per_tool: HashMap<String, ToolSavings>,
}

static RECEIPTS: OnceLock<Mutex<McpReceiptStore>> = OnceLock::new();

fn receipt_store() -> &'static Mutex<McpReceiptStore> {
    RECEIPTS.get_or_init(|| Mutex::new(McpReceiptStore::default()))
}

fn lock_store() -> MutexGuard<'static, McpReceiptStore> {
    match receipt_store().lock() {
        Ok(store) => store,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn savings_pct(tokens_in: usize, delivered: usize) -> f64 {
    if tokens_in == 0 {
        0.0
    } else {
        (1.0 - delivered as f64 / tokens_in as f64) * 100.0
    }
}

/// Stores an MCP receipt and updates its per-tool aggregate.
pub fn record_receipt(receipt: McpReceipt) {
    let mut store = lock_store();
    let summary = store
        .per_tool
        .entry(receipt.tool.clone())
        .or_insert_with(|| ToolSavings {
            tool: receipt.tool.clone(),
            ..ToolSavings::default()
        });
    summary.calls = summary.calls.saturating_add(1);
    summary.tokens_in = summary.tokens_in.saturating_add(receipt.tokens_in);
    summary.tokens_out = summary.tokens_out.saturating_add(receipt.tokens_out);
    summary.kernel_overhead = summary
        .kernel_overhead
        .saturating_add(receipt.kernel_overhead);
    summary.honest_savings_pct = savings_pct(
        summary.tokens_in,
        summary.tokens_out.saturating_add(summary.kernel_overhead),
    );
    store.receipts.push(receipt);
}

/// Returns honest accounting aggregated across all MCP receipts.
pub fn mcp_accounting() -> PostDeliveryAccounting {
    let store = lock_store();
    let mut totals = (0usize, 0usize, 0usize);
    for receipt in &store.receipts {
        totals.0 = totals.0.saturating_add(receipt.tokens_in);
        totals.1 = totals.1.saturating_add(receipt.tokens_out);
        totals.2 = totals.2.saturating_add(receipt.kernel_overhead);
    }
    compute_honest_accounting(totals.0, totals.1, totals.2, 0)
}

/// Returns per-tool savings sorted by tool name.
pub fn per_tool_savings() -> Vec<ToolSavings> {
    let mut summaries: Vec<_> = lock_store().per_tool.values().cloned().collect();
    summaries.sort_unstable_by_key(|summary| summary.tool.clone());
    summaries
}

/// Formats a deterministic per-tool savings summary.
pub fn savings_report() -> String {
    per_tool_savings()
        .iter()
        .map(|summary| {
            format!(
                "{}: {} calls, {:.2}% savings",
                summary.tool, summary.calls, summary.honest_savings_pct,
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Returns the total token overhead added by the kernel.
pub fn total_kernel_overhead() -> usize {
    mcp_accounting().kernel_overhead_tokens
}

/// Clears all recorded MCP receipts and aggregates.
pub fn reset_receipts() {
    *lock_store() = McpReceiptStore::default();
}

#[cfg(test)]
pub mod tests {
    use super::{
        McpReceipt, mcp_accounting, per_tool_savings, record_receipt, reset_receipts,
        savings_report, total_kernel_overhead,
    };
    use std::sync::MutexGuard;
    /// The kernel-wide test lock, also held by `mcp_e2e`, which resets the
    /// same process-wide receipt store; a private lock let the two race
    /// (`per_tool_aggregates` saw 1 of 3 calls).
    fn isolated_test() -> MutexGuard<'static, ()> {
        let guard = crate::core::context_kernel::kernel_config::KERNEL_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_receipts();
        guard
    }
    fn receipt(tool: &str) -> McpReceipt {
        McpReceipt {
            tool: tool.to_owned(),
            tokens_in: 100,
            tokens_out: 40,
            kernel_overhead: 10,
            accepted: true,
        }
    }
    #[test]
    fn record_and_retrieve() {
        let _guard = isolated_test();
        record_receipt(receipt("read"));
        record_receipt(receipt("search"));
        record_receipt(receipt("read"));
        assert_eq!(per_tool_savings().len(), 2);
    }
    #[test]
    fn accounting_is_honest() {
        let _guard = isolated_test();
        record_receipt(receipt("read"));
        let accounting = mcp_accounting();
        assert_eq!(accounting.kernel_overhead_tokens, 10);
        assert!((accounting.phantom_savings_pct - 0.1).abs() < f64::EPSILON);
    }
    #[test]
    fn per_tool_aggregates() {
        let _guard = isolated_test();
        for _ in 0..3 {
            record_receipt(receipt("read"));
        }
        let summaries = per_tool_savings();
        assert_eq!(summaries[0].calls, 3);
        assert_eq!(summaries[0].honest_savings_pct, 50.0);
    }
    #[test]
    fn savings_report_formatted() {
        let _guard = isolated_test();
        record_receipt(receipt("search"));
        let report = savings_report();
        assert!(report.contains("search"));
        assert!(report.contains("50.00% savings"));
    }
    #[test]
    fn reset_clears_all() {
        let _guard = isolated_test();
        record_receipt(receipt("read"));
        reset_receipts();
        assert!(per_tool_savings().is_empty());
        assert_eq!(total_kernel_overhead(), 0);
    }
}
