// SPDX-License-Identifier: Apache-2.0
//! Explicit use of an authorized personal copy; original source rights stay separate.

use anyhow::{Result, ensure};
use chrono::{DateTime, Utc};
use lean_ctx_protocol::{Sha256Digest, UtcTimestamp};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{CheckpointTransferPackageV1, HostCheckpointImportRequest, ImportOutcome};
use crate::core::{
    policy,
    session::{EvidenceKind, EvidenceRecord, SessionState},
};

const COPY_KEY: &str = "personal_context_copy";

/// Operator-owned stdin only. The private client supplies this after current
/// device/lease admission and fetching this exact authorized object revision.
/// It is not a signed server grant or an entitlement token.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::core::execution_ledger::host) struct CheckpointContinueAdmission {
    schema_version: u32,
    package_digest: Sha256Digest,
    object_id: String,
    revision: u64,
    expires_at: UtcTimestamp,
}

impl CheckpointContinueAdmission {
    pub(super) fn validate(&self, expected: &Sha256Digest) -> Result<()> {
        let expires = DateTime::parse_from_rfc3339(self.expires_at.as_str())?;
        let now = Utc::now();
        ensure!(
            self.schema_version == 1
                && &self.package_digest == expected
                && self.revision > 0
                && i64::try_from(self.revision).is_ok()
                && uuid::Uuid::parse_str(&self.object_id).is_ok_and(|id| !id.is_nil())
                && expires > now
                && expires <= now + chrono::Duration::minutes(5),
            "copied context grant invalid or expired"
        );
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CopyMetadata {
    object_id: String,
    revision: u64,
    package_digest: Sha256Digest,
    source_checkpoint_digest: Sha256Digest,
    view_digest: Sha256Digest,
}

fn view_digest(session: &SessionState) -> Result<Sha256Digest> {
    // The canonical checkpoint omits local evidence, intents and diagnostics.
    // Bind the persisted compatibility view too, excluding only this marker
    // and the dirty counter which normal persistence clears on acknowledgement.
    let mut view = session.clone();
    view.evidence.retain(|item| item.key != COPY_KEY);
    view.stats.unsaved_changes = 0;
    super::super::digest(&super::canonical_serialize(&view)).map_err(anyhow::Error::msg)
}

fn metadata(session: &SessionState) -> Option<CopyMetadata> {
    let mut copies = session.evidence.iter().filter(|item| item.key == COPY_KEY);
    let value = copies.next()?.value.as_deref()?;
    if copies.next().is_some() {
        return None;
    }
    serde_json::from_str(value).ok()
}

impl super::super::HostReceiptAuthority {
    pub(crate) fn continue_checkpoint(
        &self,
        request: &HostCheckpointImportRequest,
    ) -> Result<Value> {
        match self.import_checked(request, false, true)? {
            ImportOutcome::Continued(value) => Ok(value),
            _ => anyhow::bail!("invalid continuation action"),
        }
    }
}

fn conflict() -> Value {
    json!({"schema_version":"leanctx.checkpoint-continuation/v1", "continued":false,
        "conflict":true, "source_preserved":true, "local_state_preserved":true,
        "active_context_selected":false, "source_ledger_adopted":false})
}

fn output(
    session: &SessionState,
    grant: &CheckpointContinueAdmission,
    reused: bool,
) -> Result<Value> {
    grant.validate(&grant.package_digest)?;
    let text = session.format_compact();
    let protected = policy::content::protect_active(&text)
        .map_err(|_| anyhow::anyhow!("copied context withheld"))?;
    Ok(
        json!({"schema_version":"leanctx.checkpoint-continuation/v1", "continued":true,
        "conflict":false, "session_id":session.id, "reused":reused,
        "canonical_session_adopted":true, "active_context_selected":true,
        "source_ledger_adopted":false, "source_rights_transferred":false,
        "content_authority":"historical_personal_copy", "context":protected.as_ref()}),
    )
}

pub(super) fn publish(
    package: &CheckpointTransferPackageV1,
    root: &str,
    grant: &CheckpointContinueAdmission,
    mut session: SessionState,
) -> Result<Value> {
    let package_digest = super::request_digest(package)?;
    grant.validate(&package_digest)?;
    policy::runtime::with_project_source_view(root, || -> Result<Value> {
        super::content::admit(package, std::path::Path::new(root))?;
        let expected = SessionState::project_head(root).map_err(anyhow::Error::msg)?;
        if let Some((id, _)) = expected.as_ref() {
            let current = SessionState::load_by_id_for_project_root(id, root)
                .map_err(|_| anyhow::anyhow!("receiving session scope mismatch"))?
                .ok_or_else(|| anyhow::anyhow!("receiving session unavailable"))?;
            let Some(previous) = metadata(&current) else {
                return Ok(conflict());
            };
            if previous.object_id != grant.object_id || previous.revision > grant.revision {
                return Ok(conflict());
            }
            if previous.revision == grant.revision {
                return if previous.package_digest == package_digest {
                    output(&current, grant, true)
                } else {
                    Ok(conflict())
                };
            }
            // Only an unchanged received head may fast-forward. Any local edit
            // creates a canonical child; the foreign revision stays staged.
            let unchanged = current
                .canonical_checkpoint
                .as_ref()
                .is_some_and(|binding| {
                    binding
                        .checkpoint
                        .digest()
                        .is_ok_and(|hash| hash == previous.source_checkpoint_digest)
                });
            if !unchanged || view_digest(&current)? != previous.view_digest {
                return Ok(conflict());
            }
        }
        let initial_view_digest = view_digest(&session)?;
        session.evidence.push(EvidenceRecord {
            kind: EvidenceKind::Manual,
            key: COPY_KEY.into(),
            value: Some(serde_json::to_string(&CopyMetadata {
                object_id: grant.object_id.clone(),
                revision: grant.revision,
                package_digest: package_digest.clone(),
                source_checkpoint_digest: package.checkpoint_digest.clone(),
                view_digest: initial_view_digest,
            })?),
            tool: None,
            input_md5: None,
            output_md5: None,
            agent_id: None,
            client_name: None,
            task_id: Some(package.lineage.task_id.as_str().into()),
            timestamp: session.started_at,
        });
        grant.validate(&package_digest)?;
        super::content::admit(package, std::path::Path::new(root))?;
        if !session
            .save_new_if_project_head(expected.as_ref())
            .map_err(anyhow::Error::msg)?
        {
            return Ok(conflict());
        }
        output(&session, grant, false)
    })
    .map_err(|_| anyhow::anyhow!("copied context policy changed or unavailable"))?
}
