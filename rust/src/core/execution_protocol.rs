// SPDX-License-Identifier: Apache-2.0

//! Strict validation for the typed execution protocol carried by one lifecycle run.

mod decision_authentication;

use std::fmt::Write as _;

use ed25519_dalek::VerifyingKey;
use lean_ctx_protocol::{
    AcceptedOutcomeV1, ContextPlanProjectionV1, DecisionRecordV1, DecisionStageV1,
    EngineInvocationV1, EngineObservationV1, ExecutionPlanV1, ReceiptDocumentV1, TaskEnvelopeV1,
    UtcTimestamp,
};
use sha2::{Digest, Sha256};

use crate::core::{
    engine_interface::read_verified_engine_receipt, execution_ledger::PublishedCanonicalReceipt,
    execution_ledger::ReceiptSignerAdmissionV1,
    receipt_document_adapter::join_receipt_document_inputs,
};

/// Authoritative artifacts produced by the owners of stages 5 through 13.
///
/// This is intentionally attach-only. A surface that only knows an HTTP status,
/// process exit code, or MCP result must not fabricate protocol lineage.
#[derive(Clone, Debug)]
pub struct RecordedExecutionProtocolV1 {
    pub context_plan: ContextPlanProjectionV1,
    pub execution_plan: ExecutionPlanV1,
    pub invocation: EngineInvocationV1,
    pub observation: EngineObservationV1,
    pub receipt: ReceiptDocumentV1,
    pub published_receipt: PublishedCanonicalReceipt,
    pub accepted_outcome: AcceptedOutcomeV1,
    pub decision: DecisionRecordV1,
}

/// Borrowed protocol with trusted receipt/decision signatures and validated lineage.
///
/// Private construction and immutable borrowing prevent callers from forging an
/// admission flag or replacing artifacts between validation and consumption.
/// An Unknown receipt authenticates operational lineage, not an outcome observation;
/// its optional partial score is not authority to train on quality.
pub struct ValidatedExecutionProtocolV1<'a> {
    protocol: &'a RecordedExecutionProtocolV1,
    project_id: lean_ctx_protocol::ProjectId,
    tenant_id: Option<lean_ctx_protocol::TenantId>,
}

