// SPDX-License-Identifier: Apache-2.0

//! Additive Unicode live-state checkpoint contract.
//!
//! `ContextCheckpointV3` is deliberately separate from the deployed V1/V2
//! checkpoint types.  The older contracts remain ASCII-only and retain their
//! existing canonical bytes and signature domains; this module gives the
//! Unicode evolution its own schema and digest domain so a reader cannot
//! silently reinterpret old signed material.

use serde::{Deserialize, Deserializer, Serialize, de::Error as DeError};

use crate::context_checkpoint::{
    ContextCheckpointCarrierBindingV1, ContextCheckpointDecisionStatusV1,
    ContextCheckpointEncryptionMetadataV1, ContextCheckpointFileV1, ContextCheckpointIdentityV1,
    ContextCheckpointLearningStateV1, ContextCheckpointLineageV2, ContextCheckpointPackagePinV1,
    ContextCheckpointPolicyPinV1, ContextCheckpointSessionStateV1, ContextCheckpointTaskStatusV1,
    MAX_CONTEXT_CHECKPOINT_DECISION_REFS, MAX_CONTEXT_CHECKPOINT_DECISIONS,
    MAX_CONTEXT_CHECKPOINT_ENCODED_BYTES, MAX_CONTEXT_CHECKPOINT_FILES,
    MAX_CONTEXT_CHECKPOINT_FINDINGS, MAX_CONTEXT_CHECKPOINT_NEXT_STEPS,
    MAX_CONTEXT_CHECKPOINT_PACKAGE_PINS, MAX_CONTEXT_CHECKPOINT_POLICY_PINS,
    MAX_CONTEXT_CHECKPOINT_STEPS, MAX_CONTEXT_CHECKPOINT_TEXT_BYTES, canonical_bytes_of,
    digest_with_domain, is_unicode_format_character, reject_machine_local_content,
    require_capacity, require_sorted_unique, require_unique, validate_checkpoint_identifier,
    validate_checkpoint_reference,
};
use crate::{
    DecisionId, PlanId, ProtocolReference, SemanticVersion, Sha256Digest, TaskId, UtcTimestamp,
    ValidationError, validate_milliunit,
};

/// Schema identity of the additive Unicode checkpoint contract.
pub const CONTEXT_CHECKPOINT_V3_SCHEMA_ID: &str = "leanctx.context-checkpoint-live/v3";

/// Domain prefix for the complete V3 checkpoint digest.
pub const CONTEXT_CHECKPOINT_V3_DIGEST_DOMAIN: &[u8] = b"leanctx/context-checkpoint/v3\0";

/// Domain prefix reserved for a downstream V3 checkpoint signer.
pub const CONTEXT_CHECKPOINT_V3_SIGNATURE_DOMAIN: &[u8] =
    b"leanctx/context-checkpoint-signature/v3\0";

/// Maximum encoded V3 checkpoint size accepted before JSON parsing/allocation.
pub const MAX_CONTEXT_CHECKPOINT_V3_ENCODED_BYTES: usize = MAX_CONTEXT_CHECKPOINT_ENCODED_BYTES;

macro_rules! validated_deserialize {
    ($target:ident { $($field:ident : $field_type:ty),+ $(,)? }) => {
        impl<'de> Deserialize<'de> for $target {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Wire {
                    $($field: $field_type),+
                }

                let Wire { $($field),+ } = Wire::deserialize(deserializer)?;
                let value = Self { $($field),+ };
                value.validate().map_err(DeError::custom)?;
                Ok(value)
            }
        }
    };
}

/// Bounded human-readable Unicode text used only by V3.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ContextCheckpointTextV2(String);

impl ContextCheckpointTextV2 {
    /// Construct text after applying the portable Unicode security profile.
    pub fn new(value: impl Into<String>) -> Result<Self, ValidationError> {
        let value = value.into();
        validate_unicode_checkpoint_text(&value, "ContextCheckpointTextV2")?;
        Ok(Self(value))
    }

