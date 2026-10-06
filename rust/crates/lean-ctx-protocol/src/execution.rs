//! Planning and execution receipt contracts.

use crate::common::{
    AgentId, AttemptId, CapabilityId, ContextPlanId, ExtensionsV1, PlanId, ReceiptId, TaskId,
    ValidationError, deserialize_milliunit, deserialize_schema_version, validate_bounded_string,
    validate_milliunit, validate_schema_version, validate_unique_strings,
};
use crate::evidence::EvidenceRefV1;
use serde::{Deserialize, Serialize};

/// Strategy used to assemble context for a plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextStrategy {
    Minimal,
    Balanced,
    Comprehensive,
    CachedFirst,
}

/// Terminal condition selected by an execution plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopCondition {
    OnCompletion,
    OnAcceptance,
    OnBudgetExhaustion,
    OnError,
    Manual,
}

/// Explicit token policy; no token limit does not disable other resource limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ContextBudgetPolicyV1 {
    TokenLimit { tokens: u64 },
    NoTokenLimit {},
}

/// Optional estimates are unknown when null, never measured values or targets.
/// When present, this additive projection is authoritative over legacy scalars.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionPlanEstimatesV1 {
    pub cost_micros: Option<u64>,
    pub quality_milli: Option<u16>,
    pub latency_ms: Option<u64>,
}

/// V1 execution plan produced from a task envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionPlanV1 {
    #[serde(deserialize_with = "deserialize_schema_version")]
    pub schema_version: u32,
    pub plan_id: PlanId,
    pub task_id: TaskId,
    pub context_budget_tokens: u64,
    /// Absent retains legacy scalar semantics; explicit no-limit uses scalar zero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_budget_policy: Option<ContextBudgetPolicyV1>,
    pub context_strategy: ContextStrategy,
    pub knowledge_refs: Vec<String>,
    pub capability_ids: Vec<CapabilityId>,
    pub model: String,
    pub provider: String,
    #[serde(deserialize_with = "deserialize_milliunit")]
    pub reasoning_allocation_milli: u16,
    pub max_retries: u32,
    pub fallback_refs: Vec<String>,
    pub stop_condition: StopCondition,
    pub expected_cost_micros: u64,
    #[serde(deserialize_with = "deserialize_milliunit")]
    pub expected_quality_milli: u16,
    pub expected_latency_ms: u64,
    /// Unknown entries require zero only in the legacy compatibility scalars.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimates: Option<ExecutionPlanEstimatesV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_decision_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduler_decision_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executor_agent_id: Option<AgentId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_plan_id: Option<ContextPlanId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capability_bindings: Vec<CapabilityBindingV1>,
    /// Unknown additive V1 fields retained for lossless forwarding.
    #[serde(default, flatten)]
    pub extensions: ExtensionsV1,
}

const EXECUTION_PLAN_RESERVED_FIELDS: &[&str] = &[
    "schema_version",
    "plan_id",
    "task_id",
    "context_budget_tokens",
    "context_budget_policy",
    "context_strategy",
    "knowledge_refs",
    "capability_ids",
    "model",
    "provider",
    "reasoning_allocation_milli",
    "max_retries",
    "fallback_refs",
    "stop_condition",
    "expected_cost_micros",
    "expected_quality_milli",
    "expected_latency_ms",
    "estimates",
    "policy_decision_ref",
    "scheduler_decision_ref",
    "executor_agent_id",
    "context_plan_id",
    "capability_bindings",
];

impl ExecutionPlanV1 {
    /// Resolve the token policy after validating the plan's dual projections.
    pub fn context_token_limit(&self) -> Option<u64> {
        match self.context_budget_policy {
            Some(ContextBudgetPolicyV1::NoTokenLimit {}) => None,
            _ => Some(self.context_budget_tokens),
        }
    }

