// SPDX-License-Identifier: Apache-2.0

//! Versioned provider-execution transport for a host-prepared Engine context.
//!
//! This contract validates bounded joins and provenance labels. It does not
//! authorize a tenant, select a provider, verify a signer, charge an account,
//! publish a receipt, or admit an outcome.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    AcceptanceState, AttemptId, EngineContextSourceMaterializationResponseV1, EngineFailureV1,
    ExecutionPlanV1, MAX_SAFE_INTEGER, PlanId, SemanticVersion, Sha256Digest, TaskEnvelopeV1,
    TaskId, ValidationError, ViaProviderUsageV1, deserialize_schema_version,
    validate_bounded_string,
};

/// Domain for the canonical request digest used by adapters.
pub const ENGINE_PROVIDER_EXECUTION_REQUEST_DIGEST_DOMAIN: &[u8] =
    b"leanctx.engine.provider-execution.request.v1\0";

const INTENT_DIGEST_DOMAIN: &[u8] = b"leanctx.engine.provider-execution.intent.v1\0";

/// Request bound: the existing source transport plus bounded provider metadata.
pub const MAX_ENGINE_PROVIDER_EXECUTION_REQUEST_BYTES: usize =
    crate::MAX_ENGINE_SOURCE_PLAN_REQUEST_BYTES + 64 * 1024;

/// Response bound, including bounded provider output.
pub const MAX_ENGINE_PROVIDER_EXECUTION_RESPONSE_BYTES: usize = 1024 * 1024;

/// Provider output is an artifact payload candidate, not a receipt or trust root.
pub const MAX_ENGINE_PROVIDER_OUTPUT_BYTES: usize = 256 * 1024;

/// Provider execution may not silently widen into an unbounded output request.
pub const MAX_ENGINE_PROVIDER_OUTPUT_TOKENS: u32 = 65_536;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineProviderExecutionStatusV1 {
    Succeeded,
    Failed,
    Rejected,
    TimedOut,
    DispatchUncertain,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineProviderOutputV1 {
    pub content: String,
    pub sha256_digest: Sha256Digest,
}

impl EngineProviderOutputV1 {
    fn validate(&self) -> Result<(), ValidationError> {
        if self.content.len() > MAX_ENGINE_PROVIDER_OUTPUT_BYTES
            || sha256_digest(self.content.as_bytes())? != self.sha256_digest
        {
            return Err(ValidationError::new(
                "invalid Engine provider output payload",
            ));
        }
        Ok(())
    }
}

/// Cost provenance is explicit; zero is valid only when the selected basis says
/// it was actually observed or estimated as zero by the adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "basis", rename_all = "snake_case", deny_unknown_fields)]
pub enum EngineProviderCostV1 {
    Unavailable {},
    UsagePricedEstimate { micros: u64 },
    ObservedCharge { micros: u64 },
}

impl EngineProviderCostV1 {
    fn validate(&self) -> Result<(), ValidationError> {
        let micros = match self {
            Self::Unavailable {} => return Ok(()),
            Self::UsagePricedEstimate { micros } | Self::ObservedCharge { micros } => micros,
        };
        if *micros > MAX_SAFE_INTEGER {
            return Err(ValidationError::new(
                "Engine provider cost exceeds the safe integer ceiling",
            ));
        }
        Ok(())
    }
}

/// Host-prepared request; its fields are correlation and integrity inputs, not
/// caller authorization or provider-selection authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineProviderExecutionRequestV1 {
    #[serde(deserialize_with = "deserialize_schema_version")]
    pub schema_version: u32,
    pub transport_version: u32,
    pub engine_interface_version: SemanticVersion,
    pub attempt_id: AttemptId,
    pub task: TaskEnvelopeV1,
    pub plan: ExecutionPlanV1,
    pub materialization: EngineContextSourceMaterializationResponseV1,
    pub query: String,
    pub max_output_tokens: u32,
}