    /// Borrow the validated wire value.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume the value and return its wire representation.
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl AsRef<str> for ContextCheckpointTextV2 {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl std::str::FromStr for ContextCheckpointTextV2 {
    type Err = ValidationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<&str> for ContextCheckpointTextV2 {
    type Error = ValidationError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<String> for ContextCheckpointTextV2 {
    type Error = ValidationError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<ContextCheckpointTextV2> for String {
    fn from(value: ContextCheckpointTextV2) -> Self {
        value.0
    }
}

impl<'de> Deserialize<'de> for ContextCheckpointTextV2 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(DeError::custom)
    }
}

/// The task carried by a V3 checkpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCheckpointTaskV2 {
    /// Identity of the task.
    pub task_id: TaskId,
    /// Bounded human-readable title.
    pub title: ContextCheckpointTextV2,
    /// Lifecycle status.
    pub status: ContextCheckpointTaskStatusV1,
    /// Plan the task executes, when one is pinned.
    pub plan_id: Option<PlanId>,
}

impl ContextCheckpointTaskV2 {
    /// Validate task identity and cross-field bounds.
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_checkpoint_identifier(self.task_id.as_str(), "task.task_id")?;
        if let Some(plan_id) = &self.plan_id {
            validate_checkpoint_identifier(plan_id.as_str(), "task.plan_id")?;
        }
        Ok(())
    }
}

validated_deserialize!(ContextCheckpointTaskV2 {
    task_id: TaskId,
    title: ContextCheckpointTextV2,
    status: ContextCheckpointTaskStatusV1,
    plan_id: Option<PlanId>,
});

/// Bounded progress report for the active task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCheckpointProgressV2 {
    /// Steps finished so far.
    pub completed_steps: u32,
    /// Total steps planned.
    pub total_steps: u32,
    /// Confidence in the reported progress, in 0..=1000 milliunits.
    #[serde(deserialize_with = "deserialize_milliunit")]
    pub confidence_milliunits: u16,
    /// Bounded human-readable summary.
    pub summary: ContextCheckpointTextV2,
}

impl ContextCheckpointProgressV2 {
    /// Validate progress counters and confidence bounds.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.total_steps == 0 || self.total_steps > MAX_CONTEXT_CHECKPOINT_STEPS {
            return Err(ValidationError::new(format!(
                "total_steps must be within 1..={MAX_CONTEXT_CHECKPOINT_STEPS}"
            )));
        }
        if self.completed_steps > self.total_steps {
            return Err(ValidationError::new(
                "completed_steps must not exceed total_steps",
            ));
        }
        validate_milliunit(self.confidence_milliunits, "confidence_milliunits")
    }
}

validated_deserialize!(ContextCheckpointProgressV2 {
    completed_steps: u32,
    total_steps: u32,
    confidence_milliunits: u16,
    summary: ContextCheckpointTextV2,
});

/// One decision taken while working the task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCheckpointDecisionV2 {
    /// Identity of the decision.
    pub decision_id: DecisionId,
    /// Bounded statement of what was decided.
    pub statement: ContextCheckpointTextV2,
    /// Bounded rationale for the decision.
    pub rationale: ContextCheckpointTextV2,
    /// Current status of the decision.
    pub status: ContextCheckpointDecisionStatusV1,
    /// Evidence supporting the decision, sorted and unique.
    pub evidence_refs: Vec<ProtocolReference>,
}

impl ContextCheckpointDecisionV2 {
    /// Validate decision identity, bounds, and reference ordering.
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_checkpoint_identifier(self.decision_id.as_str(), "decision_id")?;
        if self.evidence_refs.len() > MAX_CONTEXT_CHECKPOINT_DECISION_REFS {
            return Err(ValidationError::new(format!(
                "decision evidence_refs exceeds the {MAX_CONTEXT_CHECKPOINT_DECISION_REFS} item limit"
            )));
        }
        for reference in &self.evidence_refs {
            validate_checkpoint_reference(reference.as_str(), "decision evidence_refs")?;
        }
        require_sorted_unique(
            &self.evidence_refs,
            "decision evidence_refs",
            ProtocolReference::as_str,
        )
    }
}

