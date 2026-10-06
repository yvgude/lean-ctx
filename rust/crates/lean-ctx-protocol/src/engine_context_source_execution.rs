// SPDX-License-Identifier: Apache-2.0

//! Typed wire envelope for the operator-owned explicit-source execution flow.
//!
//! This module only validates joins among the existing task, execution-plan,
//! source-plan, invocation, observation, and receipt projections.  It does not
//! authorize a caller, verify a signer, or create a second receipt authority.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    AcceptanceState, EngineContextSourceMaterializationRequestV1,
    EngineContextSourcePlanResponseV1, EngineInvocationV1, EngineObservationStatusV1,
    EngineObservationV1, EnginePolicyDecisionV1, ExecutionPlanV1, ProtocolReference, ReceiptId,
    SemanticVersion, Sha256Digest, TaskEnvelopeV1, ValidationError, deserialize_schema_version,
    validate_schema_version,
};

/// The host stdin request is bounded by the existing one-megabyte source
/// transport limit before this typed envelope is decoded.
pub const MAX_ENGINE_SOURCE_EXECUTION_REQUEST_BYTES: usize =
    crate::MAX_ENGINE_SOURCE_PLAN_REQUEST_BYTES;

/// Native Engine output uses the same one-megabyte bound as materialized input.
pub const MAX_ENGINE_SOURCE_EXECUTION_VIEW_BYTES: usize =
    crate::MAX_ENGINE_SOURCE_MATERIALIZED_CONTEXT_BYTES;

const TRANSPORT_VERSION: u32 = 1;
const ENGINE_INTERFACE_VERSION: &str = "1.0.0";
const LOCAL_NATIVE_PROVIDER: &str = "local-native";
const LOCAL_NATIVE_CAPABILITY: &str = "capability://leanctx/context-optimization";
const LOCAL_NATIVE_CAPABILITY_VERSION: &str = "1.0.0";
const INPUT_REF_PREFIX: &str = "input:source-materialization-sha256:";
const SOURCE_PLAN_EVIDENCE_PREFIX: &str = "artifact://execution/evidence/";
const TASK_REF_PREFIX: &str = "task:sha256:";
const PLAN_REF_PREFIX: &str = "plan:sha256:";

/// The exact operator request accepted by `context-sources-receipt`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineContextSourceExecutionRequestV1 {
    #[serde(deserialize_with = "deserialize_schema_version")]
    pub schema_version: u32,
    pub transport_version: u32,
    pub engine_interface_version: SemanticVersion,
    pub task: TaskEnvelopeV1,
    pub plan: ExecutionPlanV1,
    pub materialization: EngineContextSourceMaterializationRequestV1,
}

impl EngineContextSourceExecutionRequestV1 {
    /// Validate the request envelope and its cross-object identity joins.
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_header(
            self.schema_version,
            self.transport_version,
            &self.engine_interface_version,
        )?;
        self.task.validate()?;
        self.plan.validate()?;
        self.materialization.validate_payload()?;
        validate_header(
            self.materialization.source_plan.planning.schema_version,
            self.materialization.source_plan.planning.transport_version,
            &self
                .materialization
                .source_plan
                .planning
                .engine_interface_version,
        )?;
        if self.task.task_id != self.plan.task_id
            || self.materialization.source_plan.planning.task_id != self.task.task_id
        {
            return Err(ValidationError::new(
                "source execution request task IDs do not agree",
            ));
        }
        validate_local_native_plan(&self.plan)?;
        if self.plan.context_plan_id.is_some()
            || self.task.region_policy_ref.is_some()
            || self.task.model_policy_ref.is_some()
        {
            return Err(ValidationError::new(
                "source execution request is not a local-native declaration",
            ));
        }
        bounded_wire_size(self, MAX_ENGINE_SOURCE_EXECUTION_REQUEST_BYTES)
    }
}

/// The actual output view returned by the native Engine adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineContextSourceExecutionViewV1 {
    pub text: String,
    pub output_ref: Option<ProtocolReference>,
    pub output_digest: Option<Sha256Digest>,
}

