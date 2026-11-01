// SPDX-License-Identifier: Apache-2.0

//! Outcome contracts, local signal adapters, and deterministic evaluation.
//!
//! The protocol crate owns the wire representation. This module owns the
//! task-class policy used to turn local observations into that representation.

pub mod contracts;
pub mod evaluator;
pub mod fingerprint;
pub mod signals;

pub use contracts::{
    OUTCOME_CONTRACT_VERSION, OutcomeContractV1, SignalRequirement, TaskClass, bug_fix,
    bug_fix_contract, documentation, documentation_contract, investigation, investigation_contract,
    refactor, refactor_contract, test_addition, test_addition_contract,
};
pub use evaluator::{
    ContributionStatus, EVALUATOR_VERSION, EvaluationContext, EvaluationResult, OutcomeEvaluation,
    OutcomeEvaluator, OutcomeHistory, OutcomeLedger, OutcomeRecord, PolicyInput, PolicyViolation,
    SignalContribution, evaluate, evaluate_detailed, evaluate_detailed_with_context,
    evaluate_detailed_with_policy, evaluate_for_task, evaluate_with_policy,
};
pub use fingerprint::TaskFingerprint;
pub use signals::{LocalSignalAdapters, OutcomeSignal, SignalType, SignalValue};