validated_deserialize!(ContextCheckpointDecisionV2 {
    decision_id: DecisionId,
    statement: ContextCheckpointTextV2,
    rationale: ContextCheckpointTextV2,
    status: ContextCheckpointDecisionStatusV1,
    evidence_refs: Vec<ProtocolReference>,
});

/// The portable live state needed to continue a V3 checkpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCheckpointLiveStateV2 {
    /// Schema version of the live-state contract.
    pub schema_version: u32,
    /// Active task.
    pub task: ContextCheckpointTaskV2,
    /// Progress on the active task.
    pub progress: ContextCheckpointProgressV2,
    /// Decisions taken, sorted by decision identity.
    pub decisions: Vec<ContextCheckpointDecisionV2>,
    /// Findings worth carrying forward, in author order and unique.
    pub findings: Vec<ContextCheckpointTextV2>,
    /// Next steps, in execution order and unique.
    pub next_steps: Vec<ContextCheckpointTextV2>,
    /// Bounded handoff summary for the next worker.
    pub handoff_summary: ContextCheckpointTextV2,
    /// Files in scope, sorted by source identity.
    pub files: Vec<ContextCheckpointFileV1>,
    /// Tuning profile in force, when one is pinned.
    pub profile_id: Option<crate::ProfileId>,
    /// Policies pinned, sorted by policy identity.
    pub policy_pins: Vec<ContextCheckpointPolicyPinV1>,
    /// Packages pinned, sorted by package identity.
    pub package_pins: Vec<ContextCheckpointPackagePinV1>,
    /// Relevant portable session state, when one resumes a session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_state: Option<ContextCheckpointSessionStateV1>,
    /// Reference-only learning state, when one is pinned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub learning_state: Option<ContextCheckpointLearningStateV1>,
}

impl ContextCheckpointLiveStateV2 {
    /// Schema version represented by this type.
    pub const SCHEMA_VERSION: u32 = 2;

    /// Validate every live-state bound, ordering rule, and identity.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.schema_version != Self::SCHEMA_VERSION {
            return Err(ValidationError::new(format!(
                "unsupported live-state V2 schema_version {}; expected {}",
                self.schema_version,
                Self::SCHEMA_VERSION
            )));
        }
        self.task.validate()?;
        self.progress.validate()?;

        require_capacity(
            self.decisions.len(),
            MAX_CONTEXT_CHECKPOINT_DECISIONS,
            "decisions",
        )?;
        for decision in &self.decisions {
            decision.validate()?;
        }
        require_sorted_unique(&self.decisions, "decisions", |decision| {
            decision.decision_id.as_str()
        })?;

        require_capacity(
            self.findings.len(),
            MAX_CONTEXT_CHECKPOINT_FINDINGS,
            "findings",
        )?;
        require_unique(&self.findings, "findings", ContextCheckpointTextV2::as_str)?;
        require_capacity(
            self.next_steps.len(),
            MAX_CONTEXT_CHECKPOINT_NEXT_STEPS,
            "next_steps",
        )?;
        require_unique(
            &self.next_steps,
            "next_steps",
            ContextCheckpointTextV2::as_str,
        )?;

        require_capacity(self.files.len(), MAX_CONTEXT_CHECKPOINT_FILES, "files")?;
        for file in &self.files {
            file.validate()?;
        }
        require_sorted_unique(&self.files, "files", |file| file.source_id.as_str())?;

        if let Some(profile_id) = &self.profile_id {
            validate_checkpoint_identifier(profile_id.as_str(), "profile_id")?;
        }
        require_capacity(
            self.policy_pins.len(),
            MAX_CONTEXT_CHECKPOINT_POLICY_PINS,
            "policy_pins",
        )?;
        for pin in &self.policy_pins {
            pin.validate()?;
        }
        require_sorted_unique(&self.policy_pins, "policy_pins", |pin| {
            pin.policy_id.as_str()
        })?;
        require_capacity(
            self.package_pins.len(),
            MAX_CONTEXT_CHECKPOINT_PACKAGE_PINS,
            "package_pins",
        )?;
        for pin in &self.package_pins {
            pin.validate()?;
        }
        require_sorted_unique(&self.package_pins, "package_pins", |pin| {
            pin.package_id.as_str()
        })?;
        if let Some(session_state) = &self.session_state {
            session_state.validate()?;
        }
        if let Some(learning_state) = &self.learning_state {
            learning_state.validate()?;
        }
        Ok(())
    }
}

