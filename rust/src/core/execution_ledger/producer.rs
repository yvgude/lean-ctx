//! Production composition of the protocol Engine spine into a canonical receipt.

use std::fmt::Write as _;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signer as _, SigningKey, VerifyingKey};
use lean_ctx_protocol::{
    AcceptanceState, ContextBalanceV1, EngineInvocationV1, EngineObservationV1, ExecutionPlanV1,
    ReceiptChainLinkV1, ReceiptDocumentV1, ReceiptEvidenceKindV1, ReceiptEvidenceRefV1,
    ReceiptKeyAdmissionV1, ReceiptOutcomeLinkV1, ReceiptSignerV1, ReceiptTerminalStatusV1,
    ReceiptValueV1, Sha256Digest, SignatureStatus, TaskEnvelopeV1, UtcTimestamp,
};
use serde::Serialize;
use sha2::{Digest, Sha256};

pub use lean_ctx_ocla::ReceiptSignerAdmissionV1;

use super::{
    ExecutionEvent, ExecutionLedgerError, ExecutionLedgerStore, PublishedCanonicalReceipt, Result,
    publish_canonical_receipt,
};
use crate::core::engine_interface::read_verified_engine_receipt;
use crate::core::receipt_document_adapter::join_receipt_document_inputs;

/// Authoritative terminal observations supplied by the host that owns the task outcome.
#[derive(Debug, Clone)]
pub struct CanonicalReceiptRecordV1 {
    pub context_balance: ContextBalanceV1,
    pub status: ReceiptTerminalStatusV1,
    pub values: Vec<ReceiptValueV1>,
    pub outcome: ReceiptOutcomeLinkV1,
    pub evidence_refs: Vec<ReceiptEvidenceRefV1>,
    pub chain: ReceiptChainLinkV1,
    pub issued_at: UtcTimestamp,
    pub signer_admission: ReceiptSignerAdmissionV1,
}

