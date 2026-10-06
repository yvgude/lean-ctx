// SPDX-License-Identifier: Apache-2.0
//! Manual operator transfer of one already admitted canonical checkpoint.
//!
//! Export publishes the exact signed checkpoint together with the complete
//! evidence its verifier requires, taken from this host's already admitted
//! current lineage.  Import verifies that package against an independently
//! operator-pinned source key and an operator-selected receiving scope.  The
//! package never carries authority: no key, target, ledger record or admission
//! inside it is trusted, and a successful import creates no receipt, no ledger
//! entry and no second state store.

use anyhow::{Result, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signature, VerifyingKey};
use lean_ctx_protocol::{
    CONTEXT_CHECKPOINT_V2_SIGNATURE_DOMAIN, CONTEXT_CHECKPOINT_V3_SIGNATURE_DOMAIN,
    ExecutionPlanV1, ProjectId, Sha256Digest, TaskEnvelopeV1, TaskId, TenantId, UtcTimestamp,
    WorkspaceId,
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};

use super::{
    HostReceiptAuthority, ReceiptSignerAdmissionV1, canonical_serialize,
    checkpoint::{
        CheckpointEvidence, MAX_CHECKPOINT_RECEIPTS, read_task_and_plan, reconstruct_outcomes,
        verify_receipts,
    },
    checkpoint_resume::{Envelope, Signer, UnicodeEnvelope},
    digest, outcome,
};
use crate::core::{
    context_checkpoint::{VerifiedReceiptDocumentV1, verify_checkpoint_v2, verify_checkpoint_v3},
    engine_artifact,
    engine_interface::persist_engine_artifact_content,
    execution_ledger::producer::validate_signer_admission,
};

mod content;
mod continuation;
mod source_evidence;
pub(super) use continuation::CheckpointContinueAdmission;

pub(super) fn inspect_personal_sync_payload(root: &str, value: &serde_json::Value) -> Result<()> {
    content::inspect_personal_sync_payload(root, value)
}
pub(super) use source_evidence::{SourceDependency, SourceKind};

pub(super) const TRANSFER_SCHEMA: &str = "leanctx.checkpoint-transfer/v1";
const SOURCE_TRANSFER_SCHEMA: &str = "leanctx.checkpoint-transfer/v2";
const ENVELOPE_SCHEMA_V2: &str = "leanctx.host-checkpoint/v1";
const ENVELOPE_SCHEMA_V3: &str = "leanctx.host-checkpoint/v2";
const MAX_PACKAGE_BYTES: usize = 512 * 1024;
const MAX_ENVELOPE_BYTES: usize = 256 * 1024;
const MAX_ENTRY_BYTES: usize = 128 * 1024;
/// Task envelope, execution plan and at most one outcome snapshot per receipt.
const MAX_EVIDENCE_ENTRIES: usize = 3 * MAX_CHECKPOINT_RECEIPTS + 2;
/// Quarantined sibling of `execution/checkpoint-packages`: verified, not adopted.
const STAGING_NAMESPACE: &str = "execution/checkpoint-packages-staged";
const STAGED_SCHEMA: &str = "leanctx.checkpoint-transfer-staged/v1";
const MAX_STAGED_BYTES: usize = MAX_PACKAGE_BYTES + 8 * 1024;

/// Export request: one already exported local checkpoint artifact.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HostCheckpointPackageRequest {
    schema_version: u32,
    artifact_digest: Sha256Digest,
    #[serde(default)]
    include_source_evidence: bool,
}

/// Import request: the operator-transferred package and its expected digest.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HostCheckpointImportRequest {
    schema_version: u32,
    package_digest: Sha256Digest,
    package: CheckpointTransferPackageV1,
}

/// Exact canonical bytes of one digest-addressed artifact, carried as text.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TransferEntry {
    digest: Sha256Digest,
    json: String,
}

/// Serializable view of the host-owned signer trust snapshot.
///
/// The snapshot type itself is deliberately not serializable; this transfer view
/// carries the same fields so the receiving operator can compare a declared
/// signer with their own pin. It is never an admission token.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TransferSigner {
    key_id: String,
    public_key_digest: Sha256Digest,
    admitted_at: UtcTimestamp,
    expires_at: UtcTimestamp,
    revoked_at: Option<UtcTimestamp>,
}

impl From<&ReceiptSignerAdmissionV1> for TransferSigner {
    fn from(value: &ReceiptSignerAdmissionV1) -> Self {
        Self {
            key_id: value.key_id.clone(),
            public_key_digest: value.public_key_digest.clone(),
            admitted_at: value.admitted_at.clone(),
            expires_at: value.expires_at.clone(),
            revoked_at: value.revoked_at.clone(),
        }
    }
}

/// Declared source lineage. Audit information only: import compares it with the
/// verified checkpoint and the operator pin and never treats it as authority.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TransferLineage {
    task_id: TaskId,
    project_id: ProjectId,
    tenant_id: TenantId,
    workspace_id: WorkspaceId,
    current_receipt_digest: Sha256Digest,
}