validated_deserialize!(ContextCheckpointLiveStateV2 {
    schema_version: u32,
    task: ContextCheckpointTaskV2,
    progress: ContextCheckpointProgressV2,
    decisions: Vec<ContextCheckpointDecisionV2>,
    findings: Vec<ContextCheckpointTextV2>,
    next_steps: Vec<ContextCheckpointTextV2>,
    handoff_summary: ContextCheckpointTextV2,
    files: Vec<ContextCheckpointFileV1>,
    profile_id: Option<crate::ProfileId>,
    policy_pins: Vec<ContextCheckpointPolicyPinV1>,
    package_pins: Vec<ContextCheckpointPackagePinV1>,
    session_state: Option<ContextCheckpointSessionStateV1>,
    learning_state: Option<ContextCheckpointLearningStateV1>,
});

/// Canonical typed live-state checkpoint with additive Unicode prose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextCheckpointV3 {
    /// Schema version of the checkpoint contract.
    pub schema_version: u32,
    /// Parent, branch, and device identity.
    pub identity: ContextCheckpointIdentityV1,
    /// Work-graph lineage references and authoritative artifact refs.
    pub lineage: ContextCheckpointLineageV2,
    /// Portable Unicode live state.
    pub live_state: ContextCheckpointLiveStateV2,
    /// Binding to a P6 carrier envelope, when the checkpoint travels in one.
    pub carrier: Option<ContextCheckpointCarrierBindingV1>,
    /// Engine version that produced the checkpoint.
    pub engine_version: SemanticVersion,
    /// Canonical UTC creation timestamp.
    pub created_at: UtcTimestamp,
    /// Canonical UTC timestamp of the latest portable semantic update.
    pub updated_at: UtcTimestamp,
    /// Metadata for an encrypted downstream projection, when present.
    pub encryption_metadata: Option<ContextCheckpointEncryptionMetadataV1>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ContextCheckpointWireV3 {
    schema_version: u32,
    identity: ContextCheckpointIdentityV1,
    lineage: ContextCheckpointLineageV2,
    live_state: ContextCheckpointLiveStateV2,
    carrier: Option<ContextCheckpointCarrierBindingV1>,
    engine_version: SemanticVersion,
    created_at: UtcTimestamp,
    updated_at: UtcTimestamp,
    #[serde(default)]
    encryption_metadata: Option<ContextCheckpointEncryptionMetadataV1>,
}

impl<'de> Deserialize<'de> for ContextCheckpointV3 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = ContextCheckpointWireV3::deserialize(deserializer)?;
        let checkpoint = Self {
            schema_version: wire.schema_version,
            identity: wire.identity,
            lineage: wire.lineage,
            live_state: wire.live_state,
            carrier: wire.carrier,
            engine_version: wire.engine_version,
            created_at: wire.created_at,
            updated_at: wire.updated_at,
            encryption_metadata: wire.encryption_metadata,
        };
        checkpoint.validate().map_err(D::Error::custom)?;
        Ok(checkpoint)
    }
}

impl ContextCheckpointV3 {
    /// Schema version represented by this type.
    pub const SCHEMA_VERSION: u32 = 3;