impl EngineContextSourceExecutionViewV1 {
    fn validate_for(&self, observation: &EngineObservationV1) -> Result<(), ValidationError> {
        if self.text.len() > MAX_ENGINE_SOURCE_EXECUTION_VIEW_BYTES
            || self.output_ref != observation.output_ref
            || self.output_digest != observation.output_digest
        {
            return Err(ValidationError::new(
                "source execution view does not bind the Engine observation",
            ));
        }
        let output_digest = self.output_digest.as_ref().ok_or_else(|| {
            ValidationError::new("source execution view is missing its output digest")
        })?;
        let expected_output_ref = format!("output:{}", output_digest.hex());
        if self.output_ref.as_ref().map(ProtocolReference::as_str)
            != Some(expected_output_ref.as_str())
        {
            return Err(ValidationError::new(
                "source execution output ref does not bind its digest",
            ));
        }
        let digest = sha256_digest(self.text.as_bytes())?;
        if self.output_digest.as_ref() != Some(&digest) {
            return Err(ValidationError::new(
                "source execution view digest does not bind its text",
            ));
        }
        Ok(())
    }
}

/// The canonical receipt identity projection returned by the host authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineContextSourceCanonicalReceiptV1 {
    pub receipt_id: ReceiptId,
    pub receipt_ref: ProtocolReference,
    pub receipt_digest: Sha256Digest,
    pub outcome: AcceptanceState,
}

impl EngineContextSourceCanonicalReceiptV1 {
    fn validate(&self) -> Result<(), ValidationError> {
        if self.outcome != AcceptanceState::Unknown
            || self.receipt_ref.as_str() != format!("id:{}", self.receipt_digest.as_str())
        {
            return Err(ValidationError::new(
                "source execution receipt must be unknown and digest-bound",
            ));
        }
        Ok(())
    }
}

/// The actual v1 source execution response; fields intentionally match the
/// existing JSON object emitted by the CLI host adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineContextSourceExecutionResponseV1 {
    #[serde(deserialize_with = "deserialize_schema_version")]
    pub schema_version: u32,
    pub transport_version: u32,
    pub engine_interface_version: SemanticVersion,
    pub source_plan: EngineContextSourcePlanResponseV1,
    pub execution_plan: ExecutionPlanV1,
    pub view: EngineContextSourceExecutionViewV1,
    pub invocation: EngineInvocationV1,
    pub observation: EngineObservationV1,
    pub canonical_receipt: EngineContextSourceCanonicalReceiptV1,
}

impl EngineContextSourceExecutionResponseV1 {
    /// Validate response versions, canonical source evidence, and all joins
    /// needed to consume this response as one bounded execution result.
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_header(
            self.schema_version,
            self.transport_version,
            &self.engine_interface_version,
        )?;
        self.source_plan.validate_binding()?;
        self.execution_plan.validate()?;
        self.invocation.validate()?;
        self.observation.validate_for(&self.invocation)?;
        self.view.validate_for(&self.observation)?;
        self.canonical_receipt.validate()?;

