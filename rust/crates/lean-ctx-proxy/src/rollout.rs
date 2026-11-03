// SPDX-License-Identifier: Apache-2.0
//! Public rollout cohort schema and deterministic assignment reference.
//!
//! The protocol implementation hashes task IDs and returns cohort metadata;
//! it never evaluates eligibility or makes a model/provider decision.
//! Production rollout intelligence remains in `lean-ctx-enterprise` (Class D).

pub use lean_ctx_protocol::rollout::{
    RolloutAssignment, RolloutCohort, RolloutConfig, assign_rollout,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_exposes_deterministic_assignment() {
        let config = RolloutConfig {
            cohorts: vec![RolloutCohort {
                name: "internal".to_owned(),
                percentage: 100,
                eligibility_override: false,
                kill_switch: false,
            }],
        };
        let first = assign_rollout("task-1", &config);
        assert_eq!(first, config.assign("task-1"));
        assert_eq!(first.cohort.as_deref(), Some("internal"));
    }
}