/// The bounded, versioned transfer package.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CheckpointTransferPackageV1 {
    schema_version: String,
    envelope_schema: String,
    artifact_digest: Sha256Digest,
    envelope_json: String,
    checkpoint_digest: Sha256Digest,
    source_signer: TransferSigner,
    lineage: TransferLineage,
    receipts: Vec<TransferEntry>,
    evidence: Vec<TransferEntry>,
}

/// One independently pinned foreign source admitted for manual import.
///
/// The receiving project, tenant, workspace and root are the operator's, not the
/// package's.  A source scope that differs from the receiving scope requires an
/// explicit acknowledgement: the signed checkpoint keeps its own immutable
/// lineage and is never rewritten to look local.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CheckpointImportAdmission {
    schema_version: u32,
    source_public_key_hex: String,
    source_signer: super::SignerAdmission,
    admit_source_scope: TransferScope,
    receiving: ReceivingScope,
    #[serde(default)]
    allow_cross_scope_adoption: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TransferScope {
    project_id: ProjectId,
    tenant_id: TenantId,
    workspace_id: WorkspaceId,
}

/// Where a verified package is staged, and under whose identity it is recorded.
/// Staging never selects or creates a session for this root.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReceivingScope {
    project_root: PathBuf,
    project_id: ProjectId,
    tenant_id: TenantId,
    workspace_id: WorkspaceId,
}

/// Evidence resolved from an operator-transferred package.
///
/// Every entry was digest-verified when the map was built, so a lookup cannot
/// admit substituted bytes, and a missing digest fails closed instead of
/// falling back to any local artifact.
struct PackagedEvidence {
    entries: BTreeMap<String, Vec<u8>>,
}

impl CheckpointEvidence for PackagedEvidence {
    fn canonical_bytes(&self, expected: &Sha256Digest) -> Result<Vec<u8>> {
        let bytes = self
            .entries
            .get(expected.as_str())
            .ok_or_else(|| anyhow::anyhow!("transferred evidence missing"))?
            .clone();
        ensure!(
            digest(&bytes).map_err(anyhow::Error::msg)? == *expected,
            "transferred evidence digest mismatch"
        );
        Ok(bytes)
    }
}

impl PackagedEvidence {
    fn build(entries: &[TransferEntry]) -> Result<Self> {
        let mut map = BTreeMap::new();
        for entry in entries {
            let bytes = entry_bytes(entry)?;
            if let Some(previous) = map.insert(entry.digest.as_str().to_owned(), bytes) {
                ensure!(
                    previous == map[entry.digest.as_str()],
                    "conflicting transferred evidence"
                );
            }
        }
        Ok(Self { entries: map })
    }

    fn digests(&self) -> Vec<String> {
        self.entries.keys().cloned().collect()
    }
}

/// Admit only bounded, canonical JSON text; binary or control content is not a
/// supported transfer body even when its digest matches.
fn entry_bytes(entry: &TransferEntry) -> Result<Vec<u8>> {
    ensure!(
        !entry.json.is_empty()
            && entry.json.len() <= MAX_ENTRY_BYTES
            && !entry.json.chars().any(char::is_control),
        "unsupported transfer entry content"
    );
    ensure!(
        serde_json::from_str::<serde_json::Value>(&entry.json)?.is_object(),
        "unsupported transfer entry content"
    );
    let bytes = entry.json.as_bytes().to_vec();
    ensure!(
        digest(&bytes).map_err(anyhow::Error::msg)? == entry.digest,
        "transfer entry digest mismatch"
    );
    Ok(bytes)
}

fn entry(digest_value: &Sha256Digest, bytes: &[u8]) -> Result<TransferEntry> {
    let entry = TransferEntry {
        digest: digest_value.clone(),
        json: String::from_utf8(bytes.to_vec())?,
    };
    drop(entry_bytes(&entry)?);
    Ok(entry)
}

fn parse_verifying_key(value: &str) -> Result<VerifyingKey> {
    ensure!(
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "invalid pinned source key"
    );
    let mut bytes = [0_u8; 32];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)?;
    }
    Ok(VerifyingKey::from_bytes(&bytes)?)
}

