// SPDX-License-Identifier: Apache-2.0
//! Host-admitted reader adapter; the complete signed source remains recoverable.

use anyhow::{Result, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::Signature;
use lean_ctx_protocol::{
    CONTEXT_CHECKPOINT_V2_SIGNATURE_DOMAIN, CONTEXT_CHECKPOINT_V3_SIGNATURE_DOMAIN,
    ContextCheckpointDecisionStatusV1, ContextCheckpointV2, ContextCheckpointV3, ProjectId,
    Sha256Digest, TenantId, WorkspaceId,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use super::{HostReceiptAuthority, canonical_serialize, digest};
use crate::core::{
    context_checkpoint::{verify_checkpoint_v2, verify_checkpoint_v3},
    engine_artifact,
    session::{
        Decision, EvidenceKind, EvidenceRecord, Finding, ProgressEntry, SessionState, TaskInfo,
    },
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HostCheckpointResumeRequest {
    schema_version: u32,
    artifact_digest: Sha256Digest,
}

fn default_canonical_admission() -> bool {
    true
}

/// The operator, never the checkpoint or request, chooses the receiving scope.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CheckpointResumeTarget {
    project_root: PathBuf,
    project_id: ProjectId,
    tenant_id: TenantId,
    workspace_id: WorkspaceId,
    #[serde(default = "default_canonical_admission")]
    canonical: bool,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Envelope {
    pub(super) schema_version: String,
    pub(super) checkpoint: ContextCheckpointV2,
    pub(super) signer: Signer,
    pub(super) signature: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct UnicodeEnvelope {
    pub(super) schema_version: String,
    pub(super) checkpoint: ContextCheckpointV3,
    pub(super) signer: Signer,
    pub(super) signature: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Signer {
    pub(super) key_id: String,
    pub(super) public_key_digest: Sha256Digest,
}

impl HostReceiptAuthority {
    pub(crate) fn resume_checkpoint(
        &self,
        request: &HostCheckpointResumeRequest,
    ) -> Result<serde_json::Value> {
        self.validate_current().map_err(anyhow::Error::msg)?;
        ensure!(request.schema_version == 1, "unsupported resume request");
        let target = self
            .checkpoint_resume
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("checkpoint resume not authorized"))?;
        ensure!(
            target.project_root.is_absolute(),
            "absolute receiving root required"
        );
        let root = std::fs::canonicalize(&target.project_root)?;
        ensure!(
            root.is_dir() && !crate::core::pathutil::is_broad_or_unsafe_root(&root),
            "unsafe receiving root"
        );
        let root = root
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("receiving root is not UTF-8"))?;
        let bytes = engine_artifact::read_content(
            "execution/checkpoints",
            request.artifact_digest.hex(),
            "json",
        )
        .map_err(anyhow::Error::msg)?;
        ensure!(
            digest(&bytes).map_err(anyhow::Error::msg)? == request.artifact_digest,
            "checkpoint artifact digest mismatch"
        );
        let envelope_schema = serde_json::from_slice::<serde_json::Value>(&bytes)?
            .get("schema_version")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if envelope_schema == "leanctx.host-checkpoint/v2" {
            return self.resume_unicode_checkpoint(request, target, root, &bytes);
        }
        let envelope: Envelope = serde_json::from_slice(&bytes)?;
        ensure!(
            envelope.schema_version == "leanctx.host-checkpoint/v1"
                && canonical_serialize(&envelope) == bytes,
            "unsupported or noncanonical checkpoint envelope"
        );
        ensure!(
            envelope.signer.key_id == self.signer_admission.key_id
                && envelope.signer.public_key_digest == self.signer_admission.public_key_digest,
            "checkpoint signer not admitted"
        );
        let checkpoint = &envelope.checkpoint;
        ensure!(
            checkpoint.lineage.project_id == target.project_id
                && checkpoint.lineage.tenant_id == target.tenant_id
                && checkpoint.lineage.workspace_id == target.workspace_id,
            "checkpoint receiving scope mismatch"
        );
        let mut payload = CONTEXT_CHECKPOINT_V2_SIGNATURE_DOMAIN.to_vec();
        payload.extend(checkpoint.canonical_bytes()?);
        self.signing_key.verifying_key().verify_strict(
            &payload,
            &Signature::from_slice(&STANDARD.decode(&envelope.signature)?)?,
        )?;
        self.with_checkpoint_lineage(&checkpoint.lineage.artifact_lineage.receipt_refs, |task, plan, receipts, outcomes| {
            let verified = verify_checkpoint_v2(checkpoint.clone(), task, Some(plan), receipts, outcomes)?;
            self.validate_current().map_err(anyhow::Error::msg)?;
            let mut session = project_session(verified.checkpoint(), root, &bytes, &request.artifact_digest)?;
            if target.canonical {
                session.canonical_checkpoint = Some(Box::new(crate::core::context_checkpoint::CanonicalSession::admitted(
                    &verified, request.artifact_digest.clone(), root.into(),
                )?));
            }
            session.save_new().map_err(|error| anyhow::anyhow!("{error}; recovery candidate: {}", session.id))?;
            Ok(serde_json::json!({
                "schema_version":"leanctx.host-checkpoint-resume-result/v1",
                "session_id":session.id, "artifact_digest":request.artifact_digest,
                "checkpoint_digest":verified.digest()?, "legacy_projection_created":true,
                "canonical_session_adopted":target.canonical, "source_preserved":true,
                "projection_only_fields":["task_progress_percentage","accepted_decisions","findings","next_steps"],
                "source_only_fields":["identity","lineage","task_status","decision_status_and_evidence",
                    "exact_progress","files","profile_id","policy_pins","package_pins","session_state","learning_state","carrier","encryption_metadata"]
            }))
        })
    }

    fn resume_unicode_checkpoint(
        &self,
        request: &HostCheckpointResumeRequest,
        target: &CheckpointResumeTarget,
        root: &str,
        bytes: &[u8],
    ) -> Result<serde_json::Value> {
        let envelope: UnicodeEnvelope = serde_json::from_slice(bytes)?;
        ensure!(
            envelope.schema_version == "leanctx.host-checkpoint/v2"
                && canonical_serialize(&envelope) == bytes,
            "unsupported or noncanonical Unicode checkpoint envelope"
        );
        ensure!(
            envelope.signer.key_id == self.signer_admission.key_id
                && envelope.signer.public_key_digest == self.signer_admission.public_key_digest,
            "checkpoint signer not admitted"
        );
        let checkpoint = &envelope.checkpoint;
        ensure!(
            checkpoint.lineage.project_id == target.project_id
                && checkpoint.lineage.tenant_id == target.tenant_id
                && checkpoint.lineage.workspace_id == target.workspace_id,
            "checkpoint receiving scope mismatch"
        );
        let mut payload = CONTEXT_CHECKPOINT_V3_SIGNATURE_DOMAIN.to_vec();
        payload.extend(checkpoint.canonical_bytes()?);
        self.signing_key.verifying_key().verify_strict(
            &payload,
            &Signature::from_slice(&STANDARD.decode(&envelope.signature)?)?,
        )?;
        self.with_checkpoint_lineage(
            &checkpoint.lineage.artifact_lineage.receipt_refs,
            |task, plan, receipts, outcomes| {
                let verified =
                    verify_checkpoint_v3(checkpoint.clone(), task, Some(plan), receipts, outcomes)?;
                self.validate_current().map_err(anyhow::Error::msg)?;
                let mut session = project_session_v3(
                    verified.checkpoint(),
                    root,
                    bytes,
                    &request.artifact_digest,
                )?;
                if target.canonical {
                    session.canonical_checkpoint = Some(Box::new(
                        crate::core::context_checkpoint::CanonicalSession::admitted_v3(
                            &verified,
                            request.artifact_digest.clone(),
                            root.into(),
                        )?,
                    ));
                }
                session.save_new().map_err(|error| {
                    anyhow::anyhow!("{error}; recovery candidate: {}", session.id)
                })?;
                Ok(serde_json::json!({
                    "schema_version":"leanctx.host-checkpoint-resume-result/v2",
                    "session_id":session.id, "artifact_digest":request.artifact_digest,
                    "checkpoint_digest":verified.checkpoint().digest()?,
                    "legacy_projection_created":true,
                    "canonical_session_adopted":target.canonical, "source_preserved":true,
                    "projection_only_fields":["task_progress_percentage","accepted_decisions","findings","next_steps"],
                    "source_only_fields":["identity","lineage","task_status","decision_status_and_evidence",
                        "exact_progress","files","profile_id","policy_pins","package_pins","session_state","learning_state","carrier","encryption_metadata"]
                }))
            },
        )
    }
}

/// A compatibility view is not a demoted checkpoint or an authority for pins.
pub(super) fn project_session(
    checkpoint: &ContextCheckpointV2,
    root: &str,
    source: &[u8],
    source_digest: &Sha256Digest,
) -> Result<SessionState> {
    let mut session = SessionState::new();
    session.project_root = Some(root.into());
    let live = &checkpoint.live_state;
    session.task = Some(TaskInfo {
        description: live.task.title.as_str().into(),
        intent: None,
        progress_pct: if live.progress.total_steps == 0 {
            None
        } else {
            Some(u8::try_from(
                (u64::from(live.progress.completed_steps) * 100)
                    / u64::from(live.progress.total_steps),
            )?)
        },
    });
    let timestamp = session.started_at;
    session.findings = live
        .findings
        .iter()
        .map(|finding| Finding {
            file: None,
            line: None,
            summary: finding.as_str().into(),
            timestamp,
        })
        .collect();
    session.decisions = live
        .decisions
        .iter()
        .filter(|decision| decision.status == ContextCheckpointDecisionStatusV1::Accepted)
        .map(|decision| Decision {
            summary: decision.statement.as_str().into(),
            rationale: Some(decision.rationale.as_str().into()),
            timestamp,
        })
        .collect();
    session.next_steps = live
        .next_steps
        .iter()
        .map(|step| step.as_str().into())
        .collect();
    session.progress.push(ProgressEntry {
        action: "checkpoint_resume".into(),
        detail: Some(live.handoff_summary.as_str().into()),
        timestamp,
    });
    // Retain the complete, exact signed envelope, including fields with no legacy
    // display equivalent. No observation time, path, grant or policy is inferred.
    session.evidence.push(EvidenceRecord {
        kind: EvidenceKind::Manual,
        key: "canonical_checkpoint_source".into(),
        value: Some(
            serde_json::json!({"schema_version":1,"artifact_digest":source_digest,
            "envelope_json":std::str::from_utf8(source)?,"projection_only":true})
            .to_string(),
        ),
        tool: None,
        input_md5: None,
        output_md5: None,
        agent_id: None,
        client_name: None,
        task_id: Some(checkpoint.lineage.task_id.as_str().into()),
        timestamp,
    });
    Ok(session)
}

/// Project Unicode V3 into the same compatibility view while retaining the
/// complete V3 value in the existing canonical session envelope.
pub(super) fn project_session_v3(
    checkpoint: &ContextCheckpointV3,
    root: &str,
    source: &[u8],
    source_digest: &Sha256Digest,
) -> Result<SessionState> {
    let mut session = SessionState::new();
    session.project_root = Some(root.into());
    let live = &checkpoint.live_state;
    session.task = Some(TaskInfo {
        description: live.task.title.as_str().into(),
        intent: None,
        progress_pct: Some(u8::try_from(
            (u64::from(live.progress.completed_steps) * 100) / u64::from(live.progress.total_steps),
        )?),
    });
    let timestamp = session.started_at;
    session.findings = live
        .findings
        .iter()
        .map(|finding| Finding {
            file: None,
            line: None,
            summary: finding.as_str().into(),
            timestamp,
        })
        .collect();
    session.decisions = live
        .decisions
        .iter()
        .filter(|decision| decision.status == ContextCheckpointDecisionStatusV1::Accepted)
        .map(|decision| Decision {
            summary: decision.statement.as_str().into(),
            rationale: Some(decision.rationale.as_str().into()),
            timestamp,
        })
        .collect();
    session.next_steps = live
        .next_steps
        .iter()
        .map(|step| step.as_str().into())
        .collect();
    session.progress.push(ProgressEntry {
        action: "checkpoint_resume".into(),
        detail: Some(live.handoff_summary.as_str().into()),
        timestamp,
    });
    session.evidence.push(EvidenceRecord {
        kind: EvidenceKind::Manual,
        key: "canonical_checkpoint_source".into(),
        value: Some(
            serde_json::json!({"schema_version":2,"artifact_digest":source_digest,
            "envelope_json":std::str::from_utf8(source)?,"projection_only":true})
            .to_string(),
        ),
        tool: None,
        input_md5: None,
        output_md5: None,
        agent_id: None,
        client_name: None,
        task_id: Some(checkpoint.lineage.task_id.as_str().into()),
        timestamp,
    });
    Ok(session)
}
