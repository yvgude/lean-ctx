//! Nudge economy for budget advisories (#1570 P2, DCP-inspired).
//!
//! The pre-#1570 `[BUDGET WARNING]` footer fired on every call once a
//! dimension crossed its warning band — agents learned to ignore it (#1542
//! "advisory verpufft"). This module replaces fire-and-forget warnings with
//! an *economy*:
//!
//! - **Stepped thresholds** per dimension (75% soft, 100% strong, 150%
//!   strong + recovery script), evaluated on the existing
//!   [`BudgetSnapshot`] percentages.
//! - **Anchor dedup**: each (dimension, step) fires exactly once while the
//!   usage stays above the step. No repetition spam.
//! - **Hysteresis**: dropping 5 points below a step re-arms it, so a real
//!   re-crossing nudges again.
//! - **Reset-on-action** (#1570 P6 guard): a recovery tool call
//!   (`ctx_compress`, `ctx_dedup`) clears all anchors *and* starts a short
//!   cooldown, so the agent is never nudged right after it just acted —
//!   the "models become obsessed with pruning" failure mode reported
//!   against DCP (#372 there).
//!
//! Advisory by design: nothing here ever blocks a tool call (#1542).

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

use crate::core::budget_tracker::BudgetSnapshot;

/// Calls suppressed after a recovery action before nudging resumes.
const RECOVERY_COOLDOWN_CALLS: u32 = 3;

/// Re-arm margin: a step re-fires only after usage drops this many
/// percentage points below it.
const HYSTERESIS_PCT: f64 = 5.0;

/// (threshold in percent, strong?) — evaluated highest-first.
const STEPS: &[(f64, bool)] = &[(150.0, true), (100.0, true), (75.0, false)];

/// Tools whose successful use counts as budget recovery.
pub const RECOVERY_TOOLS: &[&str] = &["ctx_compress", "ctx_dedup"];

#[derive(Default)]
struct NudgeState {
    /// (dimension, step-index) anchors that already fired.
    fired: HashSet<(&'static str, usize)>,
    cooldown_calls_left: u32,
}

fn state() -> &'static Mutex<NudgeState> {
    static STATE: OnceLock<Mutex<NudgeState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(NudgeState::default()))
}

/// Record a recovery action: clear all anchors and pause nudging briefly.
pub fn record_recovery_action(tool: &str) {
    if !RECOVERY_TOOLS.contains(&tool) {
        return;
    }
    let mut guard = state()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard.fired.clear();
    guard.cooldown_calls_left = RECOVERY_COOLDOWN_CALLS;
}

/// Test/reset hook (session reset).
pub fn reset() {
    let mut guard = state()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard.fired.clear();
    guard.cooldown_calls_left = 0;
}

/// Evaluate the snapshot against the step ladder. Returns at most one nudge
/// line per call — the strongest newly-crossed step across all dimensions.
pub fn budget_nudge(snap: &BudgetSnapshot) -> Option<String> {
    let mut guard = state()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if guard.cooldown_calls_left > 0 {
        guard.cooldown_calls_left -= 1;
        return None;
    }

    let dimensions: [(&'static str, f64, String); 3] = [
        (
            "tokens",
            snap.tokens.percent.into(),
            format!("{}/{}", snap.tokens.used, snap.tokens.limit),
        ),
        (
            "shell",
            snap.shell.percent.into(),
            format!("{}/{}", snap.shell.used, snap.shell.limit),
        ),
        (
            "cost",
            snap.cost.percent.into(),
            format!("${:.2}/${:.2}", snap.cost.used_usd, snap.cost.limit_usd),
        ),
    ];

    // Hysteresis re-arm: forget anchors the usage has clearly dropped below.
    for (dim, pct, _) in &dimensions {
        for (idx, (threshold, _)) in STEPS.iter().enumerate() {
            if *pct < threshold - HYSTERESIS_PCT {
                guard.fired.remove(&(*dim, idx));
            }
        }
    }

    // Strongest newly-crossed step wins; one nudge per call, ever.
    for (idx, (threshold, strong)) in STEPS.iter().enumerate() {
        for (dim, pct, usage) in &dimensions {
            if *pct >= *threshold && guard.fired.insert((*dim, idx)) {
                let line = if *strong {
                    format!(
                        "[BUDGET] {dim} at {pct:.0}% ({usage}) — advisory, nothing is blocked. \
                         Recover: ctx_compress (compact context), ctx_dedup (drop repeated reads), \
                         or ctx_session action=reset for a fresh budget."
                    )
                } else {
                    format!(
                        "[budget nudge] {dim} at {pct:.0}% ({usage}) — consider ctx_compress or \
                         dropping stale reads soon (advisory)."
                    )
                };
                return Some(line);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::budget_tracker::BudgetTracker;

    /// Build a snapshot with a controlled token percentage by driving the
    /// global tracker. Serialized via the shared test env lock — the tracker
    /// and nudge state are process-global.
    fn snapshot_with_tokens(used: usize) -> BudgetSnapshot {
        let tracker = BudgetTracker::global();
        tracker.reset();
        tracker.record_tokens(used as u64);
        tracker.check()
    }

    #[test]
    fn nudge_ladder_fires_once_per_step_escalates_and_resets_on_action() {
        let _lock = crate::core::data_dir::test_env_lock();
        reset();
        let limit = snapshot_with_tokens(0).tokens.limit;
        if limit == 0 {
            // No token limit configured in this environment — ladder cannot
            // be exercised meaningfully.
            return;
        }

        // 80% -> soft nudge fires exactly once.
        let snap = snapshot_with_tokens(limit * 8 / 10);
        let first = budget_nudge(&snap);
        assert!(
            first
                .as_deref()
                .is_some_and(|s| s.contains("[budget nudge]")),
            "{first:?}"
        );
        assert_eq!(
            budget_nudge(&snap),
            None,
            "anchor dedup: same step never repeats"
        );

        // 105% -> escalation to strong, again exactly once.
        let snap = snapshot_with_tokens(limit + limit / 20);
        let strong = budget_nudge(&snap);
        assert!(
            strong.as_deref().is_some_and(|s| s.contains("[BUDGET]")),
            "{strong:?}"
        );
        assert_eq!(budget_nudge(&snap), None);

        // Recovery action clears anchors and starts the cooldown.
        record_recovery_action("ctx_compress");
        for _ in 0..RECOVERY_COOLDOWN_CALLS {
            assert_eq!(budget_nudge(&snap), None, "cooldown suppresses nudges");
        }
        // After the cooldown the (re-armed) strong step fires again.
        assert!(
            budget_nudge(&snap).is_some(),
            "anchors re-armed after recovery"
        );

        BudgetTracker::global().reset();
        reset();
    }

    #[test]
    fn non_recovery_tools_do_not_reset_anchors() {
        let _lock = crate::core::data_dir::test_env_lock();
        reset();
        let limit = snapshot_with_tokens(0).tokens.limit;
        if limit == 0 {
            return;
        }
        let snap = snapshot_with_tokens(limit * 8 / 10);
        assert!(budget_nudge(&snap).is_some());
        record_recovery_action("ctx_read");
        assert_eq!(
            budget_nudge(&snap),
            None,
            "ctx_read is not a recovery action"
        );
        BudgetTracker::global().reset();
        reset();
    }
}