impl HostReceiptAuthority {
    /// Package one exported local checkpoint with the complete evidence its
    /// verifier requires, proven through this host's current lineage.
    pub(crate) fn package_checkpoint(
        &self,
        request: &HostCheckpointPackageRequest,
    ) -> Result<serde_json::Value> {
        self.validate_current().map_err(anyhow::Error::msg)?;
        ensure!(
            self.allow_checkpoint_transfer_export,
            "checkpoint transfer export not authorized"
        );
        ensure!(request.schema_version == 1, "unsupported package request");
        let bytes = engine_artifact::read_content(
            "execution/checkpoints",
            request.artifact_digest.hex(),
            "json",
        )
        .map_err(anyhow::Error::msg)?;
        ensure!(
            bytes.len() <= MAX_ENVELOPE_BYTES,
            "checkpoint envelope exceeds transfer bound"
        );
        ensure!(
            digest(&bytes).map_err(anyhow::Error::msg)? == request.artifact_digest,
            "checkpoint artifact digest mismatch"
        );
        let envelope_text = String::from_utf8(bytes.clone())?;
        ensure!(
            crate::core::secret_detection::detect_secrets(&envelope_text).is_empty(),
            "checkpoint contains credential-shaped material"
        );
        let schema = serde_json::from_slice::<serde_json::Value>(&bytes)?
            .get("schema_version")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let package = if schema == ENVELOPE_SCHEMA_V3 {
            let envelope: UnicodeEnvelope = serde_json::from_slice(&bytes)?;
            ensure!(
                envelope.schema_version == ENVELOPE_SCHEMA_V3
                    && canonical_serialize(&envelope) == bytes,
                "unsupported or noncanonical Unicode checkpoint envelope"
            );
            self.admit_local_envelope(&envelope.signer, &envelope.signature, &{
                let mut payload = CONTEXT_CHECKPOINT_V3_SIGNATURE_DOMAIN.to_vec();
                payload.extend(envelope.checkpoint.canonical_bytes()?);
                payload
            })?;
            let checkpoint = &envelope.checkpoint;
            let lineage = &checkpoint.lineage;
            self.with_checkpoint_lineage(
                &lineage.artifact_lineage.receipt_refs,
                |task, plan, receipts, outcomes| {
                    let verified = verify_checkpoint_v3(
                        checkpoint.clone(),
                        task,
                        Some(plan),
                        receipts,
                        outcomes,
                    )?;
                    self.build_package(
                        ENVELOPE_SCHEMA_V3,
                        &envelope_text,
                        &request.artifact_digest,
                        &verified.checkpoint().digest()?,
                        TransferLineage {
                            task_id: lineage.task_id.clone(),
                            project_id: lineage.project_id.clone(),
                            tenant_id: lineage.tenant_id.clone(),
                            workspace_id: lineage.workspace_id.clone(),
                            current_receipt_digest: self.current_receipt_digest(task)?,
                        },
                        task,
                        plan,
                        receipts,
                        request.include_source_evidence,
                    )
                },
            )?
        } else {
            let envelope: Envelope = serde_json::from_slice(&bytes)?;
            ensure!(
                envelope.schema_version == ENVELOPE_SCHEMA_V2
                    && canonical_serialize(&envelope) == bytes,
                "unsupported or noncanonical checkpoint envelope"
            );
            self.admit_local_envelope(&envelope.signer, &envelope.signature, &{
                let mut payload = CONTEXT_CHECKPOINT_V2_SIGNATURE_DOMAIN.to_vec();
                payload.extend(envelope.checkpoint.canonical_bytes()?);
                payload
            })?;
            let checkpoint = &envelope.checkpoint;
            let lineage = &checkpoint.lineage;
            self.with_checkpoint_lineage(
                &lineage.artifact_lineage.receipt_refs,
                |task, plan, receipts, outcomes| {
                    let verified = verify_checkpoint_v2(
                        checkpoint.clone(),
                        task,
                        Some(plan),
                        receipts,
                        outcomes,
                    )?;
                    self.build_package(
                        ENVELOPE_SCHEMA_V2,
                        &envelope_text,
                        &request.artifact_digest,
                        &verified.digest()?,
                        TransferLineage {
                            task_id: lineage.task_id.clone(),
                            project_id: lineage.project_id.clone(),
                            tenant_id: lineage.tenant_id.clone(),
                            workspace_id: lineage.workspace_id.clone(),
                            current_receipt_digest: self.current_receipt_digest(task)?,
                        },
                        task,
                        plan,
                        receipts,
                        request.include_source_evidence,
                    )
                },
            )?
        };
        self.validate_current().map_err(anyhow::Error::msg)?;
        let package_bytes = canonical_serialize(&package);
        ensure!(
            package_bytes.len() <= MAX_PACKAGE_BYTES,
            "checkpoint package exceeds transfer bound"
        );
        // This operator CLI's project is local authority, never a root supplied
        // by the portable package. A pinned task scope additionally binds it.
        let root = std::env::current_dir()?;
        let root = canonical_receiving_root(&root)?;
        if let Some(identity) = self
            .admit_task_identity(Some(&root))
            .map_err(anyhow::Error::msg)?
        {
            ensure!(
                identity.project_id == package.lineage.project_id
                    && identity.tenant_id == package.lineage.tenant_id,
                "checkpoint export scope mismatch"
            );
        }
        content::admit(&package, std::path::Path::new(&root))?;
        let package_digest = digest(&package_bytes).map_err(anyhow::Error::msg)?;
        drop(
            persist_engine_artifact_content(
                "execution/checkpoint-packages",
                package_digest.hex(),
                "json",
                &package_bytes,
            )
            .map_err(anyhow::Error::msg)?,
        );
        Ok(serde_json::json!({
            "schema_version":"leanctx.host-checkpoint-package-result/v1",
            "package_digest":package_digest,
            "artifact_digest":package.artifact_digest,
            "checkpoint_digest":package.checkpoint_digest,
            "envelope_schema":package.envelope_schema,
            "receipt_count":package.receipts.len(),
            "evidence_count":package.evidence.len(),
            "package_bytes":package_bytes.len(),
            "package":package,
            "session_adopted":false,
            "source_rights_revalidation_required":true,
        }))
    }

