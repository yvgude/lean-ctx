//! Deterministic, versioned outcome evaluation.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use lean_ctx_protocol::{
    AcceptanceState, AcceptedOutcomeV1, EvidenceKind, EvidenceRefV1, OutcomeId, OutcomeSignalsV1,
    SignalState, SignatureStatus, TaskId,
};

use super::contracts::{OutcomeContractV1, TaskClass};
use super::signals::{OutcomeSignal, SignalType, SignalValue};

/// Version of the evaluator algorithm and its reasoning format.
pub const EVALUATOR_VERSION: &str = "1.0.1";

/// Versioned context used to derive a deterministic wire outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvaluationContext {
    pub task_id: String,
    pub contract_version: String,
    pub evaluator_version: String,
}

impl EvaluationContext {
    /// Build evaluation context from stable caller-provided versions.
    pub fn new(
        task_id: impl Into<String>,
        contract_version: impl Into<String>,
        evaluator_version: impl Into<String>,
    ) -> Self {
        Self {
            task_id: task_id.into(),
            contract_version: contract_version.into(),
            evaluator_version: evaluator_version.into(),
        }
    }

    /// Build deterministic context when only signals are available.
    pub fn from_signals(signals: &[OutcomeSignal]) -> Self {
        let task_id = signals
            .iter()
            .filter_map(|signal| signal.evidence_ref.as_deref())
            .find_map(|reference| reference.strip_prefix("task:"))
            .unwrap_or("task-unknown");
        Self::new(task_id, "unknown", EVALUATOR_VERSION)
    }
}

impl Default for EvaluationContext {
    fn default() -> Self {
        Self::new("task-unknown", "unknown", EVALUATOR_VERSION)
    }
}

/// Result of one required or optional signal evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContributionStatus {
    Passed,
    Failed,
    Missing,
    Unknown,
    NotRequired,
    AlternativeSatisfied,
}

/// Explain how one contract requirement contributed to the result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignalContribution {
    pub signal_type: SignalType,
    pub required: bool,
    pub weight_milli: u32,
    pub value: SignalValue,
    pub status: ContributionStatus,
    pub contribution_milli: u32,
    pub explanation: String,
}

/// Detailed evaluator result; `outcome` is the canonical wire observation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomeEvaluation {
    /// Canonical result required by the outcome-evaluation API.
    pub result: AcceptedOutcomeV1,
    /// Deterministic per-signal reasoning entries.
    pub reasoning: Vec<String>,
    /// Signal kinds considered by the evaluator, in input order.
    pub signals_used: Vec<SignalType>,
    /// Compatibility alias for older callers.
    pub outcome: AcceptedOutcomeV1,
    pub contributions: Vec<SignalContribution>,
    pub evaluator_version: String,
    pub policy_violations: Vec<PolicyViolation>,
    /// Set by an outcome history when this result supersedes an older one.
    pub supersedes: Option<String>,
}

impl OutcomeEvaluation {
    /// Return the tri-state result.
    pub const fn state(&self) -> AcceptanceState {
        self.outcome.accepted
    }

    /// Return whether the result is accepted.
    pub const fn is_accepted(&self) -> bool {
        matches!(self.outcome.accepted, AcceptanceState::Accepted)
    }

    /// Return whether required evidence is still missing.
    pub const fn is_unknown(&self) -> bool {
        matches!(self.outcome.accepted, AcceptanceState::Unknown)
    }

    /// Return the human-readable deterministic reasoning string.
    pub fn explanation(&self) -> String {
        self.reasoning.join("; ")
    }
}

/// Compatibility alias for callers that name the detailed result explicitly.
pub type EvaluationResult = OutcomeEvaluation;

/// A policy violation that forces rejection regardless of signal score.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyViolation {
    pub code: String,
    pub message: String,
}

