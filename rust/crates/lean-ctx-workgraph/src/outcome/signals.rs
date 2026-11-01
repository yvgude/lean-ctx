// SPDX-License-Identifier: Apache-2.0

//! Local signal values and adapters for command/test/CI observations.

use serde::{Deserialize, Serialize};

/// Signal kinds accepted by local outcome contracts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalType {
    BuildSuccess,
    TestsPassing,
    LintClean,
    TypecheckPassing,
    AgentCompletion,
    RetryCount,
    HumanAcceptance,
    PrMerge,
    CiPassing,
    Correction,
    Rollback,
}

impl SignalType {
    /// Stable snake-case name used in explanations and serialized values.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BuildSuccess => "build_success",
            Self::TestsPassing => "tests_passing",
            Self::LintClean => "lint_clean",
            Self::TypecheckPassing => "typecheck_passing",
            Self::AgentCompletion => "agent_completion",
            Self::RetryCount => "retry_count",
            Self::HumanAcceptance => "human_acceptance",
            Self::PrMerge => "pr_merge",
            Self::CiPassing => "ci_passing",
            Self::Correction => "correction",
            Self::Rollback => "rollback",
        }
    }
}

impl std::fmt::Display for SignalType {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A local observation and optional evidence metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomeSignal {
    pub signal_type: SignalType,
    pub value: SignalValue,
    pub evidence_ref: Option<String>,
    pub observed_at: Option<String>,
}

impl OutcomeSignal {
    /// Create an observation without optional metadata.
    pub fn new(signal_type: SignalType, value: SignalValue) -> Self {
        Self {
            signal_type,
            value,
            evidence_ref: None,
            observed_at: None,
        }
    }

    /// Create an observation with evidence and observation-time metadata.
    pub fn with_metadata(
        signal_type: SignalType,
        value: SignalValue,
        evidence_ref: Option<String>,
        observed_at: Option<String>,
    ) -> Self {
        Self {
            signal_type,
            value,
            evidence_ref,
            observed_at,
        }
    }

    /// Attach an evidence reference.
    pub fn with_evidence_ref(mut self, evidence_ref: impl Into<String>) -> Self {
        self.evidence_ref = Some(evidence_ref.into());
        self
    }

    /// Attach an observation timestamp.
    pub fn with_observed_at(mut self, observed_at: impl Into<String>) -> Self {
        self.observed_at = Some(observed_at.into());
        self
    }

    /// Create an explicitly unobserved signal.
    pub fn unknown(signal_type: SignalType) -> Self {
        Self::new(signal_type, SignalValue::Unknown)
    }
}

/// Value carried by a local signal adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalValue {
    Boolean(bool),
    Count(u32),
    Unknown,
}

/// Namespace for adapters from local command and lifecycle observations.
pub struct LocalSignalAdapters;

impl LocalSignalAdapters {
    /// Adapt a boolean result for any signal type.
    pub fn boolean(signal_type: SignalType, value: bool) -> OutcomeSignal {
        OutcomeSignal::new(signal_type, SignalValue::Boolean(value))
    }

    /// Adapt a count result for any signal type.
    pub fn count(signal_type: SignalType, value: u32) -> OutcomeSignal {
        OutcomeSignal::new(signal_type, SignalValue::Count(value))
    }

    /// Adapt an unobserved signal.
    pub fn unknown(signal_type: SignalType) -> OutcomeSignal {
        OutcomeSignal::unknown(signal_type)
    }

    pub fn build_success(value: bool) -> OutcomeSignal {
        Self::boolean(SignalType::BuildSuccess, value)
    }

    pub fn tests_passing(value: bool) -> OutcomeSignal {
        Self::boolean(SignalType::TestsPassing, value)
    }

    /// Adapt the positive delta in test count for a test-addition contract.
    pub fn test_count_increased(delta: u32) -> OutcomeSignal {
        Self::count(SignalType::TestsPassing, delta)
    }

    pub fn lint_clean(value: bool) -> OutcomeSignal {
        Self::boolean(SignalType::LintClean, value)
    }

    pub fn typecheck_passing(value: bool) -> OutcomeSignal {
        Self::boolean(SignalType::TypecheckPassing, value)
    }

    pub fn agent_completion(value: bool) -> OutcomeSignal {
        Self::boolean(SignalType::AgentCompletion, value)
    }

    pub fn retry_count(value: u32) -> OutcomeSignal {
        Self::count(SignalType::RetryCount, value)
    }

    pub fn human_acceptance(value: bool) -> OutcomeSignal {
        Self::boolean(SignalType::HumanAcceptance, value)
    }

    pub fn pr_merge(value: bool) -> OutcomeSignal {
        Self::boolean(SignalType::PrMerge, value)
    }

    pub fn ci_passing(value: bool) -> OutcomeSignal {
        Self::boolean(SignalType::CiPassing, value)
    }

    pub fn correction(value: bool) -> OutcomeSignal {
        Self::boolean(SignalType::Correction, value)
    }

    pub fn rollback(value: bool) -> OutcomeSignal {
        Self::boolean(SignalType::Rollback, value)
    }
}

pub fn adapt_boolean(signal_type: SignalType, value: bool) -> OutcomeSignal {
    LocalSignalAdapters::boolean(signal_type, value)
}

pub fn adapt_count(signal_type: SignalType, value: u32) -> OutcomeSignal {
    LocalSignalAdapters::count(signal_type, value)
}

pub fn adapt_build_success(value: bool) -> OutcomeSignal {
    LocalSignalAdapters::build_success(value)
}

pub fn adapt_tests_passing(value: bool) -> OutcomeSignal {
    LocalSignalAdapters::tests_passing(value)
}

pub fn adapt_test_count_increased(delta: u32) -> OutcomeSignal {
    LocalSignalAdapters::test_count_increased(delta)
}

pub fn adapt_lint_clean(value: bool) -> OutcomeSignal {
    LocalSignalAdapters::lint_clean(value)
}

pub fn adapt_typecheck_passing(value: bool) -> OutcomeSignal {
    LocalSignalAdapters::typecheck_passing(value)
}

pub fn adapt_agent_completion(value: bool) -> OutcomeSignal {
    LocalSignalAdapters::agent_completion(value)
}

pub fn adapt_retry_count(value: u32) -> OutcomeSignal {
    LocalSignalAdapters::retry_count(value)
}

pub fn adapt_human_acceptance(value: bool) -> OutcomeSignal {
    LocalSignalAdapters::human_acceptance(value)
}

pub fn adapt_pr_merge(value: bool) -> OutcomeSignal {
    LocalSignalAdapters::pr_merge(value)
}

pub fn adapt_ci_passing(value: bool) -> OutcomeSignal {
    LocalSignalAdapters::ci_passing(value)
}

pub fn adapt_correction(value: bool) -> OutcomeSignal {
    LocalSignalAdapters::correction(value)
}

pub fn adapt_rollback(value: bool) -> OutcomeSignal {
    LocalSignalAdapters::rollback(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapters_preserve_signal_kind_and_value() {
        assert_eq!(
            LocalSignalAdapters::build_success(true),
            OutcomeSignal::new(SignalType::BuildSuccess, SignalValue::Boolean(true))
        );
        assert_eq!(
            LocalSignalAdapters::retry_count(2).value,
            SignalValue::Count(2)
        );
        assert_eq!(
            LocalSignalAdapters::test_count_increased(1).signal_type,
            SignalType::TestsPassing
        );
    }
}
