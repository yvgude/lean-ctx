// SPDX-License-Identifier: Apache-2.0
//! Validated join between admitted task/plan and local Engine lineage.

use std::collections::BTreeSet;
use std::fmt;

use lean_ctx_protocol::{
    EngineContextSourcePlanResponseV1, EngineReceiptLinkV1, ExecutionPlanV1, ProtocolReference,
    ReceiptCapabilityLinkV1, ReceiptLineageV1, Sha256Digest, TaskEnvelopeV1,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::core::canonical;
use crate::core::engine_interface::VerifiedEngineReceiptV1;

const SOURCE_MATERIALIZATION_INPUT_PREFIX: &str = "input:source-materialization-sha256:";
const SOURCE_PLAN_EVIDENCE_PREFIX: &str = "artifact://execution/evidence/";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReceiptDocumentInputsV1 {
    pub lineage: ReceiptLineageV1,
    pub receipt_link: EngineReceiptLinkV1,
    pub source_lineage: Vec<ProtocolReference>,
    pub input_ref: ProtocolReference,
    pub input_digest: Sha256Digest,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReceiptDocumentAdapterError(String);

impl fmt::Display for ReceiptDocumentAdapterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ReceiptDocumentAdapterError {}

/// Validate every cross-document identity before exposing receipt inputs.
pub(crate) fn join_receipt_document_inputs(
    task: &TaskEnvelopeV1,
    plan: &ExecutionPlanV1,
    verified: &VerifiedEngineReceiptV1,
    receipt_link: &EngineReceiptLinkV1,
) -> Result<ReceiptDocumentInputsV1, ReceiptDocumentAdapterError> {
    let invocation = verified.invocation();
    let observation = verified.observation();
    task.validate().map_err(protocol_error)?;
    plan.validate().map_err(protocol_error)?;
    invocation.validate().map_err(protocol_error)?;
    if observation.receipt_link.is_some() {
        return fail("verified Engine receipt observation must omit receipt_link");
    }
    observation
        .validate_for(invocation)
        .map_err(protocol_error)?;
    receipt_link
        .validate_for(invocation)
        .map_err(protocol_error)?;
    if verified.digest() != &receipt_link.receipt_digest {
        return fail("Engine receipt link digest disagrees with verified artifact");
    }
    if receipt_link.receipt_ref.as_str()
        != format!("receipt:{}", receipt_link.receipt_digest.as_str())
    {
        return fail("Engine receipt ref does not bind its advertised digest");
    }
    if plan.task_id != task.task_id {
        return fail("execution plan task_id does not match task envelope");
    }
    if plan.capability_ids != [invocation.operation.capability_id.clone()]
        || plan.capability_bindings.len() != 1
        || plan.capability_bindings[0].capability_id != invocation.operation.capability_id
        || plan.capability_bindings[0].version != invocation.operation.capability_version.as_str()
    {
        return fail("execution plan does not admit the invoked capability");
    }
    if plan.policy_decision_ref.as_deref() != Some(invocation.policy_admission.policy_ref.as_str())
    {
        return fail("execution plan policy decision disagrees with Engine admission");
    }
    let invocation_sources = invocation
        .source_refs
        .iter()
        .map(ProtocolReference::as_str)
        .collect::<BTreeSet<_>>();
    let observation_sources = observation
        .source_lineage
        .iter()
        .map(ProtocolReference::as_str)
        .collect::<BTreeSet<_>>();
    if invocation_sources != observation_sources {
        return fail("Engine observation source lineage is not the exact invocation lineage");
    }
    let bindings = crate::core::engine_interface::planning::binding_refs(task, plan)
        .map_err(ReceiptDocumentAdapterError)?;
    if !bindings
        .iter()
        .all(|reference| invocation_sources.contains(reference.as_str()))
    {
        return fail("Engine invocation did not bind this task and plan before execution");
    }
    if invocation_sources.iter().any(|reference| {
        (reference.starts_with("task:") || reference.starts_with("plan:"))
            && !bindings
                .iter()
                .any(|binding| binding.as_str() == *reference)
    }) {
        return fail("Engine invocation mixes foreign task or plan bindings");
    }
    if invocation.operation.capability_id.as_str() == crate::core::engine_interface::CAPABILITY_ID {
        crate::core::engine_interface::planning::validate_native_plan(
            task,
            plan,
            &invocation.policy_admission,
        )
        .map_err(ReceiptDocumentAdapterError)?;
        if invocation
            .input_ref
            .as_str()
            .starts_with(SOURCE_MATERIALIZATION_INPUT_PREFIX)
        {
            validate_materialized_source_lineage(
                task,
                plan,
                invocation,
                &invocation_sources,
                &bindings,
            )?;
        } else if invocation_sources.len() != 4
            || !invocation_sources.contains(invocation.input_ref.as_str())
            || invocation_sources
                .iter()
                .filter(|reference| reference.starts_with("source:canonical-path-sha256:"))
                .count()
                != 1
        {
            return fail("native Engine receipt has unexpected source dependencies");
        }
    }
    let invocation_ref = canonical_digest(invocation);
    let lineage = ReceiptLineageV1 {
        task_id: task.task_id.clone(),
        task_ref: canonical_digest(task),
        plan_id: plan.plan_id.clone(),
        plan_ref: canonical_digest(plan),
        invocation_id: invocation.invocation_id.as_str().to_owned(),
        invocation_ref: invocation_ref.clone(),
        identity_ref: canonical_digest(&task.agent_id),
        policy_refs: vec![canonical_digest(&invocation.policy_admission)],
        capabilities: vec![ReceiptCapabilityLinkV1 {
            capability_id: invocation.operation.capability_id.clone(),
            capability_version: invocation.operation.capability_version.clone(),
            invocation_ref,
        }],
    };
    Ok(ReceiptDocumentInputsV1 {
        lineage,
        receipt_link: receipt_link.clone(),
        source_lineage: observation.source_lineage.clone(),
        input_ref: invocation.input_ref.clone(),
        input_digest: invocation.input_digest.clone(),
    })
}

/// Validate the bounded lineage shape used when the Engine executes an
/// already-materialized source snapshot.  The descriptor-only source-plan
/// artifact is the authoritative bundle for all selected source bindings; the
/// invocation carries only one evidence ref, keeping the protocol's 32-ref
/// lineage bound independent of the source count.
fn validate_materialized_source_lineage(
    task: &TaskEnvelopeV1,
    plan: &ExecutionPlanV1,
    invocation: &lean_ctx_protocol::EngineInvocationV1,
    invocation_sources: &BTreeSet<&str>,
    bindings: &[ProtocolReference; 2],
) -> Result<(), ReceiptDocumentAdapterError> {
    let input_hex = invocation
        .input_ref
        .as_str()
        .strip_prefix(SOURCE_MATERIALIZATION_INPUT_PREFIX)
        .ok_or_else(|| {
            ReceiptDocumentAdapterError("materialized input ref is not canonical".into())
        })?;
    let input_digest = Sha256Digest::new(format!("sha256:{input_hex}")).map_err(protocol_error)?;
    if input_digest != invocation.input_digest {
        return fail("materialized input ref does not bind the invocation digest");
    }

    let evidence_refs = invocation
        .source_refs
        .iter()
        .filter_map(|reference| {
            reference
                .as_str()
                .strip_prefix(SOURCE_PLAN_EVIDENCE_PREFIX)
                .map(|digest| (reference.as_str(), digest))
        })
        .collect::<Vec<_>>();
    if evidence_refs.len() != 1 {
        return fail("materialized source invocation must contain one source-plan evidence ref");
    }
    let (evidence_ref, evidence_hex) = evidence_refs[0];
    let evidence_digest =
        Sha256Digest::new(format!("sha256:{evidence_hex}")).map_err(protocol_error)?;
    let evidence_bytes = crate::core::engine_artifact::read_content(
        "execution/evidence",
        evidence_digest.hex(),
        "json",
    )
    .map_err(|_| {
        ReceiptDocumentAdapterError("materialized source-plan evidence unavailable".into())
    })?;
    let mut deserializer = serde_json::Deserializer::from_slice(&evidence_bytes);
    let source_plan = EngineContextSourcePlanResponseV1::deserialize(&mut deserializer)
        .map_err(protocol_error)?;
    deserializer.end().map_err(protocol_error)?;
    if canonical::canonical_serialize(&source_plan) != evidence_bytes {
        return fail("materialized source-plan evidence is not canonical");
    }
    source_plan.validate_binding().map_err(protocol_error)?;
    if source_plan.result.plan.task_id != task.task_id
        || plan.context_plan_id.as_ref() != Some(&source_plan.result.plan.context_plan_id)
        || plan.context_token_limit() != Some(source_plan.result.plan.budget_tokens)
    {
        return fail("materialized source-plan evidence does not bind the execution plan");
    }
    crate::core::engine_interface::planning::context_binding::validate_projection(
        plan,
        &source_plan.result.plan,
    )
    .map_err(ReceiptDocumentAdapterError)?;

    let mut expected_sources = BTreeSet::new();
    expected_sources.insert(invocation.input_ref.as_str());
    expected_sources.insert(evidence_ref);
    expected_sources.extend(bindings.iter().map(ProtocolReference::as_str));
    if expected_sources != *invocation_sources {
        return fail("materialized source invocation has unexpected source dependencies");
    }
    Ok(())
}

fn canonical_digest<T: Serialize>(value: &T) -> Sha256Digest {
    let mut digest = String::with_capacity(71);
    digest.push_str("sha256:");
    for byte in Sha256::digest(canonical::canonical_serialize(value)) {
        use std::fmt::Write as _;
        write!(&mut digest, "{byte:02x}").expect("writing to String cannot fail");
    }
    Sha256Digest::new(digest).expect("locally generated digest is canonical")
}

fn protocol_error(error: impl fmt::Display) -> ReceiptDocumentAdapterError {
    ReceiptDocumentAdapterError(error.to_string())
}

fn fail<T>(message: &str) -> Result<T, ReceiptDocumentAdapterError> {
    Err(ReceiptDocumentAdapterError(message.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lean_ctx_protocol::{
        ContextStrategy, EngineInvocationV1, EngineObservationStatusV1, EngineObservationV1,
        EngineOperationV1, EnginePolicyAdmissionV1, EnginePolicyDecisionV1, ExecutionPlanV1,
        PlanId, ProjectId, ReceiptId, ResolvedLocalEngineIdentityV1, RiskClass, SemanticVersion,
        SessionId, StopCondition, TaskComplexity, TaskEnvelopeV1, TaskId, TraceId,
    };

    struct Fixture {
        _data_dir: crate::core::data_dir::IsolatedDataDir,
        task: TaskEnvelopeV1,
        plan: ExecutionPlanV1,
        invocation: EngineInvocationV1,
        observation: EngineObservationV1,
        verified: VerifiedEngineReceiptV1,
    }

    fn digest(hex: char) -> Sha256Digest {
        Sha256Digest::new(format!("sha256:{}", hex.to_string().repeat(64))).unwrap()
    }

    fn artifact_digest(bytes: &[u8]) -> Sha256Digest {
        let mut hex = String::with_capacity(64);
        for byte in Sha256::digest(bytes) {
            use std::fmt::Write as _;
            write!(&mut hex, "{byte:02x}").unwrap();
        }
        Sha256Digest::new(format!("sha256:{hex}")).unwrap()
    }

    fn reference(value: &str) -> ProtocolReference {
        ProtocolReference::new(value).unwrap()
    }

    fn fixture() -> Fixture {
        let data_dir = crate::core::data_dir::isolated_data_dir();
        let task_id = TaskId::new("task-1").unwrap();
        let trace_id = TraceId::new("trace-1").unwrap();
        let project_id = ProjectId::new("project-1").unwrap();
        let session_id = SessionId::new("session-1").unwrap();
        let agent_id = lean_ctx_protocol::AgentId::new("agent-1").unwrap();
        let capability_id = lean_ctx_protocol::CapabilityId::new("capability-1").unwrap();
        let capability_version = SemanticVersion::new("1.0.0").unwrap();
        let input_ref = reference("input:1");
        let source_ref = reference("source:1");
        let policy_ref = reference("policy:1");
        let input_digest = digest('a');
        let task = TaskEnvelopeV1 {
            schema_version: 1,
            task_id: task_id.clone(),
            trace_id: trace_id.clone(),
            project_id,
            session_id,
            agent_id,
            complexity: TaskComplexity::Medium,
            created_at: "2026-08-23T12:00:00Z".to_owned(),
            parent_task_id: None,
            tenant_id: None,
            intent: None,
            task_class: None,
            risk_class: Some(RiskClass::Low),
            quality_requirement_milli: None,
            cost_budget_micros: None,
            latency_budget_ms: None,
            data_classification: None,
            region_policy_ref: None,
            model_policy_ref: None,
            context_state_ref: None,
            outcome_contract_ref: None,
            extensions: Default::default(),
        };
        let plan = ExecutionPlanV1 {
            schema_version: 1,
            plan_id: PlanId::new("plan-1").unwrap(),
            task_id,
            context_budget_tokens: 100,
            context_budget_policy: None,
            context_strategy: ContextStrategy::Balanced,
            knowledge_refs: Vec::new(),
            capability_ids: vec![capability_id.clone()],
            model: "model-1".to_owned(),
            provider: "provider-1".to_owned(),
            reasoning_allocation_milli: 500,
            max_retries: 0,
            fallback_refs: Vec::new(),
            stop_condition: StopCondition::OnCompletion,
            expected_cost_micros: 1,
            estimates: None,
            expected_quality_milli: 500,
            expected_latency_ms: 1,
            policy_decision_ref: Some(policy_ref.as_str().to_owned()),
            scheduler_decision_ref: None,
            executor_agent_id: None,
            context_plan_id: None,
            capability_bindings: vec![lean_ctx_protocol::CapabilityBindingV1 {
                capability_id: capability_id.clone(),
                version: "1.0.0".to_owned(),
                manifest_digest: None,
            }],
            extensions: Default::default(),
        };
        let invocation_id = lean_ctx_protocol::EngineInvocationIdV1::new("invocation-1").unwrap();
        let invocation = EngineInvocationV1 {
            schema_version: 1,
            invocation_id: invocation_id.clone(),
            engine: ResolvedLocalEngineIdentityV1 {
                engine_id: "engine-1".to_owned(),
                engine_version: capability_version.clone(),
            },
            operation: EngineOperationV1 {
                capability_id,
                capability_version,
            },
            input_ref: input_ref.clone(),
            input_digest,
            source_refs: {
                let mut refs = vec![input_ref, source_ref.clone()];
                refs.extend(
                    crate::core::engine_interface::planning::binding_refs(&task, &plan).unwrap(),
                );
                refs
            },
            policy_admission: EnginePolicyAdmissionV1 {
                policy_ref,
                decision: EnginePolicyDecisionV1::Admitted,
            },
        };
        let mut observation_without_link = EngineObservationV1 {
            schema_version: 1,
            invocation_id,
            status: EngineObservationStatusV1::Succeeded,
            output_ref: Some(reference("output:1")),
            output_digest: Some(digest('c')),
            source_lineage: invocation.source_refs.clone(),
            measurements: Vec::new(),
            failure: None,
            receipt_link: None,
        };
        let artifact_bytes = crate::core::engine_interface::canonical_engine_receipt_artifact_bytes(
            &invocation,
            &observation_without_link,
        );
        let receipt_digest = artifact_digest(&artifact_bytes);
        crate::core::engine_interface::persist_engine_artifact_content(
            "engine-interface/v1/receipts",
            receipt_digest.hex(),
            "json",
            &artifact_bytes,
        )
        .unwrap();
        let verified = crate::core::engine_interface::read_verified_engine_receipt(
            &receipt_digest,
            &invocation,
            &observation_without_link,
        )
        .unwrap();
        let receipt_link = EngineReceiptLinkV1 {
            schema_version: 1,
            receipt_id: ReceiptId::new("receipt-1").unwrap(),
            receipt_ref: reference(&format!("receipt:{}", receipt_digest.as_str())),
            receipt_digest,
            invocation_id: invocation.invocation_id.clone(),
        };
        observation_without_link.receipt_link = Some(receipt_link);
        let observation = observation_without_link;
        Fixture {
            _data_dir: data_dir,
            task,
            plan,
            invocation,
            observation,
            verified,
        }
    }

    fn persist_verified(
        invocation: &EngineInvocationV1,
        observation: &EngineObservationV1,
    ) -> (VerifiedEngineReceiptV1, EngineReceiptLinkV1) {
        assert!(observation.receipt_link.is_none());
        let artifact_bytes = crate::core::engine_interface::canonical_engine_receipt_artifact_bytes(
            invocation,
            observation,
        );
        let digest = artifact_digest(&artifact_bytes);
        crate::core::engine_interface::persist_engine_artifact_content(
            "engine-interface/v1/receipts",
            digest.hex(),
            "json",
            &artifact_bytes,
        )
        .unwrap();
        let verified = crate::core::engine_interface::read_verified_engine_receipt(
            &digest,
            invocation,
            observation,
        )
        .unwrap();
        let link = EngineReceiptLinkV1 {
            schema_version: 1,
            receipt_id: ReceiptId::new("receipt-test").unwrap(),
            receipt_ref: reference(&format!("receipt:{}", digest.as_str())),
            receipt_digest: digest,
            invocation_id: invocation.invocation_id.clone(),
        };
        (verified, link)
    }

    fn assert_receipt_ref_rejected(receipt_ref: &str) {
        let mut fixture = fixture();
        fixture
            .observation
            .receipt_link
            .as_mut()
            .unwrap()
            .receipt_ref = reference(receipt_ref);
        assert!(
            join_receipt_document_inputs(
                &fixture.task,
                &fixture.plan,
                &fixture.verified,
                fixture.observation.receipt_link.as_ref().unwrap(),
            )
            .is_err()
        );
    }

    #[test]
    fn exact_receipt_uri_binds_the_advertised_digest() {
        let fixture = fixture();
        let inputs = join_receipt_document_inputs(
            &fixture.task,
            &fixture.plan,
            &fixture.verified,
            fixture.observation.receipt_link.as_ref().unwrap(),
        )
        .unwrap();
        assert_eq!(
            inputs.receipt_link.receipt_ref.as_str(),
            format!("receipt:{}", inputs.receipt_link.receipt_digest.as_str())
        );
    }

    #[test]
    fn noncanonical_receipt_uri_forms_are_rejected() {
        let fixture = fixture();
        let digest = fixture
            .observation
            .receipt_link
            .as_ref()
            .unwrap()
            .receipt_digest
            .as_str()
            .to_owned();
        for receipt_ref in [
            format!("wrong:{digest}"),
            format!("prefix:receipt:{digest}"),
            format!("receipt:{digest}:suffix"),
            digest,
        ] {
            assert_receipt_ref_rejected(&receipt_ref);
        }
    }

    #[test]
    fn canonical_publication_rejects_post_execution_task_or_plan_substitution() {
        let fixture = fixture();
        let link = fixture.observation.receipt_link.as_ref().unwrap();
        let mut changed_task = fixture.task.clone();
        changed_task.intent = Some("different intent, same task id".into());
        assert!(
            join_receipt_document_inputs(&changed_task, &fixture.plan, &fixture.verified, link)
                .is_err()
        );
        let mut changed_plan = fixture.plan.clone();
        changed_plan.context_budget_tokens += 1;
        assert!(
            join_receipt_document_inputs(&fixture.task, &changed_plan, &fixture.verified, link)
                .is_err()
        );
        let mut unbound = fixture.invocation.clone();
        unbound.source_refs.retain(|reference| {
            !reference.as_str().starts_with("task:") && !reference.as_str().starts_with("plan:")
        });
        let mut observation = fixture.verified.observation().clone();
        observation.source_lineage = unbound.source_refs.clone();
        let (verified, link) = persist_verified(&unbound, &observation);
        assert!(
            join_receipt_document_inputs(&fixture.task, &fixture.plan, &verified, &link).is_err()
        );
    }

    #[test]
    fn canonical_join_rejects_missing_policy_binding_and_foreign_task_lineage() {
        let fixture = fixture();
        let link = fixture.observation.receipt_link.as_ref().unwrap();
        let mut plan = fixture.plan.clone();
        plan.policy_decision_ref = None;
        assert!(
            join_receipt_document_inputs(&fixture.task, &plan, &fixture.verified, link).is_err()
        );
        plan = fixture.plan.clone();
        plan.capability_bindings.clear();
        assert!(
            join_receipt_document_inputs(&fixture.task, &plan, &fixture.verified, link).is_err()
        );
        let mut invocation = fixture.invocation.clone();
        invocation.source_refs.push(reference("task:foreign"));
        let mut observation = fixture.verified.observation().clone();
        observation.source_lineage = invocation.source_refs.clone();
        let (verified, link) = persist_verified(&invocation, &observation);
        assert!(
            join_receipt_document_inputs(&fixture.task, &fixture.plan, &verified, &link).is_err()
        );
    }

    #[test]
    fn source_lineage_must_match_the_exact_invocation_source_set() {
        let fixture = fixture();
        let mut missing_observation = fixture.verified.observation().clone();
        missing_observation.source_lineage.pop();
        let (missing_verified, missing_link) =
            persist_verified(&fixture.invocation, &missing_observation);
        assert!(
            join_receipt_document_inputs(
                &fixture.task,
                &fixture.plan,
                &missing_verified,
                &missing_link,
            )
            .is_err()
        );

        let mut extra_invocation = fixture.invocation.clone();
        extra_invocation.source_refs.push(reference("source:extra"));
        let (extra_verified, extra_link) =
            persist_verified(&extra_invocation, fixture.verified.observation());
        assert!(
            join_receipt_document_inputs(
                &fixture.task,
                &fixture.plan,
                &extra_verified,
                &extra_link,
            )
            .is_err()
        );
    }
}