    /// Schema identity represented by this type.
    pub const SCHEMA_ID: &'static str = CONTEXT_CHECKPOINT_V3_SCHEMA_ID;

    /// Construct a V3 checkpoint from an already validated V2 value.
    ///
    /// This is a structural migration only.  It does not verify artifact
    /// ownership and therefore does not expose a signing payload; production
    /// callers must use the core lineage admission boundary first.
    pub fn from_v2_unverified(
        checkpoint: crate::ContextCheckpointV2,
    ) -> Result<Self, ValidationError> {
        checkpoint.validate()?;
        let old = checkpoint.live_state;
        let live_state = ContextCheckpointLiveStateV2 {
            schema_version: ContextCheckpointLiveStateV2::SCHEMA_VERSION,
            task: ContextCheckpointTaskV2 {
                task_id: old.task.task_id,
                title: ContextCheckpointTextV2::new(old.task.title.into_inner())?,
                status: old.task.status,
                plan_id: old.task.plan_id,
            },
            progress: ContextCheckpointProgressV2 {
                completed_steps: old.progress.completed_steps,
                total_steps: old.progress.total_steps,
                confidence_milliunits: old.progress.confidence_milliunits,
                summary: ContextCheckpointTextV2::new(old.progress.summary.into_inner())?,
            },
            decisions: old
                .decisions
                .into_iter()
                .map(|decision| {
                    Ok(ContextCheckpointDecisionV2 {
                        decision_id: decision.decision_id,
                        statement: ContextCheckpointTextV2::new(decision.statement.into_inner())?,
                        rationale: ContextCheckpointTextV2::new(decision.rationale.into_inner())?,
                        status: decision.status,
                        evidence_refs: decision.evidence_refs,
                    })
                })
                .collect::<Result<_, ValidationError>>()?,
            findings: old
                .findings
                .into_iter()
                .map(|text| ContextCheckpointTextV2::new(text.into_inner()))
                .collect::<Result<_, ValidationError>>()?,
            next_steps: old
                .next_steps
                .into_iter()
                .map(|text| ContextCheckpointTextV2::new(text.into_inner()))
                .collect::<Result<_, ValidationError>>()?,
            handoff_summary: ContextCheckpointTextV2::new(old.handoff_summary.into_inner())?,
            files: old.files,
            profile_id: old.profile_id,
            policy_pins: old.policy_pins,
            package_pins: old.package_pins,
            session_state: old.session_state,
            learning_state: old.learning_state,
        };
        let migrated = Self {
            schema_version: Self::SCHEMA_VERSION,
            identity: checkpoint.identity,
            lineage: checkpoint.lineage,
            live_state,
            carrier: checkpoint.carrier,
            engine_version: checkpoint.engine_version,
            created_at: checkpoint.created_at,
            updated_at: checkpoint.updated_at,
            encryption_metadata: checkpoint.encryption_metadata,
        };
        migrated.validate()?;
        Ok(migrated)
    }