impl ValidatedExecutionProtocolV1<'_> {
    /// Scope authenticated by the signed receipt's canonical task digest.
    pub fn project_id(&self) -> &lean_ctx_protocol::ProjectId {
        &self.project_id
    }

    pub fn tenant_id(&self) -> Option<&lean_ctx_protocol::TenantId> {
        self.tenant_id.as_ref()
    }

    pub fn context_plan(&self) -> &ContextPlanProjectionV1 {
        &self.protocol.context_plan
    }

    pub fn execution_plan(&self) -> &ExecutionPlanV1 {
        &self.protocol.execution_plan
    }

    pub fn outcome(&self) -> &AcceptedOutcomeV1 {
        &self.protocol.accepted_outcome
    }

    pub fn receipt_id(&self) -> &str {
        self.protocol.receipt.receipt_id.as_str()
    }

    /// Keep the full authenticated planning payload bound at the learning consumer.
    /// The shortened autopilot ID and projection alone omit executable policies.
    pub(crate) fn validate_learning_handoff(
        &self,
        context: &crate::core::context_kernel::autopilot::TaskAutopilotDecision,
    ) -> Result<(), ExecutionProtocolError> {
        if self.protocol.decision.decision_stage == DecisionStageV1::Planning {
            let bytes = context.canonical_bytes().map_err(protocol_error)?;
            let selected: serde_json::Value =
                serde_json::from_slice(&bytes).map_err(protocol_error)?;
            if selected != self.protocol.decision.selected_result {
                return Err(fail(
                    "learning handoff differs from authenticated planning payload",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionProtocolError(String);

impl std::fmt::Display for ExecutionProtocolError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ExecutionProtocolError {}

impl RecordedExecutionProtocolV1 {
    /// Authenticate both purposes and lifecycle lineage before issuing a token.
    ///
    /// The host must resolve the signer snapshot and public key from its trusted
    /// configuration, never from receipt-supplied key material. Admission must be
    /// valid both at issuance and at the host's verification time. This authenticates
    /// the receipt signer's claims, not independently signed evaluator artifacts.
    pub fn validated_for(
        &self,
        task: &TaskEnvelopeV1,
        receipt_authority: (&ReceiptSignerAdmissionV1, &VerifyingKey),
        decision_authority: (&lean_ctx_ocla::DecisionSignerAdmissionV1, &VerifyingKey),
        actual_context: Option<&crate::core::context_kernel::autopilot::TaskAutopilotDecision>,
        verified_at: &UtcTimestamp,
    ) -> Result<ValidatedExecutionProtocolV1<'_>, ExecutionProtocolError> {
        let (trusted_signer, verifying_key) = receipt_authority;
        lean_ctx_ocla::verify_receipt_signature(
            &self.receipt,
            trusted_signer,
            verifying_key,
            verified_at,
        )
        .map_err(|error| match error {
            error @ lean_ctx_ocla::ReceiptVerificationError::SignerNotAdmitted => protocol_error(
                crate::core::execution_ledger::ExecutionLedgerError::InvalidRecord(
                    error.to_string(),
                ),
            ),
            error => protocol_error(error),
        })?;
        lean_ctx_ocla::verify_decision_signature(
            &self.decision,
            task,
            decision_authority.0,
            decision_authority.1,
            verified_at,
        )
        .map_err(protocol_error)?;
        self.validate_for(task)?;
        decision_authentication::validate(self, task, actual_context)?;
        Ok(ValidatedExecutionProtocolV1 {
            protocol: self,
            project_id: task.project_id.clone(),
            tenant_id: task.tenant_id.clone(),
        })
    }

    /// Validate the complete Task -> Context -> Plan -> Invocation -> Receipt ->
    /// Outcome -> Decision chain without repairing or synthesizing any artifact.
    /// This structural check does not establish signer trust; use `validated_for`
    /// before admitting an outcome to learning.
    pub fn validate_for(&self, task: &TaskEnvelopeV1) -> Result<(), ExecutionProtocolError> {
        task.validate().map_err(protocol_error)?;
        self.context_plan.validate().map_err(protocol_error)?;
        self.execution_plan.validate().map_err(protocol_error)?;
        self.invocation.validate().map_err(protocol_error)?;
        self.observation
            .validate_for(&self.invocation)
            .map_err(protocol_error)?;
        self.receipt.validate().map_err(protocol_error)?;
        self.accepted_outcome.validate().map_err(protocol_error)?;
        self.decision.validate().map_err(protocol_error)?;

        let projection_digest = self
            .context_plan
            .projection_digest
            .as_ref()
            .ok_or_else(|| fail("context projection requires its canonical digest"))?;
        if self.context_plan.task_id != task.task_id
            || self.execution_plan.task_id != task.task_id
            || self.accepted_outcome.task_id != task.task_id
            || self.decision.task_id != task.task_id
        {
            return Err(fail("protocol task_id lineage mismatch"));
        }
        crate::core::engine_interface::planning::context_binding::validate_projection(
            &self.execution_plan,
            &self.context_plan,
        )
        .map_err(protocol_error)?;
        let capability = &self.invocation.operation;
        if !self
            .execution_plan
            .capability_ids
            .contains(&capability.capability_id)
            || !self
                .execution_plan
                .capability_bindings
                .iter()
                .any(|binding| {
                    binding.capability_id == capability.capability_id
                        && binding.version == capability.capability_version.as_str()
                })
        {
            return Err(fail(
                "execution plan does not bind the invoked capability version",
            ));
        }
        if self.execution_plan.policy_decision_ref.as_deref()
            != Some(self.invocation.policy_admission.policy_ref.as_str())
        {
            return Err(fail("execution plan policy does not bind Engine admission"));
        }

        let receipt_link = self
            .observation
            .receipt_link
            .as_ref()
            .ok_or_else(|| fail("Engine observation requires a verified receipt link"))?;
        let mut unsigned_observation = self.observation.clone();
        unsigned_observation.receipt_link = None;
        let verified = read_verified_engine_receipt(
            &receipt_link.receipt_digest,
            &self.invocation,
            &unsigned_observation,
        )
        .map_err(protocol_error)?;
        let expected =
            join_receipt_document_inputs(task, &self.execution_plan, &verified, receipt_link)
                .map_err(protocol_error)?;
        if self.receipt.lineage != expected.lineage {
            return Err(fail(
                "canonical receipt lineage disagrees with verified Engine inputs",
            ));
        }

        let (persisted, receipt_bytes) = self
            .published_receipt
            .read_canonical()
            .map_err(protocol_error)?;
        if persisted != self.receipt
            || self.published_receipt.receipt_id != self.receipt.receipt_id.as_str()
        {
            return Err(fail(
                "published canonical receipt disagrees with lifecycle receipt",
            ));
        }
        let persisted_digest = sha256_digest(&receipt_bytes);
        if self.published_receipt.receipt_digest != persisted_digest
            || self.published_receipt.receipt_ref != format!("id:{persisted_digest}")
        {
            return Err(fail(
                "published receipt reference does not bind canonical bytes",
            ));
        }

        // Canonical Unknown receipts forbid outcome references. Keep operational
        // bindings mandatory without inventing an observation to satisfy this join.
        let has_outcome_observation =
            self.accepted_outcome.accepted != lean_ctx_protocol::AcceptanceState::Unknown;
        if self.accepted_outcome.plan_id.as_ref() != Some(&self.execution_plan.plan_id)
            || self
                .accepted_outcome
                .receipt_id
                .as_ref()
                .map(lean_ctx_protocol::ReceiptId::as_str)
                != Some(self.receipt.receipt_id.as_str())
            || self.receipt.outcome.state != self.accepted_outcome.accepted
            || (has_outcome_observation
                && self.receipt.outcome.outcome_id.as_ref()
                    != Some(&self.accepted_outcome.outcome_id))
        {
            return Err(fail(
                "accepted outcome does not bind plan and canonical receipt",
            ));
        }
        if has_outcome_observation {
            let outcome_digest = self
                .receipt
                .outcome
                .outcome_ref
                .as_ref()
                .ok_or_else(|| fail("terminal outcome requires a receipt outcome reference"))?;
            if !self
                .accepted_outcome
                .evidence_refs
                .iter()
                .any(|evidence| evidence.digest == outcome_digest.as_str())
            {
                return Err(fail(
                    "accepted outcome evidence does not bind receipt outcome",
                ));
            }
        }

        if self.decision.decision_stage == DecisionStageV1::Admission
            || self.decision.plan_id.as_ref() != Some(&self.execution_plan.plan_id)
            || !self
                .accepted_outcome
                .decision_refs
                .iter()
                .any(|reference| reference == self.decision.decision_id.as_str())
        {
            return Err(fail("post-admission decision does not bind task outcome"));
        }
        let planning = self.decision.decision_stage == DecisionStageV1::Planning;
        for required in [
            projection_digest.as_str(),
            self.receipt.lineage.plan_ref.as_str(),
        ]
        .into_iter()
        .chain((!planning).then_some(self.receipt.lineage.invocation_ref.as_str()))
        .chain((!planning).then_some(self.receipt.receipt_id.as_str()))
        {
            if !self
                .decision
                .input_refs
                .iter()
                .any(|reference| reference == required)
            {
                return Err(fail(
                    "decision input_refs omit authoritative protocol lineage",
                ));
            }
        }
        if has_outcome_observation
            && !planning
            && !self.decision.evidence_refs.iter().any(|decision_evidence| {
                self.accepted_outcome
                    .evidence_refs
                    .iter()
                    .any(|outcome_evidence| outcome_evidence.digest == decision_evidence.digest)
            })
        {
            return Err(fail(
                "decision evidence does not bind accepted outcome evidence",
            ));
        }
        Ok(())
    }
}

fn sha256_digest(bytes: &[u8]) -> String {
    let mut value = String::with_capacity(71);
    value.push_str("sha256:");
    for byte in Sha256::digest(bytes) {
        write!(&mut value, "{byte:02x}").expect("writing to String cannot fail");
    }
    value
}

fn protocol_error(error: impl std::fmt::Display) -> ExecutionProtocolError {
    fail(&error.to_string())
}

fn fail(message: &str) -> ExecutionProtocolError {
    ExecutionProtocolError(message.to_owned())
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::collections::BTreeMap;

    use ed25519_dalek::{SigningKey, VerifyingKey};
    use lean_ctx_protocol::{
        AcceptanceState, CapabilityBindingV1, ContextBalanceV1, ContextPlanId,
        ContextPlanProjectionV1, ContextStrategy, DecisionId, DecisionKind, DecisionRecordV1,
        DecisionStageV1, EvidenceKind, EvidenceRefV1, ExecutionPlanV1, OutcomeId, OutcomeSignalsV1,
        PlanId, ReceiptChainLinkV1, ReceiptDocumentV1, ReceiptEvidenceKindV1, ReceiptEvidenceRefV1,
        ReceiptOutcomeLinkV1, ReceiptTerminalStatusV1, Sha256Digest, SignalState, SignatureStatus,
        StopCondition, TaskEnvelopeV1, UtcTimestamp,
    };

    use super::{RecordedExecutionProtocolV1, sha256_digest};
    use crate::core::{
        data_dir::{IsolatedDataDir, isolated_data_dir},
        engine_interface::NativeContextEngine,
        execution_ledger::{
            CanonicalReceiptRecordV1, ExecutionLedgerStore, ReceiptSignerAdmissionV1,
            record_canonical_engine_receipt,
        },
    };

    pub(crate) struct ProtocolFixture {
        pub protocol: RecordedExecutionProtocolV1,
        pub signer_admission: ReceiptSignerAdmissionV1,
        pub verifying_key: VerifyingKey,
        pub decision_admission: lean_ctx_ocla::DecisionSignerAdmissionV1,
        pub verified_at: UtcTimestamp,
        _data_dir: IsolatedDataDir,
        _engine_root: tempfile::TempDir,
    }

    impl ProtocolFixture {
        pub(crate) fn validated_for(
            &self,
            task: &TaskEnvelopeV1,
        ) -> Result<super::ValidatedExecutionProtocolV1<'_>, super::ExecutionProtocolError>
        {
            self.protocol.validated_for(
                task,
                (&self.signer_admission, &self.verifying_key),
                (&self.decision_admission, &self.verifying_key),
                None,
                &self.verified_at,
            )
        }
    }

    pub(crate) fn build(task: &TaskEnvelopeV1) -> ProtocolFixture {
        build_for_context(task, None, None, AcceptanceState::Accepted)
    }

    pub(crate) fn build_for_context(
        task: &TaskEnvelopeV1,
        projection: Option<&ContextPlanProjectionV1>,
        autopilot: Option<&crate::core::context_kernel::autopilot::TaskAutopilotDecision>,
        acceptance: AcceptanceState,
    ) -> ProtocolFixture {
        let data_dir = isolated_data_dir();
        let engine_root = tempfile::tempdir().expect("engine root");
        let source = engine_root.path().join("fixture.md");
        std::fs::write(&source, "stable native context").expect("source fixture");
        let policy = lean_ctx_protocol::EnginePolicyAdmissionV1 {
            policy_ref: lean_ctx_protocol::ProtocolReference::new("policy:fixture")
                .expect("policy ref"),
            decision: lean_ctx_protocol::EnginePolicyDecisionV1::Admitted,
        };
        let engine = NativeContextEngine::with_root(engine_root.path()).expect("engine root");
        let mut context_plan = ContextPlanProjectionV1 {
            schema_version: 1,
            context_plan_id: ContextPlanId::try_from("context-plan-1".to_owned()).unwrap(),
            task_id: task.task_id.clone(),
            projection_digest: None,
            budget_tokens: 1_000,
            selections: Vec::new(),
            provider_stats: BTreeMap::new(),
            policy_decision_refs: vec!["policy:fixture".to_owned()],
            evidence: Vec::new(),
            extensions: Default::default(),
        };
        context_plan.projection_digest = Some(
            context_plan
                .compute_projection_digest()
                .expect("context projection digest"),
        );
        if let Some(projection) = projection {
            context_plan = projection.clone();
        }
        let plan = ExecutionPlanV1 {
            schema_version: 1,
            plan_id: PlanId::try_from("plan-1".to_owned()).unwrap(),
            task_id: task.task_id.clone(),
            context_budget_tokens: context_plan.budget_tokens,
            context_budget_policy: None,
            context_strategy: ContextStrategy::Balanced,
            knowledge_refs: Vec::new(),
            capability_ids: vec![
                lean_ctx_protocol::CapabilityId::new(crate::core::engine_interface::CAPABILITY_ID)
                    .unwrap(),
            ],
            model: "local-native".to_owned(),
            provider: "local-native".to_owned(),
            reasoning_allocation_milli: 0,
            max_retries: 0,
            fallback_refs: Vec::new(),
            stop_condition: StopCondition::OnCompletion,
            expected_cost_micros: 0,
            estimates: None,
            expected_quality_milli: 900,
            expected_latency_ms: 100,
            policy_decision_ref: Some("policy:fixture".to_owned()),
            scheduler_decision_ref: None,
            executor_agent_id: Some(task.agent_id.clone()),
            context_plan_id: Some(context_plan.context_plan_id.clone()),
            capability_bindings: vec![CapabilityBindingV1 {
                capability_id: lean_ctx_protocol::CapabilityId::new(
                    crate::core::engine_interface::CAPABILITY_ID,
                )
                .unwrap(),
                version: crate::core::engine_interface::CAPABILITY_VERSION.to_owned(),
                manifest_digest: None,
            }],
            extensions: Default::default(),
        };

        let plan = autopilot.map_or_else(
            || plan.clone(),
            |handoff| {
                handoff
                    .bind_execution_plan(plan.clone())
                    .expect("bind autopilot plan")
            },
        );

        let canonical_source = std::fs::canonicalize(&source).unwrap();
        let (invocation, observation) = engine
            .execute_ctx_read_rooted_snapshot_with_plan(
                canonical_source.to_str().unwrap(),
                "stable native context",
                policy,
                task,
                &plan,
            )
            .expect("native Engine invocation binds pre-execution task and plan");

        let engine_receipt_digest = observation
            .receipt_link
            .as_ref()
            .expect("Engine receipt link")
            .receipt_digest
            .clone();
        let outcome_digest =
            Sha256Digest::new(sha256_digest(format!("{acceptance:?} outcome").as_bytes())).unwrap();
        let acceptance_digest =
            Sha256Digest::new(sha256_digest(b"acceptance measurement")).unwrap();
        let outcome_id = OutcomeId::try_from("outcome-1".to_owned()).unwrap();
        let receipt_evidence = vec![
            receipt_evidence(
                ReceiptEvidenceKindV1::Measurement,
                "artifact://engine/receipt",
                engine_receipt_digest,
            ),
            receipt_evidence(
                ReceiptEvidenceKindV1::Outcome,
                "artifact://outcome/observation",
                outcome_digest.clone(),
            ),
            receipt_evidence(
                ReceiptEvidenceKindV1::Measurement,
                "artifact://outcome/acceptance",
                acceptance_digest.clone(),
            ),
        ];
        let signing_key = SigningKey::from_bytes(&[17; 32]);
        let issued_at = UtcTimestamp::new("2026-08-23T12:00:00Z").unwrap();
        let signer_admission = ReceiptSignerAdmissionV1 {
            key_id: "test-key".to_owned(),
            public_key_digest: Sha256Digest::new(sha256_digest(
                signing_key.verifying_key().as_bytes(),
            ))
            .expect("public key digest"),
            admitted_at: UtcTimestamp::new("2026-01-01T00:00:00Z").expect("admission time"),
            expires_at: UtcTimestamp::new(if acceptance == AcceptanceState::Unknown {
                "9999-01-01T00:00:00Z"
            } else {
                "2027-01-01T00:00:00Z"
            })
            .expect("expiry time"),
            revoked_at: None,
        };
        let ledger = ExecutionLedgerStore::new(engine_root.path().join("ledger.jsonl"));
        let published = if acceptance == AcceptanceState::Unknown {
            // Exercise the real host publication path, which must not invent
            // an outcome or acceptance reference for an unobserved result.
            let settings = serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "signing_key_hex": "11".repeat(32),
                "signer": {
                    "key_id": signer_admission.key_id,
                    "public_key_digest": signer_admission.public_key_digest,
                    "admitted_at": signer_admission.admitted_at,
                    "expires_at": signer_admission.expires_at,
                    "revoked_at": signer_admission.revoked_at,
                },
                "ledger_path": engine_root.path().join("ledger.jsonl"),
            }))
            .expect("host settings");
            let authority = crate::core::execution_ledger::host::HostReceiptAuthority::from_reader(
                &mut settings.as_slice(),
            )
            .expect("trusted host authority");
            let attempt = authority.begin(task, &plan).expect("host task admission");
            authority
                .publish(&attempt, &invocation, &observation, "stable native context")
                .expect("publish actual Unknown receipt")
        } else {
            record_canonical_engine_receipt(
                task,
                &plan,
                &invocation,
                &observation,
                CanonicalReceiptRecordV1 {
                    context_balance: ContextBalanceV1 {
                        original_tokens: 100,
                        materialized_tokens: 80,
                        delivered_tokens: 60,
                        provider_billed_tokens: 60,
                    },
                    status: if acceptance == AcceptanceState::Rejected {
                        ReceiptTerminalStatusV1::Rejected
                    } else {
                        ReceiptTerminalStatusV1::Succeeded
                    },
                    values: Vec::new(),
                    outcome: ReceiptOutcomeLinkV1 {
                        state: acceptance,
                        outcome_id: (acceptance != AcceptanceState::Unknown)
                            .then(|| outcome_id.clone()),
                        outcome_ref: (acceptance != AcceptanceState::Unknown)
                            .then(|| outcome_digest.clone()),
                        acceptance_evidence_digest: (acceptance == AcceptanceState::Accepted)
                            .then(|| acceptance_digest.clone()),
                    },
                    evidence_refs: receipt_evidence,
                    chain: ReceiptChainLinkV1 {
                        chain_id: "chain-1".to_owned(),
                        sequence_number: 1,
                        previous_receipt_id: None,
                        previous_signature_digest: None,
                    },
                    issued_at: issued_at.clone(),
                    signer_admission: signer_admission.clone(),
                },
                &signing_key,
                &ledger,
            )
            .expect("record canonical receipt exactly once")
        };
        let receipt = ReceiptDocumentV1::from_canonical_bytes(
            &std::fs::read(&published.path).expect("published receipt"),
        )
        .expect("strict receipt document");
        let outcome_evidence = protocol_evidence(
            EvidenceKind::QualityMeasurement,
            "artifact://outcome/observation",
            outcome_digest.as_str(),
        );
        let acceptance_evidence = protocol_evidence(
            EvidenceKind::QualityMeasurement,
            "artifact://outcome/acceptance",
            acceptance_digest.as_str(),
        );
        let decision_id = DecisionId::try_from("decision-1".to_owned()).unwrap();
        let accepted_outcome = lean_ctx_protocol::AcceptedOutcomeV1 {
            schema_version: 1,
            outcome_id,
            task_id: task.task_id.clone(),
            accepted: acceptance,
            quality_score_milli: (acceptance != AcceptanceState::Unknown).then_some(
                if acceptance == AcceptanceState::Accepted {
                    1_000
                } else {
                    0
                },
            ),
            signals: OutcomeSignalsV1 {
                build: None,
                tests: Some(match acceptance {
                    AcceptanceState::Accepted => SignalState::Passed,
                    AcceptanceState::Rejected => SignalState::Failed,
                    AcceptanceState::Unknown => SignalState::Unknown,
                }),
                lint: None,
                typecheck: None,
                completion: Some(SignalState::Passed),
                pr: None,
                correction: None,
                rollback: None,
                retry: None,
            },
            contract_ref: Some("contract:test".to_owned()),
            evidence_refs: if acceptance == AcceptanceState::Unknown {
                Vec::new()
            } else {
                vec![outcome_evidence.clone(), acceptance_evidence]
            },
            observed_at: receipt.issued_at.as_str().to_owned(),
            plan_id: Some(plan.plan_id.clone()),
            receipt_id: Some(
                lean_ctx_protocol::ReceiptId::try_from(receipt.receipt_id.as_str().to_owned())
                    .unwrap(),
            ),
            decision_refs: vec![decision_id.as_str().to_owned()],
            extensions: Default::default(),
        };
        let mut decision = DecisionRecordV1 {
            schema_version: 1,
            decision_id,
            task_id: task.task_id.clone(),
            plan_id: Some(plan.plan_id.clone()),
            decision_stage: DecisionStageV1::Outcome,
            decision_kind: DecisionKind::Stop,
            input_refs: vec![
                sha256_digest(&task.canonical_bytes().unwrap()),
                context_plan
                    .projection_digest
                    .as_ref()
                    .unwrap()
                    .as_str()
                    .to_owned(),
                receipt.lineage.plan_ref.as_str().to_owned(),
                receipt.lineage.invocation_ref.as_str().to_owned(),
                receipt.receipt_id.as_str().to_owned(),
            ],
            constraint_refs: vec!["contract:test".to_owned()],
            selected_result: if acceptance == AcceptanceState::Unknown {
                serde_json::json!({"acceptance": "unknown"})
            } else {
                serde_json::json!({"accepted": acceptance == AcceptanceState::Accepted})
            },
            rationale_code: if acceptance == AcceptanceState::Unknown {
                "outcome_unobserved"
            } else {
                "evidence_satisfied"
            }
            .to_owned(),
            rationale_ref: None,
            policy_ref: Some("policy:fixture".to_owned()),
            decision_system_name: "outcome-evaluator".to_owned(),
            decision_system_version: "1.0.0".to_owned(),
            evidence_refs: if acceptance == AcceptanceState::Unknown {
                Vec::new()
            } else {
                vec![outcome_evidence]
            },
            observed_at: receipt.issued_at.as_str().to_owned(),
            signature: String::new(),
            supersedes: None,
            extensions: Default::default(),
        };

        // Explicit test-host authority, never inferred from a supplied record.
        let decision_admission = lean_ctx_ocla::DecisionSignerAdmissionV1 {
            key_admission: signer_admission.clone(),
            task: task.clone(),
            stage: DecisionStageV1::Outcome,
            kind: DecisionKind::Stop,
        };
        decision.signature = lean_ctx_ocla::sign_decision_record(
            &decision,
            task,
            &decision_admission,
            &signing_key,
            &receipt.issued_at,
        )
        .unwrap();

        ProtocolFixture {
            signer_admission,
            decision_admission,
            verifying_key: signing_key.verifying_key(),
            verified_at: receipt.issued_at.clone(),
            protocol: RecordedExecutionProtocolV1 {
                context_plan,
                execution_plan: plan,
                invocation,
                observation,
                receipt,
                published_receipt: published,
                accepted_outcome,
                decision,
            },
            _data_dir: data_dir,
            _engine_root: engine_root,
        }
    }

    fn receipt_evidence(
        kind: ReceiptEvidenceKindV1,
        uri: &str,
        digest: Sha256Digest,
    ) -> ReceiptEvidenceRefV1 {
        ReceiptEvidenceRefV1 {
            kind,
            uri: lean_ctx_protocol::ProtocolReference::new(uri).unwrap(),
            digest,
            media_type: "application/json".to_owned(),
            signature_status: SignatureStatus::NotSigned,
        }
    }

    fn protocol_evidence(kind: EvidenceKind, uri: &str, digest: &str) -> EvidenceRefV1 {
        EvidenceRefV1 {
            schema_version: Some(1),
            kind,
            uri: uri.to_owned(),
            digest: digest.to_owned(),
            signature_status: SignatureStatus::NotSigned,
            media_type: Some("application/json".to_owned()),
            extensions: Default::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use ed25519_dalek::SigningKey;
    use lean_ctx_protocol::{AcceptanceState, Sha256Digest, TaskEnvelopeV1, UtcTimestamp};

    use super::test_support;

    fn task() -> TaskEnvelopeV1 {
        serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "task_id": "task-1",
            "trace_id": "trace-1",
            "project_id": "project-1",
            "session_id": "session-1",
            "agent_id": "agent-1",
            "complexity": "medium",
            "created_at": "2026-08-23T11:59:00Z"
        }))
        .unwrap()
    }

    #[test]
    fn producer_recorded_chain_validates_end_to_end() {
        let task = task();
        let fixture = test_support::build(&task);
        fixture.protocol.validate_for(&task).unwrap();
        fixture.validated_for(&task).expect("trusted signed chain");
    }

    #[test]
    fn host_unknown_receipt_validates_without_quality_or_outcome_claims() {
        let task = task();
        let fixture = test_support::build_for_context(&task, None, None, AcceptanceState::Unknown);
        let receipt_outcome = &fixture.protocol.receipt.outcome;
        assert_eq!(receipt_outcome.state, AcceptanceState::Unknown);
        assert!(receipt_outcome.outcome_id.is_none());
        assert!(receipt_outcome.outcome_ref.is_none());
        assert!(receipt_outcome.acceptance_evidence_digest.is_none());
        assert!(fixture.protocol.accepted_outcome.evidence_refs.is_empty());
        assert!(
            fixture
                .protocol
                .accepted_outcome
                .quality_score_milli
                .is_none()
        );
        assert!(fixture.protocol.decision.evidence_refs.is_empty());
        let admitted = fixture
            .validated_for(&task)
            .expect("authenticated operational Unknown");
        assert_eq!(admitted.project_id(), &task.project_id);
        assert_eq!(admitted.outcome().accepted, AcceptanceState::Unknown);
    }

    #[test]
    fn unknown_receipt_still_requires_trusted_scope_and_complete_operational_lineage() {
        let task = task();
        let mut fixture =
            test_support::build_for_context(&task, None, None, AcceptanceState::Unknown);
        fixture.validated_for(&task).expect("valid Unknown control");
        let original = fixture.protocol.clone();
        for change in 0..8 {
            fixture.protocol = original.clone();
            match change {
                0 => fixture.protocol.receipt.signature = STANDARD.encode([0_u8; 64]),
                1 => fixture.protocol.context_plan.budget_tokens += 1,
                2 => fixture.protocol.decision.input_refs.clear(),
                3 => fixture.protocol.accepted_outcome.plan_id = None,
                4 => fixture.protocol.accepted_outcome.receipt_id = None,
                5 => fixture.protocol.accepted_outcome.decision_refs.clear(),
                6 | 7 => {
                    // An attacker relabels real transport evidence as quality.
                    // This must fail the signed state binding, not DTO shape validation.
                    let measurement = fixture
                        .protocol
                        .receipt
                        .evidence_refs
                        .first()
                        .expect("actual host measurement");
                    let outcome = &mut fixture.protocol.accepted_outcome;
                    outcome.accepted = if change == 6 {
                        AcceptanceState::Accepted
                    } else {
                        AcceptanceState::Rejected
                    };
                    outcome.quality_score_milli = Some(if change == 6 { 1_000 } else { 0 });
                    outcome
                        .evidence_refs
                        .push(lean_ctx_protocol::EvidenceRefV1 {
                            schema_version: Some(1),
                            kind: lean_ctx_protocol::EvidenceKind::QualityMeasurement,
                            uri: measurement.uri.as_str().to_owned(),
                            digest: measurement.digest.as_str().to_owned(),
                            signature_status: lean_ctx_protocol::SignatureStatus::NotSigned,
                            media_type: Some(measurement.media_type.clone()),
                            extensions: Default::default(),
                        });
                    outcome
                        .validate()
                        .expect("shape-valid forged known outcome");
                }
                _ => unreachable!("bounded test cases"),
            }
            assert!(fixture.validated_for(&task).is_err(), "mutation {change}");
        }
        fixture.protocol = original;
        let mut other_task = task.clone();
        other_task.project_id = lean_ctx_protocol::ProjectId::new("other-project").expect("scope");
        assert!(fixture.validated_for(&other_task).is_err());
        let mut other_tenant = task.clone();
        other_tenant.tenant_id =
            Some(lean_ctx_protocol::TenantId::new("other-tenant").expect("tenant scope"));
        assert!(fixture.validated_for(&other_tenant).is_err());
        fixture
            .validated_for(&task)
            .expect("unchanged Unknown control");
    }

    #[test]
    fn structurally_valid_forged_signature_cannot_issue_learning_token() {
        let task = task();
        let mut fixture = test_support::build(&task);
        fixture
            .validated_for(&task)
            .expect("valid signature control");
        fixture.protocol.receipt.signature = STANDARD.encode([0_u8; 64]);
        let bytes = fixture
            .protocol
            .receipt
            .canonical_bytes()
            .expect("canonical bytes");
        let digest = Sha256Digest::new(super::sha256_digest(&bytes)).expect("receipt digest");
        drop(
            crate::core::engine_interface::persist_engine_artifact_content(
                "execution/receipts",
                digest.hex(),
                "json",
                &bytes,
            )
            .expect("persist untrusted receipt at its actual digest"),
        );
        fixture.protocol.published_receipt.path = fixture
            .protocol
            .published_receipt
            .path
            .with_file_name(format!("{}.json", digest.hex()));
        fixture.protocol.published_receipt.receipt_digest = digest.as_str().to_owned();
        fixture.protocol.published_receipt.receipt_ref = format!("id:{}", digest.as_str());
        fixture
            .protocol
            .validate_for(&task)
            .expect("structural checks pass");
        assert!(fixture.validated_for(&task).is_err());
    }

    #[test]
    fn signer_identity_key_digest_and_signature_key_must_all_match() {
        let task = task();
        let mut fixture = test_support::build(&task);
        let trusted = fixture.signer_admission.clone();
        fixture.signer_admission.key_id = "different-key".to_owned();
        assert!(fixture.validated_for(&task).is_err());

        fixture.signer_admission = trusted;
        fixture.verifying_key = SigningKey::from_bytes(&[99; 32]).verifying_key();
        assert!(fixture.validated_for(&task).is_err());

        // Even an admitted alternative key cannot verify another key's receipt.
        fixture.signer_admission.public_key_digest =
            Sha256Digest::new(super::sha256_digest(fixture.verifying_key.as_bytes()))
                .expect("key digest");
        assert!(fixture.validated_for(&task).is_err());
    }

    #[test]
    fn signer_admission_must_cover_issuance_and_current_verification() {
        let task = task();
        let mut fixture = test_support::build(&task);
        let time = |value| UtcTimestamp::new(value).expect("test timestamp");
        let trusted = fixture.signer_admission.clone();
        let now = time("2026-08-23T13:00:00Z");
        fixture.verified_at = now.clone();
        fixture.validated_for(&task).expect("valid trust window");

        fixture.verified_at = time("2026-08-23T11:59:59Z");
        assert!(fixture.validated_for(&task).is_err());
        fixture.verified_at = now.clone();

        fixture.signer_admission.admitted_at = time("2026-08-23T12:00:01Z");
        assert!(fixture.validated_for(&task).is_err());
        fixture.signer_admission = trusted.clone();

        for expires_at in [fixture.protocol.receipt.issued_at.clone(), now.clone()] {
            fixture.signer_admission.expires_at = expires_at;
            assert!(fixture.validated_for(&task).is_err());
        }
        fixture.signer_admission = trusted.clone();

        for revoked_at in [fixture.protocol.receipt.issued_at.clone(), now.clone()] {
            fixture.signer_admission.revoked_at = Some(revoked_at);
            assert!(fixture.validated_for(&task).is_err());
        }
        fixture.signer_admission = trusted;
        fixture.signer_admission.admitted_at = fixture.protocol.receipt.issued_at.clone();
        fixture.signer_admission.revoked_at = Some(time("2026-08-23T13:00:01Z"));
        fixture
            .validated_for(&task)
            .expect("inclusive admission, future revocation");
    }

    #[test]
    fn tampered_context_projection_is_rejected() {
        let task = task();
        let mut fixture = test_support::build(&task);
        fixture.protocol.context_plan.budget_tokens += 1;
        assert!(fixture.protocol.validate_for(&task).is_err());
    }

    #[test]
    fn shared_signer_verification_preserves_engine_diagnostic() {
        let task = task();
        let mut fixture = test_support::build(&task);
        fixture.signer_admission.expires_at = fixture.protocol.receipt.issued_at.clone();
        let error = fixture
            .validated_for(&task)
            .err()
            .expect("expired signer must fail");
        assert_eq!(
            error.to_string(),
            "execution ledger record is invalid: receipt signing key is not admitted by the server trust snapshot"
        );
    }

    #[test]
    fn missing_decision_lineage_is_rejected() {
        let task = task();
        let mut fixture = test_support::build(&task);
        fixture.protocol.decision.input_refs.clear();
        assert!(fixture.protocol.validate_for(&task).is_err());
    }
}
