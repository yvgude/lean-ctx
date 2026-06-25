//! Homeostatic Memory Guard — Proactive multi-level resource management.
//!
//! Scientific basis: Homeostasis (biological systems) — maintains equilibrium by
//! continuously monitoring internal state and applying graduated corrective responses.
//! Unlike reactive systems that only respond to crisis, homeostasis proactively
//! maintains optimal operating conditions through negative feedback loops.
//!
//! 4 escalation levels based on memory pressure:
//! Level 1 (70%): Trim cached outputs (soft pressure)
//! Level 2 (80%): Evict probationary cache entries
//! Level 3 (90%): Unload indices (BM25, embeddings)
//! Level 4 (95%): Aggressive eviction of protected entries

/// Pressure levels as fraction of budget consumed (0.0 - 1.0).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PressureLevel {
    Nominal,   // < 70%
    Elevated,  // 70-80%
    High,      // 80-90%
    Critical,  // 90-95%
    Emergency, // > 95%
}

impl PressureLevel {
    #[must_use]
    pub fn from_utilization(util: f64) -> Self {
        if util >= 0.95 {
            Self::Emergency
        } else if util >= 0.90 {
            Self::Critical
        } else if util >= 0.80 {
            Self::High
        } else if util >= 0.70 {
            Self::Elevated
        } else {
            Self::Nominal
        }
    }
}

/// Actions the homeostasis system can recommend.
#[derive(Debug, Clone, PartialEq)]
pub enum HomeostasisAction {
    /// No action needed.
    None,
    /// Trim compressed outputs in cache (max 2 per entry instead of 3).
    TrimOutputs,
    /// Evict probationary cache entries (`read_count` <= 1).
    EvictProbationary { target_tokens: usize },
    /// Unload heavy indices from memory.
    UnloadIndices,
    /// Evict protected cache entries with lowest Boltzmann energy.
    EvictProtected { target_tokens: usize },
    /// Emergency: drop all non-essential structures.
    EmergencyDrop,
}

/// Homeostatic controller with feedback loop.
pub struct HomeostasisController {
    /// Last observed pressure level.
    last_level: PressureLevel,
    /// Number of consecutive cycles at the same or higher level (hysteresis).
    consecutive_at_level: u32,
    /// Token budget (max cache size in tokens).
    budget_tokens: usize,
    /// Whether the last action successfully reduced pressure.
    last_action_effective: bool,
}

impl HomeostasisController {
    #[must_use]
    pub fn new(budget_tokens: usize) -> Self {
        Self {
            last_level: PressureLevel::Nominal,
            consecutive_at_level: 0,
            budget_tokens,
            last_action_effective: true,
        }
    }

    /// Evaluate current state and determine the appropriate action.
    /// The feedback loop: if the last action didn't help, escalate faster.
    pub fn evaluate(&mut self, current_tokens: usize) -> HomeostasisAction {
        let util = if self.budget_tokens == 0 {
            0.0
        } else {
            current_tokens as f64 / self.budget_tokens as f64
        };

        let level = PressureLevel::from_utilization(util);

        // Hysteresis: track consecutive cycles at elevated+ levels
        if level as u8 >= self.last_level as u8 && level != PressureLevel::Nominal {
            self.consecutive_at_level += 1;
        } else {
            self.consecutive_at_level = 0;
            self.last_action_effective = true;
        }

        self.last_level = level;

        // If last action didn't help and we're still under pressure, escalate
        let escalate = !self.last_action_effective && self.consecutive_at_level > 2;

        match level {
            PressureLevel::Nominal => HomeostasisAction::None,
            PressureLevel::Elevated => {
                if escalate {
                    HomeostasisAction::EvictProbationary {
                        target_tokens: self.target_free(0.60),
                    }
                } else {
                    HomeostasisAction::TrimOutputs
                }
            }
            PressureLevel::High => HomeostasisAction::EvictProbationary {
                target_tokens: self.target_free(0.70),
            },
            PressureLevel::Critical => {
                if escalate {
                    HomeostasisAction::EvictProtected {
                        target_tokens: self.target_free(0.75),
                    }
                } else {
                    HomeostasisAction::UnloadIndices
                }
            }
            PressureLevel::Emergency => HomeostasisAction::EmergencyDrop,
        }
    }

    /// Report whether the last action was effective (reduced pressure).
    pub fn report_outcome(&mut self, pressure_reduced: bool) {
        self.last_action_effective = pressure_reduced;
    }

    /// Calculate token target to free down to a given utilization fraction.
    fn target_free(&self, target_util: f64) -> usize {
        (self.budget_tokens as f64 * target_util) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nominal_pressure_no_action() {
        let mut ctrl = HomeostasisController::new(100_000);
        let action = ctrl.evaluate(50_000); // 50% utilization
        assert_eq!(action, HomeostasisAction::None);
    }

    #[test]
    fn elevated_pressure_trims_outputs() {
        let mut ctrl = HomeostasisController::new(100_000);
        let action = ctrl.evaluate(72_000); // 72% utilization
        assert_eq!(action, HomeostasisAction::TrimOutputs);
    }

    #[test]
    fn high_pressure_evicts_probationary() {
        let mut ctrl = HomeostasisController::new(100_000);
        let action = ctrl.evaluate(85_000); // 85% utilization
        assert!(matches!(
            action,
            HomeostasisAction::EvictProbationary { .. }
        ));
    }

    #[test]
    fn critical_pressure_unloads_indices() {
        let mut ctrl = HomeostasisController::new(100_000);
        let action = ctrl.evaluate(92_000); // 92% utilization
        assert_eq!(action, HomeostasisAction::UnloadIndices);
    }

    #[test]
    fn emergency_drops_everything() {
        let mut ctrl = HomeostasisController::new(100_000);
        let action = ctrl.evaluate(96_000); // 96% utilization
        assert_eq!(action, HomeostasisAction::EmergencyDrop);
    }

    #[test]
    fn escalation_on_ineffective_action() {
        let mut ctrl = HomeostasisController::new(100_000);

        // Sustained critical pressure without relief
        ctrl.evaluate(92_000);
        ctrl.report_outcome(false);
        ctrl.evaluate(92_000);
        ctrl.report_outcome(false);
        ctrl.evaluate(92_000);
        ctrl.report_outcome(false);
        let action = ctrl.evaluate(92_000);

        // Should escalate to EvictProtected since UnloadIndices didn't work
        assert!(matches!(action, HomeostasisAction::EvictProtected { .. }));
    }

    #[test]
    fn recovery_resets_escalation() {
        let mut ctrl = HomeostasisController::new(100_000);

        // Build up pressure
        ctrl.evaluate(92_000);
        ctrl.report_outcome(false);
        ctrl.evaluate(92_000);

        // Pressure drops
        let action = ctrl.evaluate(50_000);
        assert_eq!(action, HomeostasisAction::None);

        // Next time pressure rises, starts fresh (no immediate escalation)
        let action = ctrl.evaluate(72_000);
        assert_eq!(action, HomeostasisAction::TrimOutputs);
    }
}
