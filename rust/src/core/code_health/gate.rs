//! Edit-time code-health gate — shared by `ctx_edit`, `ctx_patch`, and the
//! native-edit hook.
//!
//! Drift prevention is the lever lean-ctx has that a post-hoc scanner does not:
//! it sits *in the edit path*, so it can catch a complexity regression at the
//! moment it is introduced. The gate is pure except for reading `[code_health]`
//! config, and its notice text is deterministic (#498-safe).

use super::{GateMode, cognitive_delta, format_gate_notice, worst_regression};
use crate::core::config::Config;

/// Decision of the edit-gate for one edit.
pub enum GateOutcome {
    /// Allow the write; if `Some`, append this advisory notice to the output.
    Allow(Option<String>),
    /// Block the write with this reason (only in `gate="block"` mode).
    Block(String),
}

/// Evaluate the gate for an edit from `old`→`new` source in a file with `ext`,
/// using the active `[code_health]` config (threshold + mode).
pub fn evaluate(old: &str, new: &str, ext: &str) -> GateOutcome {
    let cfg = Config::load();
    evaluate_with(
        old,
        new,
        ext,
        GateMode::parse(&cfg.code_health.gate),
        cfg.code_health.cognitive_threshold,
    )
}

/// Pure gate evaluation with explicit `mode`/`threshold` — the unit-tested core.
pub fn evaluate_with(
    old: &str,
    new: &str,
    ext: &str,
    mode: GateMode,
    threshold: u32,
) -> GateOutcome {
    if matches!(mode, GateMode::Off) {
        return GateOutcome::Allow(None);
    }
    let deltas = cognitive_delta(old, new, ext);
    let Some(worst) = worst_regression(&deltas, threshold) else {
        return GateOutcome::Allow(None);
    };
    let notice = format_gate_notice(worst, threshold);
    // Block only a genuine clean→over-threshold regression; otherwise advise.
    if matches!(mode, GateMode::Block) && worst.crosses_threshold(threshold) {
        GateOutcome::Block(format!(
            "{notice}\n(set [code_health] gate=\"warn\" to allow)"
        ))
    } else {
        GateOutcome::Allow(Some(notice))
    }
}

#[cfg(all(test, feature = "tree-sitter"))]
mod tests {
    use super::*;

    const FLAT: &str = "fn f(a: bool) { if a {} }";
    // 1+2+3+4+5+6 = 21 cognitive → over the default threshold of 15.
    const DEEP: &str = "fn f(a: bool) { if a { if a { if a { if a { if a { if a {} } } } } } }";

    #[test]
    fn off_mode_allows_silently() {
        match evaluate_with(FLAT, DEEP, "rs", GateMode::Off, 15) {
            GateOutcome::Allow(None) => {}
            _ => panic!("off mode must allow with no notice"),
        }
    }

    #[test]
    fn warn_mode_allows_with_notice() {
        match evaluate_with(FLAT, DEEP, "rs", GateMode::Warn, 15) {
            GateOutcome::Allow(Some(notice)) => assert!(notice.contains("[CODE HEALTH]")),
            _ => panic!("warn mode must allow with a notice"),
        }
    }

    #[test]
    fn block_mode_blocks_threshold_crossing() {
        match evaluate_with(FLAT, DEEP, "rs", GateMode::Block, 15) {
            GateOutcome::Block(reason) => assert!(reason.contains("[CODE HEALTH]")),
            GateOutcome::Allow(_) => panic!("block mode must block a clean→over edit"),
        }
    }

    #[test]
    fn no_regression_allows_silently() {
        match evaluate_with(FLAT, FLAT, "rs", GateMode::Block, 15) {
            GateOutcome::Allow(None) => {}
            _ => panic!("unchanged complexity must allow silently"),
        }
    }
}
