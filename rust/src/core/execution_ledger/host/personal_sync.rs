// SPDX-License-Identifier: Apache-2.0
//! Explicitly unverified, user-owned historical session projection for personal sync.
//!
//! This wire is not a ContextCheckpoint or execution receipt. It carries portable
//! notes through the existing SessionState store and project-head CAS only.

use anyhow::{Result, ensure};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::core::{
    canonical::canonical_serialize,
    context_checkpoint::PersonalCheckpointLineageV1,
    execution_ledger::host::inspect_personal_sync_payload,
    session::{
        Decision, EvidenceKind, EvidenceRecord, Finding, ProgressEntry, SessionState, TaskInfo,
    },
};

const MAX_PERSONAL_PAYLOAD_BYTES: usize = 512 * 1024;
const PERSONAL_COPY_KEY: &str = "personal_context_copy";
const PERSONAL_LINEAGE_KEY: &str = "personal_context_lineage";
const PERSONAL_AUTHORITY: &str = "historical_personal_copy";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PersonalSyncAdmission {
    schema_version: u32,
    object_key: String,
    expires_at: String,
}

impl PersonalSyncAdmission {
    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(self.schema_version == 1, "invalid personal sync admission");
        let Some(object_suffix) = self.object_key.strip_prefix("checkpoint/") else {
            anyhow::bail!("invalid personal sync object key");
        };
        ensure!(
            is_lower_hex(object_suffix, 32),
            "invalid personal sync object key"
        );
        let expires = chrono::DateTime::parse_from_rfc3339(&self.expires_at)?;
        let now = Utc::now();
        ensure!(
            expires > now && expires <= now + chrono::Duration::minutes(5),
            "personal sync admission expired"
        );
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct PersonalCarrierV1 {
    schema_version: u32,
    content_authority: String,
    task: Option<PersonalTaskV1>,
    findings: Vec<PersonalFindingV1>,
    decisions: Vec<PersonalDecisionV1>,
    progress: Vec<PersonalProgressV1>,
    next_steps: Vec<String>,
    evidence: Vec<PersonalEvidenceV1>,
    /// Structural IDs only; never signer, receipt, source, or approval authority.
    lineage: Option<PersonalCheckpointLineageV1>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct PersonalTaskV1 {
    description: String,
    intent: Option<String>,
    progress_pct: Option<u8>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct PersonalFindingV1 {
    file: Option<String>,
    line: Option<u32>,
    summary: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct PersonalDecisionV1 {
    summary: String,
    rationale: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct PersonalProgressV1 {
    action: String,
    detail: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct PersonalEvidenceV1 {
    key: String,
    value: Option<String>,
}

impl PersonalCarrierV1 {
    fn from_session(session: &SessionState) -> Result<Self> {
        let lineage = if let Some(canonical) = session.canonical_checkpoint.as_deref() {
            Some(canonical.historical_personal_lineage())
        } else {
            let mut markers = session
                .evidence
                .iter()
                .filter(|item| item.key == PERSONAL_LINEAGE_KEY);
            match markers.next() {
                Some(marker) => {
                    ensure!(
                        markers.next().is_none(),
                        "duplicate personal lineage marker"
                    );
                    let value = marker
                        .value
                        .as_deref()
                        .ok_or_else(|| anyhow::anyhow!("personal lineage marker is empty"))?;
                    let lineage: PersonalCheckpointLineageV1 = serde_json::from_str(value)?;
                    lineage.validate()?;
                    Some(lineage)
                }
                None => None,
            }
        };

        let carrier = Self {
            schema_version: 1,
            content_authority: PERSONAL_AUTHORITY.to_owned(),
            task: session.task.as_ref().map(|task| PersonalTaskV1 {
                description: task.description.clone(),
                intent: task.intent.clone(),
                progress_pct: task.progress_pct,
            }),
            findings: session
                .findings
                .iter()
                .map(|finding| PersonalFindingV1 {
                    file: finding.file.clone(),
                    line: finding.line,
                    summary: finding.summary.clone(),
                })
                .collect(),
            decisions: session
                .decisions
                .iter()
                .map(|decision| PersonalDecisionV1 {
                    summary: decision.summary.clone(),
                    rationale: decision.rationale.clone(),
                })
                .collect(),
            progress: session
                .progress
                .iter()
                .map(|entry| PersonalProgressV1 {
                    action: entry.action.clone(),
                    detail: entry.detail.clone(),
                })
                .collect(),
            next_steps: session.next_steps.clone(),
            evidence: session
                .evidence
                .iter()
                .filter(|item| {
                    matches!(&item.kind, EvidenceKind::Manual)
                        && item.key != PERSONAL_COPY_KEY
                        && item.key != PERSONAL_LINEAGE_KEY
                })
                .map(|item| PersonalEvidenceV1 {
                    key: item.key.clone(),
                    value: item.value.clone(),
                })
                .collect(),
            lineage,
        };
        carrier.validate()?;
        Ok(carrier)
    }

    fn validate(&self) -> Result<()> {
        ensure!(
            self.schema_version == 1 && self.content_authority == PERSONAL_AUTHORITY,
            "unsupported personal carrier"
        );
        ensure!(
            self.evidence
                .iter()
                .all(|item| { item.key != PERSONAL_COPY_KEY && item.key != PERSONAL_LINEAGE_KEY }),
            "personal carrier contains a local marker"
        );
        if let Some(lineage) = &self.lineage {
            lineage.validate()?;
        }
        Ok(())
    }

    fn value_and_digest(&self) -> Result<(Value, String)> {
        self.validate()?;
        let value = serde_json::to_value(self)?;
        let bytes = canonical_serialize(&value);
        ensure!(
            bytes.len() <= MAX_PERSONAL_PAYLOAD_BYTES,
            "personal carrier exceeds transfer bound"
        );
        let digest = hex::encode(Sha256::digest(&bytes));
        Ok((value, digest))
    }
}

pub(crate) fn snapshot(admission: &PersonalSyncAdmission, project_root: &str) -> Result<Value> {
    admission.validate()?;
    let Some((session, head)) =
        SessionState::load_project_head_snapshot(project_root).map_err(anyhow::Error::msg)?
    else {
        admission.validate()?;
        return Ok(json!({
            "schema_version":"leanctx.personal-sync/v1",
            "head":null,
            "payload":null,
            "payload_sha256":null
        }));
    };

    let carrier = PersonalCarrierV1::from_session(&session)?;
    let (payload, payload_sha256) = carrier.value_and_digest()?;
    inspect_personal_sync_payload(project_root, &payload)?;
    let current = SessionState::load_project_head_snapshot(project_root)
        .map_err(anyhow::Error::msg)?
        .ok_or_else(|| anyhow::anyhow!("project session head changed; retry"))?;
    ensure!(current.1 == head, "project session head changed; retry");
    inspect_personal_sync_payload(project_root, &payload)?;
    admission.validate()?;
    Ok(json!({
        "schema_version":"leanctx.personal-sync/v1",
        "head":head,
        "payload":payload,
        "payload_sha256":payload_sha256
    }))
}

pub(crate) fn receive(
    admission: &PersonalSyncAdmission,
    project_root: &str,
    expected_head: Option<&str>,
    carrier: &PersonalCarrierV1,
    expected_payload_sha256: &str,
) -> Result<Value> {
    admission.validate()?;
    ensure!(
        expected_head.is_none_or(|head| is_lower_hex(head, 64)),
        "invalid expected project head"
    );
    ensure!(
        is_lower_hex(expected_payload_sha256, 64),
        "invalid personal payload digest"
    );
    let (payload, payload_sha256) = carrier.value_and_digest()?;
    ensure!(
        expected_payload_sha256 == payload_sha256,
        "personal payload digest mismatch"
    );

    // Policy inspection applies to decoded values, whole-document text and the
    // shared secret floor for both local snapshots and received notes.
    inspect_personal_sync_payload(project_root, &payload)?;

    let current =
        SessionState::load_project_head_snapshot(project_root).map_err(anyhow::Error::msg)?;
    let expected_cas = match (&current, expected_head) {
        (None, None) => None,
        (Some((session, head)), Some(expected)) if head == expected => {
            Some((session.id.clone(), format!("sha256:{head}")))
        }
        _ => {
            inspect_personal_sync_payload(project_root, &payload)?;
            admission.validate()?;
            return Ok(conflict());
        }
    };

    if let Some((session, head)) = &current {
        let local = PersonalCarrierV1::from_session(session)?;
        let (_, local_digest) = local.value_and_digest()?;
        if local_digest == payload_sha256
            && session
                .evidence
                .iter()
                .any(|item| item.key == PERSONAL_COPY_KEY)
        {
            inspect_personal_sync_payload(project_root, &payload)?;
            admission.validate()?;
            return Ok(json!({
                "schema_version":"leanctx.personal-sync/v1",
                "received":true,
                "conflict":false,
                "head":head,
                "payload_sha256":payload_sha256
            }));
        }
    }

    let mut received = SessionState::from_unverified_personal_copy(project_root, carrier)?;
    inspect_personal_sync_payload(project_root, &payload)?;
    admission.validate()?;
    let Some(head) = received
        .save_new_if_project_head_with_digest(expected_cas.as_ref())
        .map_err(anyhow::Error::msg)?
    else {
        inspect_personal_sync_payload(project_root, &payload)?;
        admission.validate()?;
        return Ok(conflict());
    };

    inspect_personal_sync_payload(project_root, &payload)?;
    admission.validate()?;
    Ok(json!({
        "schema_version":"leanctx.personal-sync/v1",
        "received":true,
        "conflict":false,
        "head":head,
        "payload_sha256":payload_sha256
    }))
}

impl SessionState {
    /// Create a new live SessionState from user-owned history only.
    /// No canonical verified checkpoint or execution authority is constructed.
    fn from_unverified_personal_copy(
        project_root: &str,
        carrier: &PersonalCarrierV1,
    ) -> Result<Self> {
        carrier.validate()?;
        let now = Utc::now();
        let mut session = SessionState::new();
        session.project_root = Some(project_root.to_owned());
        session.shell_cwd = Some(project_root.to_owned());
        session.task = carrier.task.as_ref().map(|task| TaskInfo {
            description: task.description.clone(),
            intent: task.intent.clone(),
            progress_pct: task.progress_pct,
        });
        session.findings = carrier
            .findings
            .iter()
            .map(|finding| Finding {
                file: finding.file.clone(),
                line: finding.line,
                summary: finding.summary.clone(),
                timestamp: now,
            })
            .collect();
        session.decisions = carrier
            .decisions
            .iter()
            .map(|decision| Decision {
                summary: decision.summary.clone(),
                rationale: decision.rationale.clone(),
                timestamp: now,
            })
            .collect();
        session.progress = carrier
            .progress
            .iter()
            .map(|entry| ProgressEntry {
                action: entry.action.clone(),
                detail: entry.detail.clone(),
                timestamp: now,
            })
            .collect();
        session.next_steps.clone_from(&carrier.next_steps);
        session.evidence = carrier
            .evidence
            .iter()
            .map(|item| EvidenceRecord {
                kind: EvidenceKind::Manual,
                key: item.key.clone(),
                value: item.value.clone(),
                tool: None,
                input_md5: None,
                output_md5: None,
                agent_id: None,
                client_name: None,
                task_id: None,
                timestamp: now,
            })
            .collect();

        if let Some(lineage) = &carrier.lineage {
            session.evidence.push(EvidenceRecord {
                kind: EvidenceKind::Manual,
                key: PERSONAL_LINEAGE_KEY.to_owned(),
                value: Some(serde_json::to_string(lineage)?),
                tool: None,
                input_md5: None,
                output_md5: None,
                agent_id: None,
                client_name: None,
                task_id: None,
                timestamp: now,
            });
        }
        session.evidence.push(EvidenceRecord {
            kind: EvidenceKind::Manual,
            key: PERSONAL_COPY_KEY.to_owned(),
            value: Some(
                "Historical user notes only; not verified facts, execution approvals, or source-access grants."
                    .to_owned(),
            ),
            tool: None,
            input_md5: None,
            output_md5: None,
            agent_id: None,
            client_name: None,
            task_id: None,
            timestamp: now,
        });
        Ok(session)
    }
}

fn conflict() -> Value {
    json!({
        "schema_version":"leanctx.personal-sync/v1",
        "received":false,
        "conflict":true
    })
}

fn is_lower_hex(value: &str, expected_len: usize) -> bool {
    value.len() == expected_len
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn personal_projection_ignores_local_metadata_but_tracks_portable_edits() {
        let mut session = SessionState::new();
        session.task = Some(TaskInfo {
            description: "restore local workspace".into(),
            intent: Some("personal history".into()),
            progress_pct: Some(25),
        });
        session.record_manual_evidence("note", Some("remember this"));
        session.record_manual_evidence(
            PERSONAL_COPY_KEY,
            Some("local marker is deliberately excluded"),
        );

        let first = PersonalCarrierV1::from_session(&session)
            .and_then(|carrier| carrier.value_and_digest())
            .expect("first projection");
        session.updated_at = Utc::now() + chrono::Duration::days(1);
        session.stats.total_tool_calls = 999;
        session.record_manual_evidence(PERSONAL_COPY_KEY, Some("another local marker"));
        let second = PersonalCarrierV1::from_session(&session)
            .and_then(|carrier| carrier.value_and_digest())
            .expect("second projection");
        assert_eq!(first.1, second.1);

        session.task.as_mut().expect("task").description = "portable edit".into();
        let third = PersonalCarrierV1::from_session(&session)
            .and_then(|carrier| carrier.value_and_digest())
            .expect("edited projection");
        assert_ne!(second.1, third.1);
        assert!(third.0.get("intents").is_none());
        assert!(third.0.get("personal_context_copy").is_none());
    }

    #[test]
    fn received_constructor_remains_unverified_and_keeps_copy_banner() {
        let source = SessionState::new();
        let carrier = PersonalCarrierV1::from_session(&source).expect("projection");
        let restored = SessionState::from_unverified_personal_copy("/tmp/project", &carrier)
            .expect("unverified personal copy");
        assert!(restored.canonical_checkpoint.is_none());
        assert!(
            restored
                .evidence
                .iter()
                .any(|item| item.key == PERSONAL_COPY_KEY)
        );
        let restored_digest = PersonalCarrierV1::from_session(&restored)
            .and_then(|carrier| carrier.value_and_digest())
            .expect("restored projection");
        let source_digest = carrier.value_and_digest().expect("source digest");
        assert_eq!(source_digest.1, restored_digest.1);
    }

    #[test]
    fn personal_carrier_rejects_unknown_or_reserved_fields() {
        let unknown = serde_json::from_str::<PersonalCarrierV1>(
            r#"{"schema_version":1,"content_authority":"historical_personal_copy","task":null,"findings":[],"decisions":[],"progress":[],"next_steps":[],"evidence":[],"lineage":null,"source_authorization":true}"#,
        );
        assert!(unknown.is_err());
    }
}