    /// The envelope must be this host's own admitted and signed artifact.
    fn admit_local_envelope(&self, signer: &Signer, signature: &str, payload: &[u8]) -> Result<()> {
        ensure!(
            signer.key_id == self.signer_admission.key_id
                && signer.public_key_digest == self.signer_admission.public_key_digest,
            "checkpoint signer not admitted"
        );
        Ok(self.signing_key.verifying_key().verify_strict(
            payload,
            &Signature::from_slice(&STANDARD.decode(signature)?)?,
        )?)
    }

    fn current_receipt_digest(&self, task: &TaskEnvelopeV1) -> Result<Sha256Digest> {
        let head = self
            .ledger
            .canonical_receipt_for_task_verified(task.task_id.as_str())?
            .ok_or_else(|| anyhow::anyhow!("host receipt missing"))?;
        Ok(Sha256Digest::new(head.receipt_digest)?)
    }

    #[allow(clippy::too_many_arguments)]
    fn build_package(
        &self,
        envelope_schema: &str,
        envelope_text: &str,
        artifact_digest: &Sha256Digest,
        checkpoint_digest: &Sha256Digest,
        lineage: TransferLineage,
        task: &TaskEnvelopeV1,
        plan: &ExecutionPlanV1,
        receipts: &[VerifiedReceiptDocumentV1],
        include_source_evidence: bool,
    ) -> Result<CheckpointTransferPackageV1> {
        let first = receipts
            .first()
            .ok_or_else(|| anyhow::anyhow!("missing receipt"))?;
        let receipt_lineage = &first.document().lineage;
        // The verifier rebuilds the lineage from these bytes, so an exported
        // task or plan that no longer hashes to its signed reference is refused
        // here instead of failing opaquely on the receiving host.
        ensure!(
            digest(&canonical_serialize(task)).map_err(anyhow::Error::msg)?
                == receipt_lineage.task_ref
                && digest(&canonical_serialize(plan)).map_err(anyhow::Error::msg)?
                    == receipt_lineage.plan_ref,
            "exported task or plan disagrees with its signed reference"
        );
        let mut evidence = vec![
            entry(
                &receipt_lineage.task_ref,
                &outcome::read_bytes(&receipt_lineage.task_ref)?,
            )?,
            entry(
                &receipt_lineage.plan_ref,
                &outcome::read_bytes(&receipt_lineage.plan_ref)?,
            )?,
        ];
        let mut receipt_entries = Vec::with_capacity(receipts.len());
        for receipt in receipts {
            receipt_entries.push(entry(
                receipt.canonical_digest(),
                receipt.canonical_bytes(),
            )?);
            if let Some(reference) = receipt.document().outcome.outcome_ref.as_ref() {
                evidence.push(entry(reference, &outcome::read_bytes(reference)?)?);
            }
        }
        if include_source_evidence {
            let sources = source_evidence::derive(receipts, outcome::read_bytes)?;
            for reference in sources.references {
                let hash = Sha256Digest::new(reference)?;
                evidence.push(entry(&hash, &outcome::read_bytes(&hash)?)?);
            }
        }
        ensure!(
            receipt_entries.len() <= MAX_CHECKPOINT_RECEIPTS
                && evidence.len() <= MAX_EVIDENCE_ENTRIES,
            "checkpoint package evidence exceeds transfer bound"
        );
        ensure!(
            receipts
                .iter()
                .any(|receipt| *receipt.canonical_digest() == lineage.current_receipt_digest),
            "checkpoint omits the current receipt"
        );
        Ok(CheckpointTransferPackageV1 {
            schema_version: if include_source_evidence {
                SOURCE_TRANSFER_SCHEMA
            } else {
                TRANSFER_SCHEMA
            }
            .to_owned(),
            envelope_schema: envelope_schema.to_owned(),
            artifact_digest: artifact_digest.clone(),
            envelope_json: envelope_text.to_owned(),
            checkpoint_digest: checkpoint_digest.clone(),
            source_signer: TransferSigner::from(&self.signer_admission),
            lineage,
            receipts: receipt_entries,
            evidence,
        })
    }

    /// Admit one transferred checkpoint against an independently pinned source
    /// and an operator-selected receiving scope.
    pub(crate) fn import_checkpoint(
        &self,
        request: &HostCheckpointImportRequest,
    ) -> Result<serde_json::Value> {
        match self.import_checked(request, false, false)? {
            ImportOutcome::Staged(value) => Ok(value),
            _ => anyhow::bail!("invalid import action"),
        }
    }