/// Join, sign, durably publish, and ledger-project one real Engine observation.
pub fn record_canonical_engine_receipt(
    task: &TaskEnvelopeV1,
    plan: &ExecutionPlanV1,
    invocation: &EngineInvocationV1,
    observation: &EngineObservationV1,
    mut record: CanonicalReceiptRecordV1,
    signing_key: &SigningKey,
    ledger: &ExecutionLedgerStore,
) -> Result<PublishedCanonicalReceipt> {
    task.validate().map_err(invalid_protocol)?;
    plan.validate().map_err(invalid_protocol)?;
    record
        .context_balance
        .validate()
        .map_err(invalid_protocol)?;
    validate_terminal_join(record.status, record.outcome.state)?;
    validate_signer_admission(
        &record.signer_admission,
        &signing_key.verifying_key(),
        &record.issued_at,
    )?;
    invocation.validate().map_err(invalid_protocol)?;
    observation
        .validate_for(invocation)
        .map_err(invalid_protocol)?;
    let receipt_link = observation.receipt_link.as_ref().ok_or_else(|| {
        ExecutionLedgerError::InvalidRecord(
            "canonical receipt requires an Engine receipt link".to_owned(),
        )
    })?;
    if receipt_link.receipt_ref.as_str()
        != format!("receipt:{}", receipt_link.receipt_digest.as_str())
    {
        return Err(ExecutionLedgerError::InvalidRecord(
            "Engine receipt ref does not bind its advertised digest".to_owned(),
        ));
    }
    let mut expected_observation = observation.clone();
    expected_observation.receipt_link = None;
    let verified = read_verified_engine_receipt(
        &receipt_link.receipt_digest,
        invocation,
        &expected_observation,
    )
    .map_err(ExecutionLedgerError::InvalidRecord)?;
    let inputs = join_receipt_document_inputs(task, plan, &verified, receipt_link)
        .map_err(|error| ExecutionLedgerError::InvalidRecord(error.to_string()))?;
    let observation_bytes = crate::core::canonical::canonical_serialize(observation);
    let observation_digest = Sha256Digest::new(digest(&observation_bytes))
        .map_err(|error| ExecutionLedgerError::InvalidRecord(error.to_string()))?;
    if !record
        .evidence_refs
        .iter()
        .any(|evidence| evidence.digest == observation_digest)
    {
        record.evidence_refs.push(ReceiptEvidenceRefV1 {
            kind: ReceiptEvidenceKindV1::Measurement,
            uri: lean_ctx_protocol::ProtocolReference::new("artifact://engine/observation")
                .map_err(invalid_protocol)?,
            digest: observation_digest.clone(),
            media_type: "application/json".to_owned(),
            signature_status: SignatureStatus::NotSigned,
        });
    }

    if !record
        .evidence_refs
        .iter()
        .any(|evidence| evidence.digest == inputs.receipt_link.receipt_digest)
    {
        return Err(ExecutionLedgerError::InvalidRecord(
            "canonical receipt evidence must bind the Engine receipt ref and digest".to_owned(),
        ));
    }

    persist_lineage_sidecar(&task, &inputs.lineage.task_ref)?;
    persist_lineage_sidecar(&plan, &inputs.lineage.plan_ref)?;
    persist_lineage_sidecar(&invocation, &inputs.lineage.invocation_ref)?;
    persist_lineage_sidecar(observation, &observation_digest)?;
    persist_lineage_sidecar(&task.agent_id, &inputs.lineage.identity_ref)?;
    persist_lineage_sidecar(
        &invocation.policy_admission,
        inputs
            .lineage
            .policy_refs
            .first()
            .expect("adapter emits one policy reference"),
    )?;

    let mut receipt = ReceiptDocumentV1 {
        schema_version: 1,
        receipt_id: zero_digest(),
        lineage: inputs.lineage,
        chain: record.chain,
        status: record.status,
        values: record.values,
        outcome: record.outcome,
        evidence_refs: record.evidence_refs,
        issued_at: record.issued_at,
        signer: ReceiptSignerV1 {
            algorithm: "ed25519".to_owned(),
            key_id: record.signer_admission.key_id,
            key_admission: ReceiptKeyAdmissionV1::ExternalTrustStore,
        },
        signature: STANDARD.encode([0_u8; 64]),
    };
    receipt.receipt_id = receipt.derived_receipt_id().map_err(invalid_protocol)?;
    receipt.signature = STANDARD.encode(
        signing_key
            .sign(&receipt.signing_bytes().map_err(invalid_protocol)?)
            .to_bytes(),
    );
    receipt.validate().map_err(invalid_protocol)?;

    let task_id = task.task_id.as_str().to_owned();
    let trace_id = task.trace_id.as_str().to_owned();
    let timestamp = receipt.issued_at.as_str().to_owned();
    ledger.append(ExecutionEvent::TaskStarted {
        task_id: task_id.clone(),
        trace_id: trace_id.clone(),
        envelope_ref: receipt.lineage.task_ref.as_str().to_owned(),
        timestamp: task.created_at.as_str().to_owned(),
        sequence_number: 0,
        prev_hash: String::new(),
    })?;
    ledger.append(ExecutionEvent::PlanCreated {
        task_id: task_id.clone(),
        trace_id: trace_id.clone(),
        plan_id: plan.plan_id.as_str().to_owned(),
        plan_ref: receipt.lineage.plan_ref.as_str().to_owned(),
        timestamp: timestamp.clone(),
        sequence_number: 0,
        prev_hash: String::new(),
    })?;
    ledger.append(ExecutionEvent::ContextDelivered {
        task_id: task_id.clone(),
        trace_id: trace_id.clone(),
        context_balance: record.context_balance,
        timestamp: timestamp.clone(),
        sequence_number: 0,
        prev_hash: String::new(),
    })?;
    ledger.append(ExecutionEvent::EngineInvoked {
        task_id: task_id.clone(),
        trace_id: trace_id.clone(),
        plan_id: plan.plan_id.as_str().to_owned(),
        invocation_id: invocation.invocation_id.as_str().to_owned(),
        invocation_ref: receipt.lineage.invocation_ref.as_str().to_owned(),
        capability_id: invocation.operation.capability_id.as_str().to_owned(),
        capability_version: invocation.operation.capability_version.as_str().to_owned(),
        timestamp: timestamp.clone(),
        sequence_number: 0,
        prev_hash: String::new(),
    })?;

    let published = publish_canonical_receipt(&receipt, ledger, &trace_id, &receipt.issued_at)?;
    if receipt.outcome.state != AcceptanceState::Unknown {
        ledger.append(ExecutionEvent::OutcomeRecorded {
            task_id,
            trace_id,
            outcome_id: receipt
                .outcome
                .outcome_id
                .as_ref()
                .expect("validated terminal outcome has an ID")
                .as_str()
                .to_owned(),
            receipt_id: receipt.receipt_id.as_str().to_owned(),
            accepted: receipt.outcome.state,
            timestamp,
            sequence_number: 0,
            prev_hash: String::new(),
        })?;
    }
    Ok(published)
}