    /// Validate invariants that also apply to values constructed in Rust.
    pub fn validate(&self) -> Result<(), ValidationError> {
        self.extensions
            .validate_reserved(EXECUTION_PLAN_RESERVED_FIELDS)?;
        validate_schema_version(self.schema_version)?;
        let projected_budget = match self.context_budget_policy {
            Some(ContextBudgetPolicyV1::TokenLimit { tokens }) => tokens,
            Some(ContextBudgetPolicyV1::NoTokenLimit {}) => 0,
            None => self.context_budget_tokens,
        };
        if projected_budget != self.context_budget_tokens {
            return Err(ValidationError::new(
                "context budget policy disagrees with legacy scalar",
            ));
        }
        if let Some(estimates) = &self.estimates {
            if let Some(quality) = estimates.quality_milli {
                validate_milliunit(quality, "estimated quality_milli")?;
            }
            if estimates.cost_micros.unwrap_or(0) != self.expected_cost_micros
                || estimates.quality_milli.unwrap_or(0) != self.expected_quality_milli
                || estimates.latency_ms.unwrap_or(0) != self.expected_latency_ms
            {
                return Err(ValidationError::new(
                    "plan estimates disagree with legacy scalars",
                ));
            }
        }
        validate_milliunit(
            self.reasoning_allocation_milli,
            "reasoning_allocation_milli",
        )?;
        validate_milliunit(self.expected_quality_milli, "expected_quality_milli")?;
        validate_bounded_string(&self.model, "model")?;
        validate_bounded_string(&self.provider, "provider")?;
        validate_unique_strings(&self.knowledge_refs, "knowledge_refs")?;
        validate_unique_strings(&self.capability_ids, "capability_ids")?;
        if self.capability_ids.is_empty() {
            return Err(ValidationError::new(
                "execution plan requires at least one capability_id",
            ));
        }
        validate_unique_strings(&self.fallback_refs, "fallback_refs")?;
        for (value, field) in [
            (&self.policy_decision_ref, "policy_decision_ref"),
            (&self.scheduler_decision_ref, "scheduler_decision_ref"),
        ] {
            if let Some(value) = value {
                validate_bounded_string(value, field)?;
            }
        }
        validate_capability_bindings(&self.capability_bindings, &self.capability_ids)?;
        Ok(())
    }
}

/// Four-stage token balance carried by an execution receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextBalanceV1 {
    pub original_tokens: u64,
    pub materialized_tokens: u64,
    pub delivered_tokens: u64,
    pub provider_billed_tokens: u64,
}

/// Version-pinned capability selected by an execution plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityBindingV1 {
    pub capability_id: CapabilityId,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_digest: Option<String>,
}

/// Explicit observations; absence means unavailable rather than factual zero.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionObservationsV1 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_calls: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retries: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actual_cost_micros: Option<u64>,
}

impl ContextBalanceV1 {
    /// Validate monotonic context accounting across the four stages.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.materialized_tokens > self.original_tokens {
            return Err(ValidationError::new(
                "materialized_tokens exceeds original_tokens",
            ));
        }
        if self.delivered_tokens > self.materialized_tokens {
            return Err(ValidationError::new(
                "delivered_tokens exceeds materialized_tokens",
            ));
        }
        Ok(())
    }
}

/// Auditable result of executing an execution plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionReceiptV1 {
    #[serde(deserialize_with = "deserialize_schema_version")]
    pub schema_version: u32,
    pub receipt_id: ReceiptId,
    pub task_id: TaskId,
    pub plan_id: PlanId,
    pub context_balance: ContextBalanceV1,
    pub fresh_input_tokens: u64,
    pub cached_input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
    pub requested_model: String,
    pub selected_model: String,
    pub provider: String,
    /// Capability that produced this receipt, when the producer is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability_id: Option<String>,
    /// Version of the capability that produced this receipt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability_version: Option<String>,
    pub model_calls: u32,
    pub retries: u32,
    pub latency_ms: u64,
    pub actual_cost_micros: u64,
    pub baseline_cost_micros: u64,
    pub avoided_cost_micros: u64,
    pub etpao_milli: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub knowledge_refs: Vec<String>,
    pub decision_refs: Vec<String>,
    pub evidence_refs: Vec<EvidenceRefV1>,
    pub signature: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executor_agent_id: Option<AgentId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<AttemptId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_plan_id: Option<ContextPlanId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_receipt_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capability_bindings: Vec<CapabilityBindingV1>,
    #[serde(default, skip_serializing_if = "ExecutionObservationsV1::is_empty")]
    pub observations: ExecutionObservationsV1,
    /// Unknown additive V1 fields retained for lossless forwarding.
    #[serde(default, flatten)]
    pub extensions: ExtensionsV1,
}