    pub(super) fn inspect_checkpoint_sources(
        &self,
        request: &HostCheckpointImportRequest,
    ) -> Result<VerifiedTransferSources> {
        match self.import_checked(request, true, false)? {
            ImportOutcome::Inspected(value) => Ok(value),
            _ => anyhow::bail!("invalid import action"),
        }
    }

    fn import_checked(
        &self,
        request: &HostCheckpointImportRequest,
        inspect_sources: bool,
        continue_copy: bool,
    ) -> Result<ImportOutcome> {
        self.validate_current().map_err(anyhow::Error::msg)?;
        ensure!(request.schema_version == 1, "unsupported import request");
        let admission = self
            .checkpoint_import
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("checkpoint import not authorized"))?;
        ensure!(
            admission.schema_version == 1,
            "unsupported import admission"
        );
        let package = &request.package;
        let package_bytes = canonical_serialize(package);
        ensure!(
            package_bytes.len() <= MAX_PACKAGE_BYTES,
            "checkpoint package exceeds transfer bound"
        );
        ensure!(
            digest(&package_bytes).map_err(anyhow::Error::msg)? == request.package_digest,
            "checkpoint package digest mismatch"
        );
        ensure!(
            matches!(
                package.schema_version.as_str(),
                TRANSFER_SCHEMA | SOURCE_TRANSFER_SCHEMA
            ) && (!inspect_sources || package.schema_version == SOURCE_TRANSFER_SCHEMA),
            "unsupported checkpoint package"
        );
        ensure!(
            matches!(
                package.envelope_schema.as_str(),
                ENVELOPE_SCHEMA_V2 | ENVELOPE_SCHEMA_V3
            ),
            "unsupported checkpoint envelope schema"
        );
        ensure!(
            package.envelope_json.len() <= MAX_ENVELOPE_BYTES
                && !package.envelope_json.chars().any(char::is_control)
                && package.receipts.len() <= MAX_CHECKPOINT_RECEIPTS
                && !package.receipts.is_empty()
                && package.evidence.len() <= MAX_EVIDENCE_ENTRIES,
            "checkpoint package exceeds transfer bound"
        );
        ensure!(
            crate::core::secret_detection::detect_secrets(&package.envelope_json).is_empty(),
            "checkpoint contains credential-shaped material"
        );

        // The pinned source authority is the operator's configuration, never the
        // package.  A stale or revoked pin fails here, before any evidence is read.
        let verifying_key = parse_verifying_key(&admission.source_public_key_hex)?;
        let pinned = admission.source_signer.snapshot();
        // This also rejects a pin whose digest disagrees with its own key, and a
        // pin that is expired or revoked at the current host clock.
        validate_signer_admission(
            &pinned,
            &verifying_key,
            &super::now().map_err(anyhow::Error::msg)?,
        )?;
        ensure!(
            canonical_serialize(&package.source_signer)
                == canonical_serialize(&TransferSigner::from(&pinned)),
            "declared source signer is not the pinned source"
        );