fn persist_lineage_sidecar<T: Serialize>(value: &T, expected: &Sha256Digest) -> Result<()> {
    let bytes = crate::core::canonical::canonical_serialize(value);
    let actual = digest(&bytes);
    if actual != expected.as_str() {
        return Err(ExecutionLedgerError::InvalidRecord(
            "canonical lineage sidecar digest disagrees with adapter join".to_owned(),
        ));
    }
    let digest_hex = actual
        .strip_prefix("sha256:")
        .expect("locally generated digest has prefix");
    drop(
        crate::core::engine_interface::persist_engine_artifact_content(
            "execution/evidence",
            digest_hex,
            "json",
            &bytes,
        )
        .map_err(ExecutionLedgerError::InvalidRecord)?,
    );
    Ok(())
}

pub(crate) fn validate_signer_admission(
    admission: &ReceiptSignerAdmissionV1,
    verifying_key: &VerifyingKey,
    issued_at: &UtcTimestamp,
) -> Result<()> {
    lean_ctx_ocla::validate_signer_admission(admission, verifying_key, issued_at)
        .map_err(invalid_protocol)
}

fn validate_terminal_join(status: ReceiptTerminalStatusV1, outcome: AcceptanceState) -> Result<()> {
    match (status, outcome) {
        (ReceiptTerminalStatusV1::Rejected, AcceptanceState::Rejected)
        | (ReceiptTerminalStatusV1::Succeeded, AcceptanceState::Accepted) => Ok(()),
        (ReceiptTerminalStatusV1::Rejected, _) => Err(ExecutionLedgerError::InvalidRecord(
            "rejected terminal status requires a rejected outcome".to_owned(),
        )),
        (_, AcceptanceState::Rejected) => Err(ExecutionLedgerError::InvalidRecord(
            "rejected outcome requires a rejected terminal status".to_owned(),
        )),
        (_, AcceptanceState::Accepted) => Err(ExecutionLedgerError::InvalidRecord(
            "accepted outcome requires a succeeded terminal status".to_owned(),
        )),
        (_, AcceptanceState::Unknown) => Ok(()),
    }
}

fn digest(bytes: &[u8]) -> String {
    let mut value = String::with_capacity(71);
    value.push_str("sha256:");
    for byte in Sha256::digest(bytes) {
        write!(&mut value, "{byte:02x}").expect("writing to String cannot fail");
    }
    value
}

fn zero_digest() -> Sha256Digest {
    Sha256Digest::new(format!("sha256:{}", "0".repeat(64))).expect("zero digest is canonical")
}