        if self.execution_plan.task_id != self.source_plan.result.plan.task_id
            || self.execution_plan.context_plan_id.as_ref()
                != Some(&self.source_plan.result.plan.context_plan_id)
            || self.execution_plan.context_token_limit()
                != Some(self.source_plan.result.plan.budget_tokens)
        {
            return Err(ValidationError::new(
                "source execution response plan does not bind its source plan",
            ));
        }
        validate_local_native_plan(&self.execution_plan)?;
        if self.invocation.operation.capability_id.as_str() != LOCAL_NATIVE_CAPABILITY
            || self.invocation.operation.capability_version.as_str()
                != LOCAL_NATIVE_CAPABILITY_VERSION
        {
            return Err(ValidationError::new(
                "source execution response is not bound to the local-native capability",
            ));
        }
        if self.invocation.policy_admission.decision != EnginePolicyDecisionV1::Admitted
            || self.observation.status != EngineObservationStatusV1::Succeeded
            || self.observation.source_lineage != self.invocation.source_refs
        {
            return Err(ValidationError::new(
                "source execution response is not an admitted complete observation",
            ));
        }
        let source_plan_digest = canonical_sha256(&self.source_plan)?;
        let execution_plan_digest = canonical_sha256(&self.execution_plan)?;
        validate_source_references(
            &self.invocation,
            &source_plan_digest,
            &execution_plan_digest,
        )?;
        let receipt_link = self.observation.receipt_link.as_ref().ok_or_else(|| {
            ValidationError::new("source execution response is missing the Engine receipt link")
        })?;
        if receipt_link.receipt_ref.as_str()
            != format!("receipt:{}", receipt_link.receipt_digest.as_str())
        {
            return Err(ValidationError::new(
                "source execution Engine receipt ref does not bind its digest",
            ));
        }
        Ok(())
    }

    /// Validate this response against the exact request that admitted it.
    /// The task evidence is not repeated in the response wire object, so this
    /// join is explicit for consumers that retain the original request.
    pub fn validate_against(
        &self,
        request: &EngineContextSourceExecutionRequestV1,
    ) -> Result<(), ValidationError> {
        request.validate()?;
        self.validate()?;
        if self.source_plan.binding_digest != request.materialization.expected_binding_digest
            || self.execution_plan.task_id != request.task.task_id
            || self.execution_plan.provider != request.plan.provider
            || self.execution_plan.model != request.plan.model
            || self.execution_plan.capability_ids != request.plan.capability_ids
            || self.execution_plan.context_token_limit() != request.plan.context_token_limit()
        {
            return Err(ValidationError::new(
                "source execution response does not bind its original request",
            ));
        }
        let mut expected_plan = request.plan.clone();
        expected_plan.context_plan_id = self.execution_plan.context_plan_id.clone();
        let decision_ref = "context_autopilot_decision_ref";
        if !expected_plan.extensions.contains_key(decision_ref) {
            let value = self
                .execution_plan
                .extensions
                .get(decision_ref)
                .filter(|value| value.as_str().is_some_and(|id| !id.is_empty()))
                .ok_or_else(|| {
                    ValidationError::new("source execution decision reference is missing")
                })?;
            expected_plan
                .extensions
                .insert(decision_ref, value.clone())?;
        }
        if expected_plan != self.execution_plan
            || self.source_plan.result.plan.budget_tokens
                > u64::from(request.materialization.source_plan.planning.budget_tokens)
            || self.source_plan.source_bindings.iter().any(|binding| {
                !request
                    .materialization
                    .source_plan
                    .sources
                    .iter()
                    .any(|source| source.descriptor == *binding)
            })
        {
            return Err(ValidationError::new(
                "source execution changed the declared plan or sources",
            ));
        }
        if let Some(epoch) = &request.materialization.planning_evaluation_time {
            let actual = self
                .source_plan
                .result
                .plan
                .extensions
                .get("context_plan_evaluation_v1")
                .and_then(|marker| marker.get("evaluation_time"))
                .and_then(serde_json::Value::as_str);
            if actual != Some(epoch.as_str()) {
                return Err(ValidationError::new(
                    "source execution changed the declared evaluation time",
                ));
            }
        }
        let task_digest = canonical_sha256(&request.task)?;
        let expected_task_ref = format!("{TASK_REF_PREFIX}{}", task_digest.hex());
        if !self
            .invocation
            .source_refs
            .iter()
            .any(|reference| reference.as_str() == expected_task_ref)
        {
            return Err(ValidationError::new(
                "source execution task evidence does not bind the original task",
            ));
        }
        Ok(())
    }
}

fn validate_header(
    schema_version: u32,
    transport_version: u32,
    engine_interface_version: &SemanticVersion,
) -> Result<(), ValidationError> {
    validate_schema_version(schema_version)?;
    if transport_version != TRANSPORT_VERSION
        || engine_interface_version.as_str() != ENGINE_INTERFACE_VERSION
    {
        return Err(ValidationError::new(
            "unsupported source execution transport or Engine interface version",
        ));
    }
    Ok(())
}

