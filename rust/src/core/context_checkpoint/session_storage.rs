// SPDX-License-Identifier: Apache-2.0
//! Versioned canonical continuation at the existing session persistence boundary.

use crate::core::session::SessionState;
use anyhow::{Result, ensure};
use lean_ctx_protocol::{
    ContextCheckpointDecisionStatusV1, ContextCheckpointDecisionV1, ContextCheckpointDecisionV2,
    ContextCheckpointDeviceIdV1, ContextCheckpointIdV1, ContextCheckpointLiveStateV1,
    ContextCheckpointLiveStateV2, ContextCheckpointProgressV1, ContextCheckpointTaskV1,
    ContextCheckpointTextV1, ContextCheckpointTextV2, ContextCheckpointV2, ContextCheckpointV3,
    DecisionId, Sha256Digest, UtcTimestamp, ValidationError,
};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as DeError};
use serde_json::value::RawValue;

const SCHEMA: &str = "leanctx.session-checkpoint/v1";
const NO_RATIONALE: &str = "No rationale supplied";
const MAX_BYTES: usize = 4 * 1024 * 1024;

/// One canonical checkpoint value carried by the existing session envelope.
///
/// The enum is an internal version dispatch only. It is serialized as the
/// selected checkpoint object itself, never as an externally tagged enum.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CanonicalCheckpoint {
    V2(ContextCheckpointV2),
    V3(ContextCheckpointV3),
}

impl CanonicalCheckpoint {
    pub(crate) fn as_v2(&self) -> Option<&ContextCheckpointV2> {
        match self {
            Self::V2(checkpoint) => Some(checkpoint),
            Self::V3(_) => None,
        }
    }

    pub(crate) fn as_v3(&self) -> Option<&ContextCheckpointV3> {
        match self {
            Self::V2(_) => None,
            Self::V3(checkpoint) => Some(checkpoint),
        }
    }

    pub(crate) fn validate(&self) -> Result<(), ValidationError> {
        match self {
            Self::V2(checkpoint) => checkpoint.validate(),
            Self::V3(checkpoint) => checkpoint.validate(),
        }
    }

    pub(crate) fn digest(&self) -> Result<Sha256Digest, ValidationError> {
        match self {
            Self::V2(checkpoint) => checkpoint.digest(),
            Self::V3(checkpoint) => checkpoint.digest(),
        }
    }

    pub(crate) fn identity(&self) -> &lean_ctx_protocol::ContextCheckpointIdentityV1 {
        match self {
            Self::V2(checkpoint) => &checkpoint.identity,
            Self::V3(checkpoint) => &checkpoint.identity,
        }
    }

    pub(crate) fn identity_mut(&mut self) -> &mut lean_ctx_protocol::ContextCheckpointIdentityV1 {
        match self {
            Self::V2(checkpoint) => &mut checkpoint.identity,
            Self::V3(checkpoint) => &mut checkpoint.identity,
        }
    }

    pub(crate) fn set_updated_at(&mut self, updated_at: UtcTimestamp) {
        match self {
            Self::V2(checkpoint) => checkpoint.updated_at = updated_at,
            Self::V3(checkpoint) => checkpoint.updated_at = updated_at,
        }
    }

    /// Clear protected/carrier metadata after a local plaintext revision.
    pub(crate) fn clear_protected_metadata(&mut self) {
        match self {
            Self::V2(checkpoint) => {
                checkpoint.carrier = None;
                checkpoint.encryption_metadata = None;
            }
            Self::V3(checkpoint) => {
                checkpoint.carrier = None;
                checkpoint.encryption_metadata = None;
            }
        }
    }

    /// Return the live state through the Unicode adapter without establishing
    /// artifact ownership or signing authority.
    pub(crate) fn unicode_live_state(
        &self,
    ) -> Result<ContextCheckpointLiveStateV2, ValidationError> {
        match self {
            Self::V2(checkpoint) => {
                Ok(ContextCheckpointV3::from_v2_unverified(checkpoint.clone())?.live_state)
            }
            Self::V3(checkpoint) => Ok(checkpoint.live_state.clone()),
        }
    }

    /// Replace live state while keeping the stored wire version unchanged.
    /// V2 therefore fails closed if a Unicode edit cannot be represented.
    pub(crate) fn replace_unicode_live_state(
        &mut self,
        live_state: ContextCheckpointLiveStateV2,
    ) -> Result<(), ValidationError> {
        match self {
            Self::V2(checkpoint) => {
                checkpoint.live_state = unicode_live_state_to_v1(live_state)?;
                checkpoint.validate()
            }
            Self::V3(checkpoint) => {
                live_state.validate()?;
                checkpoint.live_state = live_state;
                checkpoint.validate()
            }
        }
    }
}

