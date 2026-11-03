// SPDX-License-Identifier: Apache-2.0
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};

/// Tracks whether MCP schema overhead is justified by proxy compression savings.
pub struct BreakEvenCalculator {
    proxy_tokens_saved: AtomicI64,
    schema_overhead_per_turn: u64,
    turn_count: AtomicU64,
    has_used_ctx_tools: AtomicBool,
}

/// Snapshot of a session's MCP break-even state.
#[derive(Debug, Clone)]
pub struct BreakEvenSummary {
    pub proxy_savings: i64,
    pub estimated_mcp_overhead: i64,
    pub net: i64,
    pub mcp_recommended: bool,
    pub reason: &'static str,
}

impl BreakEvenCalculator {
    /// Create with the estimated schema overhead per turn (in tokens).
    /// Typical values: 1500 (minimal profile) to 3500 (full profile).
    pub fn new(schema_overhead_per_turn: u64) -> Self {
        Self {
            proxy_tokens_saved: AtomicI64::new(0),
            schema_overhead_per_turn,
            turn_count: AtomicU64::new(0),
            has_used_ctx_tools: AtomicBool::new(false),
        }
    }

    /// Record tokens saved by proxy-level compression this turn.
    pub fn record_proxy_savings(&self, tokens: i64) {
        self.proxy_tokens_saved.fetch_add(tokens, Ordering::Relaxed);
    }

    /// Advance the turn counter. Call once per API request.
    pub fn record_turn(&self) {
        self.turn_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Mark that the agent has used a ctx_* MCP tool in this session.
    /// Once set, MCP is never disabled mid-session.
    pub fn mark_ctx_tool_used(&self) {
        self.has_used_ctx_tools.store(true, Ordering::Relaxed);
    }

    /// Whether MCP tools should be enabled for this session.
    pub fn should_enable_mcp(&self) -> bool {
        if self.has_used_ctx_tools.load(Ordering::Relaxed) {
            return true;
        }

        let turns = self.turn_count.load(Ordering::Relaxed);
        if turns < 2 {
            return false;
        }

        let savings = self.proxy_tokens_saved.load(Ordering::Relaxed);
        let overhead = self.schema_overhead_per_turn as i64 * turns as i64;
        savings > overhead
    }

    /// Snapshot of the current break-even state.
    pub fn summary(&self) -> BreakEvenSummary {
        let turns = self.turn_count.load(Ordering::Relaxed);
        let savings = self.proxy_tokens_saved.load(Ordering::Relaxed);
        let overhead = self.schema_overhead_per_turn as i64 * turns as i64;
        let net = savings - overhead;
        let mcp_recommended = self.should_enable_mcp();

        let reason = if self.has_used_ctx_tools.load(Ordering::Relaxed) {
            "ctx_tools used — MCP locked on"
        } else if turns < 2 {
            "too few turns — proxy-only"
        } else if mcp_recommended {
            "savings exceed schema overhead"
        } else {
            "schema overhead exceeds savings"
        };

        BreakEvenSummary {
            proxy_savings: savings,
            estimated_mcp_overhead: overhead,
            net,
            mcp_recommended,
            reason,
        }
    }

    /// Current turn count.
    pub fn turn_count(&self) -> u64 {
        self.turn_count.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::BreakEvenCalculator;

    #[test]
    fn new_calculator_starts_proxy_only() {
        let calc = BreakEvenCalculator::new(3500);
        assert!(!calc.should_enable_mcp());
        assert_eq!(calc.turn_count(), 0);
    }

    #[test]
    fn first_two_turns_always_proxy_only() {
        let calc = BreakEvenCalculator::new(1500);
        calc.record_proxy_savings(50000);
        calc.record_turn();
        assert!(!calc.should_enable_mcp());
    }

    #[test]
    fn high_savings_enables_mcp() {
        let calc = BreakEvenCalculator::new(1500);
        for _ in 0..5 {
            calc.record_proxy_savings(5000);
            calc.record_turn();
        }
        assert!(calc.should_enable_mcp());
    }

    #[test]
    fn low_savings_stays_proxy_only() {
        let calc = BreakEvenCalculator::new(3500);
        for _ in 0..3 {
            calc.record_proxy_savings(500);
            calc.record_turn();
        }
        assert!(!calc.should_enable_mcp());
    }

    #[test]
    fn ctx_tool_usage_locks_mcp_on() {
        let calc = BreakEvenCalculator::new(3500);
        calc.mark_ctx_tool_used();
        assert!(calc.should_enable_mcp());
        assert!(calc.should_enable_mcp());
    }

    #[test]
    fn summary_reflects_state() {
        let calc = BreakEvenCalculator::new(2000);
        calc.record_turn();
        calc.record_turn();
        calc.record_proxy_savings(3000);
        let summary = calc.summary();
        assert_eq!(summary.proxy_savings, 3000);
        assert_eq!(summary.estimated_mcp_overhead, 4000);
        assert_eq!(summary.net, -1000);
        assert!(!summary.mcp_recommended);
    }

    #[test]
    fn summary_reason_for_ctx_tools() {
        let calc = BreakEvenCalculator::new(2000);
        calc.mark_ctx_tool_used();
        let summary = calc.summary();
        assert_eq!(summary.reason, "ctx_tools used — MCP locked on");
        assert!(summary.mcp_recommended);
    }
}