impl EngineProviderExecutionRequestV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.schema_version != 1
            || self.transport_version != 1
            || self.engine_interface_version.as_str() != "1.0.0"
            || self.query.trim().is_empty()
            || self.query.len() > crate::MAX_ENGINE_CONTEXT_PLAN_QUERY_BYTES
            || self.query.contains('\0')
            || !(1..=MAX_ENGINE_PROVIDER_OUTPUT_TOKENS).contains(&self.max_output_tokens)
        {
            return Err(ValidationError::new(
                "invalid Engine provider execution request envelope",
            ));
        }
        self.task.validate()?;
        self.plan.validate()?;
        self.materialization.validate()?;
        validate_provider_plan(&self.plan)?;
        let source_plan = &self.materialization.plan.result.plan;
        if self.plan.task_id != self.task.task_id
            || source_plan.task_id != self.task.task_id
            || self.plan.context_plan_id.as_ref() != Some(&source_plan.context_plan_id)
            || self.plan.context_token_limit() != Some(source_plan.budget_tokens)
        {
            return Err(ValidationError::new(
                "Engine provider request task, context plan, or budget does not bind",
            ));
        }
        bounded_wire_size(self, MAX_ENGINE_PROVIDER_EXECUTION_REQUEST_BYTES)
    }

    /// Recursively sorted request JSON, without the digest domain prefix.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ValidationError> {
        self.validate()?;
        let value = serde_json::to_value(self)
            .map_err(|_| ValidationError::new("encode Engine provider request"))?;
        serde_json::to_vec(&crate::entitlement::sort_json(value))
            .map_err(|_| ValidationError::new("canonicalize Engine provider request"))
    }

    /// Digest over the fixed domain followed by canonical request bytes.
    pub fn canonical_digest(&self) -> Result<Sha256Digest, ValidationError> {
        digest_with_domain(
            ENGINE_PROVIDER_EXECUTION_REQUEST_DIGEST_DOMAIN,
            &self.canonical_bytes()?,
        )
    }

    /// Stable admission identity before the store assigns an attempt ID.
    /// Only attempt_id is excluded; every task, plan, query and context field binds.
    pub fn canonical_intent_digest(&self) -> Result<Sha256Digest, ValidationError> {
        self.validate()?;
        let mut value = serde_json::to_value(self)
            .map_err(|_| ValidationError::new("encode Engine provider intent"))?;
        value
            .as_object_mut()
            .ok_or_else(|| ValidationError::new("invalid Engine provider intent"))?
            .remove("attempt_id");
        let bytes = serde_json::to_vec(&crate::entitlement::sort_json(value))
            .map_err(|_| ValidationError::new("canonicalize Engine provider intent"))?;
        digest_with_domain(INTENT_DIGEST_DOMAIN, &bytes)
    }
}

/// Provider result; it contains no signature, acceptance claim, savings claim,
/// or artifact-fetch authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineProviderExecutionResponseV1 {
    #[serde(deserialize_with = "deserialize_schema_version")]
    pub schema_version: u32,
    pub transport_version: u32,
    pub engine_interface_version: SemanticVersion,
    pub attempt_id: AttemptId,
    pub task_id: TaskId,
    pub plan_id: PlanId,
    pub context_digest: Sha256Digest,
    /// Full canonical request digest, including the assigned attempt identity.
    pub request_digest: Sha256Digest,
    pub provider: String,
    pub model: String,
    pub status: EngineProviderExecutionStatusV1,
    pub acceptance: AcceptanceState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<EngineProviderOutputV1>,
    pub usage: ViaProviderUsageV1,
    pub cost: EngineProviderCostV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<EngineFailureV1>,
}

