// SPDX-License-Identifier: Apache-2.0

//! Versioned acceptance contracts for coding task classes.

use serde::{Deserialize, Serialize};

use super::signals::SignalType;

/// Current local outcome-contract version.
pub const OUTCOME_CONTRACT_VERSION: &str = "1.0.0";

/// Coding task class used to select an acceptance policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskClass {
    BugFix,
    Refactor,
    TestAddition,
    Documentation,
    Investigation,
}

impl TaskClass {
    /// Stable snake-case identifier used in contract references.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BugFix => "bug_fix",
            Self::Refactor => "refactor",
            Self::TestAddition => "test_addition",
            Self::Documentation => "documentation",
            Self::Investigation => "investigation",
        }
    }
}

impl std::fmt::Display for TaskClass {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One signal and its contribution to the contract's acceptance score.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignalRequirement {
    pub signal_type: SignalType,
    pub required: bool,
    /// Contribution in milli-units; canonical constructors keep this in 0..=1000.
    pub weight_milli: u32,
}

impl SignalRequirement {
    /// Build a required signal requirement.
    pub const fn required(signal_type: SignalType, weight_milli: u32) -> Self {
        Self {
            signal_type,
            required: true,
            weight_milli,
        }
    }

    /// Build an optional signal requirement.
    pub const fn optional(signal_type: SignalType, weight_milli: u32) -> Self {
        Self {
            signal_type,
            required: false,
            weight_milli,
        }
    }

    /// Build a requirement with explicit requiredness.
    pub const fn new(signal_type: SignalType, required: bool, weight_milli: u32) -> Self {
        Self {
            signal_type,
            required,
            weight_milli,
        }
    }
}

/// Acceptance policy for one coding-task class.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomeContractV1 {
    pub task_class: TaskClass,
    pub contract_version: String,
    pub required_signals: Vec<SignalRequirement>,
    pub optional_signals: Vec<SignalRequirement>,
    pub expiry_window_hours: u32,
    pub supersession_allowed: bool,
}

impl OutcomeContractV1 {
    /// Return the canonical contract for a task class.
    pub fn for_task_class(task_class: TaskClass) -> Self {
        match task_class {
            TaskClass::BugFix => bug_fix_contract(),
            TaskClass::Refactor => refactor_contract(),
            TaskClass::TestAddition => test_addition_contract(),
            TaskClass::Documentation => documentation_contract(),
            TaskClass::Investigation => investigation_contract(),
        }
    }

    /// Stable reference used by accepted outcomes.
    pub fn reference(&self) -> String {
        format!(
            "outcome-contract/{}/v{}",
            self.task_class.as_str(),
            self.contract_version
        )
    }

    /// Validate the local contract invariants.
    pub fn validate(&self) -> Result<(), String> {
        if self.contract_version.trim().is_empty() {
            return Err("contract_version must not be empty".to_owned());
        }

        if self
            .required_signals
            .iter()
            .chain(self.optional_signals.iter())
            .any(|requirement| requirement.weight_milli > 1000)
        {
            return Err("signal weight_milli must be between 0 and 1000".to_owned());
        }

        if self
            .optional_signals
            .iter()
            .any(|requirement| requirement.required)
        {
            return Err("optional signal requirements must set required=false".to_owned());
        }

        Ok(())
    }
}

/// Canonical bug-fix contract: build and tests must pass; PR merge is optional.
pub fn bug_fix_contract() -> OutcomeContractV1 {
    OutcomeContractV1 {
        task_class: TaskClass::BugFix,
        contract_version: OUTCOME_CONTRACT_VERSION.to_owned(),
        required_signals: vec![
            SignalRequirement::required(SignalType::BuildSuccess, 500),
            SignalRequirement::required(SignalType::TestsPassing, 500),
        ],
        optional_signals: vec![SignalRequirement::optional(SignalType::PrMerge, 100)],
        expiry_window_hours: 168,
        supersession_allowed: true,
    }
}