impl Serialize for CanonicalCheckpoint {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::V2(checkpoint) => checkpoint.serialize(serializer),
            Self::V3(checkpoint) => checkpoint.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for CanonicalCheckpoint {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct SchemaDiscriminator {
            schema_version: u32,
        }

        let raw = Box::<RawValue>::deserialize(deserializer)?;
        let schema_version = serde_json::from_str::<SchemaDiscriminator>(raw.get())
            .map_err(DeError::custom)?
            .schema_version;
        match schema_version {
            2 => serde_json::from_str::<ContextCheckpointV2>(raw.get())
                .map(Self::V2)
                .map_err(DeError::custom),
            3 => serde_json::from_str::<ContextCheckpointV3>(raw.get())
                .map(Self::V3)
                .map_err(DeError::custom),
            _ => Err(DeError::custom(format!(
                "unsupported canonical checkpoint schema_version {schema_version}"
            ))),
        }
    }
}

fn unicode_live_state_to_v1(
    live: ContextCheckpointLiveStateV2,
) -> Result<ContextCheckpointLiveStateV1, ValidationError> {
    Ok(ContextCheckpointLiveStateV1 {
        schema_version: ContextCheckpointLiveStateV1::SCHEMA_VERSION,
        task: ContextCheckpointTaskV1 {
            task_id: live.task.task_id,
            title: ContextCheckpointTextV1::new(live.task.title.into_inner())?,
            status: live.task.status,
            plan_id: live.task.plan_id,
        },
        progress: ContextCheckpointProgressV1 {
            completed_steps: live.progress.completed_steps,
            total_steps: live.progress.total_steps,
            confidence_milliunits: live.progress.confidence_milliunits,
            summary: ContextCheckpointTextV1::new(live.progress.summary.into_inner())?,
        },
        decisions: live
            .decisions
            .into_iter()
            .map(|decision| {
                Ok(ContextCheckpointDecisionV1 {
                    decision_id: decision.decision_id,
                    statement: ContextCheckpointTextV1::new(decision.statement.into_inner())?,
                    rationale: ContextCheckpointTextV1::new(decision.rationale.into_inner())?,
                    status: decision.status,
                    evidence_refs: decision.evidence_refs,
                })
            })
            .collect::<Result<_, ValidationError>>()?,
        findings: live
            .findings
            .into_iter()
            .map(|text| ContextCheckpointTextV1::new(text.into_inner()))
            .collect::<Result<_, ValidationError>>()?,
        next_steps: live
            .next_steps
            .into_iter()
            .map(|text| ContextCheckpointTextV1::new(text.into_inner()))
            .collect::<Result<_, ValidationError>>()?,
        handoff_summary: ContextCheckpointTextV1::new(live.handoff_summary.into_inner())?,
        files: live.files,
        profile_id: live.profile_id,
        policy_pins: live.policy_pins,
        package_pins: live.package_pins,
        session_state: live.session_state,
        learning_state: live.learning_state,
    })
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CanonicalSession {
    /// Exact committed primary bytes observed by this reader; not a wire field.
    #[serde(skip)]
    pub(crate) storage_digest: Option<Sha256Digest>,
    pub(crate) checkpoint: CanonicalCheckpoint,
    source_artifact_digest: Sha256Digest,
    local_device: ContextCheckpointDeviceIdV1,
    project_root: String,
}

/// Opaque canonical checkpoint identity retained as personal history only.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct PersonalCheckpointLineageV1 {
    pub(crate) checkpoint_id: String,
    pub(crate) parent_checkpoint_id: Option<String>,
    pub(crate) parent_device_id: Option<String>,
    pub(crate) parent_branch_id: Option<String>,
    pub(crate) branch_id: String,
    pub(crate) device_id: String,
    pub(crate) device_sequence: u64,
}