        let root = canonical_receiving_root(&admission.receiving.project_root)?;
        let continue_grant = if continue_copy {
            let grant = self
                .checkpoint_continue
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("copied context use not authorized"))?;
            grant.validate(&request.package_digest)?;
            if let Some(identity) = self
                .admit_task_identity(Some(&root))
                .map_err(anyhow::Error::msg)?
            {
                ensure!(
                    identity.project_id == admission.receiving.project_id
                        && identity.tenant_id == admission.receiving.tenant_id,
                    "copied context receiving scope mismatch"
                );
            }
            Some(grant.clone())
        } else {
            None
        };
        ensure!(
            digest(package.envelope_json.as_bytes()).map_err(anyhow::Error::msg)?
                == package.artifact_digest,
            "checkpoint artifact digest mismatch"
        );
        let envelope_bytes = package.envelope_json.as_bytes().to_vec();
        let evidence = PackagedEvidence::build(&package.evidence)?;
        let receipt_source = PackagedEvidence::build(&package.receipts)?;

        if package.envelope_schema == ENVELOPE_SCHEMA_V3 {
            self.import_unicode(
                package,
                admission,
                &pinned,
                &verifying_key,
                &ImportInputs {
                    envelope_bytes,
                    evidence,
                    receipt_source,
                    root,
                    inspect_sources,
                    continue_grant,
                },
            )
        } else {
            self.import_legacy(
                package,
                admission,
                &pinned,
                &verifying_key,
                &ImportInputs {
                    envelope_bytes,
                    evidence,
                    receipt_source,
                    root,
                    inspect_sources,
                    continue_grant,
                },
            )
        }
    }

    fn import_legacy(
        &self,
        package: &CheckpointTransferPackageV1,
        admission: &CheckpointImportAdmission,
        pinned: &ReceiptSignerAdmissionV1,
        verifying_key: &VerifyingKey,
        inputs: &ImportInputs,
    ) -> Result<ImportOutcome> {
        let envelope: Envelope = serde_json::from_slice(&inputs.envelope_bytes)?;
        ensure!(
            envelope.schema_version == ENVELOPE_SCHEMA_V2
                && canonical_serialize(&envelope) == inputs.envelope_bytes,
            "unsupported or noncanonical checkpoint envelope"
        );
        admit_pinned_envelope(
            &envelope.signer,
            &envelope.signature,
            pinned,
            verifying_key,
            &{
                let mut payload = CONTEXT_CHECKPOINT_V2_SIGNATURE_DOMAIN.to_vec();
                payload.extend(envelope.checkpoint.canonical_bytes()?);
                payload
            },
        )?;
        let checkpoint = &envelope.checkpoint;
        admit_scope(
            admission,
            &checkpoint.lineage.project_id,
            &checkpoint.lineage.tenant_id,
            &checkpoint.lineage.workspace_id,
        )?;
        let (receipts, task, plan, outcomes) = admit_evidence(
            &checkpoint.lineage.artifact_lineage.receipt_refs,
            package,
            pinned,
            verifying_key,
            inputs,
        )?;
        let verified =
            verify_checkpoint_v2(checkpoint.clone(), &task, Some(&plan), &receipts, &outcomes)?;
        ensure!(
            verified.digest()? == package.checkpoint_digest,
            "declared checkpoint digest mismatch"
        );
        self.validate_current().map_err(anyhow::Error::msg)?;
        let lineage = &verified.checkpoint().lineage;
        let continuation = if inputs.continue_grant.is_some() {
            let mut session = super::checkpoint_resume::project_session(
                verified.checkpoint(),
                &inputs.root,
                &inputs.envelope_bytes,
                &package.artifact_digest,
            )?;
            session.canonical_checkpoint = Some(Box::new(
                crate::core::context_checkpoint::CanonicalSession::admitted(
                    &verified,
                    package.artifact_digest.clone(),
                    inputs.root.clone(),
                )?,
            ));
            Some(session)
        } else {
            None
        };
        stage_verified_package(
            package,
            admission,
            inputs,
            &VerifiedScope {
                task_id: lineage.task_id.clone(),
                project_id: lineage.project_id.clone(),
                tenant_id: lineage.tenant_id.clone(),
                workspace_id: lineage.workspace_id.clone(),
            },
            "leanctx.host-checkpoint-import-result/v1",
            &receipts,
            continuation,
        )
    }

    fn import_unicode(
        &self,
        package: &CheckpointTransferPackageV1,
        admission: &CheckpointImportAdmission,
        pinned: &ReceiptSignerAdmissionV1,
        verifying_key: &VerifyingKey,
        inputs: &ImportInputs,
    ) -> Result<ImportOutcome> {
        let envelope: UnicodeEnvelope = serde_json::from_slice(&inputs.envelope_bytes)?;
        ensure!(
            envelope.schema_version == ENVELOPE_SCHEMA_V3
                && canonical_serialize(&envelope) == inputs.envelope_bytes,
            "unsupported or noncanonical Unicode checkpoint envelope"
        );
        admit_pinned_envelope(
            &envelope.signer,
            &envelope.signature,
            pinned,
            verifying_key,
            &{
                let mut payload = CONTEXT_CHECKPOINT_V3_SIGNATURE_DOMAIN.to_vec();
                payload.extend(envelope.checkpoint.canonical_bytes()?);
                payload
            },
        )?;
        let checkpoint = &envelope.checkpoint;
        admit_scope(
            admission,
            &checkpoint.lineage.project_id,
            &checkpoint.lineage.tenant_id,
            &checkpoint.lineage.workspace_id,
        )?;
        let (receipts, task, plan, outcomes) = admit_evidence(
            &checkpoint.lineage.artifact_lineage.receipt_refs,
            package,
            pinned,
            verifying_key,
            inputs,
        )?;
        let verified =
            verify_checkpoint_v3(checkpoint.clone(), &task, Some(&plan), &receipts, &outcomes)?;
        ensure!(
            verified.checkpoint().digest()? == package.checkpoint_digest,
            "declared checkpoint digest mismatch"
        );
        self.validate_current().map_err(anyhow::Error::msg)?;
        let lineage = &verified.checkpoint().lineage;
        let continuation = if inputs.continue_grant.is_some() {
            let mut session = super::checkpoint_resume::project_session_v3(
                verified.checkpoint(),
                &inputs.root,
                &inputs.envelope_bytes,
                &package.artifact_digest,
            )?;
            session.canonical_checkpoint = Some(Box::new(
                crate::core::context_checkpoint::CanonicalSession::admitted_v3(
                    &verified,
                    package.artifact_digest.clone(),
                    inputs.root.clone(),
                )?,
            ));
            Some(session)
        } else {
            None
        };
        stage_verified_package(
            package,
            admission,
            inputs,
            &VerifiedScope {
                task_id: lineage.task_id.clone(),
                project_id: lineage.project_id.clone(),
                tenant_id: lineage.tenant_id.clone(),
                workspace_id: lineage.workspace_id.clone(),
            },
            "leanctx.host-checkpoint-import-result/v2",
            &receipts,
            continuation,
        )
    }
}