    /// Validate every V3 invariant, including lineage and cross-field bindings.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.schema_version != Self::SCHEMA_VERSION {
            return Err(ValidationError::new(format!(
                "unsupported checkpoint V3 schema_version {}; expected {}",
                self.schema_version,
                Self::SCHEMA_VERSION
            )));
        }
        self.identity.validate()?;
        self.lineage.validate()?;
        self.live_state.validate()?;
        if self.live_state.task.task_id != self.lineage.task_id {
            return Err(ValidationError::new(
                "live_state task_id must equal lineage task_id",
            ));
        }
        if self.live_state.task.plan_id != self.lineage.plan_id {
            return Err(ValidationError::new(
                "live_state plan_id must equal lineage plan_id",
            ));
        }
        if let Some(carrier) = &self.carrier {
            carrier.validate()?;
            if carrier.workspace_id != self.lineage.workspace_id {
                return Err(ValidationError::new(
                    "carrier workspace_id must equal lineage workspace_id",
                ));
            }
        }
        if let Some(encryption_metadata) = &self.encryption_metadata {
            encryption_metadata.validate()?;
        }
        if let Some(session) = &self.live_state.session_state {
            if session.identity.tenant_id.as_ref() != Some(&self.lineage.tenant_id)
                || session.identity.project_id != self.lineage.project_id
                || session.identity.workspace_id.as_ref() != Some(&self.lineage.workspace_id)
                || session.identity.task_id != self.lineage.task_id
            {
                return Err(ValidationError::new(
                    "session identity must match checkpoint tenant, project, workspace, and task",
                ));
            }
            if self.lineage.plan_id.is_none() {
                return Err(ValidationError::new(
                    "session checkpoints require an explicit plan_id",
                ));
            }
            if session.state.active_plan_id != self.lineage.plan_id {
                return Err(ValidationError::new(
                    "session active_plan_id must equal checkpoint lineage plan_id",
                ));
            }
        }
        if self.updated_at < self.created_at {
            return Err(ValidationError::new(
                "updated_at must not precede created_at",
            ));
        }
        Ok(())
    }

    /// Return canonical compact JSON bytes with lexicographically sorted keys.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ValidationError> {
        self.validate()?;
        canonical_bytes_of(self, "checkpoint V3")
    }

    /// Return the canonical JSON encoding as UTF-8 text.
    pub fn canonical_json(&self) -> Result<String, ValidationError> {
        String::from_utf8(self.canonical_bytes()?)
            .map_err(|error| ValidationError::new(format!("checkpoint V3 UTF-8: {error}")))
    }

    /// Decode exactly canonical V3 JSON and validate every invariant.
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self, ValidationError> {
        if bytes.len() > MAX_CONTEXT_CHECKPOINT_V3_ENCODED_BYTES {
            return Err(ValidationError::new(format!(
                "checkpoint V3 exceeds the {MAX_CONTEXT_CHECKPOINT_V3_ENCODED_BYTES} byte encoded limit"
            )));
        }
        let checkpoint = serde_json::from_slice::<Self>(bytes)
            .map_err(|error| ValidationError::new(format!("decode checkpoint V3: {error}")))?;
        if checkpoint.canonical_bytes()? != bytes {
            return Err(ValidationError::new(
                "checkpoint V3 JSON is not canonical UTF-8, compactness or key order",
            ));
        }
        Ok(checkpoint)
    }

    /// Domain-separated content identity of the complete V3 checkpoint.
    pub fn digest(&self) -> Result<Sha256Digest, ValidationError> {
        digest_with_domain(
            CONTEXT_CHECKPOINT_V3_DIGEST_DOMAIN,
            &self.canonical_bytes()?,
        )
    }
}

impl Serialize for ContextCheckpointV3 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.validate().map_err(serde::ser::Error::custom)?;
        #[derive(Serialize)]
        struct Wire<'a> {
            schema_version: u32,
            identity: &'a ContextCheckpointIdentityV1,
            lineage: &'a ContextCheckpointLineageV2,
            live_state: &'a ContextCheckpointLiveStateV2,
            carrier: &'a Option<ContextCheckpointCarrierBindingV1>,
            engine_version: &'a SemanticVersion,
            created_at: &'a UtcTimestamp,
            updated_at: &'a UtcTimestamp,
            #[serde(skip_serializing_if = "Option::is_none")]
            encryption_metadata: &'a Option<ContextCheckpointEncryptionMetadataV1>,
        }
        Wire {
            schema_version: self.schema_version,
            identity: &self.identity,
            lineage: &self.lineage,
            live_state: &self.live_state,
            carrier: &self.carrier,
            engine_version: &self.engine_version,
            created_at: &self.created_at,
            updated_at: &self.updated_at,
            encryption_metadata: &self.encryption_metadata,
        }
        .serialize(serializer)
    }
}