impl PersonalCheckpointLineageV1 {
    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        let valid = |value: &str| {
            !value.is_empty()
                && value.len() <= 256
                && value.chars().all(|character| !character.is_control())
        };
        anyhow::ensure!(
            valid(&self.checkpoint_id)
                && valid(&self.branch_id)
                && valid(&self.device_id)
                && self.device_sequence > 0
                && self.parent_checkpoint_id.as_deref().is_none_or(valid)
                && self.parent_device_id.as_deref().is_none_or(valid)
                && self.parent_branch_id.as_deref().is_none_or(valid)
                && (self.parent_checkpoint_id.is_some() == self.parent_device_id.is_some())
                && (self.parent_checkpoint_id.is_some() == self.parent_branch_id.is_some()),
            "invalid historical checkpoint identity"
        );
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredSession {
    storage_schema: String,
    version: u32,
    canonical: CanonicalSession,
    checkpoint_digest: Sha256Digest,
    #[serde(default)]
    previous_storage_digest: Option<Sha256Digest>,
    view: SessionState,
}

impl CanonicalSession {
    pub(crate) fn admitted(
        verified: &super::VerifiedContextCheckpointV2,
        source_artifact_digest: Sha256Digest,
        project_root: String,
    ) -> Result<Self> {
        let checkpoint = CanonicalCheckpoint::V2(verified.checkpoint().clone());
        Self::admitted_checkpoint(checkpoint, source_artifact_digest, project_root)
    }

    pub(crate) fn admitted_v3(
        verified: &super::VerifiedContextCheckpointV3,
        source_artifact_digest: Sha256Digest,
        project_root: String,
    ) -> Result<Self> {
        let checkpoint = CanonicalCheckpoint::V3(verified.checkpoint().clone());
        Self::admitted_checkpoint(checkpoint, source_artifact_digest, project_root)
    }

    /// Keep structural identity as user history without verifying or adopting it.
    pub(crate) fn historical_personal_lineage(&self) -> PersonalCheckpointLineageV1 {
        let identity = self.checkpoint.identity();
        PersonalCheckpointLineageV1 {
            checkpoint_id: identity.checkpoint_id.as_str().to_owned(),
            parent_checkpoint_id: identity
                .parent_checkpoint_id
                .as_ref()
                .map(|value| value.as_str().to_owned()),
            parent_device_id: identity
                .parent_device_id
                .as_ref()
                .map(|value| value.as_str().to_owned()),
            parent_branch_id: identity
                .parent_branch_id
                .as_ref()
                .map(|value| value.as_str().to_owned()),
            branch_id: identity.branch_id.as_str().to_owned(),
            device_id: identity.device_id.as_str().to_owned(),
            device_sequence: identity.device_sequence,
        }
    }

    fn admitted_checkpoint(
        checkpoint: CanonicalCheckpoint,
        source_artifact_digest: Sha256Digest,
        project_root: String,
    ) -> Result<Self> {
        checkpoint.validate()?;
        Ok(Self {
            storage_digest: None,
            checkpoint,
            source_artifact_digest,
            local_device: ContextCheckpointDeviceIdV1::new(format!(
                "local-{}",
                uuid::Uuid::new_v4()
            ))?,
            project_root,
        })
    }

    fn synchronize(&self, session: &SessionState) -> Result<CanonicalCheckpoint> {
        ensure!(
            session.project_root.as_ref() == Some(&self.project_root),
            "canonical session scope changed"
        );
        let mut next = self.checkpoint.clone();
        let mut live = next.unicode_live_state()?;
        let task = session
            .task
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("canonical task cannot be cleared"))?;
        live.task.title = ContextCheckpointTextV2::new(task.description.clone())?;
        let current_pct = if live.progress.total_steps == 0 {
            None
        } else {
            Some(u8::try_from(
                u64::from(live.progress.completed_steps) * 100
                    / u64::from(live.progress.total_steps),
            )?)
        };
        if task.progress_pct != current_pct {
            if let Some(percent) = task.progress_pct {
                ensure!(percent <= 100, "invalid progress percentage");
                live.progress.completed_steps = u32::from(percent);
                live.progress.total_steps = 100;
            } else {
                anyhow::bail!("canonical progress cannot be cleared");
            }
        }
        live.findings = session
            .findings
            .iter()
            .map(|finding| ContextCheckpointTextV2::new(finding.summary.clone()))
            .collect::<std::result::Result<_, _>>()?;
        live.next_steps = session
            .next_steps
            .iter()
            .map(|step| ContextCheckpointTextV2::new(step.clone()))
            .collect::<std::result::Result<_, _>>()?;
        let accepted: Vec<_> = live
            .decisions
            .iter()
            .filter(|decision| decision.status == ContextCheckpointDecisionStatusV1::Accepted)
            .collect();
        let decisions_unchanged = accepted.len() == session.decisions.len()
            && accepted
                .iter()
                .zip(&session.decisions)
                .all(|(canonical, view)| {
                    canonical.statement.as_str() == view.summary
                        && canonical.rationale.as_str()
                            == view.rationale.as_deref().unwrap_or(NO_RATIONALE)
                });
        if !decisions_unchanged {
            let mut decisions: Vec<_> = live
                .decisions
                .iter()
                .filter(|decision| decision.status != ContextCheckpointDecisionStatusV1::Accepted)
                .cloned()
                .collect();
            for view in &session.decisions {
                let rationale = view.rationale.as_deref().unwrap_or(NO_RATIONALE);
                let existing = accepted.iter().find(|decision| {
                    decision.statement.as_str() == view.summary
                        && decision.rationale.as_str() == rationale
                });
                decisions.push(if let Some(existing) = existing {
                    (*existing).clone()
                } else {
                    ContextCheckpointDecisionV2 {
                        decision_id: DecisionId::new(format!("decision-{}", uuid::Uuid::new_v4()))?,
                        statement: ContextCheckpointTextV2::new(view.summary.clone())?,
                        rationale: ContextCheckpointTextV2::new(rationale.to_owned())?,
                        status: ContextCheckpointDecisionStatusV1::Accepted,
                        evidence_refs: Vec::new(),
                    }
                });
            }
            decisions.sort_by(|left, right| left.decision_id.cmp(&right.decision_id));
            live.decisions = decisions;
        }
        live.validate()?;
        next.replace_unicode_live_state(live)?;
        next.validate()?;
        Ok(next)
    }