fn invalid_protocol(error: impl std::fmt::Display) -> ExecutionLedgerError {
    ExecutionLedgerError::InvalidRecord(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::customer_proof_v2::{
        CustomerProofArtifact, CustomerProofArtifactKind, CustomerProofDraftV2,
        CustomerProofRedactionClass, CustomerProofSigner, CustomerProofTrustBasis,
        assemble_customer_proof_v2,
    };
    use lean_ctx_protocol::{
        EnginePolicyAdmissionV1, EnginePolicyDecisionV1, ReceiptEvidenceKindV1,
        ReceiptValueClassificationV1, SignatureStatus,
    };

    fn canonical<T: serde::Serialize>(value: &T) -> Vec<u8> {
        crate::core::canonical::canonical_serialize(value)
    }

    #[test]
    fn native_engine_to_signed_receipt_and_ledger_is_one_verified_path() {
        let _data_dir = crate::core::data_dir::isolated_data_dir();
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("fixture.md");
        std::fs::write(&source, "stable native context").unwrap();
        let policy = EnginePolicyAdmissionV1 {
            policy_ref: lean_ctx_protocol::ProtocolReference::new("policy:fixture").unwrap(),
            decision: EnginePolicyDecisionV1::Admitted,
        };
        let engine = crate::core::engine_interface::NativeContextEngine::with_root(root.path())
            .expect("test root must canonicalize securely");
        let task: TaskEnvelopeV1 = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "task_id": "task-1",
            "trace_id": "trace-1",
            "project_id": "project-1",
            "session_id": "session-1",
            "agent_id": "agent-1",
            "complexity": "medium",
            "created_at": "2026-08-23T11:59:00Z"
        }))
        .unwrap();
        let plan: ExecutionPlanV1 = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "plan_id": "plan-1",
            "task_id": "task-1",
            "context_budget_tokens": 1000,
            "context_strategy": "balanced",
            "knowledge_refs": [],
            "capability_ids": [crate::core::engine_interface::CAPABILITY_ID],
            "model": "local-native",
            "provider": "local-native",
            "capability_bindings": [{"capability_id":crate::core::engine_interface::CAPABILITY_ID,"version":"1.0.0"}],
            "reasoning_allocation_milli": 0,
            "max_retries": 0,
            "fallback_refs": [],
            "stop_condition": "on_completion",
            "expected_cost_micros": 0,
            "expected_quality_milli": 900,
            "expected_latency_ms": 100,
            "policy_decision_ref": "policy:fixture"
        }))
        .unwrap();
        let canonical_source = std::fs::canonicalize(&source).unwrap();
        let (invocation, observation) = engine
            .execute_ctx_read_rooted_snapshot_with_plan(
                canonical_source.to_str().unwrap(),
                "stable native context",
                policy,
                &task,
                &plan,
            )
            .unwrap();
        let engine_receipt = observation.receipt_link.as_ref().unwrap();
        let evidence = ReceiptEvidenceRefV1 {
            kind: ReceiptEvidenceKindV1::Measurement,
            uri: lean_ctx_protocol::ProtocolReference::new("artifact://engine/receipt").unwrap(),
            digest: engine_receipt.receipt_digest.clone(),
            media_type: "application/json".to_owned(),
            signature_status: SignatureStatus::NotSigned,
        };
        let record = CanonicalReceiptRecordV1 {
            context_balance: ContextBalanceV1 {
                original_tokens: 100,
                materialized_tokens: 80,
                delivered_tokens: 60,
                provider_billed_tokens: 60,
            },
            status: ReceiptTerminalStatusV1::Succeeded,
            values: vec![ReceiptValueV1 {
                name: "input_tokens".to_owned(),
                unit: "token".to_owned(),
                classification: ReceiptValueClassificationV1::Measured,
                value: Some(60),
                evidence_digests: vec![engine_receipt.receipt_digest.clone()],
                formula_digest: None,
                price_table_digest: None,
                reconciliation_digest: None,
            }],
            outcome: ReceiptOutcomeLinkV1 {
                state: AcceptanceState::Unknown,
                outcome_id: None,
                outcome_ref: None,
                acceptance_evidence_digest: None,
            },
            evidence_refs: vec![evidence],
            chain: ReceiptChainLinkV1 {
                chain_id: "chain-1".to_owned(),
                sequence_number: 1,
                previous_receipt_id: None,
                previous_signature_digest: None,
            },
            issued_at: UtcTimestamp::new("2026-08-23T12:00:00Z").unwrap(),
            signer_admission: ReceiptSignerAdmissionV1 {
                key_id: "test-key".to_owned(),
                public_key_digest: Sha256Digest::new(digest(
                    SigningKey::from_bytes(&[17; 32]).verifying_key().as_bytes(),
                ))
                .unwrap(),
                admitted_at: UtcTimestamp::new("2026-01-01T00:00:00Z").unwrap(),
                expires_at: UtcTimestamp::new("2027-01-01T00:00:00Z").unwrap(),
                revoked_at: None,
            },
        };
        let ledger = ExecutionLedgerStore::new(root.path().join("ledger.jsonl"));
        let signing_key = SigningKey::from_bytes(&[17; 32]);
        let mut invalid_terminal = record.clone();
        invalid_terminal.status = ReceiptTerminalStatusV1::Rejected;
        assert!(
            record_canonical_engine_receipt(
                &task,
                &plan,
                &invocation,
                &observation,
                invalid_terminal,
                &signing_key,
                &ledger,
            )
            .is_err()
        );
        assert!(ledger.load().unwrap().is_empty());
        let mut revoked = record.clone();
        revoked.signer_admission.revoked_at =
            Some(UtcTimestamp::new("2026-08-23T11:00:00Z").unwrap());
        assert!(
            record_canonical_engine_receipt(
                &task,
                &plan,
                &invocation,
                &observation,
                revoked,
                &signing_key,
                &ledger,
            )
            .is_err()
        );
        assert!(ledger.load().unwrap().is_empty());

        let mut wrong_ref = observation.clone();
        wrong_ref.receipt_link.as_mut().unwrap().receipt_ref =
            lean_ctx_protocol::ProtocolReference::new("receipt:wrong").unwrap();
        assert!(
            record_canonical_engine_receipt(
                &task,
                &plan,
                &invocation,
                &wrong_ref,
                record.clone(),
                &signing_key,
                &ledger,
            )
            .is_err()
        );

        let missing_digest = Sha256Digest::new(format!("sha256:{}", "f".repeat(64))).unwrap();
        let mut missing_artifact = observation.clone();
        missing_artifact.receipt_link.as_mut().unwrap().receipt_ref =
            lean_ctx_protocol::ProtocolReference::new(format!(
                "receipt:{}",
                missing_digest.as_str()
            ))
            .unwrap();
        missing_artifact
            .receipt_link
            .as_mut()
            .unwrap()
            .receipt_digest = missing_digest;
        assert!(
            record_canonical_engine_receipt(
                &task,
                &plan,
                &invocation,
                &missing_artifact,
                record.clone(),
                &signing_key,
                &ledger,
            )
            .is_err()
        );

        let tampered_digest = Sha256Digest::new(format!("sha256:{}", "d".repeat(64))).unwrap();
        let tampered_path = _data_dir
            .path()
            .join("engine-interface/v1/receipts")
            .join(format!("{}.json", tampered_digest.hex()));
        std::fs::create_dir_all(tampered_path.parent().unwrap()).unwrap();
        std::fs::write(&tampered_path, b"tampered Engine receipt bytes").unwrap();
        let mut tampered = observation.clone();
        tampered.receipt_link.as_mut().unwrap().receipt_ref =
            lean_ctx_protocol::ProtocolReference::new(format!(
                "receipt:{}",
                tampered_digest.as_str()
            ))
            .unwrap();
        tampered.receipt_link.as_mut().unwrap().receipt_digest = tampered_digest;
        assert!(
            record_canonical_engine_receipt(
                &task,
                &plan,
                &invocation,
                &tampered,
                record.clone(),
                &signing_key,
                &ledger,
            )
            .is_err()
        );

        let mut mixed_invocation = invocation.clone();
        mixed_invocation.invocation_id =
            lean_ctx_protocol::EngineInvocationIdV1::new("mixed-invocation").unwrap();
        let mut mixed_observation = observation.clone();
        mixed_observation.invocation_id = mixed_invocation.invocation_id.clone();
        mixed_observation.receipt_link = None;
        let mixed_bytes = crate::core::engine_interface::canonical_engine_receipt_artifact_bytes(
            &mixed_invocation,
            &mixed_observation,
        );
        let mixed_digest = Sha256Digest::new(digest(&mixed_bytes)).unwrap();
        crate::core::engine_interface::persist_engine_artifact_content(
            "engine-interface/v1/receipts",
            mixed_digest.hex(),
            "json",
            &mixed_bytes,
        )
        .unwrap();
        let mut mixed_link = observation.receipt_link.clone().unwrap();
        mixed_link.receipt_ref =
            lean_ctx_protocol::ProtocolReference::new(format!("receipt:{}", mixed_digest.as_str()))
                .unwrap();
        mixed_link.receipt_digest = mixed_digest;
        let mut mixed = observation.clone();
        mixed.receipt_link = Some(mixed_link);
        assert!(
            record_canonical_engine_receipt(
                &task,
                &plan,
                &invocation,
                &mixed,
                record.clone(),
                &signing_key,
                &ledger,
            )
            .is_err()
        );

        let self_bytes = crate::core::engine_interface::canonical_engine_receipt_artifact_bytes(
            &invocation,
            &observation,
        );
        let self_digest = Sha256Digest::new(digest(&self_bytes)).unwrap();
        crate::core::engine_interface::persist_engine_artifact_content(
            "engine-interface/v1/receipts",
            self_digest.hex(),
            "json",
            &self_bytes,
        )
        .unwrap();
        let mut self_link = observation.receipt_link.clone().unwrap();
        self_link.receipt_ref =
            lean_ctx_protocol::ProtocolReference::new(format!("receipt:{}", self_digest.as_str()))
                .unwrap();
        self_link.receipt_digest = self_digest;
        let mut self_linked = observation.clone();
        self_linked.receipt_link = Some(self_link);
        assert!(
            record_canonical_engine_receipt(
                &task,
                &plan,
                &invocation,
                &self_linked,
                record.clone(),
                &signing_key,
                &ledger,
            )
            .is_err()
        );
        assert!(ledger.load().unwrap().is_empty());

        let published = record_canonical_engine_receipt(
            &task,
            &plan,
            &invocation,
            &observation,
            record.clone(),
            &signing_key,
            &ledger,
        )
        .unwrap();
        let repeated = record_canonical_engine_receipt(
            &task,
            &plan,
            &invocation,
            &observation,
            record.clone(),
            &signing_key,
            &ledger,
        )
        .unwrap();

        assert_eq!(published, repeated);
        assert_eq!(ledger.load_verified().unwrap().len(), 5);
        assert!(ledger.verify_chain().unwrap());
        let projected = ledger
            .canonical_receipt_for_task_verified("task-1")
            .unwrap()
            .unwrap();
        assert_eq!(projected.receipt_id, published.receipt_id);
        let receipt =
            ReceiptDocumentV1::from_canonical_bytes(&std::fs::read(&published.path).unwrap())
                .unwrap();
        assert_eq!(
            receipt.lineage.invocation_id,
            invocation.invocation_id.as_str()
        );

        let mut treatment_record = record;
        treatment_record.chain.chain_id = "chain-2".to_owned();
        let treatment_ledger =
            ExecutionLedgerStore::new(root.path().join("treatment-ledger.jsonl"));
        let treatment = record_canonical_engine_receipt(
            &task,
            &plan,
            &invocation,
            &observation,
            treatment_record,
            &signing_key,
            &treatment_ledger,
        )
        .unwrap();

        let artifact = |kind, path: &str, bytes: Vec<u8>| CustomerProofArtifact {
            kind,
            path: path.to_owned(),
            redaction_class: CustomerProofRedactionClass::None,
            bytes,
        };
        let task_bytes = canonical(&task);
        let plan_bytes = canonical(&plan);
        let invocation_bytes = canonical(&invocation);
        let observation_bytes = canonical(&observation);
        let identity_bytes = canonical(&task.agent_id);
        let policy_bytes = canonical(&invocation.policy_admission);
        let input_bytes = b"stable native context".to_vec();
        let engine_receipt_bytes = crate::core::engine_artifact::read_content(
            "engine-interface/v1/receipts",
            engine_receipt.receipt_digest.hex(),
            "json",
        )
        .unwrap();
        let quality_bytes = crate::core::canonical::canonical_serialize(&serde_json::json!({
            "method": "provider_free_engine_fixture",
            "score_milli": 1000
        }));
        let replay_result_bytes = b"standalone-verifier-accepted".to_vec();
        let mut artifacts = vec![
            artifact(
                CustomerProofArtifactKind::ArmReceipt,
                "arms/control.json",
                std::fs::read(&published.path).unwrap(),
            ),
            artifact(
                CustomerProofArtifactKind::ArmReceipt,
                "arms/treatment.json",
                std::fs::read(&treatment.path).unwrap(),
            ),
            artifact(
                CustomerProofArtifactKind::TaskEnvelope,
                "lineage/task.json",
                task_bytes,
            ),
            artifact(
                CustomerProofArtifactKind::ExecutionPlan,
                "lineage/plan.json",
                plan_bytes,
            ),
            artifact(
                CustomerProofArtifactKind::EngineInvocation,
                "lineage/invocation.json",
                invocation_bytes,
            ),
            artifact(
                CustomerProofArtifactKind::EngineObservation,
                "lineage/observation.json",
                observation_bytes,
            ),
            artifact(
                CustomerProofArtifactKind::RunMetadata,
                "lineage/identity.json",
                identity_bytes,
            ),
            artifact(
                CustomerProofArtifactKind::ClaimBasis,
                "lineage/policy.json",
                policy_bytes,
            ),
            artifact(
                CustomerProofArtifactKind::Measurement,
                "engine/receipt.json",
                engine_receipt_bytes,
            ),
            artifact(
                CustomerProofArtifactKind::QualityMeasurement,
                "quality/measurement.json",
                quality_bytes,
            ),
            artifact(
                CustomerProofArtifactKind::ReplayInput,
                "replay/input.txt",
                input_bytes,
            ),
            artifact(
                CustomerProofArtifactKind::ReplayResult,
                "replay/result.txt",
                replay_result_bytes,
            ),
        ];
        let control_ref = artifacts[0].reference();
        let treatment_ref = artifacts[1].reference();
        let quality_ref = artifacts[9].reference();
        let replay_input_ref = artifacts[10].reference();
        let replay_result_ref = artifacts[11].reference();
        let workload_digest = invocation.input_digest.as_str();
        let shared_identity = serde_json::json!({
            "provider": "leanctx",
            "model": "local-engine",
            "source_commit": "git:2c7d0044302d50af8d218a041b45726ad710757c",
            "workload_digest": workload_digest
        });
        let arm = |role: &str, arm_id: &str, evidence_ref: &str| {
            serde_json::json!({
                "role": role,
                "arm_id": arm_id,
                "identity": shared_identity.clone(),
                "status": "complete",
                "measurements": {
                    "input_tokens": 4,
                    "cached_input_tokens": 0,
                    "output_tokens": 4,
                    "latency_ms": 1,
                    "cost": {
                        "currency": "USD",
                        "amount_micros": 0,
                        "status": "observed"
                    },
                    "status": "observed"
                },
                "evidence_refs": [evidence_ref]
            })
        };
        let draft = CustomerProofDraftV2 {
            created_at: "2026-08-24T12:00:00Z".to_owned(),
            status: "complete".to_owned(),
            subject: serde_json::json!({
                "customer_ref": "customer:provider-free",
                "project_ref": "project:lean-ctx",
                "workload_ref": format!("id:{workload_digest}")
            }),
            matched_arms: serde_json::json!({
                "match_id": format!("id:{}", digest(b"provider-free-engine-match")),
                "match_basis": ["provider", "model", "source_commit", "workload_digest"],
                "shared_identity": shared_identity,
                "control": arm(
                    "control",
                    &format!("id:{}", published.receipt_id),
                    &control_ref
                ),
                "treatment": arm(
                    "treatment",
                    &format!("id:{}", treatment.receipt_id),
                    &treatment_ref
                )
            }),
            quality: serde_json::json!({
                "status": "preserved",
                "metric": "score_milli",
                "control_score_milli": 1000,
                "treatment_score_milli": 1000,
                "confidence": "high",
                "method": "automated",
                "evidence_refs": [quality_ref.clone()]
            }),
            replay: serde_json::json!({
                "status": "replayable",
                "mode": "offline",
                "determinism": "same_inputs_expected",
                "input_refs": [replay_input_ref],
                "result_refs": [replay_result_ref],
                "notes": "real NativeContextEngine provider-free fixture"
            }),
            limitations: serde_json::json!({
                "known_limitations": [],
                "unproven": ["omission_before_capture"]
            }),
            redaction: serde_json::json!({
                "class": "none",
                "policy": "no_redaction",
                "reversible": false,
                "notes": "provider-free local fixture"
            }),
            claims: serde_json::json!([{
                "claim_id": format!("id:{}", digest(b"provider-free-quality-claim")),
                "claim_type": "quality_preserved",
                "statement": "The real local Engine run preserved fixture quality.",
                "claim_validity": "supported",
                "scope": "matched_run",
                "basis_refs": [quality_ref]
            }]),
        };
        let proof = assemble_customer_proof_v2(
            &draft,
            std::mem::take(&mut artifacts),
            CustomerProofSigner {
                signing_key: &signing_key,
                trust_basis: CustomerProofTrustBasis::OutOfBand,
            },
        )
        .unwrap();
        let proof_root = root.path().join("provider-free-proof");
        proof.write_to(&proof_root).unwrap();
        let bundle_key_id = format!("id:{}", digest(signing_key.verifying_key().as_bytes()));
        let trust_store = crate::core::canonical::canonical_serialize(&serde_json::json!({
            "schema_version": "leanctx.customer-proof-trust-store/v1",
            "trust_revision": 1,
            "evaluated_at": "2026-08-24T12:00:00Z",
            "trusted_signers": [{
                "trusted_signer_ref": format!("signer:{bundle_key_id}"),
                "key_id": bundle_key_id,
                "public_key": crate::core::agent_identity::hex_encode(
                    signing_key.verifying_key().as_bytes()
                ),
                "allowed_trust_bases": ["out_of_band"],
                "receipt_key_ids": ["test-key"],
                "revision": 1,
                "admitted_at": "2026-01-01T00:00:00Z",
                "expires_at": "2027-01-01T00:00:00Z",
                "revoked_at": null
            }],
            "receipt_chain_heads": [
                {
                    "chain_id": "chain-1",
                    "sequence_number": 1,
                    "receipt_id": published.receipt_id
                },
                {
                    "chain_id": "chain-2",
                    "sequence_number": 1,
                    "receipt_id": treatment.receipt_id
                }
            ]
        }));
        let report = leanctx_verify::v2::verify_v2_document(
            &proof.canonical_json,
            Some(&trust_store),
            Some(&proof_root),
        );
        assert!(report.valid, "{:?}", report.steps);
        assert!(report.proof_eligible);
    }
}
