// SPDX-License-Identifier: Apache-2.0

//! Trusted host authority for native Engine receipt publication.

mod checkpoint;
mod checkpoint_resume;
mod checkpoint_transfer;
use checkpoint_transfer::inspect_personal_sync_payload;
mod context_decision;
mod context_protocol;
mod outcome;
mod outcome_transport;
mod personal_sync;
mod source_recheck;
mod source_replay;
mod task_scope;
pub(crate) use checkpoint::HostCheckpointRequest;
use checkpoint_resume::CheckpointResumeTarget;
pub(crate) use checkpoint_resume::HostCheckpointResumeRequest;
use checkpoint_transfer::CheckpointImportAdmission;
pub(crate) use checkpoint_transfer::{HostCheckpointImportRequest, HostCheckpointPackageRequest};
pub(crate) use outcome::HostOutcomeRequest;
pub(crate) use personal_sync::{
    PersonalCarrierV1, PersonalSyncAdmission, receive as receive_personal_sync,
    snapshot as snapshot_personal_sync,
};

use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use ed25519_dalek::SigningKey;
use lean_ctx_protocol::{
    AcceptanceState, ContextBalanceV1, DecisionRecordV1, EngineInvocationV1,
    EngineObservationStatusV1, EngineObservationV1, EngineValueClassificationV1, ExecutionPlanV1,
    ReceiptChainLinkV1, ReceiptEvidenceKindV1, ReceiptEvidenceRefV1, ReceiptOutcomeLinkV1,
    ReceiptTerminalStatusV1, ReceiptValueClassificationV1, ReceiptValueV1, Sha256Digest,
    SignatureStatus, TaskEnvelopeV1, UtcTimestamp,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use super::{
    CanonicalReceiptRecordV1, ExecutionEvent, ExecutionLedgerStore, PublishedCanonicalReceipt,
    ReceiptSignerAdmissionV1, record_canonical_engine_receipt,
};
use crate::core::execution_ledger::producer::validate_signer_admission;
use crate::core::{
    canonical::canonical_serialize,
    context_kernel::autopilot::TaskAutopilotDecision,
    engine_interface::{
        MAX_TRANSPORT_BUDGET_TOKENS, persist_engine_artifact_content, verified_output_view,
    },
};

const HOST_LIMIT: usize = 16 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HostSettings {
    schema_version: u32,
    #[serde(deserialize_with = "deserialize_secret")]
    signing_key_hex: Zeroizing<String>,
    signer: SignerAdmission,
    ledger_path: PathBuf,
    /// Separate operator grant; receipt authority never implies this purpose.
    #[serde(default)]
    allow_context_decision_signing: bool,
    /// Independent operator grant for attested terminal outcomes, never tool arguments.
    #[serde(default)]
    allow_outcome_signing: bool,
    /// Independent operator grant for canonical continuation signing.
    #[serde(default)]
    allow_checkpoint_signing: bool,
    /// Optional, independently scoped grant to resume a local checkpoint.
    #[serde(default)]
    checkpoint_resume: Option<CheckpointResumeTarget>,
    /// Independent operator grant to package a local checkpoint for transfer.
    #[serde(default)]
    allow_checkpoint_transfer_export: bool,
    /// Optional, independently pinned foreign source admitted for manual import.
    #[serde(default)]
    checkpoint_import: Option<CheckpointImportAdmission>,
    #[serde(default)]
    checkpoint_continue: Option<checkpoint_transfer::CheckpointContinueAdmission>,
    /// Receiver-owned source mapping, independent of foreign package metadata.
    #[serde(default)]
    checkpoint_source_files: Option<Vec<source_recheck::ReceivingFileBinding>>,
    #[serde(default)]
    checkpoint_source_providers: Option<Vec<source_recheck::ReceivingProviderBinding>>,
    /// Optional operator binding for tasks admitted through this local host.
    #[serde(default)]
    task_scope: Option<task_scope::HostTaskScope>,
}

fn deserialize_secret<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Zeroizing<String>, D::Error> {
    String::deserialize(deserializer).map(Zeroizing::new)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SignerAdmission {
    key_id: String,
    public_key_digest: Sha256Digest,
    admitted_at: UtcTimestamp,
    expires_at: UtcTimestamp,
    revoked_at: Option<UtcTimestamp>,
}

impl SignerAdmission {
    pub(super) fn snapshot(&self) -> ReceiptSignerAdmissionV1 {
        ReceiptSignerAdmissionV1 {
            key_id: self.key_id.clone(),
            public_key_digest: self.public_key_digest.clone(),
            admitted_at: self.admitted_at.clone(),
            expires_at: self.expires_at.clone(),
            revoked_at: self.revoked_at.clone(),
        }
    }
}

/// Private host signing authority loaded from an operator-owned configuration.
///
/// The signing key, admission snapshot, and ledger store never leave this
/// authority.  In particular, this type intentionally does not implement
/// `Debug`.
pub(crate) struct HostReceiptAuthority {
    signing_key: SigningKey,
    signer_admission: ReceiptSignerAdmissionV1,
    ledger: ExecutionLedgerStore,
    allow_context_decision_signing: bool,
    allow_outcome_signing: bool,
    allow_checkpoint_signing: bool,
    checkpoint_resume: Option<CheckpointResumeTarget>,
    allow_checkpoint_transfer_export: bool,
    checkpoint_import: Option<CheckpointImportAdmission>,
    checkpoint_continue: Option<checkpoint_transfer::CheckpointContinueAdmission>,
    checkpoint_source_files: Option<Vec<source_recheck::ReceivingFileBinding>>,
    checkpoint_source_providers: Option<Vec<source_recheck::ReceivingProviderBinding>>,
    task_scope: Option<task_scope::HostTaskScope>,
}

/// One admitted task attempt retaining its task lock through publication.
pub(crate) struct HostReceiptAttempt {
    task: TaskEnvelopeV1,
    plan: ExecutionPlanV1,
    context_decision: Option<DecisionRecordV1>,
    context: Option<TaskAutopilotDecision>,
    _task_lock: File,
}

impl HostReceiptAttempt {
    pub(crate) fn context_decision(&self) -> Option<&DecisionRecordV1> {
        self.context_decision.as_ref()
    }
}

impl HostReceiptAuthority {
    /// Load the strict, bounded host settings document from an existing reader.
    pub(crate) fn from_reader(input: &mut dyn Read) -> Result<Self, &'static str> {
        Self::from_reader_scoped(input, None)
    }

    /// Constrain an operator-selected ledger to the caller's trusted state root.
    /// Uses the existing descriptor-pinned ledger store; no second store authority.
    pub(crate) fn from_reader_scoped(
        input: &mut dyn Read,
        ledger_root: Option<&Path>,
    ) -> Result<Self, &'static str> {
        let bytes = read_bounded(input, HOST_LIMIT, "invalid_host_configuration")?;
        let settings: HostSettings =
            serde_json::from_slice(&bytes).map_err(|_| "invalid_host_configuration")?;
        let key = parse_key(settings.signing_key_hex.as_str())?;
        let admission = settings.signer.snapshot();
        validate_settings(&settings, &admission)?;
        validate_admission(&admission, &key)?;
        let ledger = match ledger_root {
            Some(root) => {
                let relative = settings
                    .ledger_path
                    .strip_prefix(root)
                    .map_err(|_| "host_ledger_outside_root")?;
                ExecutionLedgerStore::new_verified(root, relative)
                    .map_err(|_| "invalid_host_ledger_root")?
            }
            None => ExecutionLedgerStore::new(settings.ledger_path),
        };
        Ok(Self {
            signing_key: key,
            signer_admission: admission,
            ledger,
            allow_context_decision_signing: settings.allow_context_decision_signing,
            allow_outcome_signing: settings.allow_outcome_signing,
            allow_checkpoint_signing: settings.allow_checkpoint_signing,
            checkpoint_resume: settings.checkpoint_resume,
            allow_checkpoint_transfer_export: settings.allow_checkpoint_transfer_export,
            checkpoint_import: settings.checkpoint_import,
            checkpoint_continue: settings.checkpoint_continue,
            checkpoint_source_files: settings.checkpoint_source_files,
            checkpoint_source_providers: settings.checkpoint_source_providers,
            task_scope: settings.task_scope,
        })
    }

    /// Revalidate the trusted signer against the host's current UTC clock.
    pub(crate) fn validate_current(&self) -> Result<(), &'static str> {
        if !valid_key_id(&self.signer_admission.key_id) {
            return Err("invalid_host_configuration");
        }
        validate_admission(&self.signer_admission, &self.signing_key)
    }

    /// Require the independent operator grant before starting a source-bound attempt.
    pub(crate) fn require_context_decision_signing(&self) -> Result<(), &'static str> {
        self.validate_current()?;
        if !self.allow_context_decision_signing {
            return Err("host_context_decision_not_authorized");
        }
        Ok(())
    }

    /// Admit one task, persist intent, and retain its exclusive task lock.
    pub(crate) fn begin(
        &self,
        task: &TaskEnvelopeV1,
        plan: &ExecutionPlanV1,
    ) -> Result<HostReceiptAttempt, &'static str> {
        self.begin_with_context(task, plan, None)
    }

    /// Capture the actual immutable planning handoff before Engine execution.
    pub(crate) fn begin_with_context(
        &self,
        task: &TaskEnvelopeV1,
        plan: &ExecutionPlanV1,
        context: Option<&TaskAutopilotDecision>,
    ) -> Result<HostReceiptAttempt, &'static str> {
        self.validate_current()?;
        task.validate().map_err(|_| "invalid_host_task")?;
        plan.validate().map_err(|_| "invalid_host_plan")?;
        if task.task_id != plan.task_id
            || plan
                .executor_agent_id
                .as_ref()
                .is_some_and(|agent| agent != &task.agent_id)
            || plan
                .context_token_limit()
                .is_some_and(|limit| limit == 0 || limit > MAX_TRANSPORT_BUDGET_TOKENS)
        {
            return Err("invalid_host_plan");
        }

        let task_lock = task_lock(task)?;
        if !self
            .ledger
            .by_task_verified(task.task_id.as_str())
            .map_err(|_| "host_ledger_unavailable")?
            .is_empty()
        {
            return Err("host_task_already_recorded");
        }
        let context_decision = if self.allow_context_decision_signing {
            Some(context_decision::capture(
                self,
                task,
                plan,
                context.ok_or("host_context_decision_missing")?,
            )?)
        } else {
            None
        };
        record_attempt(task, &self.ledger)?;
        Ok(HostReceiptAttempt {
            task: task.clone(),
            plan: plan.clone(),
            context_decision,
            context: self
                .allow_context_decision_signing
                .then(|| context.cloned())
                .flatten(),
            _task_lock: task_lock,
        })
    }

    /// Verify and publish the exact delivered view as one canonical receipt.
    pub(crate) fn publish(
        &self,
        attempt: &HostReceiptAttempt,
        invocation: &EngineInvocationV1,
        observation: &EngineObservationV1,
        delivered: &str,
    ) -> Result<PublishedCanonicalReceipt, &'static str> {
        self.validate_current()?;
        if attempt.task.task_id != attempt.plan.task_id {
            return Err("invalid_host_plan");
        }
        if observation.status != EngineObservationStatusV1::Succeeded {
            return Err("host_engine_evidence_unavailable");
        }

        let view = verified_output_view(invocation, observation)
            .map_err(|_| "host_engine_evidence_unavailable")?;
        if view.text != delivered {
            return Err("host_engine_evidence_unavailable");
        }

        let measurement = |name: &str| -> Result<u64, &'static str> {
            observation
                .measurements
                .iter()
                .find(|entry| {
                    entry.name == name
                        && entry.unit == "token"
                        && entry.classification == EngineValueClassificationV1::Measured
                })
                .and_then(|entry| entry.value)
                .ok_or("host_engine_measurement_unavailable")
        };
        let original_tokens = measurement("input_tokens")?;
        let delivered_tokens = measurement("output_tokens")?;
        if attempt
            .plan
            .context_token_limit()
            .is_some_and(|limit| delivered_tokens > limit)
        {
            return Err("host_context_budget_exceeded");
        }

        let receipt_link = observation
            .receipt_link
            .as_ref()
            .ok_or("host_engine_evidence_unavailable")?;
        let observation_digest = digest(&canonical_serialize(observation))?;
        let values = [
            ("input_tokens", original_tokens),
            ("output_tokens", delivered_tokens),
        ]
        .into_iter()
        .map(|(name, value)| ReceiptValueV1 {
            name: name.into(),
            unit: "token".into(),
            classification: ReceiptValueClassificationV1::Measured,
            value: Some(value),
            evidence_digests: vec![observation_digest.clone()],
            formula_digest: None,
            price_table_digest: None,
            reconciliation_digest: None,
        })
        .chain([ReceiptValueV1 {
            name: "provider_cost_micros".into(),
            unit: "microusd".into(),
            classification: ReceiptValueClassificationV1::Unavailable,
            value: None,
            evidence_digests: vec![],
            formula_digest: None,
            price_table_digest: None,
            reconciliation_digest: None,
        }])
        .collect();
        let chain_id = digest(&canonical_serialize(&(
            "host-receipt-chain-v1",
            &attempt.task,
            &attempt.plan,
        )))?;
        let mut evidence_refs = vec![ReceiptEvidenceRefV1 {
            kind: ReceiptEvidenceKindV1::Measurement,
            uri: lean_ctx_protocol::ProtocolReference::new(format!(
                "artifact://engine/receipts/{}",
                receipt_link.receipt_digest.hex(),
            ))
            .map_err(|_| "host_engine_evidence_unavailable")?,
            digest: receipt_link.receipt_digest.clone(),
            media_type: "application/json".into(),
            signature_status: SignatureStatus::NotSigned,
        }];
        if let Some(decision) = &attempt.context_decision {
            // Capture already persisted and signed this exact planning record.
            // Bind its address so outcome consumers need no artifact-store scan.
            let decision_digest = digest(&canonical_serialize(decision))?;
            evidence_refs.push(ReceiptEvidenceRefV1 {
                kind: ReceiptEvidenceKindV1::Runtime,
                uri: lean_ctx_protocol::ProtocolReference::new(format!(
                    "artifact://execution/evidence/{}",
                    decision_digest.hex(),
                ))
                .map_err(|_| "host_context_decision_invalid")?,
                digest: decision_digest,
                media_type: "application/json".into(),
                signature_status: SignatureStatus::Unverified,
            });
        }
        record_canonical_engine_receipt(
            &attempt.task,
            &attempt.plan,
            invocation,
            observation,
            CanonicalReceiptRecordV1 {
                context_balance: ContextBalanceV1 {
                    original_tokens,
                    materialized_tokens: delivered_tokens,
                    delivered_tokens,
                    provider_billed_tokens: 0,
                },
                status: ReceiptTerminalStatusV1::Succeeded,
                values,
                outcome: ReceiptOutcomeLinkV1 {
                    state: AcceptanceState::Unknown,
                    outcome_id: None,
                    outcome_ref: None,
                    acceptance_evidence_digest: None,
                },
                evidence_refs,
                chain: ReceiptChainLinkV1 {
                    chain_id: chain_id.as_str().into(),
                    sequence_number: 1,
                    previous_receipt_id: None,
                    previous_signature_digest: None,
                },
                issued_at: now()?,
                signer_admission: self.signer_admission.clone(),
            },
            &self.signing_key,
            &self.ledger,
        )
        .map_err(|_| "host_receipt_publication_failed")
    }
}

