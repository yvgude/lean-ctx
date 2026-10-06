// SPDX-License-Identifier: Apache-2.0

//! Purpose-scoped authentication of canonical decision records.
//!
//! Signature success is not proof that the selected action actually executed,
//! that referenced artifacts exist, or that an outcome may train a learner.
//! Those bindings remain the production consumer's responsibility.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use lean_ctx_protocol::{
    DecisionKind, DecisionRecordV1, DecisionStageV1, TaskEnvelopeV1, UtcTimestamp,
};

use crate::{ReceiptSignerAdmissionV1, validate_signer_admission};

/// Host-owned, explicitly authorized decision purpose and exact task snapshot.
///
/// Resolve this grant from trusted host policy, never from record/request fields.
/// `key_admission` reuses key-window metadata; receipt-signing permission alone
/// does NOT authorize construction of this decision grant. There is deliberately
/// no deserializer or automatic conversion from a receipt signer snapshot.
#[derive(Debug, Clone)]
pub struct DecisionSignerAdmissionV1 {
    pub key_admission: ReceiptSignerAdmissionV1,
    pub task: TaskEnvelopeV1,
    pub stage: DecisionStageV1,
    pub kind: DecisionKind,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum DecisionVerificationError {
    #[error("decision purpose or task is not authorized by the host grant")]
    ScopeNotAuthorized,
    #[error("decision signing key is not admitted at observation and verification time")]
    SignerNotAdmitted,
    #[error("{0}")]
    InvalidDecision(String),
}

fn invalid(error: impl std::fmt::Display) -> DecisionVerificationError {
    DecisionVerificationError::InvalidDecision(error.to_string())
}

fn validate_grant(
    record: &DecisionRecordV1,
    task: &TaskEnvelopeV1,
    grant: &DecisionSignerAdmissionV1,
    verifying_key: &VerifyingKey,
    verified_at: &UtcTimestamp,
) -> Result<(), DecisionVerificationError> {
    record.validate().map_err(invalid)?;
    task.validate().map_err(invalid)?;
    if task != &grant.task
        || record.task_id != task.task_id
        || record.decision_stage != grant.stage
        || record.decision_kind != grant.kind
    {
        return Err(DecisionVerificationError::ScopeNotAuthorized);
    }
    let task_digest =
        crate::receipt_verification::sha256_digest(&task.canonical_bytes().map_err(invalid)?);
    if !record
        .input_refs
        .iter()
        .any(|reference| reference == &task_digest)
    {
        return Err(invalid(
            "decision input_refs omit the canonical task digest",
        ));
    }
    // This signing profile uses the existing canonical UTC-second timestamp.
    // Generic DecisionRecord structural validation remains backward compatible.
    let observed_at = UtcTimestamp::new(record.observed_at.clone()).map_err(invalid)?;
    if observed_at.as_str() > verified_at.as_str() {
        return Err(invalid("decision observation is in the future"));
    }
    for at in [&observed_at, verified_at] {
        validate_signer_admission(&grant.key_admission, verifying_key, at)
            .map_err(|_| DecisionVerificationError::SignerNotAdmitted)?;
    }
    Ok(())
}

/// Sign the complete decision payload for an explicitly authorized host purpose.
///
/// The caller supplies the genuinely observed decision time and actual selected
/// result; this function does not manufacture either. It returns the signature
/// without mutating the record, even on failure.
pub fn sign_decision_record(
    record: &DecisionRecordV1,
    task: &TaskEnvelopeV1,
    grant: &DecisionSignerAdmissionV1,
    signing_key: &SigningKey,
    signed_at: &UtcTimestamp,
) -> Result<String, DecisionVerificationError> {
    validate_grant(record, task, grant, &signing_key.verifying_key(), signed_at)?;
    let payload = record.signing_bytes().map_err(invalid)?;
    Ok(STANDARD.encode(signing_key.sign(&payload).to_bytes()))
}

/// Verify purpose/task authorization, signed full-task digest, key windows and Ed25519.
///
/// No filesystem, caller-supplied trust discovery, replay or learning side effect
/// occurs here. Production must additionally verify plan/handoff/artifact lineage
/// and compare the selected result to the actual captured immutable decision.
pub fn verify_decision_signature(
    record: &DecisionRecordV1,
    task: &TaskEnvelopeV1,
    grant: &DecisionSignerAdmissionV1,
    verifying_key: &VerifyingKey,
    verified_at: &UtcTimestamp,
) -> Result<(), DecisionVerificationError> {
    validate_grant(record, task, grant, verifying_key, verified_at)?;
    let signature = STANDARD.decode(&record.signature).map_err(invalid)?;
    let signature = Signature::from_slice(&signature).map_err(invalid)?;
    verifying_key
        .verify_strict(&record.signing_bytes().map_err(invalid)?, &signature)
        .map_err(invalid)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timestamp(value: &str) -> UtcTimestamp {
        UtcTimestamp::new(value).expect("timestamp")
    }

    fn fixture() -> (DecisionRecordV1, DecisionSignerAdmissionV1, SigningKey) {
        let task: TaskEnvelopeV1 = serde_json::from_value(serde_json::json!({
            "schema_version":1,"task_id":"task-1","trace_id":"trace-1",
            "project_id":"project-1","session_id":"session-1","agent_id":"agent-1",
            "complexity":"unknown","created_at":"2026-09-01T00:00:00Z",
            "tenant_id":"tenant-1"
        }))
        .expect("task fixture");
        let key = SigningKey::from_bytes(&[17; 32]);
        let grant = DecisionSignerAdmissionV1 {
            key_admission: ReceiptSignerAdmissionV1 {
                key_id: "explicit-decision-key".into(),
                public_key_digest: lean_ctx_protocol::Sha256Digest::new(
                    crate::receipt_verification::sha256_digest(key.verifying_key().as_bytes()),
                )
                .expect("key digest"),
                admitted_at: timestamp("2026-09-01T00:00:00Z"),
                expires_at: timestamp("2026-10-01T00:00:00Z"),
                revoked_at: None,
            },
            task,
            stage: DecisionStageV1::Planning,
            kind: DecisionKind::ContextSelection,
        };
        let task_ref =
            crate::receipt_verification::sha256_digest(&grant.task.canonical_bytes().unwrap());
        let record = serde_json::from_value(serde_json::json!({
            "schema_version":1,"decision_id":"decision-1","task_id":"task-1",
            "plan_id":"plan-1","decision_stage":"planning","decision_kind":"context_selection",
            "input_refs":[task_ref],"constraint_refs":[],
            "selected_result":{"read_policy":{"mode":"map"}},
            "rationale_code":"required_primary_source","decision_system_name":"context-kernel",
            "decision_system_version":"test","evidence_refs":[],
            "observed_at":"2026-09-01T00:00:00Z","signature":""
        }))
        .expect("decision fixture");
        (record, grant, key)
    }

    fn signed_fixture() -> (DecisionRecordV1, DecisionSignerAdmissionV1, SigningKey) {
        let (mut record, grant, key) = fixture();
        record.signature = sign_decision_record(
            &record,
            &grant.task,
            &grant,
            &key,
            &timestamp("2026-09-02T00:00:00Z"),
        )
        .unwrap();
        (record, grant, key)
    }

    fn verify(
        record: &DecisionRecordV1,
        grant: &DecisionSignerAdmissionV1,
        key: &SigningKey,
    ) -> Result<(), DecisionVerificationError> {
        verify_decision_signature(
            record,
            &grant.task,
            grant,
            &key.verifying_key(),
            &timestamp("2026-09-02T00:00:00Z"),
        )
    }

    #[test]
    fn explicit_purpose_and_exact_task_signature_round_trip() {
        let (record, grant, key) = signed_fixture();
        verify(&record, &grant, &key).unwrap();
        assert!(!record.signature.is_empty());
    }

    #[test]
    fn changed_payload_and_unsigned_domain_are_rejected() {
        let (record, grant, key) = signed_fixture();
        for field in [
            "decision_id",
            "selected_result",
            "rationale_code",
            "observed_at",
        ] {
            let mut changed = serde_json::to_value(&record).unwrap();
            changed[field] = match field {
                "selected_result" => serde_json::json!({"read_policy":{"mode":"full"}}),
                "observed_at" => serde_json::json!("2026-09-01T00:00:01Z"),
                _ => serde_json::json!("changed"),
            };
            let changed = serde_json::from_value(changed).unwrap();
            assert!(verify(&changed, &grant, &key).is_err(), "{field}");
        }
        let mut wrong_domain = record.clone();
        wrong_domain.signature = STANDARD.encode(
            key.sign(&record.unsigned_canonical_bytes().unwrap())
                .to_bytes(),
        );
        assert!(verify(&wrong_domain, &grant, &key).is_err());
        let mut extension = record;
        extension
            .extensions
            .insert("additional_fact", serde_json::json!(true))
            .unwrap();
        assert!(verify(&extension, &grant, &key).is_err());
    }

    #[test]
    fn receipt_key_alone_cannot_bypass_decision_purpose_or_task_scope() {
        let (record, mut grant, key) = signed_fixture();
        grant.kind = DecisionKind::Stop;
        assert_eq!(
            verify(&record, &grant, &key),
            Err(DecisionVerificationError::ScopeNotAuthorized)
        );
        grant.kind = DecisionKind::ContextSelection;
        grant.stage = DecisionStageV1::Outcome;
        assert_eq!(
            verify(&record, &grant, &key),
            Err(DecisionVerificationError::ScopeNotAuthorized)
        );
        grant.stage = DecisionStageV1::Planning;
        let mut wrong_task = grant.task.clone();
        wrong_task.project_id = lean_ctx_protocol::ProjectId::new("other-project").unwrap();
        assert_eq!(
            verify_decision_signature(
                &record,
                &wrong_task,
                &grant,
                &key.verifying_key(),
                &timestamp("2026-09-02T00:00:00Z")
            ),
            Err(DecisionVerificationError::ScopeNotAuthorized)
        );
        // Even a new purpose grant for a changed task cannot reuse the old signature.
        grant.task = wrong_task;
        assert!(verify(&record, &grant, &key).is_err());
        let mut relabeled = record.clone();
        relabeled.input_refs = vec![crate::receipt_verification::sha256_digest(
            &grant.task.canonical_bytes().unwrap(),
        )];
        relabeled.validate().unwrap();
        assert!(verify(&relabeled, &grant, &key).is_err());
        grant.task.project_id = lean_ctx_protocol::ProjectId::new("project-1").unwrap();
        grant.task.tenant_id = Some(lean_ctx_protocol::TenantId::new("other-tenant").unwrap());
        assert!(verify(&record, &grant, &key).is_err());
    }

    #[test]
    fn trust_windows_and_invalid_signatures_fail_closed() {
        let (record, grant, key) = signed_fixture();
        verify_decision_signature(
            &record,
            &grant.task,
            &grant,
            &key.verifying_key(),
            &timestamp("2026-09-01T00:00:00Z"),
        )
        .unwrap();
        assert!(
            verify_decision_signature(
                &record,
                &grant.task,
                &grant,
                &key.verifying_key(),
                &timestamp("2026-08-31T23:59:59Z")
            )
            .is_err()
        );
        assert_eq!(
            verify_decision_signature(
                &record,
                &grant.task,
                &grant,
                &key.verifying_key(),
                &grant.key_admission.expires_at
            ),
            Err(DecisionVerificationError::SignerNotAdmitted)
        );
        let mut revoked = grant.clone();
        revoked.key_admission.revoked_at = Some(timestamp("2026-09-02T00:00:00Z"));
        assert_eq!(
            verify(&record, &revoked, &key),
            Err(DecisionVerificationError::SignerNotAdmitted)
        );
        assert!(verify(&record, &grant, &SigningKey::from_bytes(&[18; 32])).is_err());
        let mut late_admission = grant.clone();
        late_admission.key_admission.admitted_at = timestamp("2026-09-01T00:00:01Z");
        assert_eq!(
            verify(&record, &late_admission, &key),
            Err(DecisionVerificationError::SignerNotAdmitted)
        );
        for observed_at in ["", "2026-09-01T00:00:00+00:00", "2026-09-01T00:00:00.1Z"] {
            let mut bad = record.clone();
            bad.observed_at = observed_at.into();
            assert!(verify(&bad, &grant, &key).is_err());
        }
        for signature in [
            "",
            "not-base64",
            &STANDARD.encode([0_u8; 64]),
            &STANDARD.encode([0_u8; 63]),
        ] {
            let mut bad = record.clone();
            bad.signature = signature.into();
            assert!(verify(&bad, &grant, &key).is_err());
        }
    }
}