fn validate_local_native_plan(plan: &ExecutionPlanV1) -> Result<(), ValidationError> {
    if plan.provider != LOCAL_NATIVE_PROVIDER
        || plan.model != LOCAL_NATIVE_PROVIDER
        || plan.capability_ids.len() != 1
        || plan.capability_ids[0].as_str() != LOCAL_NATIVE_CAPABILITY
        || plan.capability_bindings.len() != 1
        || plan.capability_bindings[0].capability_id.as_str() != LOCAL_NATIVE_CAPABILITY
        || plan.capability_bindings[0].version != LOCAL_NATIVE_CAPABILITY_VERSION
    {
        return Err(ValidationError::new(
            "source execution plan is not bound to the local-native capability",
        ));
    }
    Ok(())
}

fn validate_source_references(
    invocation: &EngineInvocationV1,
    source_plan_digest: &Sha256Digest,
    execution_plan_digest: &Sha256Digest,
) -> Result<(), ValidationError> {
    if invocation.source_refs.len() != 4 {
        return Err(ValidationError::new(
            "source execution invocation must contain four bounded lineage references",
        ));
    }
    let declared_input_digest = invocation
        .input_ref
        .as_str()
        .strip_prefix(INPUT_REF_PREFIX)
        .ok_or_else(|| {
            ValidationError::new("source execution input_ref is not a materialization reference")
        })
        .and_then(digest_suffix)?;
    if declared_input_digest != invocation.input_digest {
        return Err(ValidationError::new(
            "source execution input_ref does not bind input_digest",
        ));
    }
    let mut input_digest = None;
    let mut evidence_digest = None;
    let mut task_count = 0_u8;
    let mut plan_digest = None;
    for reference in &invocation.source_refs {
        let value = reference.as_str();
        if let Some(suffix) = value.strip_prefix(INPUT_REF_PREFIX) {
            if input_digest.replace(digest_suffix(suffix)?).is_some() {
                return Err(ValidationError::new(
                    "duplicate source execution input reference",
                ));
            }
        } else if let Some(suffix) = value.strip_prefix(SOURCE_PLAN_EVIDENCE_PREFIX) {
            if evidence_digest.replace(digest_suffix(suffix)?).is_some() {
                return Err(ValidationError::new(
                    "duplicate source execution evidence reference",
                ));
            }
        } else if let Some(suffix) = value.strip_prefix(TASK_REF_PREFIX) {
            digest_suffix(suffix)?;
            task_count = task_count.saturating_add(1);
        } else if let Some(suffix) = value.strip_prefix(PLAN_REF_PREFIX) {
            if plan_digest.replace(digest_suffix(suffix)?).is_some() {
                return Err(ValidationError::new(
                    "duplicate source execution plan reference",
                ));
            }
        } else {
            return Err(ValidationError::new(
                "source execution invocation contains an unknown lineage reference",
            ));
        }
    }
    if input_digest.as_ref() != Some(&invocation.input_digest)
        || evidence_digest.as_ref() != Some(source_plan_digest)
        || task_count != 1
        || plan_digest.is_none()
    {
        return Err(ValidationError::new(
            "source execution invocation lineage does not bind its input and source plan",
        ));
    }
    if plan_digest.as_ref() != Some(execution_plan_digest) {
        return Err(ValidationError::new(
            "source execution plan evidence does not bind execution_plan",
        ));
    }
    Ok(())
}

fn digest_suffix(suffix: &str) -> Result<Sha256Digest, ValidationError> {
    Sha256Digest::new(format!("sha256:{suffix}"))
}

pub(super) fn canonical_sha256<T: Serialize>(value: &T) -> Result<Sha256Digest, ValidationError> {
    let value = serde_json::to_value(value).map_err(|error| {
        ValidationError::new(format!("serialize source execution value: {error}"))
    })?;
    let bytes = serde_json::to_vec(&crate::entitlement::sort_json(value)).map_err(|error| {
        ValidationError::new(format!("canonicalize source execution value: {error}"))
    })?;
    sha256_digest(&bytes)
}