/// The scope the signed checkpoint itself binds, taken from the verified value.
struct VerifiedScope {
    task_id: TaskId,
    project_id: ProjectId,
    tenant_id: TenantId,
    workspace_id: WorkspaceId,
}

pub(super) struct VerifiedTransferSources {
    pub(super) root: PathBuf,
    pub(super) package_digest: Sha256Digest,
    pub(super) dependencies: BTreeMap<String, SourceDependency>,
}

enum ImportOutcome {
    Staged(serde_json::Value),
    Inspected(VerifiedTransferSources),
    Continued(serde_json::Value),
}

/// Verify-and-stage.
///
/// Verify current content rules before staging the immutable package. Ordinary
/// imports select no session; explicit personal-copy continuation additionally
/// requires a current host admission and conflict-safe canonical publication.
/// Neither path imports original source rights or execution approval authority.
fn stage_verified_package(
    package: &CheckpointTransferPackageV1,
    admission: &CheckpointImportAdmission,
    inputs: &ImportInputs,
    scope: &VerifiedScope,
    schema_version: &str,
    receipts: &[VerifiedReceiptDocumentV1],
    continuation: Option<crate::core::session::SessionState>,
) -> Result<ImportOutcome> {
    // The declared lineage is operator-facing audit information. It must agree
    // with the verified checkpoint before anything is persisted, so a staged
    // artifact can never misreport the source or a cross-scope transfer.
    ensure!(
        package.lineage.task_id == scope.task_id
            && package.lineage.project_id == scope.project_id
            && package.lineage.tenant_id == scope.tenant_id
            && package.lineage.workspace_id == scope.workspace_id,
        "declared package lineage disagrees with the verified checkpoint"
    );
    let receiving = &admission.receiving;
    content::admit(package, std::path::Path::new(&inputs.root))?;
    if inputs.inspect_sources {
        let sources =
            source_evidence::derive(receipts, |hash| inputs.evidence.canonical_bytes(hash))?;
        return Ok(ImportOutcome::Inspected(VerifiedTransferSources {
            root: PathBuf::from(&inputs.root),
            package_digest: request_digest(package)?,
            dependencies: sources.dependencies,
        }));
    }
    let cross_scope = receiving.project_id != scope.project_id
        || receiving.tenant_id != scope.tenant_id
        || receiving.workspace_id != scope.workspace_id;
    let package_digest = request_digest(package)?;
    let staged = serde_json::json!({
        "schema_version":STAGED_SCHEMA,
        "package_digest":package_digest,
        "package":package,
        "receiving":{
            "project_root":inputs.root,
            "project_id":receiving.project_id,
            "tenant_id":receiving.tenant_id,
            "workspace_id":receiving.workspace_id,
        },
        "verified_source_scope":{
            "task_id":scope.task_id,
            "project_id":scope.project_id,
            "tenant_id":scope.tenant_id,
            "workspace_id":scope.workspace_id,
        },
        "source_key_id":package.source_signer.key_id,
        "cross_scope_adoption":cross_scope,
        "staged":true,
        "canonical_session_adopted":false,
        "legacy_projection_created":false,
        "source_ledger_adopted":false,
        "source_rights_revalidation_required":true,
    });
    let bytes = canonical_serialize(&staged);
    ensure!(
        bytes.len() <= MAX_STAGED_BYTES,
        "staged checkpoint package exceeds transfer bound"
    );
    let staged_digest = digest(&bytes).map_err(anyhow::Error::msg)?;
    drop(
        persist_engine_artifact_content(STAGING_NAMESPACE, staged_digest.hex(), "json", &bytes)
            .map_err(anyhow::Error::msg)?,
    );
    if let Some(session) = continuation {
        let grant = inputs
            .continue_grant
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("copied context use not authorized"))?;
        return continuation::publish(package, &inputs.root, grant, session)
            .map(ImportOutcome::Continued);
    }
    Ok(ImportOutcome::Staged(serde_json::json!({
        "schema_version":schema_version,
        "staged":true,
        "staged_digest":staged_digest,
        "staged_ref":format!("artifact://{STAGING_NAMESPACE}/{}", staged_digest.hex()),
        "package_digest":package_digest,
        "artifact_digest":package.artifact_digest,
        "checkpoint_digest":package.checkpoint_digest,
        "source_key_id":package.source_signer.key_id,
        "source_project_id":scope.project_id,
        "receiving_project_id":receiving.project_id,
        "receiving_tenant_id":receiving.tenant_id,
        "receiving_workspace_id":receiving.workspace_id,
        "cross_scope_adoption":cross_scope,
        "canonical_session_adopted":false,
        "legacy_projection_created":false,
        "active_context_selected":false,
        "source_ledger_adopted":false,
        "source_preserved":true,
        "source_rights_revalidation_required":true,
    })))
}