fn validate_settings(
    settings: &HostSettings,
    admission: &ReceiptSignerAdmissionV1,
) -> Result<(), &'static str> {
    if settings.schema_version != 1
        || !settings.ledger_path.is_absolute()
        || !valid_key_id(&admission.key_id)
    {
        return Err("invalid_host_configuration");
    }
    Ok(())
}

fn validate_admission(
    admission: &ReceiptSignerAdmissionV1,
    key: &SigningKey,
) -> Result<(), &'static str> {
    validate_signer_admission(admission, &key.verifying_key(), &now()?)
        .map_err(|_| "host_signer_not_admitted")
}

fn valid_key_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:/-".contains(&byte))
        && !value.starts_with("base64:")
        && !value.starts_with("hex:")
}

fn record_attempt(
    task: &TaskEnvelopeV1,
    ledger: &ExecutionLedgerStore,
) -> Result<(), &'static str> {
    let bytes = canonical_serialize(task);
    let task_ref = digest(&bytes)?;
    drop(
        persist_engine_artifact_content("execution/evidence", task_ref.hex(), "json", &bytes)
            .map_err(|_| "host_task_evidence_unavailable")?,
    );
    if !ledger
        .append_if_new(ExecutionEvent::TaskStarted {
            task_id: task.task_id.as_str().into(),
            trace_id: task.trace_id.as_str().into(),
            envelope_ref: task_ref.as_str().into(),
            timestamp: task.created_at.as_str().into(),
            sequence_number: 0,
            prev_hash: String::new(),
        })
        .map_err(|_| "host_ledger_unavailable")?
    {
        return Err("host_task_already_recorded");
    }
    Ok(())
}