/// Short alias for [`bug_fix_contract`].
pub fn bug_fix() -> OutcomeContractV1 {
    bug_fix_contract()
}

/// Canonical refactor contract: build, tests, and lint must pass.
pub fn refactor_contract() -> OutcomeContractV1 {
    OutcomeContractV1 {
        task_class: TaskClass::Refactor,
        contract_version: OUTCOME_CONTRACT_VERSION.to_owned(),
        required_signals: vec![
            SignalRequirement::required(SignalType::BuildSuccess, 334),
            SignalRequirement::required(SignalType::TestsPassing, 333),
            SignalRequirement::required(SignalType::LintClean, 333),
        ],
        optional_signals: vec![SignalRequirement::optional(
            SignalType::TypecheckPassing,
            100,
        )],
        expiry_window_hours: 168,
        supersession_allowed: true,
    }
}

/// Short alias for [`refactor_contract`].
pub fn refactor() -> OutcomeContractV1 {
    refactor_contract()
}

/// Canonical test-addition contract.
///
/// The two `TestsPassing` requirements intentionally distinguish a boolean
/// passing result from a positive count delta. The evaluator uses the first
/// occurrence for the suite result and the second for the count adapter.
pub fn test_addition_contract() -> OutcomeContractV1 {
    OutcomeContractV1 {
        task_class: TaskClass::TestAddition,
        contract_version: OUTCOME_CONTRACT_VERSION.to_owned(),
        required_signals: vec![
            SignalRequirement::required(SignalType::TestsPassing, 600),
            SignalRequirement::required(SignalType::TestsPassing, 400),
        ],
        optional_signals: vec![SignalRequirement::optional(SignalType::CiPassing, 100)],
        expiry_window_hours: 336,
        supersession_allowed: true,
    }
}

/// Short alias for [`test_addition_contract`].
pub fn test_addition() -> OutcomeContractV1 {
    test_addition_contract()
}

/// Canonical documentation contract: the documentation build must pass.
pub fn documentation_contract() -> OutcomeContractV1 {
    OutcomeContractV1 {
        task_class: TaskClass::Documentation,
        contract_version: OUTCOME_CONTRACT_VERSION.to_owned(),
        required_signals: vec![SignalRequirement::required(SignalType::BuildSuccess, 1000)],
        optional_signals: Vec::new(),
        expiry_window_hours: 720,
        supersession_allowed: true,
    }
}

/// Short alias for [`documentation_contract`].
pub fn documentation() -> OutcomeContractV1 {
    documentation_contract()
}

/// Canonical investigation contract.
///
/// The required human-acceptance requirement is evaluated as an explicit
/// alternative to `AgentCompletion`; the evaluator documents that alternative
/// in the contribution explanation.
pub fn investigation_contract() -> OutcomeContractV1 {
    OutcomeContractV1 {
        task_class: TaskClass::Investigation,
        contract_version: OUTCOME_CONTRACT_VERSION.to_owned(),
        required_signals: vec![SignalRequirement::required(
            SignalType::HumanAcceptance,
            1000,
        )],
        optional_signals: vec![SignalRequirement::optional(
            SignalType::AgentCompletion,
            1000,
        )],
        expiry_window_hours: 168,
        supersession_allowed: true,
    }
}

/// Short alias for [`investigation_contract`].
pub fn investigation() -> OutcomeContractV1 {
    investigation_contract()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_contracts_have_distinct_task_classes() {
        let contracts = [
            bug_fix_contract(),
            refactor_contract(),
            test_addition_contract(),
            documentation_contract(),
            investigation_contract(),
        ];

        assert_eq!(
            contracts
                .iter()
                .map(|contract| contract.task_class)
                .collect::<std::collections::HashSet<_>>()
                .len(),
            5
        );
        assert!(contracts.iter().all(|contract| contract.validate().is_ok()));
    }
}