struct ImportInputs {
    envelope_bytes: Vec<u8>,
    evidence: PackagedEvidence,
    receipt_source: PackagedEvidence,
    root: String,
    inspect_sources: bool,
    continue_grant: Option<CheckpointContinueAdmission>,
}

fn request_digest(package: &CheckpointTransferPackageV1) -> Result<Sha256Digest> {
    digest(&canonical_serialize(package)).map_err(anyhow::Error::msg)
}

fn admit_pinned_envelope(
    signer: &Signer,
    signature: &str,
    pinned: &ReceiptSignerAdmissionV1,
    verifying_key: &VerifyingKey,
    payload: &[u8],
) -> Result<()> {
    ensure!(
        signer.key_id == pinned.key_id && signer.public_key_digest == pinned.public_key_digest,
        "checkpoint signer is not the pinned source"
    );
    Ok(verifying_key.verify_strict(
        payload,
        &Signature::from_slice(&STANDARD.decode(signature)?)?,
    )?)
}

/// The admitted source scope is exact; the receiving scope is separate and a
/// difference must be acknowledged rather than silently discarded.
fn admit_scope(
    admission: &CheckpointImportAdmission,
    project_id: &ProjectId,
    tenant_id: &TenantId,
    workspace_id: &WorkspaceId,
) -> Result<()> {
    let admitted = &admission.admit_source_scope;
    ensure!(
        admitted.project_id == *project_id
            && admitted.tenant_id == *tenant_id
            && admitted.workspace_id == *workspace_id,
        "checkpoint source scope not admitted"
    );
    let receiving = &admission.receiving;
    ensure!(
        admission.allow_cross_scope_adoption
            || (receiving.project_id == *project_id
                && receiving.tenant_id == *tenant_id
                && receiving.workspace_id == *workspace_id),
        "checkpoint receiving scope mismatch"
    );
    Ok(())
}

/// Resolve exactly the evidence set the signed checkpoint binds. An omitted,
/// injected or substituted artifact fails closed.
fn admit_evidence(
    receipt_refs: &[Sha256Digest],
    package: &CheckpointTransferPackageV1,
    pinned: &ReceiptSignerAdmissionV1,
    verifying_key: &VerifyingKey,
    inputs: &ImportInputs,
) -> Result<(
    Vec<VerifiedReceiptDocumentV1>,
    TaskEnvelopeV1,
    ExecutionPlanV1,
    Vec<lean_ctx_protocol::AcceptedOutcomeV1>,
)> {
    let mut expected_receipts: Vec<String> = receipt_refs
        .iter()
        .map(|value| value.as_str().to_owned())
        .collect();
    expected_receipts.sort_unstable();
    expected_receipts.dedup();
    ensure!(
        inputs.receipt_source.digests() == expected_receipts,
        "transferred receipt set disagrees with the signed checkpoint"
    );
    let receipts = verify_receipts(receipt_refs, pinned, verifying_key, &|hash| {
        inputs.receipt_source.canonical_bytes(hash)
    })?;
    ensure!(
        receipts
            .iter()
            .any(|receipt| *receipt.canonical_digest() == package.lineage.current_receipt_digest),
        "transferred package omits its declared current receipt"
    );
    let first = receipts
        .first()
        .ok_or_else(|| anyhow::anyhow!("missing receipt"))?;
    let mut expected_evidence = vec![
        first.document().lineage.task_ref.as_str().to_owned(),
        first.document().lineage.plan_ref.as_str().to_owned(),
    ];
    for receipt in &receipts {
        if let Some(reference) = receipt.document().outcome.outcome_ref.as_ref() {
            expected_evidence.push(reference.as_str().to_owned());
        }
    }
    if package.schema_version == SOURCE_TRANSFER_SCHEMA {
        expected_evidence.extend(
            source_evidence::derive(&receipts, |hash| inputs.evidence.canonical_bytes(hash))?
                .references,
        );
    }
    expected_evidence.sort_unstable();
    expected_evidence.dedup();
    ensure!(
        inputs.evidence.digests() == expected_evidence,
        "transferred evidence set disagrees with the signed receipts"
    );
    let (task, plan) = read_task_and_plan(first, &inputs.evidence)?;
    let outcomes = reconstruct_outcomes(&plan, &receipts, &inputs.evidence)?;
    Ok((receipts, task, plan, outcomes))
}

fn canonical_receiving_root(configured: &std::path::Path) -> Result<String> {
    ensure!(configured.is_absolute(), "absolute receiving root required");
    let root = std::fs::canonicalize(configured)?;
    ensure!(
        root.is_dir() && !crate::core::pathutil::is_broad_or_unsafe_root(&root),
        "unsafe receiving root"
    );
    Ok(root
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("receiving root is not UTF-8"))?
        .to_owned())
}