#[cfg(not(windows))]
fn open_task_lock_file(identity: &str, bytes: &[u8]) -> Result<File, String> {
    persist_engine_artifact_content("execution/host-locks", identity, "json", bytes)
}

/// Windows byte-range locks are mandatory: a lock on the content artifact
/// would stop the next opener from verifying its bytes, so the lock lives in
/// a sidecar beside it.
#[cfg(windows)]
fn open_task_lock_file(identity: &str, bytes: &[u8]) -> Result<File, String> {
    drop(persist_engine_artifact_content(
        "execution/host-locks",
        identity,
        "json",
        bytes,
    )?);
    crate::core::engine_interface::open_engine_artifact_lock("execution/host-locks", identity)
}

fn task_lock_identity(task: &TaskEnvelopeV1) -> Result<(String, Vec<u8>), &'static str> {
    let bytes = canonical_serialize(&(
        "host-receipt-task-v1",
        &task.project_id,
        &task.tenant_id,
        &task.task_id,
    ));
    let identity = digest(&bytes)?;
    Ok((identity.hex().to_owned(), bytes))
}

fn task_lock(task: &TaskEnvelopeV1) -> Result<File, &'static str> {
    let (identity, bytes) = task_lock_identity(task)?;
    let file = open_task_lock_file(&identity, &bytes).map_err(|_| "host_task_lock_unavailable")?;
    let started = Instant::now();
    loop {
        match fs2::FileExt::try_lock_exclusive(&file) {
            Ok(()) => return Ok(file),
            Err(error)
                if crate::core::file_lock::is_contended(&error)
                    && started.elapsed() < Duration::from_secs(2) =>
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return Err("host_task_busy"),
        }
    }
}