impl PolicyViolation {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

impl From<String> for PolicyViolation {
    fn from(message: String) -> Self {
        Self::new("policy_violation", message)
    }
}

impl From<&str> for PolicyViolation {
    fn from(message: &str) -> Self {
        Self::new("policy_violation", message)
    }
}

/// Input accepted by policy-aware evaluator helpers.
pub trait PolicyInput {
    fn violation_messages(&self) -> Vec<PolicyViolation>;
}

impl PolicyInput for bool {
    fn violation_messages(&self) -> Vec<PolicyViolation> {
        if *self {
            vec![PolicyViolation::from("policy violation")]
        } else {
            Vec::new()
        }
    }
}

impl PolicyInput for Vec<String> {
    fn violation_messages(&self) -> Vec<PolicyViolation> {
        self.iter().cloned().map(PolicyViolation::from).collect()
    }
}

impl PolicyInput for &[String] {
    fn violation_messages(&self) -> Vec<PolicyViolation> {
        self.iter().cloned().map(PolicyViolation::from).collect()
    }
}

impl<'a> PolicyInput for &'a [&'a str] {
    fn violation_messages(&self) -> Vec<PolicyViolation> {
        self.iter()
            .map(|message| PolicyViolation::from(*message))
            .collect()
    }
}

impl PolicyInput for PolicyViolation {
    fn violation_messages(&self) -> Vec<PolicyViolation> {
        vec![self.clone()]
    }
}

impl PolicyInput for Vec<PolicyViolation> {
    fn violation_messages(&self) -> Vec<PolicyViolation> {
        self.clone()
    }
}

impl PolicyInput for &[PolicyViolation] {
    fn violation_messages(&self) -> Vec<PolicyViolation> {
        self.to_vec()
    }
}

/// Deterministic evaluator with no clock, randomness, or mutable global state.
#[derive(Debug, Clone, Copy, Default)]
pub struct OutcomeEvaluator;

impl OutcomeEvaluator {
    pub const fn new() -> Self {
        Self
    }

    /// Evaluate a contract with context derived only from the signal input.
    pub fn evaluate(
        &self,
        contract: &OutcomeContractV1,
        signals: &[OutcomeSignal],
    ) -> OutcomeEvaluation {
        self.evaluate_with_context(contract, signals, EvaluationContext::from_signals(signals))
    }

    /// Evaluate a contract with explicit stable wire identity and timestamp.
    #[allow(clippy::needless_pass_by_value)]
    pub fn evaluate_with_context(
        &self,
        contract: &OutcomeContractV1,
        signals: &[OutcomeSignal],
        context: EvaluationContext,
    ) -> OutcomeEvaluation {
        self.evaluate_internal(contract, signals, context, Vec::new())
    }

    /// Evaluate a contract with policy violations that override signal success.
    #[allow(clippy::needless_pass_by_value)]
    pub fn evaluate_with_policy<P: PolicyInput>(
        &self,
        contract: &OutcomeContractV1,
        signals: &[OutcomeSignal],
        policy: P,
    ) -> OutcomeEvaluation {
        self.evaluate_internal(
            contract,
            signals,
            EvaluationContext::from_signals(signals),
            policy.violation_messages(),
        )
    }

    /// Return only the canonical wire outcome.
    pub fn evaluate_outcome(
        &self,
        contract: &OutcomeContractV1,
        signals: &[OutcomeSignal],
    ) -> AcceptedOutcomeV1 {
        self.evaluate(contract, signals).outcome
    }