const EXECUTION_RECEIPT_RESERVED_FIELDS: &[&str] = &[
    "schema_version",
    "receipt_id",
    "task_id",
    "plan_id",
    "context_balance",
    "fresh_input_tokens",
    "cached_input_tokens",
    "output_tokens",
    "reasoning_tokens",
    "requested_model",
    "selected_model",
    "provider",
    "capability_id",
    "capability_version",
    "model_calls",
    "retries",
    "latency_ms",
    "actual_cost_micros",
    "baseline_cost_micros",
    "avoided_cost_micros",
    "etpao_milli",
    "outcome_ref",
    "knowledge_refs",
    "decision_refs",
    "evidence_refs",
    "signature",
    "executor_agent_id",
    "attempt_id",
    "context_plan_id",
    "context_receipt_ref",
    "capability_bindings",
    "observations",
    // Reserved ingress-only field from the canonical receipt envelope. It must
    // never be silently accepted as a V1 extension by validation.
    "canonical_receipt",
];

impl ExecutionReceiptV1 {
    /// Validate receipt accounting and schema invariants.
    pub fn validate(&self) -> Result<(), ValidationError> {
        self.extensions
            .validate_reserved(EXECUTION_RECEIPT_RESERVED_FIELDS)?;
        validate_schema_version(self.schema_version)?;
        self.context_balance.validate()?;
        if self.avoided_cost_micros > self.baseline_cost_micros {
            return Err(ValidationError::new(
                "avoided_cost_micros exceeds baseline_cost_micros",
            ));
        }
        validate_bounded_string(&self.requested_model, "requested_model")?;
        validate_bounded_string(&self.selected_model, "selected_model")?;
        validate_bounded_string(&self.provider, "provider")?;
        validate_unique_strings(&self.knowledge_refs, "knowledge_refs")?;
        validate_unique_strings(&self.decision_refs, "decision_refs")?;
        validate_capability_bindings(&self.capability_bindings, &[])?;
        if self.evidence_refs.len() > crate::MAX_PROTOCOL_ITEMS {
            return Err(ValidationError::new("evidence_refs exceeds item limit"));
        }
        if self.capability_id.is_some() != self.capability_version.is_some() {
            return Err(ValidationError::new(
                "legacy capability_id and capability_version must appear together",
            ));
        }
        for (value, field) in [
            (&self.capability_id, "capability_id"),
            (&self.capability_version, "capability_version"),
            (&self.outcome_ref, "outcome_ref"),
            (&self.context_receipt_ref, "context_receipt_ref"),
        ] {
            if let Some(value) = value {
                validate_bounded_string(value, field)?;
            }
        }
        validate_observation(
            self.observations.model_calls,
            self.model_calls,
            "model_calls",
        )?;
        validate_observation(self.observations.retries, self.retries, "retries")?;
        validate_observation(self.observations.latency_ms, self.latency_ms, "latency_ms")?;
        validate_observation(
            self.observations.actual_cost_micros,
            self.actual_cost_micros,
            "actual_cost_micros",
        )?;
        for evidence in &self.evidence_refs {
            evidence.validate()?;
        }
        Ok(())
    }
}

impl ExecutionObservationsV1 {
    pub fn is_empty(&self) -> bool {
        self.model_calls.is_none()
            && self.retries.is_none()
            && self.latency_ms.is_none()
            && self.actual_cost_micros.is_none()
    }
}