fn read_bounded(
    input: &mut dyn Read,
    limit: usize,
    code: &'static str,
) -> Result<Zeroizing<Vec<u8>>, &'static str> {
    let mut bytes = Zeroizing::new(Vec::new());
    input
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| code)?;
    if bytes.len() > limit {
        return Err(code);
    }
    Ok(bytes)
}

pub(crate) fn parse_key(value: &str) -> Result<SigningKey, &'static str> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("invalid_signing_key");
    }
    let mut bytes = Zeroizing::new([0_u8; 32]);
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .map_err(|_| "invalid_signing_key")?;
    }
    Ok(SigningKey::from_bytes(&bytes))
}

fn now() -> Result<UtcTimestamp, &'static str> {
    UtcTimestamp::new(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .map_err(|_| "host_clock_unavailable")
}

pub(crate) fn digest(bytes: &[u8]) -> Result<Sha256Digest, &'static str> {
    Sha256Digest::new(format!(
        "sha256:{}",
        crate::core::agent_identity::hex_encode(&Sha256::digest(bytes))
    ))
    .map_err(|_| "host_digest_unavailable")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_lock_is_bounded_and_reusable_after_owner_exit() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let task: TaskEnvelopeV1 = serde_json::from_value(serde_json::json!({
            "schema_version":1,"task_id":"host-lock-task","trace_id":"host-lock-trace",
            "project_id":"host-project","session_id":"host-session","agent_id":"host-agent",
            "complexity":"unknown","created_at":"2026-01-01T00:00:00Z"
        }))
        .unwrap();
        let owner = task_lock(&task).unwrap();
        assert_eq!(task_lock(&task).err(), Some("host_task_busy"));
        drop(owner);
        assert!(task_lock(&task).is_ok());
    }
}