pub(super) fn sha256_digest(bytes: &[u8]) -> Result<Sha256Digest, ValidationError> {
    let mut value = String::with_capacity(71);
    value.push_str("sha256:");
    for byte in Sha256::digest(bytes) {
        write!(&mut value, "{byte:02x}")
            .map_err(|_| ValidationError::new("source execution digest encoding failed"))?;
    }
    Sha256Digest::new(value)
}

fn bounded_wire_size<T: Serialize>(value: &T, limit: usize) -> Result<(), ValidationError> {
    let encoded = serde_json::to_vec(value).map_err(|error| {
        ValidationError::new(format!("serialize source execution request: {error}"))
    })?;
    if encoded.len() > limit {
        return Err(ValidationError::new(
            "source execution request exceeds its byte bound",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        CapabilityId, EngineInvocationIdV1, EngineOperationV1, EnginePolicyAdmissionV1,
        ResolvedLocalEngineIdentityV1,
    };

    fn digest(value: &str) -> Sha256Digest {
        sha256_digest(value.as_bytes()).expect("digest")
    }

    #[test]
    fn source_lineage_binds_input_and_descriptor_evidence() {
        let input = digest("materialized");
        let evidence = digest("source-plan");
        let invocation = EngineInvocationV1 {
            schema_version: 1,
            invocation_id: EngineInvocationIdV1::new("invocation-1").unwrap(),
            engine: ResolvedLocalEngineIdentityV1 {
                engine_id: "engine-1".into(),
                engine_version: SemanticVersion::new("1.0.0").unwrap(),
            },
            operation: EngineOperationV1 {
                capability_id: CapabilityId::new("capability://leanctx/context-optimization")
                    .unwrap(),
                capability_version: SemanticVersion::new("1.0.0").unwrap(),
            },
            input_ref: ProtocolReference::new(format!("{INPUT_REF_PREFIX}{}", input.hex()))
                .unwrap(),
            input_digest: input.clone(),
            source_refs: vec![
                ProtocolReference::new(format!("{INPUT_REF_PREFIX}{}", input.hex())).unwrap(),
                ProtocolReference::new(format!("{SOURCE_PLAN_EVIDENCE_PREFIX}{}", evidence.hex()))
                    .unwrap(),
                ProtocolReference::new(format!("{TASK_REF_PREFIX}{}", digest("task").hex()))
                    .unwrap(),
                ProtocolReference::new(format!("{PLAN_REF_PREFIX}{}", digest("plan").hex()))
                    .unwrap(),
            ],
            policy_admission: EnginePolicyAdmissionV1 {
                policy_ref: ProtocolReference::new("policy:engine-transport-v1:admitted").unwrap(),
                decision: EnginePolicyDecisionV1::Admitted,
            },
        };
        validate_source_references(&invocation, &evidence, &digest("plan")).unwrap();
        let mut altered = invocation.clone();
        altered.input_digest = digest("different");
        assert!(validate_source_references(&altered, &evidence, &digest("plan")).is_err());
        let mut altered_input_ref = invocation.clone();
        altered_input_ref.input_ref = altered_input_ref.source_refs[2].clone();
        assert!(
            validate_source_references(&altered_input_ref, &evidence, &digest("plan")).is_err()
        );
        let mut altered_plan = invocation;
        altered_plan.source_refs[3] =
            ProtocolReference::new(format!("{PLAN_REF_PREFIX}{}", digest("other").hex())).unwrap();
        assert!(validate_source_references(&altered_plan, &evidence, &digest("plan")).is_err());
    }

    #[test]
    fn canonical_receipt_is_unknown_and_id_ref_digest_bound() {
        let receipt_digest = digest("receipt");
        let receipt = EngineContextSourceCanonicalReceiptV1 {
            receipt_id: ReceiptId::new("receipt-id").unwrap(),
            receipt_ref: ProtocolReference::new(format!("id:{}", receipt_digest.as_str())).unwrap(),
            receipt_digest,
            outcome: AcceptanceState::Unknown,
        };
        receipt.validate().unwrap();
        let mut accepted = receipt;
        accepted.outcome = AcceptanceState::Accepted;
        assert!(accepted.validate().is_err());
    }
}