    #[allow(clippy::manual_checked_ops)]
    fn evaluate_internal(
        &self,
        contract: &OutcomeContractV1,
        signals: &[OutcomeSignal],
        context: EvaluationContext,
        policy_violations: Vec<PolicyViolation>,
    ) -> OutcomeEvaluation {
        let mut contributions = Vec::new();
        let mut required_occurrences = [0usize; 11];

        for requirement in &contract.required_signals {
            let occurrence = occurrence_index(&mut required_occurrences, requirement.signal_type);
            contributions.push(evaluate_requirement(
                contract.task_class,
                requirement.signal_type,
                requirement.required,
                requirement.weight_milli,
                occurrence,
                signals,
            ));
        }

        for requirement in &contract.optional_signals {
            contributions.push(evaluate_requirement(
                contract.task_class,
                requirement.signal_type,
                false,
                requirement.weight_milli,
                0,
                signals,
            ));
        }

        let total_required_weight: u32 = contributions
            .iter()
            .filter(|item| item.required)
            .map(|item| item.weight_milli)
            .sum();
        let passed_required_weight: u32 = contributions
            .iter()
            .filter(|item| item.required)
            .filter(|item| {
                matches!(
                    item.status,
                    ContributionStatus::Passed | ContributionStatus::AlternativeSatisfied
                )
            })
            .map(|item| item.weight_milli)
            .sum();
        let required_failed = contributions
            .iter()
            .any(|item| item.required && matches!(item.status, ContributionStatus::Failed));
        let required_unknown = contributions.iter().any(|item| {
            item.required
                && matches!(
                    item.status,
                    ContributionStatus::Missing | ContributionStatus::Unknown
                )
        });

        let state = if !policy_violations.is_empty() || required_failed {
            AcceptanceState::Rejected
        } else if required_unknown {
            AcceptanceState::Unknown
        } else {
            AcceptanceState::Accepted
        };

        let quality_score_milli = match state {
            AcceptanceState::Accepted => Some(1000),
            AcceptanceState::Rejected => Some(0),
            // No observation is not a measured zero-quality result.
            AcceptanceState::Unknown if signals.is_empty() => None,
            AcceptanceState::Unknown => Some(if total_required_weight == 0 {
                0
            } else {
                (passed_required_weight.saturating_mul(1000) / total_required_weight).min(1000)
                    as u16
            }),
        };

        let mut reasoning = contributions
            .iter()
            .map(|item| {
                format!(
                    "{}={} (required={}, weight={}, contribution={}): {}",
                    item.signal_type,
                    item.status,
                    item.required,
                    item.weight_milli,
                    item.contribution_milli,
                    item.explanation
                )
            })
            .collect::<Vec<_>>();

        if policy_violations.is_empty() {
            reasoning.push(format!("final state: {}", state.as_str()));
        } else {
            reasoning.push(format!(
                "policy violation overrides signal result: {}",
                policy_violations
                    .iter()
                    .map(|violation| format!("{}: {}", violation.code, violation.message))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            reasoning.push(format!("final state: {}", state.as_str()));
        }

        let outcome_id = context_outcome_id(&context);
        let evidence_refs = if state == AcceptanceState::Unknown {
            Vec::new()
        } else {
            let digest = Sha256::digest(
                serde_json::to_vec(&(contract, signals))
                    .expect("outcome contract and signals are serializable"),
            );
            let digest = digest
                .iter()
                .fold(String::with_capacity(64), |mut output, byte| {
                    use std::fmt::Write;
                    write!(&mut output, "{byte:02x}").expect("writing to a string cannot fail");
                    output
                });
            vec![EvidenceRefV1 {
                schema_version: Some(1),
                kind: EvidenceKind::QualityMeasurement,
                uri: format!("urn:lean-ctx:outcome-evaluation:{}", outcome_id.as_str()),
                digest: format!("sha256:{digest}"),
                signature_status: SignatureStatus::NotSigned,
                media_type: Some("application/json".to_owned()),
                extensions: Default::default(),
            }]
        };
        let outcome = AcceptedOutcomeV1 {
            schema_version: 1,
            outcome_id,
            task_id: make_task_id(&context.task_id),
            accepted: state,
            quality_score_milli,
            signals: project_protocol_signals(signals),
            contract_ref: Some(contract.reference()),
            evidence_refs,
            observed_at: DETERMINISTIC_OBSERVED_AT.to_owned(),
            plan_id: None,
            receipt_id: None,
            decision_refs: Vec::new(),
            extensions: Default::default(),
        };
        let signals_used = signals.iter().map(|signal| signal.signal_type).collect();

        OutcomeEvaluation {
            result: outcome.clone(),
            reasoning,
            signals_used,
            outcome,
            contributions,
            evaluator_version: context.evaluator_version,
            policy_violations,
            supersedes: None,
        }
    }
}

/// Evaluate and return the canonical wire outcome.
pub fn evaluate(contract: &OutcomeContractV1, signals: &[OutcomeSignal]) -> AcceptedOutcomeV1 {
    OutcomeEvaluator::new().evaluate_outcome(contract, signals)
}

/// Evaluate and retain detailed per-signal reasoning.
pub fn evaluate_detailed(
    contract: &OutcomeContractV1,
    signals: &[OutcomeSignal],
) -> OutcomeEvaluation {
    OutcomeEvaluator::new().evaluate(contract, signals)
}

/// Evaluate with explicit outcome/task identity and timestamp.
pub fn evaluate_detailed_with_context(
    contract: &OutcomeContractV1,
    signals: &[OutcomeSignal],
    context: EvaluationContext,
) -> OutcomeEvaluation {
    OutcomeEvaluator::new().evaluate_with_context(contract, signals, context)
}

/// Evaluate with policy violations that always force rejection.
pub fn evaluate_detailed_with_policy<P: PolicyInput>(
    contract: &OutcomeContractV1,
    signals: &[OutcomeSignal],
    policy: P,
) -> OutcomeEvaluation {
    OutcomeEvaluator::new().evaluate_with_policy(contract, signals, policy)
}

/// Evaluate with stable caller-provided identity and timestamp.
pub fn evaluate_for_task(
    contract: &OutcomeContractV1,
    signals: &[OutcomeSignal],
    outcome_id: impl Into<String>,
    task_id: impl Into<String>,
    observed_at: impl Into<String>,
) -> AcceptedOutcomeV1 {
    let wire_outcome_id = outcome_id.into();
    let wire_task_id = task_id.into();
    let observed_at = observed_at.into();
    let mut outcome = OutcomeEvaluator::new()
        .evaluate_with_context(
            contract,
            signals,
            EvaluationContext::new(&wire_task_id, &contract.contract_version, EVALUATOR_VERSION),
        )
        .result;
    outcome.outcome_id = make_outcome_id(&wire_outcome_id);
    outcome.task_id = make_task_id(&wire_task_id);
    outcome.observed_at = observed_at;
    outcome
}

/// Evaluate with policy violations and return only the wire outcome.
pub fn evaluate_with_policy<P: PolicyInput>(
    contract: &OutcomeContractV1,
    signals: &[OutcomeSignal],
    policy: P,
) -> AcceptedOutcomeV1 {
    OutcomeEvaluator::new()
        .evaluate_with_policy(contract, signals, policy)
        .outcome
}

/// Append-only lineage record. A late signal creates a new record and points
/// at the prior record; it never overwrites the prior accepted outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomeRecord {
    pub outcome: AcceptedOutcomeV1,
    pub supersedes: Option<String>,
}

/// In-memory append-only outcome history for local evaluation and tests.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomeHistory {
    records: Vec<OutcomeRecord>,
}

impl OutcomeHistory {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append an outcome, superseding the latest outcome for the same task and
    /// contract when one exists.
    pub fn append(&mut self, mut outcome: AcceptedOutcomeV1) -> OutcomeRecord {
        self.append_internal(&mut outcome, true)
    }