fn validate_unicode_checkpoint_text(value: &str, field: &str) -> Result<(), ValidationError> {
    if value.trim().is_empty() {
        return Err(ValidationError::new(format!("{field} must not be empty")));
    }
    if value.len() > MAX_CONTEXT_CHECKPOINT_TEXT_BYTES {
        return Err(ValidationError::new(format!(
            "{field} exceeds the {MAX_CONTEXT_CHECKPOINT_TEXT_BYTES} byte limit"
        )));
    }
    if value != value.trim() {
        return Err(ValidationError::new(format!(
            "{field} must not have leading or trailing whitespace"
        )));
    }
    if value.chars().any(|character| {
        character.is_control()
            || matches!(character, '\u{2028}' | '\u{2029}')
            || is_unicode_format_character(character)
    }) {
        return Err(ValidationError::new(format!(
            "{field} must not contain controls, line separators, or Unicode format characters"
        )));
    }
    reject_machine_local_content(value, field)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context_checkpoint::{
        CONTEXT_CHECKPOINT_V2_SCHEMA_ID, ContextCheckpointArtifactLineageV1,
        ContextCheckpointBranchIdV1, ContextCheckpointDeviceIdV1, ContextCheckpointIdV1,
        ContextCheckpointTextV1,
    };
    use crate::{ProjectId, TenantId, WorkspaceId};

    fn digest(seed: char) -> Sha256Digest {
        Sha256Digest::new(format!("sha256:{}", seed.to_string().repeat(64))).expect("digest")
    }

    fn fixture() -> ContextCheckpointV3 {
        let identity = ContextCheckpointIdentityV1::try_new(
            ContextCheckpointIdV1::new("11111111-2222-4333-8444-555555555555").expect("id"),
            None,
            None,
            None,
            None,
            ContextCheckpointBranchIdV1::new("main").expect("branch"),
            ContextCheckpointDeviceIdV1::new("device-a").expect("device"),
            1,
        )
        .expect("identity");
        let task_id = TaskId::new("task-a").expect("task");
        let lineage = ContextCheckpointLineageV2 {
            project_id: ProjectId::new("project-a").expect("project"),
            workspace_id: WorkspaceId::new("aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee")
                .expect("workspace"),
            tenant_id: TenantId::new("tenant-a").expect("tenant"),
            task_id: task_id.clone(),
            plan_id: None,
            receipt_ids: Vec::new(),
            artifact_lineage: ContextCheckpointArtifactLineageV1 {
                task_ref: digest('1'),
                plan_ref: None,
                receipt_refs: Vec::new(),
            },
            context_ir_digest: None,
            hosted_index_digest: None,
            evidence_refs: Vec::new(),
            knowledge_refs: Vec::new(),
            gotcha_refs: Vec::new(),
            snapshot_refs: Vec::new(),
        };
        ContextCheckpointV3 {
            schema_version: ContextCheckpointV3::SCHEMA_VERSION,
            identity,
            lineage,
            live_state: ContextCheckpointLiveStateV2 {
                schema_version: ContextCheckpointLiveStateV2::SCHEMA_VERSION,
                task: ContextCheckpointTaskV2 {
                    task_id,
                    title: ContextCheckpointTextV2::new("Résumé").expect("title"),
                    status: ContextCheckpointTaskStatusV1::InProgress,
                    plan_id: None,
                },
                progress: ContextCheckpointProgressV2 {
                    completed_steps: 0,
                    total_steps: 1,
                    confidence_milliunits: 500,
                    summary: ContextCheckpointTextV2::new("Étape prête").expect("summary"),
                },
                decisions: Vec::new(),
                findings: vec![ContextCheckpointTextV2::new("Δ finding").expect("finding")],
                next_steps: vec![ContextCheckpointTextV2::new("Étape suivante").expect("step")],
                handoff_summary: ContextCheckpointTextV2::new("Prêt").expect("handoff"),
                files: Vec::new(),
                profile_id: None,
                policy_pins: Vec::new(),
                package_pins: Vec::new(),
                session_state: None,
                learning_state: None,
            },
            carrier: None,
            engine_version: SemanticVersion::new("4.0.0").expect("version"),
            created_at: UtcTimestamp::new("2026-09-06T12:00:00Z").expect("created"),
            updated_at: UtcTimestamp::new("2026-09-06T12:00:00Z").expect("updated"),
            encryption_metadata: None,
        }
    }

    #[test]
    fn unicode_text_round_trips_without_normalization() {
        let composed = ContextCheckpointTextV2::new("café").expect("composed text");
        let decomposed = ContextCheckpointTextV2::new("cafe\u{301}").expect("decomposed text");
        assert_ne!(composed, decomposed);
        assert_eq!(composed.as_str(), "café");
        assert!(ContextCheckpointTextV1::new("café").is_err());
    }

    #[test]
    fn unicode_text_rejects_controls_paths_and_credentials() {
        for value in [
            "line\nfeed",
            "\u{2028}",
            "\u{200b}",
            "/private/state",
            "token=secret",
        ] {
            assert!(ContextCheckpointTextV2::new(value).is_err(), "{value:?}");
        }
    }

    #[test]
    fn v3_domain_and_schema_are_distinct_from_v2() {
        assert_ne!(
            CONTEXT_CHECKPOINT_V3_SCHEMA_ID,
            CONTEXT_CHECKPOINT_V2_SCHEMA_ID
        );
        assert_ne!(
            CONTEXT_CHECKPOINT_V3_DIGEST_DOMAIN,
            crate::CONTEXT_CHECKPOINT_V2_DIGEST_DOMAIN
        );
        assert_ne!(
            CONTEXT_CHECKPOINT_V3_SIGNATURE_DOMAIN,
            crate::CONTEXT_CHECKPOINT_V2_SIGNATURE_DOMAIN
        );
    }

    #[test]
    fn v3_unicode_fixture_round_trips_and_digest_changes() {
        let checkpoint = fixture();
        let bytes = checkpoint.canonical_bytes().expect("canonical bytes");
        let decoded = ContextCheckpointV3::from_canonical_bytes(&bytes).expect("round trip");
        assert_eq!(decoded, checkpoint);
        assert!(String::from_utf8(bytes).expect("UTF-8").contains("Résumé"));

        let original_digest = checkpoint.digest().expect("digest");
        let mut changed = checkpoint;
        changed.live_state.findings[0] =
            ContextCheckpointTextV2::new("Δ changed").expect("changed");
        assert_ne!(original_digest, changed.digest().expect("changed digest"));
    }

    #[test]
    fn v3_rejects_pretty_escaped_unknown_and_wrong_version_wire() {
        let checkpoint = fixture();
        let bytes = checkpoint.canonical_bytes().expect("canonical bytes");
        assert!(
            ContextCheckpointV3::from_canonical_bytes(
                &serde_json::to_vec_pretty(&checkpoint).expect("pretty")
            )
            .is_err()
        );
        let escaped = String::from_utf8(bytes.clone())
            .expect("UTF-8")
            .replace("Résumé", "R\\u00e9sum\\u00e9");
        assert!(ContextCheckpointV3::from_canonical_bytes(escaped.as_bytes()).is_err());

        let mut unknown: serde_json::Value = serde_json::from_slice(&bytes).expect("JSON");
        unknown["unknown"] = serde_json::Value::Bool(true);
        assert!(
            ContextCheckpointV3::from_canonical_bytes(
                &serde_json::to_vec(&unknown).expect("unknown")
            )
            .is_err()
        );

        let mut wrong_version: serde_json::Value = serde_json::from_slice(&bytes).expect("JSON");
        wrong_version["live_state"]["schema_version"] = serde_json::Value::from(1);
        assert!(
            ContextCheckpointV3::from_canonical_bytes(
                &serde_json::to_vec(&wrong_version).expect("wrong version")
            )
            .is_err()
        );
    }
}