impl EngineProviderExecutionResponseV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.schema_version != 1
            || self.transport_version != 1
            || self.engine_interface_version.as_str() != "1.0.0"
            || self.acceptance != AcceptanceState::Unknown
        {
            return Err(ValidationError::new(
                "invalid Engine provider execution response envelope",
            ));
        }
        validate_bounded_string(&self.provider, "provider")?;
        validate_bounded_string(&self.model, "model")?;
        self.usage.validate("provider usage")?;
        self.cost.validate()?;
        if matches!(self.cost, EngineProviderCostV1::UsagePricedEstimate { .. })
            && self.usage.state == crate::ViaUsageStateV1::Unavailable
        {
            return Err(ValidationError::new(
                "cannot price unavailable provider usage",
            ));
        }
        if let Some(output) = &self.output {
            output.validate()?;
        }
        if let Some(failure) = &self.failure {
            failure.validate()?;
            if failure.retryable_by_host {
                return Err(ValidationError::new(
                    "provider failures must not request automatic host retry",
                ));
            }
        }
        match (self.status, self.output.is_some(), self.failure.is_some()) {
            (EngineProviderExecutionStatusV1::Succeeded, true, false) => Ok(()),
            (
                EngineProviderExecutionStatusV1::Failed
                | EngineProviderExecutionStatusV1::Rejected
                | EngineProviderExecutionStatusV1::TimedOut
                | EngineProviderExecutionStatusV1::DispatchUncertain,
                false,
                true,
            ) => Ok(()),
            _ => Err(ValidationError::new(
                "Engine provider response status, output, and failure disagree",
            )),
        }?;
        bounded_wire_size(self, MAX_ENGINE_PROVIDER_EXECUTION_RESPONSE_BYTES)
    }

    pub fn validate_for(
        &self,
        request: &EngineProviderExecutionRequestV1,
    ) -> Result<(), ValidationError> {
        request.validate()?;
        self.validate()?;
        if self.attempt_id != request.attempt_id
            || self.task_id != request.task.task_id
            || self.plan_id != request.plan.plan_id
            || self.context_digest != request.materialization.materialized_digest
            || self.request_digest != request.canonical_digest()?
            || self.provider != request.plan.provider
            || self.model != request.plan.model
        {
            return Err(ValidationError::new(
                "Engine provider response does not bind its request",
            ));
        }
        Ok(())
    }
}

fn validate_provider_plan(plan: &ExecutionPlanV1) -> Result<(), ValidationError> {
    if plan.provider == "local-native"
        || plan.model == "local-native"
        || plan.provider == "auto"
        || plan.model == "auto"
        || plan.max_retries != 0
        || !plan.fallback_refs.is_empty()
    {
        return Err(ValidationError::new(
            "Engine provider request requires a concrete non-local provider plan",
        ));
    }
    Ok(())
}

fn bounded_wire_size<T: Serialize>(value: &T, limit: usize) -> Result<(), ValidationError> {
    let encoded = serde_json::to_vec(value)
        .map_err(|_| ValidationError::new("encode Engine provider wire value"))?;
    if encoded.len() > limit {
        return Err(ValidationError::new(
            "Engine provider wire value exceeds its byte bound",
        ));
    }
    Ok(())
}

fn digest_with_domain(domain: &[u8], bytes: &[u8]) -> Result<Sha256Digest, ValidationError> {
    let mut hash = Sha256::new();
    hash.update(domain);
    hash.update(bytes);
    let hex = hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Sha256Digest::new(format!("sha256:{hex}"))
}

