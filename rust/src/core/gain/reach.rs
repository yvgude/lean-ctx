// SPDX-License-Identifier: Apache-2.0
//! Reach: how much of the agent's *observed* tool traffic went through lean-ctx.
//!
//! The denominator is only what lean-ctx can see — calls routed through it plus
//! native shell calls a hook observed and let pass. Native tool calls that bypass
//! every hook are invisible, so reach is never a share of all agent activity.
//! A pure function of observed counts; a host capability model can attach to it.

use serde::Serialize;

/// Observed call counts for one window.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReachCounts {
    pub routed_calls: u64,
    pub native_passthrough_calls: u64,
}

/// Native calls no hook saw. Their number cannot be known locally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Unobserved {
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReachView {
    pub routed_calls: u64,
    pub native_passthrough_calls: u64,
    /// The denominator: routed + observed native passthrough.
    pub observed_calls: u64,
    /// `routed / observed` in percent; `None` when nothing was observed.
    pub routed_pct_of_observed: Option<f64>,
    pub unobserved_native_calls: Unobserved,
}

#[must_use]
pub fn reach_view(counts: ReachCounts) -> ReachView {
    let observed_calls = counts
        .routed_calls
        .saturating_add(counts.native_passthrough_calls);
    ReachView {
        routed_calls: counts.routed_calls,
        native_passthrough_calls: counts.native_passthrough_calls,
        observed_calls,
        routed_pct_of_observed: (observed_calls > 0)
            .then(|| counts.routed_calls as f64 * 100.0 / observed_calls as f64),
        unobserved_native_calls: Unobserved::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn share_is_of_observed_calls_and_absent_without_observations() {
        let none = reach_view(ReachCounts::default());
        assert_eq!(none.observed_calls, 0);
        assert_eq!(none.routed_pct_of_observed, None);

        let view = reach_view(ReachCounts {
            routed_calls: 9,
            native_passthrough_calls: 1,
        });
        assert_eq!(view.observed_calls, 10);
        assert_eq!(view.routed_pct_of_observed, Some(90.0));
        let json = serde_json::to_value(&view).unwrap();
        assert_eq!(json["unobserved_native_calls"], "unknown");
        assert_eq!(json["routed_pct_of_observed"], 90.0);
    }
}