fn validate_capability_bindings(
    bindings: &[CapabilityBindingV1],
    selected: &[CapabilityId],
) -> Result<(), ValidationError> {
    if bindings.len() > crate::common::MAX_PROTOCOL_ITEMS {
        return Err(ValidationError::new(
            "capability_bindings exceeds item limit",
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    for binding in bindings {
        validate_bounded_string(&binding.version, "capability version")?;
        if let Some(digest) = &binding.manifest_digest {
            validate_bounded_string(digest, "capability manifest digest")?;
            let digest = digest
                .strip_prefix("sha256:")
                .or_else(|| digest.strip_prefix("blake3:"))
                .unwrap_or(digest);
            if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(ValidationError::new(
                    "capability manifest digest must contain a supported 64-digit hexadecimal digest",
                ));
            }
        }
        if !seen.insert(binding.capability_id.as_str()) {
            return Err(ValidationError::new("duplicate capability binding"));
        }
        if !selected.is_empty() && !selected.contains(&binding.capability_id) {
            return Err(ValidationError::new(
                "capability binding is not selected by plan",
            ));
        }
    }
    Ok(())
}

fn validate_observation<T: PartialEq>(
    observation: Option<T>,
    legacy_value: T,
    field: &str,
) -> Result<(), ValidationError> {
    if observation
        .as_ref()
        .is_some_and(|value| value != &legacy_value)
    {
        return Err(ValidationError::new(format!(
            "observations.{field} contradicts legacy {field}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).expect("identifier should be valid")
    }

    fn balance() -> ContextBalanceV1 {
        ContextBalanceV1 {
            original_tokens: 1_000,
            materialized_tokens: 800,
            delivered_tokens: 700,
            provider_billed_tokens: 650,
        }
    }

    fn plan_fixture() -> ExecutionPlanV1 {
        ExecutionPlanV1 {
            schema_version: 1,
            plan_id: id("plan-1"),
            task_id: id("task-1"),
            context_budget_tokens: 10_000,
            context_budget_policy: None,
            context_strategy: ContextStrategy::Balanced,
            knowledge_refs: vec!["knowledge:1".to_owned()],
            capability_ids: vec![id("capability:search")],
            model: "model-1".to_owned(),
            provider: "provider-1".to_owned(),
            reasoning_allocation_milli: 500,
            max_retries: 2,
            fallback_refs: vec!["model:fallback".to_owned()],
            stop_condition: StopCondition::OnAcceptance,
            expected_cost_micros: 5_000,
            estimates: None,
            expected_quality_milli: 850,
            expected_latency_ms: 1_500,
            policy_decision_ref: Some("decision:policy".to_owned()),
            scheduler_decision_ref: Some("decision:scheduler".to_owned()),
            executor_agent_id: Some(id("agent-1")),
            context_plan_id: Some(id("context-plan-1")),
            capability_bindings: vec![CapabilityBindingV1 {
                capability_id: id("capability:search"),
                version: "1.0.0".to_owned(),
                manifest_digest: None,
            }],
            extensions: Default::default(),
        }
    }

    #[test]
    fn plan_serialization_round_trip() {
        let plan = plan_fixture();
        let json = serde_json::to_string(&plan).expect("plan should serialize");
        assert!(!json.contains("context_budget_policy"));
        assert!(!json.contains("estimates"));
        let decoded: ExecutionPlanV1 =
            serde_json::from_str(&json).expect("plan should deserialize");
        assert_eq!(plan, decoded);
        plan.validate().expect("plan should satisfy invariants");
        let mut without_capability = plan.clone();
        without_capability.capability_ids.clear();
        assert!(without_capability.validate().is_err());
    }

    #[test]
    fn explicit_budget_policy_preserves_legacy_scalar_and_unlimited_distinction() {
        let mut plan = plan_fixture();
        assert_eq!(plan.context_token_limit(), Some(10_000));
        plan.context_budget_policy = Some(ContextBudgetPolicyV1::TokenLimit { tokens: 10_000 });
        plan.validate().unwrap();
        plan.context_budget_tokens = 0;
        assert!(plan.validate().is_err());
        plan.context_budget_policy = Some(ContextBudgetPolicyV1::NoTokenLimit {});
        plan.validate().unwrap();
        assert_eq!(plan.context_token_limit(), None);
        let decoded: ExecutionPlanV1 =
            serde_json::from_value(serde_json::to_value(&plan).unwrap()).unwrap();
        assert_eq!(decoded, plan);
        plan.context_budget_tokens = 1;
        assert!(plan.validate().is_err());
        plan.context_budget_policy = None;
        plan.context_budget_tokens = 0;
        assert_eq!(plan.context_token_limit(), Some(0));
    }

    #[test]
    fn explicit_estimates_distinguish_unknown_from_known_zero() {
        let mut plan = plan_fixture();
        plan.estimates = Some(ExecutionPlanEstimatesV1::default());
        assert!(plan.validate().is_err());
        plan.expected_cost_micros = 0;
        plan.expected_quality_milli = 0;
        plan.expected_latency_ms = 0;
        plan.validate().unwrap();
        let unknown = serde_json::to_value(&plan).unwrap();
        assert!(unknown["estimates"]["cost_micros"].is_null());
        plan.estimates.as_mut().unwrap().cost_micros = Some(0);
        plan.validate().unwrap();
        assert_eq!(
            serde_json::to_value(&plan).unwrap()["estimates"]["cost_micros"],
            0
        );
        plan.estimates.as_mut().unwrap().quality_milli = Some(1001);
        plan.expected_quality_milli = 1001;
        assert!(plan.validate().is_err());
        assert!(
            serde_json::from_value::<ExecutionPlanEstimatesV1>(serde_json::json!({"invented":0}))
                .is_err()
        );
        assert!(
            serde_json::from_value::<ContextBudgetPolicyV1>(
                serde_json::json!({"kind":"no_token_limit","tokens":0})
            )
            .is_err()
        );
    }

    #[test]
    fn receipt_serialization_round_trip() {
        let receipt = ExecutionReceiptV1 {
            schema_version: 1,
            receipt_id: id("receipt-1"),
            task_id: id("task-1"),
            plan_id: id("plan-1"),
            context_balance: balance(),
            fresh_input_tokens: 600,
            cached_input_tokens: 100,
            output_tokens: 200,
            reasoning_tokens: 50,
            requested_model: "model-requested".to_owned(),
            selected_model: "model-selected".to_owned(),
            provider: "provider-1".to_owned(),
            capability_id: Some("capability://leanctx/context".to_owned()),
            capability_version: Some("1.0.0".to_owned()),
            model_calls: 2,
            retries: 1,
            latency_ms: 900,
            actual_cost_micros: 2_000,
            baseline_cost_micros: 3_000,
            avoided_cost_micros: 1_000,
            etpao_milli: 1_250,
            outcome_ref: Some("outcome:1".to_owned()),
            knowledge_refs: vec!["knowledge:1".to_owned()],
            decision_refs: vec!["decision:1".to_owned()],
            evidence_refs: vec![],
            signature: "signature".to_owned(),
            executor_agent_id: Some(id("agent-1")),
            attempt_id: Some(id("attempt-1")),
            context_plan_id: Some(id("context-plan-1")),
            context_receipt_ref: Some("context-receipt-1".to_owned()),
            capability_bindings: vec![CapabilityBindingV1 {
                capability_id: id("capability:search"),
                version: "1.0.0".to_owned(),
                manifest_digest: None,
            }],
            observations: ExecutionObservationsV1 {
                model_calls: Some(2),
                retries: Some(1),
                latency_ms: Some(900),
                actual_cost_micros: Some(2_000),
            },
            extensions: Default::default(),
        };
        let json = serde_json::to_string(&receipt).expect("receipt should serialize");
        let decoded: ExecutionReceiptV1 =
            serde_json::from_str(&json).expect("receipt should deserialize");
        assert_eq!(receipt, decoded);
        receipt
            .validate()
            .expect("receipt should satisfy invariants");
    }

    #[test]
    fn receipt_capability_metadata_is_optional_and_backward_compatible() {
        let receipt = ExecutionReceiptV1 {
            schema_version: 1,
            receipt_id: id("receipt-1"),
            task_id: id("task-1"),
            plan_id: id("plan-1"),
            context_balance: balance(),
            fresh_input_tokens: 600,
            cached_input_tokens: 100,
            output_tokens: 200,
            reasoning_tokens: 50,
            requested_model: "model-requested".to_owned(),
            selected_model: "model-selected".to_owned(),
            provider: "provider-1".to_owned(),
            capability_id: Some("capability://leanctx/context".to_owned()),
            capability_version: Some("1.0.0".to_owned()),
            model_calls: 2,
            retries: 1,
            latency_ms: 900,
            actual_cost_micros: 2_000,
            baseline_cost_micros: 3_000,
            avoided_cost_micros: 1_000,
            etpao_milli: 1_250,
            outcome_ref: Some("outcome:1".to_owned()),
            knowledge_refs: vec!["knowledge:1".to_owned()],
            decision_refs: vec!["decision:1".to_owned()],
            evidence_refs: vec![],
            signature: "signature".to_owned(),
            executor_agent_id: None,
            attempt_id: None,
            context_plan_id: None,
            context_receipt_ref: None,
            capability_bindings: vec![],
            observations: ExecutionObservationsV1::default(),
            extensions: Default::default(),
        };
        let json = serde_json::to_value(&receipt).expect("receipt should serialize");
        assert_eq!(json["capability_id"], "capability://leanctx/context");
        assert_eq!(
            serde_json::from_value::<ExecutionReceiptV1>(json.clone())
                .expect("receipt with capability metadata should deserialize"),
            receipt
        );

        let mut legacy = json;
        let object = legacy
            .as_object_mut()
            .expect("serialized receipt should be an object");
        object.remove("capability_id");
        object.remove("capability_version");

        let decoded: ExecutionReceiptV1 =
            serde_json::from_value(legacy).expect("legacy receipt should deserialize");
        assert_eq!(decoded.capability_id, None);
        assert_eq!(decoded.capability_version, None);

        let without_capability = serde_json::to_value(decoded)
            .expect("receipt without capability metadata should serialize");
        assert!(without_capability.get("capability_id").is_none());
        assert!(without_capability.get("capability_version").is_none());
    }
}
