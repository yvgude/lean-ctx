//! Evidence tiers: what a quality result is allowed to claim.
//!
//! A tier describes *how* evidence was produced, not whether it was positive. The
//! ordering is by claim strength: a Tier A result shows that the evaluation pipeline
//! works and must never back a Tier D statement such as "LeanCTX keeps answer quality".

use serde::{Deserialize, Serialize};

use crate::core::eval_ab::model::{ModelFingerprint, PROVIDER_RECORDED};

/// Kind of evidence behind a quality result, ordered from weakest to strongest claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceTier {
    /// Tier A — fixture answers; proves the evaluation pipeline runs end to end.
    Mechanism,
    /// Tier B — deterministic invariants (fidelity, retention, recovery, security).
    DeterministicQuality,
    /// Tier C — replay of a previously captured real-model run; detects drift
    /// relative to that run.
    RecordedRegression,
    /// Tier D — live baseline/treatment run against a real model. Only a powered
    /// run of this tier supports an empirical non-inferiority claim.
    LiveTaskEvaluation,
    /// Tier E — outcomes observed in a real deployment (holdouts).
    ProductionOutcome,
}

impl EvidenceTier {
    pub fn code(self) -> &'static str {
        match self {
            Self::Mechanism => "A",
            Self::DeterministicQuality => "B",
            Self::RecordedRegression => "C",
            Self::LiveTaskEvaluation => "D",
            Self::ProductionOutcome => "E",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Mechanism => "mechanism",
            Self::DeterministicQuality => "deterministic quality",
            Self::RecordedRegression => "recorded regression",
            Self::LiveTaskEvaluation => "live task evaluation",
            Self::ProductionOutcome => "production outcome",
        }
    }

    /// Whether a non-regressing result of this tier says anything about task
    /// quality on a real model. Mechanism and deterministic tiers do not.
    pub fn supports_task_quality_claim(self) -> bool {
        self >= Self::RecordedRegression
    }

    /// Tier of a paired model evaluation, derived from where the answers came from.
    ///
    /// Answers produced by a fixture (`provider = recorded` at capture time) can only
    /// show the mechanism works. Replaying a capture of a real model is Tier C; calling
    /// the model live is Tier D.
    pub fn for_model_run(fingerprint: &ModelFingerprint, replayed: bool) -> Self {
        if fingerprint.provider == PROVIDER_RECORDED {
            Self::Mechanism
        } else if replayed {
            Self::RecordedRegression
        } else {
            Self::LiveTaskEvaluation
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::eval_ab::model::{ModelParams, PROVIDER_OPENAI};

    fn fp(provider: &str) -> ModelFingerprint {
        ModelFingerprint {
            provider: provider.into(),
            endpoint: "e".into(),
            params: ModelParams::default(),
        }
    }

    #[test]
    fn fixture_answers_never_exceed_mechanism_tier() {
        assert_eq!(
            EvidenceTier::for_model_run(&fp(PROVIDER_RECORDED), true),
            EvidenceTier::Mechanism
        );
        assert_eq!(
            EvidenceTier::for_model_run(&fp(PROVIDER_RECORDED), false),
            EvidenceTier::Mechanism
        );
    }

    #[test]
    fn replay_of_real_model_is_recorded_tier_and_live_is_tier_d() {
        assert_eq!(
            EvidenceTier::for_model_run(&fp(PROVIDER_OPENAI), true),
            EvidenceTier::RecordedRegression
        );
        assert_eq!(
            EvidenceTier::for_model_run(&fp(PROVIDER_OPENAI), false),
            EvidenceTier::LiveTaskEvaluation
        );
    }
}