    fn advance(&self, session: &SessionState) -> Result<Self> {
        let mut next = self.synchronize(session)?;
        if next != self.checkpoint {
            let live = next.unicode_live_state()?;
            if let Some(lifecycle) = &live.session_state {
                use lean_ctx_protocol::{ContextSessionPhaseV1, ContextSessionRecoveryStateV1};
                ensure!(
                    !matches!(
                        lifecycle.state.phase,
                        ContextSessionPhaseV1::Closed | ContextSessionPhaseV1::Aborted
                    ) && !matches!(
                        lifecycle.state.recovery_state,
                        ContextSessionRecoveryStateV1::InspectOnly
                            | ContextSessionRecoveryStateV1::Corrupt
                    ),
                    "checkpoint lifecycle refuses local continuation"
                );
            }
            let parent = self.checkpoint.identity();
            *next.identity_mut() = lean_ctx_protocol::ContextCheckpointIdentityV1::try_new(
                ContextCheckpointIdV1::new(uuid::Uuid::new_v4().to_string())?,
                Some(parent.checkpoint_id.clone()),
                Some(parent.device_id.clone()),
                Some(parent.branch_id.clone()),
                Some(parent.device_sequence),
                parent.branch_id.clone(),
                self.local_device.clone(),
                if parent.device_id == self.local_device {
                    parent
                        .device_sequence
                        .checked_add(1)
                        .ok_or_else(|| anyhow::anyhow!("checkpoint sequence exhausted"))?
                } else {
                    1
                },
            )?;
            next.set_updated_at(UtcTimestamp::new(
                chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            )?);
            // Changed plaintext is not the old protected/carrier projection.
            next.clear_protected_metadata();
            next.validate()?;
        }
        Ok(Self {
            checkpoint: next,
            ..self.clone()
        })
    }
}

impl SessionState {
    /// Export only a validated committed checkpoint, never a pending view or legacy projection.
    pub(crate) fn canonical_checkpoint_for_export(
        id: &str,
    ) -> Result<(CanonicalCheckpoint, String), String> {
        let session = Self::load_by_id(id).ok_or("canonical session cannot be loaded")?;
        let binding = session
            .canonical_checkpoint
            .ok_or("session has no canonical checkpoint; use the legacy export path")?;
        Ok((binding.checkpoint, binding.project_root))
    }

    /// Decode the current canonical storage envelope or the historical flat shape.
    /// This is local state, never signer admission or a policy/entitlement grant.
    pub fn from_storage_json(json: &str) -> Result<Self, String> {
        decode(json).map_err(|error| error.to_string())
    }