fn sha256_digest(bytes: &[u8]) -> Result<Sha256Digest, ValidationError> {
    digest_with_domain(&[], bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        CapabilityId, ContextBudgetPolicyV1, ContextPlanId, ContextPlanProjectionV1,
        ContextStrategy, EngineContextPlanResponseV1, EngineContextSourcePlanResponseV1,
        ExecutionPlanV1, StopCondition, TaskComplexity,
    };

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).expect("valid identifier")
    }

    fn request_fixture() -> EngineProviderExecutionRequestV1 {
        let task_id: TaskId = id("task-1");
        let context_plan_id: ContextPlanId = id("context-plan-1");
        let mut projection = ContextPlanProjectionV1 {
            schema_version: 1,
            context_plan_id: context_plan_id.clone(),
            task_id: task_id.clone(),
            projection_digest: None,
            budget_tokens: 128,
            selections: Vec::new(),
            provider_stats: Default::default(),
            policy_decision_refs: Vec::new(),
            evidence: Vec::new(),
            extensions: Default::default(),
        };
        projection.projection_digest = Some(projection.compute_projection_digest().unwrap());
        let source_result = EngineContextPlanResponseV1 {
            schema_version: 1,
            transport_version: 1,
            engine_interface_version: SemanticVersion::new("1.0.0").unwrap(),
            plan: projection,
        };
        let source_plan =
            EngineContextSourcePlanResponseV1::new(source_result, Vec::new()).expect("source plan");
        let content = "context";
        let materialization = EngineContextSourceMaterializationResponseV1 {
            schema_version: 1,
            transport_version: 1,
            engine_interface_version: SemanticVersion::new("1.0.0").unwrap(),
            plan: source_plan,
            materialized_digest: sha256_digest(content.as_bytes()).unwrap(),
            materialized_token_count: 1,
            content: content.to_owned(),
        };
        EngineProviderExecutionRequestV1 {
            schema_version: 1,
            transport_version: 1,
            engine_interface_version: SemanticVersion::new("1.0.0").unwrap(),
            attempt_id: id("attempt-1"),
            task: TaskEnvelopeV1 {
                schema_version: 1,
                task_id: task_id.clone(),
                trace_id: id("trace-1"),
                project_id: id("project-1"),
                session_id: id("session-1"),
                agent_id: id("agent-1"),
                complexity: TaskComplexity::Medium,
                created_at: "2026-09-20T00:00:00Z".to_owned(),
                parent_task_id: None,
                tenant_id: Some(id("tenant-1")),
                intent: None,
                task_class: None,
                risk_class: None,
                quality_requirement_milli: None,
                cost_budget_micros: None,
                latency_budget_ms: None,
                data_classification: None,
                region_policy_ref: None,
                model_policy_ref: None,
                context_state_ref: None,
                outcome_contract_ref: None,
                extensions: Default::default(),
            },
            plan: ExecutionPlanV1 {
                schema_version: 1,
                plan_id: id("plan-1"),
                task_id: task_id.clone(),
                context_budget_tokens: 128,
                context_budget_policy: Some(ContextBudgetPolicyV1::TokenLimit { tokens: 128 }),
                context_strategy: ContextStrategy::Balanced,
                knowledge_refs: Vec::new(),
                capability_ids: vec![CapabilityId::new("provider-call").unwrap()],
                model: "model-1".to_owned(),
                provider: "provider-1".to_owned(),
                reasoning_allocation_milli: 0,
                max_retries: 0,
                fallback_refs: Vec::new(),
                stop_condition: StopCondition::OnCompletion,
                expected_cost_micros: 0,
                expected_quality_milli: 0,
                expected_latency_ms: 0,
                estimates: None,
                policy_decision_ref: None,
                scheduler_decision_ref: None,
                executor_agent_id: None,
                context_plan_id: Some(context_plan_id),
                capability_bindings: Vec::new(),
                extensions: Default::default(),
            },
            materialization,
            query: "find relevant context".to_owned(),
            max_output_tokens: 128,
        }
    }

    fn succeeded_response(
        request: &EngineProviderExecutionRequestV1,
    ) -> EngineProviderExecutionResponseV1 {
        let content = "provider output";
        EngineProviderExecutionResponseV1 {
            schema_version: 1,
            transport_version: 1,
            engine_interface_version: SemanticVersion::new("1.0.0").unwrap(),
            attempt_id: request.attempt_id.clone(),
            task_id: request.task.task_id.clone(),
            plan_id: request.plan.plan_id.clone(),
            context_digest: request.materialization.materialized_digest.clone(),
            request_digest: request.canonical_digest().unwrap(),
            provider: request.plan.provider.clone(),
            model: request.plan.model.clone(),
            status: EngineProviderExecutionStatusV1::Succeeded,
            acceptance: AcceptanceState::Unknown,
            output: Some(EngineProviderOutputV1 {
                content: content.to_owned(),
                sha256_digest: sha256_digest(content.as_bytes()).unwrap(),
            }),
            usage: ViaProviderUsageV1::measured(1, 0, 0, 1).unwrap(),
            cost: EngineProviderCostV1::Unavailable {},
            failure: None,
        }
    }

    #[test]
    fn request_and_response_bind_full_provider_join() {
        let request = request_fixture();
        let response = succeeded_response(&request);
        request.validate().unwrap();
        response.validate_for(&request).unwrap();
        assert_eq!(
            request.canonical_digest().unwrap(),
            request.canonical_digest().unwrap()
        );
    }

    #[test]
    fn request_rejects_local_native_or_retry_fallback_plan() {
        let mut request = request_fixture();
        request.plan.provider = "local-native".to_owned();
        assert!(request.validate().is_err());
        let mut request = request_fixture();
        request.plan.provider = "provider-1".to_owned();
        request.plan.max_retries = 1;
        assert!(request.validate().is_err());
    }

    #[test]
    fn response_rejects_unknown_fields_and_retryable_failures() {
        let request = request_fixture();
        let response = succeeded_response(&request);
        let mut value = serde_json::to_value(&request).unwrap();
        value["unknown"] = serde_json::json!(true);
        assert!(serde_json::from_value::<EngineProviderExecutionRequestV1>(value).is_err());

        let mut failed = response;
        failed.status = EngineProviderExecutionStatusV1::Failed;
        failed.output = None;
        failed.failure = Some(EngineFailureV1 {
            code: crate::EngineFailureCodeV1::Internal,
            retryable_by_host: true,
            recovery_ref: None,
        });
        assert!(failed.validate_for(&request).is_err());
    }

    #[test]
    fn cost_states_are_explicit_and_safe() {
        let request = request_fixture();
        let mut response = succeeded_response(&request);
        response.cost = EngineProviderCostV1::ObservedCharge { micros: 0 };
        assert!(response.validate_for(&request).is_ok());
        response.cost = EngineProviderCostV1::UsagePricedEstimate {
            micros: MAX_SAFE_INTEGER + 1,
        };
        assert!(response.validate_for(&request).is_err());
    }

    #[test]
    fn query_and_output_bounds_fail_closed() {
        let mut request = request_fixture();
        request.query = " ".to_owned();
        assert!(request.validate().is_err());
        let mut request = request_fixture();
        request.max_output_tokens = 0;
        assert!(request.validate().is_err());

        let request = request_fixture();
        let mut response = succeeded_response(&request);
        response.output.as_mut().unwrap().content =
            "x".repeat(MAX_ENGINE_PROVIDER_OUTPUT_BYTES + 1);
        assert!(response.validate_for(&request).is_err());
    }

    #[test]
    fn intent_is_stable_but_response_binds_every_attempt_and_query() {
        let first = request_fixture();
        let response = succeeded_response(&first);
        let mut next = first.clone();
        next.attempt_id = id("attempt-2");
        assert_eq!(
            first.canonical_intent_digest().unwrap(),
            next.canonical_intent_digest().unwrap()
        );
        assert_ne!(
            first.canonical_digest().unwrap(),
            next.canonical_digest().unwrap()
        );
        assert!(response.validate_for(&next).is_err());
        next = first.clone();
        next.query = "different request".into();
        assert_ne!(
            first.canonical_intent_digest().unwrap(),
            next.canonical_intent_digest().unwrap()
        );
        assert!(response.validate_for(&next).is_err());
        next = first.clone();
        next.max_output_tokens += 1;
        assert!(response.validate_for(&next).is_err());
        let mut unknown = response;
        unknown.usage = ViaProviderUsageV1::unavailable();
        unknown.cost = EngineProviderCostV1::UsagePricedEstimate { micros: 0 };
        assert!(unknown.validate().is_err());
    }
}
