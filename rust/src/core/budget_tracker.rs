//! Runtime budget tracking against role limits.
//!
//! Compares accumulated session counters with the active role's `RoleLimits`
//! and produces `BudgetStatus` verdicts (Ok / Warning / Exhausted).

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::OnceLock;

use serde::Serialize;

use crate::core::roles::{self, RoleLimits};

static TRACKER: OnceLock<BudgetTracker> = OnceLock::new();

pub struct BudgetTracker {
    context_tokens: AtomicU64,
    shell_invocations: AtomicUsize,
    cost_millicents: AtomicU64,
}

impl BudgetTracker {
    fn new() -> Self {
        Self {
            context_tokens: AtomicU64::new(0),
            shell_invocations: AtomicUsize::new(0),
            cost_millicents: AtomicU64::new(0),
        }
    }

    pub fn global() -> &'static BudgetTracker {
        TRACKER.get_or_init(BudgetTracker::new)
    }

    pub fn record_tokens(&self, tokens: u64) {
        self.context_tokens.fetch_add(tokens, Ordering::Relaxed);
    }

    pub fn record_shell(&self) {
        self.shell_invocations.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_cost_usd(&self, usd: f64) {
        let mc = (usd * 100_000.0) as u64;
        self.cost_millicents.fetch_add(mc, Ordering::Relaxed);
    }

    pub fn tokens_used(&self) -> u64 {
        self.context_tokens.load(Ordering::Relaxed)
    }

    pub fn shell_used(&self) -> usize {
        self.shell_invocations.load(Ordering::Relaxed)
    }

    pub fn cost_usd(&self) -> f64 {
        self.cost_millicents.load(Ordering::Relaxed) as f64 / 100_000.0
    }

    pub fn reset(&self) {
        self.context_tokens.store(0, Ordering::Relaxed);
        self.shell_invocations.store(0, Ordering::Relaxed);
        self.cost_millicents.store(0, Ordering::Relaxed);
    }

    pub fn check(&self) -> BudgetSnapshot {
        let limits = roles::active_role().limits;
        let role_name = roles::active_role_name();

        let tokens = self.tokens_used();
        let shell = self.shell_used();
        let cost = self.cost_usd();

        BudgetSnapshot {
            role: role_name,
            tokens: DimensionStatus::evaluate(tokens as usize, limits.max_context_tokens, &limits),
            shell: DimensionStatus::evaluate(shell, limits.max_shell_invocations, &limits),
            cost: CostStatus::evaluate(cost, limits.max_cost_usd, &limits),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum BudgetLevel {
    Ok,
    Warning,
    Exhausted,
}

impl std::fmt::Display for BudgetLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ok => write!(f, "OK"),
            Self::Warning => write!(f, "WARNING"),
            Self::Exhausted => write!(f, "EXHAUSTED"),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DimensionStatus {
    pub used: usize,
    pub limit: usize,
    pub percent: u8,
    pub level: BudgetLevel,
}

impl DimensionStatus {
    fn evaluate(used: usize, limit: usize, limits: &RoleLimits) -> Self {
        if limit == 0 {
            // Zero limit with any usage => Warning (not Exhausted, LeanCTX never blocks)
            return Self {
                used,
                limit,
                percent: 0,
                level: if used > 0 {
                    BudgetLevel::Warning
                } else {
                    BudgetLevel::Ok
                },
            };
        }
        let percent = ((used as f64 / limit as f64) * 100.0).min(254.0) as u8;
        // block_at_percent == 255 means blocking is disabled (LeanCTX default)
        let level = if limits.block_at_percent < 255 && percent >= limits.block_at_percent {
            BudgetLevel::Exhausted
        } else if percent >= limits.warn_at_percent {
            BudgetLevel::Warning
        } else {
            BudgetLevel::Ok
        };
        Self {
            used,
            limit,
            percent,
            level,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CostStatus {
    pub used_usd: f64,
    pub limit_usd: f64,
    pub percent: u8,
    pub level: BudgetLevel,
}

impl CostStatus {
    fn evaluate(used: f64, limit: f64, limits: &RoleLimits) -> Self {
        if limit <= 0.0 {
            // Zero limit with any usage => Warning (not Exhausted, LeanCTX never blocks)
            return Self {
                used_usd: used,
                limit_usd: limit,
                percent: 0,
                level: if used > 0.0 {
                    BudgetLevel::Warning
                } else {
                    BudgetLevel::Ok
                },
            };
        }
        let pct = ((used / limit) * 100.0).min(254.0) as u8;
        // block_at_percent == 255 means blocking is disabled (LeanCTX default)
        let level = if limits.block_at_percent < 255 && pct >= limits.block_at_percent {
            BudgetLevel::Exhausted
        } else if pct >= limits.warn_at_percent {
            BudgetLevel::Warning
        } else {
            BudgetLevel::Ok
        };
        Self {
            used_usd: used,
            limit_usd: limit,
            percent: pct,
            level,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct BudgetSnapshot {
    pub role: String,
    pub tokens: DimensionStatus,
    pub shell: DimensionStatus,
    pub cost: CostStatus,
}

impl BudgetSnapshot {
    pub fn worst_level(&self) -> &BudgetLevel {
        for level in [&self.tokens.level, &self.shell.level, &self.cost.level] {
            if *level == BudgetLevel::Exhausted {
                return level;
            }
        }
        for level in [&self.tokens.level, &self.shell.level, &self.cost.level] {
            if *level == BudgetLevel::Warning {
                return level;
            }
        }
        &BudgetLevel::Ok
    }

    pub fn format_compact(&self) -> String {
        format!(
            "Budget[{}]: tokens {}/{} ({}%) | shell {}/{} ({}%) | cost ${:.2}/${:.2} ({}%) → {}",
            self.role,
            self.tokens.used,
            self.tokens.limit,
            self.tokens.percent,
            self.shell.used,
            self.shell.limit,
            self.shell.percent,
            self.cost.used_usd,
            self.cost.limit_usd,
            self.cost.percent,
            self.worst_level(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracker_starts_at_zero() {
        let t = BudgetTracker::new();
        assert_eq!(t.tokens_used(), 0);
        assert_eq!(t.shell_used(), 0);
        assert!((t.cost_usd() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn record_and_read() {
        let t = BudgetTracker::new();
        t.record_tokens(5000);
        t.record_tokens(3000);
        t.record_shell();
        t.record_shell();
        t.record_cost_usd(0.50);
        assert_eq!(t.tokens_used(), 8000);
        assert_eq!(t.shell_used(), 2);
        assert!((t.cost_usd() - 0.50).abs() < 0.001);
    }

    #[test]
    fn reset_clears_all() {
        let t = BudgetTracker::new();
        t.record_tokens(10_000);
        t.record_shell();
        t.record_cost_usd(1.0);
        t.reset();
        assert_eq!(t.tokens_used(), 0);
        assert_eq!(t.shell_used(), 0);
        assert!((t.cost_usd() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn dimension_status_ok() {
        let limits = RoleLimits::default();
        let s = DimensionStatus::evaluate(50_000, 200_000, &limits);
        assert_eq!(s.level, BudgetLevel::Ok);
        assert_eq!(s.percent, 25);
    }

    #[test]
    fn dimension_status_warning() {
        let limits = RoleLimits::default();
        let s = DimensionStatus::evaluate(170_000, 200_000, &limits);
        assert_eq!(s.level, BudgetLevel::Warning);
        assert_eq!(s.percent, 85);
    }

    #[test]
    fn dimension_status_at_100_percent_is_warning_by_default() {
        // With block_at_percent=255 (default), 100% usage is Warning, not Exhausted
        let limits = RoleLimits::default();
        assert_eq!(limits.block_at_percent, 255); // Default = never block
        let s = DimensionStatus::evaluate(200_000, 200_000, &limits);
        assert_eq!(s.level, BudgetLevel::Warning);
        assert_eq!(s.percent, 100);
    }

    #[test]
    fn dimension_status_exhausted_when_blocking_enabled() {
        // Exhausted only happens when block_at_percent is explicitly set low
        let limits = RoleLimits {
            block_at_percent: 100,
            ..Default::default()
        };
        let s = DimensionStatus::evaluate(200_000, 200_000, &limits);
        assert_eq!(s.level, BudgetLevel::Exhausted);
    }

    #[test]
    fn zero_limit_warns_usage() {
        // Zero limit with any usage => Warning (not Exhausted, LeanCTX never blocks by default)
        let limits = RoleLimits::default();
        let s = DimensionStatus::evaluate(1, 0, &limits);
        assert_eq!(s.level, BudgetLevel::Warning);
    }

    #[test]
    fn cost_status_warning() {
        let limits = RoleLimits::default();
        let s = CostStatus::evaluate(4.5, 5.0, &limits);
        assert_eq!(s.level, BudgetLevel::Warning);
    }

    #[test]
    fn snapshot_worst_level() {
        let limits = RoleLimits::default();
        let snap = BudgetSnapshot {
            role: "test".into(),
            tokens: DimensionStatus::evaluate(50_000, 200_000, &limits),
            shell: DimensionStatus::evaluate(90, 100, &limits),
            cost: CostStatus::evaluate(1.0, 5.0, &limits),
        };
        assert_eq!(*snap.worst_level(), BudgetLevel::Warning);
    }

    #[test]
    fn format_compact_includes_all() {
        let s = BudgetSnapshot {
            role: "coder".into(),
            tokens: DimensionStatus {
                used: 1000,
                limit: 200_000,
                percent: 0,
                level: BudgetLevel::Ok,
            },
            shell: DimensionStatus {
                used: 5,
                limit: 100,
                percent: 5,
                level: BudgetLevel::Ok,
            },
            cost: CostStatus {
                used_usd: 0.25,
                limit_usd: 5.0,
                percent: 5,
                level: BudgetLevel::Ok,
            },
        };
        let out = s.format_compact();
        assert!(out.contains("coder"));
        assert!(out.contains("tokens"));
        assert!(out.contains("shell"));
        assert!(out.contains("cost"));
        assert!(out.contains("OK"));
    }
}