    pub(crate) fn storage_json(&self) -> Result<(String, Option<Box<CanonicalSession>>), String> {
        let mut view = self.clone();
        view.stats.unsaved_changes = 0;
        let Some(current) = self.canonical_checkpoint.as_ref() else {
            return serde_json::to_string_pretty(&view)
                .map(|json| (json, None))
                .map_err(|error| error.to_string());
        };
        let advanced = current.advance(self).map_err(|error| error.to_string())?;
        let checkpoint_digest = advanced
            .checkpoint
            .digest()
            .map_err(|error| error.to_string())?;
        let stored = StoredSession {
            storage_schema: SCHEMA.into(),
            version: self.version,
            canonical: advanced.clone(),
            checkpoint_digest,
            previous_storage_digest: current.storage_digest.clone(),
            view,
        };
        let json = serde_json::to_string_pretty(&stored).map_err(|error| error.to_string())?;
        if json.len() > MAX_BYTES {
            return Err("canonical session exceeds storage bound".into());
        }
        Ok((json, Some(Box::new(advanced))))
    }
}

fn decode(json: &str) -> Result<SessionState> {
    let value: serde_json::Value = serde_json::from_str(json)?;
    if value.get("storage_schema").is_none() {
        return Ok(serde_json::from_str(json)?);
    }
    ensure!(json.len() <= MAX_BYTES, "session exceeds storage bound");
    let mut stored: StoredSession = serde_json::from_str(json)?;
    ensure!(
        stored.storage_schema == SCHEMA,
        "unsupported session storage schema"
    );
    ensure!(
        stored.version == stored.view.version,
        "session version mismatch"
    );
    ensure!(
        stored.checkpoint_digest == stored.canonical.checkpoint.digest()?,
        "checkpoint digest mismatch"
    );
    ensure!(
        stored.canonical.synchronize(&stored.view)? == stored.canonical.checkpoint,
        "session view disagrees with canonical checkpoint"
    );
    stored.canonical.storage_digest = Some(storage_digest(json.as_bytes())?);
    stored.view.canonical_checkpoint = Some(Box::new(stored.canonical));
    Ok(stored.view)
}

fn storage_digest(bytes: &[u8]) -> Result<Sha256Digest> {
    use sha2::{Digest as _, Sha256};
    Ok(Sha256Digest::new(format!(
        "sha256:{}",
        crate::core::agent_identity::hex_encode(&Sha256::digest(bytes))
    ))?)
}

/// The existing per-session writer calls this while holding its save lock.
/// Final lineage follows the actually committed head, not a deferred snapshot.
pub(crate) fn prepare_commit(candidate: &str, previous: Option<&[u8]>) -> Result<String, String> {
    commit(candidate, previous).map_err(|error| error.to_string())
}

fn commit(candidate: &str, previous: Option<&[u8]>) -> Result<String> {
    let mut incoming = decode(candidate)?;
    let prior = previous
        .map(|bytes| -> Result<SessionState> { decode(std::str::from_utf8(bytes)?) })
        .transpose()?;
    let Some(binding) = incoming.canonical_checkpoint.as_ref() else {
        ensure!(
            prior
                .as_ref()
                .is_none_or(|state| state.canonical_checkpoint.is_none()),
            "refusing canonical session downgrade"
        );
        return Ok(candidate.into());
    };
    let mut stored: StoredSession = serde_json::from_str(candidate)?;
    ensure!(
        stored.previous_storage_digest == previous.map(storage_digest).transpose()?,
        "canonical session changed since it was read; reload before retrying"
    );
    if let Some(prior) = prior {
        ensure!(
            prior.id == incoming.id,
            "canonical session identity mismatch"
        );
        let head = prior
            .canonical_checkpoint
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("canonical adoption requires a fresh session"))?;
        ensure!(
            head.source_artifact_digest == binding.source_artifact_digest
                && head.project_root == binding.project_root
                && head.local_device == binding.local_device,
            "canonical session authority changed"
        );
        stored.canonical = head.advance(&incoming)?;
        ensure!(
            prior.version < incoming.version || stored.canonical.checkpoint == head.checkpoint,
            "conflicting canonical session revision"
        );
        stored.checkpoint_digest = stored.canonical.checkpoint.digest()?;
        if let Some(bytes) = previous {
            let hash = storage_digest(bytes)?;
            drop(
                crate::core::engine_artifact::persist_content(
                    "sessions/checkpoints",
                    hash.hex(),
                    "json",
                    bytes,
                )
                .map_err(anyhow::Error::msg)?,
            );
            stored.previous_storage_digest = Some(hash);
        }
    }
    // The view is an explicitly checked compatibility cache, never a fallback
    // authority when it disagrees with the checkpoint.
    incoming.canonical_checkpoint = None;
    stored.view = incoming;
    let encoded = serde_json::to_string_pretty(&stored)?;
    ensure!(
        encoded.len() <= MAX_BYTES,
        "canonical session exceeds storage bound"
    );
    Ok(encoded)
}