    /// Append using the contract's supersession policy.
    pub fn append_for_contract(
        &mut self,
        mut outcome: AcceptedOutcomeV1,
        contract: &OutcomeContractV1,
    ) -> OutcomeRecord {
        self.append_internal(&mut outcome, contract.supersession_allowed)
    }

    /// Append a detailed evaluation and return its lineage record.
    pub fn append_evaluation(&mut self, evaluation: &OutcomeEvaluation) -> OutcomeRecord {
        self.append(evaluation.outcome.clone())
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub fn records(&self) -> &[OutcomeRecord] {
        &self.records
    }

    pub fn latest(&self) -> Option<&OutcomeRecord> {
        self.records.last()
    }

    fn append_internal(
        &mut self,
        outcome: &mut AcceptedOutcomeV1,
        supersession_allowed: bool,
    ) -> OutcomeRecord {
        let previous = if supersession_allowed {
            self.records.iter().rev().find(|record| {
                record.outcome.task_id == outcome.task_id
                    && record.outcome.contract_ref == outcome.contract_ref
            })
        } else {
            None
        };

        let supersedes = previous.map(|record| record.outcome.outcome_id.as_str().to_owned());
        if let Some(previous_id) = &supersedes {
            if outcome.outcome_id.as_str() == previous_id {
                let replacement = format!("outcome-superseding-{}", self.records.len() + 1);
                outcome.outcome_id = make_outcome_id(&replacement);
            }
        }

        let record = OutcomeRecord {
            outcome: outcome.clone(),
            supersedes,
        };
        self.records.push(record.clone());
        record
    }
}

/// Alias emphasizing append-only ledger semantics.
pub type OutcomeLedger = OutcomeHistory;

fn occurrence_index(counts: &mut [usize; 11], signal_type: SignalType) -> usize {
    let index = signal_type_index(signal_type);
    let occurrence = counts[index];
    counts[index] += 1;
    occurrence
}

fn signal_type_index(signal_type: SignalType) -> usize {
    match signal_type {
        SignalType::BuildSuccess => 0,
        SignalType::TestsPassing => 1,
        SignalType::LintClean => 2,
        SignalType::TypecheckPassing => 3,
        SignalType::AgentCompletion => 4,
        SignalType::RetryCount => 5,
        SignalType::HumanAcceptance => 6,
        SignalType::PrMerge => 7,
        SignalType::CiPassing => 8,
        SignalType::Correction => 9,
        SignalType::Rollback => 10,
    }
}

fn evaluate_requirement(
    task_class: TaskClass,
    signal_type: SignalType,
    required: bool,
    weight_milli: u32,
    occurrence: usize,
    signals: &[OutcomeSignal],
) -> SignalContribution {
    let observation = find_observation(task_class, signal_type, occurrence, signals);
    let (value, status, explanation) = match observation {
        None => (
            SignalValue::Unknown,
            if required {
                ContributionStatus::Missing
            } else {
                ContributionStatus::NotRequired
            },
            if required {
                "required signal not observed".to_owned()
            } else {
                "optional signal not observed".to_owned()
            },
        ),
        Some(signal) => match signal.value {
            SignalValue::Unknown => (
                SignalValue::Unknown,
                ContributionStatus::Unknown,
                "signal observed with unknown value".to_owned(),
            ),
            value => {
                let passed = value_passes(task_class, signal_type, occurrence, &value);
                let status = if passed {
                    if task_class == TaskClass::Investigation
                        && signal_type == SignalType::HumanAcceptance
                        && !has_positive_signal(SignalType::HumanAcceptance, signals)
                    {
                        ContributionStatus::AlternativeSatisfied
                    } else {
                        ContributionStatus::Passed
                    }
                } else {
                    ContributionStatus::Failed
                };
                (
                    value,
                    status,
                    format!(
                        "observed value contributed {}",
                        if passed { "success" } else { "failure" }
                    ),
                )
            }
        },
    };
    let contribution_milli = if matches!(
        status,
        ContributionStatus::Passed | ContributionStatus::AlternativeSatisfied
    ) {
        weight_milli
    } else {
        0
    };

    SignalContribution {
        signal_type,
        required,
        weight_milli,
        value,
        status,
        contribution_milli,
        explanation,
    }
}

fn find_observation(
    task_class: TaskClass,
    signal_type: SignalType,
    occurrence: usize,
    signals: &[OutcomeSignal],
) -> Option<OutcomeSignal> {
    if task_class == TaskClass::Investigation && signal_type == SignalType::HumanAcceptance {
        if let Some(human) = signals
            .iter()
            .rfind(|signal| signal.signal_type == SignalType::HumanAcceptance)
        {
            if !matches!(human.value, SignalValue::Unknown) {
                return Some(human.clone());
            }
        }

        return signals
            .iter()
            .rfind(|signal| signal.signal_type == SignalType::AgentCompletion)
            .cloned();
    }

    if task_class == TaskClass::TestAddition && signal_type == SignalType::TestsPassing {
        if occurrence == 0 {
            return signals
                .iter()
                .rfind(|signal| {
                    signal.signal_type == SignalType::TestsPassing
                        && matches!(signal.value, SignalValue::Boolean(_))
                })
                .cloned();
        }

        if let Some(count_delta) = signals.iter().rfind(|signal| {
            signal.signal_type == SignalType::TestsPassing
                && matches!(signal.value, SignalValue::Count(_))
        }) {
            return Some(count_delta.clone());
        }

        return signals
            .iter()
            .rfind(|signal| {
                signal.signal_type == SignalType::AgentCompletion
                    && matches!(signal.value, SignalValue::Count(_))
            })
            .cloned();
    }

    signals
        .iter()
        .rfind(|signal| signal.signal_type == signal_type)
        .cloned()
}

fn value_passes(
    _task_class: TaskClass,
    signal_type: SignalType,
    _occurrence: usize,
    value: &SignalValue,
) -> bool {
    match value {
        SignalValue::Boolean(value) => *value,
        SignalValue::Count(value) => match signal_type {
            SignalType::RetryCount => true,
            _ => *value > 0,
        },
        SignalValue::Unknown => false,
    }
}

fn has_positive_signal(signal_type: SignalType, signals: &[OutcomeSignal]) -> bool {
    signals.iter().any(|signal| {
        signal.signal_type == signal_type
            && value_passes(TaskClass::BugFix, signal_type, 0, &signal.value)
    })
}

const DETERMINISTIC_OBSERVED_AT: &str = "1970-01-01T00:00:00Z";

fn context_outcome_id(context: &EvaluationContext) -> OutcomeId {
    let seed = format!(
        "outcome-v1:{}:{}:{}",
        context.task_id, context.contract_version, context.evaluator_version
    );
    let value = format!("outcome:{}", blake3::hash(seed.as_bytes()).to_hex());
    make_outcome_id(&value)
}

fn make_outcome_id(value: &str) -> OutcomeId {
    OutcomeId::new(value.to_owned()).unwrap_or_else(|_| {
        OutcomeId::new("outcome-unknown".to_owned()).unwrap_or_else(|_| {
            unreachable!("fixed outcome identifier must satisfy protocol bounds")
        })
    })
}

fn make_task_id(value: &str) -> TaskId {
    TaskId::new(value.to_owned()).unwrap_or_else(|_| {
        TaskId::new("task-unknown".to_owned())
            .unwrap_or_else(|_| unreachable!("fixed task identifier must satisfy protocol bounds"))
    })
}

fn project_protocol_signals(signals: &[OutcomeSignal]) -> OutcomeSignalsV1 {
    OutcomeSignalsV1 {
        build: latest_state(signals, &[SignalType::BuildSuccess, SignalType::CiPassing]),
        tests: latest_state(signals, &[SignalType::TestsPassing]),
        lint: latest_state(signals, &[SignalType::LintClean]),
        typecheck: latest_state(signals, &[SignalType::TypecheckPassing]),
        completion: latest_state(
            signals,
            &[SignalType::AgentCompletion, SignalType::HumanAcceptance],
        ),
        pr: latest_state(signals, &[SignalType::PrMerge]),
        correction: latest_state(signals, &[SignalType::Correction]),
        rollback: latest_state(signals, &[SignalType::Rollback]),
        retry: latest_state(signals, &[SignalType::RetryCount]),
    }
}

fn latest_state(signals: &[OutcomeSignal], signal_types: &[SignalType]) -> Option<SignalState> {
    signals
        .iter()
        .rfind(|signal| signal_types.contains(&signal.signal_type))
        .map(|signal| match signal.value {
            SignalValue::Boolean(value) => {
                if value {
                    SignalState::Passed
                } else {
                    SignalState::Failed
                }
            }
            SignalValue::Count(value) => {
                if value > 0 {
                    SignalState::Passed
                } else {
                    SignalState::Failed
                }
            }
            SignalValue::Unknown => SignalState::Unknown,
        })
}

trait AcceptanceStateText {
    fn as_str(&self) -> &'static str;
}

impl AcceptanceStateText for AcceptanceState {
    fn as_str(&self) -> &'static str {
        match self {
            AcceptanceState::Accepted => "accepted",
            AcceptanceState::Rejected => "rejected",
            AcceptanceState::Unknown => "unknown",
        }
    }
}

impl std::fmt::Display for ContributionStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Missing => "missing",
            Self::Unknown => "unknown",
            Self::NotRequired => "not_required",
            Self::AlternativeSatisfied => "alternative_satisfied",
        };
        formatter.write_str(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::outcome::contracts::{
        bug_fix_contract, investigation_contract, test_addition_contract,
    };
    use crate::core::outcome::signals::LocalSignalAdapters;

    fn bug_fix_signals() -> Vec<OutcomeSignal> {
        vec![
            LocalSignalAdapters::build_success(true),
            LocalSignalAdapters::tests_passing(true),
            LocalSignalAdapters::pr_merge(true),
        ]
    }

    #[test]
    fn bug_fix_with_all_signals_is_accepted() {
        let result = evaluate_detailed(&bug_fix_contract(), &bug_fix_signals());

        assert_eq!(result.outcome.accepted, AcceptanceState::Accepted);
        assert_eq!(result.outcome.quality_score_milli, Some(1000));
        assert_eq!(result.contributions.len(), 3);
        assert!(result.explanation().contains("build_success"));
        assert!(result.explanation().contains("tests_passing"));
    }

    #[test]
    fn bug_fix_with_failed_tests_is_rejected() {
        let signals = vec![
            LocalSignalAdapters::build_success(true),
            LocalSignalAdapters::tests_passing(false),
        ];
        let result = evaluate_detailed(&bug_fix_contract(), &signals);

        assert_eq!(result.outcome.accepted, AcceptanceState::Rejected);
        assert_eq!(result.outcome.quality_score_milli, Some(0));
    }

    #[test]
    fn empty_signals_never_fabricate_a_quality_score() {
        for task_class in [
            TaskClass::BugFix,
            TaskClass::Refactor,
            TaskClass::TestAddition,
            TaskClass::Documentation,
            TaskClass::Investigation,
        ] {
            let result = evaluate_detailed(&OutcomeContractV1::for_task_class(task_class), &[]);
            assert_eq!(result.outcome.accepted, AcceptanceState::Unknown);
            assert_eq!(result.outcome.quality_score_milli, None);
            assert!(result.outcome.evidence_refs.is_empty());
            assert_eq!(result.result, result.outcome);
        }
    }

    #[test]
    fn bug_fix_with_missing_signals_is_unknown() {
        let result = evaluate_detailed(
            &bug_fix_contract(),
            &[LocalSignalAdapters::build_success(true)],
        );

        assert_eq!(result.outcome.accepted, AcceptanceState::Unknown);
        assert!(result.outcome.quality_score_milli.is_some());
        assert!(
            result
                .contributions
                .iter()
                .any(|item| item.signal_type == SignalType::TestsPassing
                    && item.status == ContributionStatus::Missing)
        );
    }

    #[test]
    fn policy_violation_overrides_acceptance() {
        let result = evaluate_detailed_with_policy(&bug_fix_contract(), &bug_fix_signals(), true);

        assert_eq!(result.outcome.accepted, AcceptanceState::Rejected);
        assert!(
            result
                .explanation()
                .contains("policy violation overrides signal result")
        );
    }

    #[test]
    fn evaluator_is_deterministic() {
        let contract = bug_fix_contract();
        let signals = bug_fix_signals();

        assert_eq!(
            evaluate_detailed(&contract, &signals),
            evaluate_detailed(&contract, &signals)
        );
    }

    #[test]
    fn late_signal_creates_superseding_outcome() {
        let contract = bug_fix_contract();
        let first = evaluate_detailed(&contract, &[LocalSignalAdapters::build_success(true)]);
        let second = evaluate_detailed(&contract, &bug_fix_signals());
        let mut history = OutcomeHistory::new();

        let first_record = history.append_for_contract(first.outcome, &contract);
        let second_record = history.append_for_contract(second.outcome, &contract);

        assert_eq!(history.len(), 2);
        assert_eq!(
            second_record.supersedes,
            Some(first_record.outcome.outcome_id.as_str().to_owned())
        );
        assert_ne!(
            first_record.outcome.outcome_id,
            second_record.outcome.outcome_id
        );
        assert_eq!(history.records()[0], first_record);
    }

    #[test]
    fn investigation_accepts_completion_as_explicit_alternative() {
        let result = evaluate_detailed(
            &investigation_contract(),
            &[LocalSignalAdapters::agent_completion(true)],
        );

        assert_eq!(result.outcome.accepted, AcceptanceState::Accepted);
        assert!(result.explanation().contains("alternative_satisfied"));
    }

    #[test]
    fn test_addition_requires_passing_suite_and_count_delta() {
        let signals = vec![
            LocalSignalAdapters::tests_passing(true),
            LocalSignalAdapters::test_count_increased(1),
        ];

        let result = evaluate_detailed(&test_addition_contract(), &signals);
        assert_eq!(result.outcome.accepted, AcceptanceState::Accepted);
    }
}
