// SPDX-License-Identifier: Apache-2.0

//! Caller-owned target inputs for each legacy family.

use super::{
    CONTEXT_CHECKPOINT_ADAPTER_SCHEMA_VERSION, ContextCheckpointLegacyCommitV1,
    ContextCheckpointLegacyErrorV1, ContextCheckpointLegacyFieldV1, ContextCheckpointLegacyHashV1,
    ContextCheckpointLegacyLabelV1, ContextCheckpointLegacyTargetV1,
    MAX_CONTEXT_CHECKPOINT_CARRIER_ENVELOPE_BYTES, parse_json_object, require_capacity,
    validate_optional_label, validate_project_identity, validate_sorted_unique_timestamps,
    validate_timestamp,
};
use lean_ctx_protocol::{SemanticVersion, Sha256Digest, UtcTimestamp, WorkspaceId};
use serde::{Deserialize, Deserializer, Serialize, de::Error as DeError};

/// Caller-supplied policy identity; never derived from the checkpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCheckpointLegacyPolicyIdentityV1 {
    /// Deployed policy name (role or tuning profile).
    pub name: ContextCheckpointLegacyLabelV1,
    /// Content hash of the resolved policy document.
    pub policy_digest: ContextCheckpointLegacyHashV1,
}

/// Caller-supplied project identity hashes; the checkpoint carries no paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCheckpointLegacyProjectIdentityV1 {
    /// Hash of the local project root, when the caller resolved one.
    pub project_root_hash: Option<ContextCheckpointLegacyHashV1>,
    /// Hash of the project remote identity, when the caller resolved one.
    pub project_identity_hash: Option<ContextCheckpointLegacyHashV1>,
}

/// Caller-supplied session counters; the checkpoint carries no telemetry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCheckpointLegacySessionCountersV1 {
    pub total_tool_calls: u32,
    pub total_tokens_saved: u64,
    pub total_tokens_input: u64,
    pub cache_hits: u32,
    pub files_read: u32,
    pub commands_run: u32,
    pub intents_inferred: u32,
    pub intents_explicit: u32,
    pub unsaved_changes: u32,
}

/// Caller-supplied git anchor; the checkpoint carries no repository state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCheckpointLegacyGitAnchorV1 {
    pub commit: Option<ContextCheckpointLegacyCommitV1>,
    pub branch: Option<ContextCheckpointLegacyLabelV1>,
    pub dirty: bool,
}

/// Caller-supplied token ROI; the checkpoint carries no ROI state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCheckpointLegacyRoiV1 {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub tokens_saved: u64,
}

/// Caller-supplied ledger totals; the checkpoint carries no ledger state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCheckpointLegacyLedgerTotalsV1 {
    pub window_size: u32,
    pub total_tokens_sent: u32,
    pub total_tokens_saved: u32,
    pub lineage_items_recorded: u64,
}

/// Target-owned inputs for the deployed Session Bundle v1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCheckpointSessionBundleInputsV1 {
    pub exported_at: UtcTimestamp,
    pub project: ContextCheckpointLegacyProjectIdentityV1,
    pub role: ContextCheckpointLegacyPolicyIdentityV1,
    pub profile: ContextCheckpointLegacyPolicyIdentityV1,
    /// Observation time per checkpoint decision, in checkpoint order.
    pub decision_observed_at: Vec<UtcTimestamp>,
    /// Observation time per checkpoint finding, in checkpoint order.
    pub finding_observed_at: Vec<UtcTimestamp>,
    pub stats: ContextCheckpointLegacySessionCountersV1,
    pub compression_level: Option<ContextCheckpointLegacyLabelV1>,
    pub terse_mode: bool,
}

impl ContextCheckpointSessionBundleInputsV1 {
    /// Validate the source-independent input bounds.
    pub fn validate(&self) -> Result<(), ContextCheckpointLegacyErrorV1> {
        require_capacity(
            self.decision_observed_at.len(),
            lean_ctx_protocol::MAX_CONTEXT_CHECKPOINT_DECISIONS,
            ContextCheckpointLegacyFieldV1::DecisionObservedAt,
        )?;
        require_capacity(
            self.finding_observed_at.len(),
            lean_ctx_protocol::MAX_CONTEXT_CHECKPOINT_FINDINGS,
            ContextCheckpointLegacyFieldV1::FindingObservedAt,
        )?;
        validate_sorted_unique_timestamps(
            &self.decision_observed_at,
            ContextCheckpointLegacyFieldV1::DecisionObservedAt,
        )?;
        validate_sorted_unique_timestamps(
            &self.finding_observed_at,
            ContextCheckpointLegacyFieldV1::FindingObservedAt,
        )
    }
}

validated_deserialize!(ContextCheckpointSessionBundleInputsV1 {
    exported_at: UtcTimestamp,
    project: ContextCheckpointLegacyProjectIdentityV1,
    role: ContextCheckpointLegacyPolicyIdentityV1,
    profile: ContextCheckpointLegacyPolicyIdentityV1,
    decision_observed_at: Vec<UtcTimestamp>,
    finding_observed_at: Vec<UtcTimestamp>,
    stats: ContextCheckpointLegacySessionCountersV1,
    compression_level: Option<ContextCheckpointLegacyLabelV1>,
    terse_mode: bool,
});

/// Target-owned inputs for the deployed Handoff Transfer Bundle v1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCheckpointHandoffBundleInputsV1 {
    pub exported_at: UtcTimestamp,
    pub ledger_created_at: UtcTimestamp,
    /// Content hash of the tool manifest the ledger pins.
    pub manifest_digest: ContextCheckpointLegacyHashV1,
    pub project: ContextCheckpointLegacyProjectIdentityV1,
    pub agent_id: Option<ContextCheckpointLegacyLabelV1>,
    pub client_name: Option<ContextCheckpointLegacyLabelV1>,
}

impl ContextCheckpointHandoffBundleInputsV1 {
    /// Validate every caller-owned handoff identity and timestamp.
    pub fn validate(&self) -> Result<(), ContextCheckpointLegacyErrorV1> {
        validate_project_identity(&self.project)?;
        validate_timestamp(&self.exported_at, ContextCheckpointLegacyFieldV1::Payload)?;
        validate_timestamp(
            &self.ledger_created_at,
            ContextCheckpointLegacyFieldV1::Payload,
        )?;
        validate_optional_label(self.agent_id.as_ref())?;
        validate_optional_label(self.client_name.as_ref())
    }
}

validated_deserialize!(ContextCheckpointHandoffBundleInputsV1 {
    exported_at: UtcTimestamp,
    ledger_created_at: UtcTimestamp,
    manifest_digest: ContextCheckpointLegacyHashV1,
    project: ContextCheckpointLegacyProjectIdentityV1,
    agent_id: Option<ContextCheckpointLegacyLabelV1>,
    client_name: Option<ContextCheckpointLegacyLabelV1>,
});

/// Target-owned inputs for the deployed Context Snapshot v1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCheckpointContextSnapshotInputsV1 {
    pub created_at: UtcTimestamp,
    pub lean_ctx_version: SemanticVersion,
    pub project: ContextCheckpointLegacyProjectIdentityV1,
    pub parent_snapshot_id: Option<ContextCheckpointLegacyHashV1>,
    pub git: ContextCheckpointLegacyGitAnchorV1,
    pub roi: ContextCheckpointLegacyRoiV1,
    pub ledger_totals: ContextCheckpointLegacyLedgerTotalsV1,
}

impl ContextCheckpointContextSnapshotInputsV1 {
    /// Validate every caller-owned snapshot anchor and bounded counter.
    pub fn validate(&self) -> Result<(), ContextCheckpointLegacyErrorV1> {
        validate_project_identity(&self.project)?;
        validate_timestamp(&self.created_at, ContextCheckpointLegacyFieldV1::Payload)?;
        if let Some(parent) = &self.parent_snapshot_id {
            ContextCheckpointLegacyHashV1::new(parent.as_str().to_owned())?;
        }
        if let Some(commit) = &self.git.commit {
            ContextCheckpointLegacyCommitV1::new(commit.as_str().to_owned())?;
        }
        if let Some(branch) = &self.git.branch {
            ContextCheckpointLegacyLabelV1::new(branch.as_str().to_owned())?;
        }
        Ok(())
    }
}

validated_deserialize!(ContextCheckpointContextSnapshotInputsV1 {
    created_at: UtcTimestamp,
    lean_ctx_version: SemanticVersion,
    project: ContextCheckpointLegacyProjectIdentityV1,
    parent_snapshot_id: Option<ContextCheckpointLegacyHashV1>,
    git: ContextCheckpointLegacyGitAnchorV1,
    roi: ContextCheckpointLegacyRoiV1,
    ledger_totals: ContextCheckpointLegacyLedgerTotalsV1,
});

/// Target-owned inputs for the deployed `.ctxpkg` carrier envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCheckpointCtxpkgInputsV1 {
    /// Encoded P6 `.ctxpkg` checkpoint carrier envelope, when the caller has one.
    pub carrier_envelope_json: Option<String>,
}

impl ContextCheckpointCtxpkgInputsV1 {
    /// Validate the encoded envelope bound.
    pub fn validate(&self) -> Result<(), ContextCheckpointLegacyErrorV1> {
        if let Some(json) = &self.carrier_envelope_json {
            if json.len() > MAX_CONTEXT_CHECKPOINT_CARRIER_ENVELOPE_BYTES {
                return Err(ContextCheckpointLegacyErrorV1::LegacyCapacityExceeded {
                    field: ContextCheckpointLegacyFieldV1::Payload,
                    limit: MAX_CONTEXT_CHECKPOINT_CARRIER_ENVELOPE_BYTES as u64,
                    actual: json.len() as u64,
                });
            }
            parse_json_object(json)?;
        }
        Ok(())
    }
}

validated_deserialize!(ContextCheckpointCtxpkgInputsV1 {
    carrier_envelope_json: Option<String>,
});

/// Typed target inputs; the variant fixes the target family.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextCheckpointLegacyInputsV1 {
    SessionBundle(ContextCheckpointSessionBundleInputsV1),
    HandoffBundle(ContextCheckpointHandoffBundleInputsV1),
    Ctxpkg(ContextCheckpointCtxpkgInputsV1),
    ContextSnapshot(ContextCheckpointContextSnapshotInputsV1),
}

impl ContextCheckpointLegacyInputsV1 {
    /// The target family this payload belongs to.
    pub const fn target(&self) -> ContextCheckpointLegacyTargetV1 {
        match self {
            Self::SessionBundle(_) => ContextCheckpointLegacyTargetV1::SessionBundle,
            Self::HandoffBundle(_) => ContextCheckpointLegacyTargetV1::HandoffBundle,
            Self::Ctxpkg(_) => ContextCheckpointLegacyTargetV1::Ctxpkg,
            Self::ContextSnapshot(_) => ContextCheckpointLegacyTargetV1::ContextSnapshot,
        }
    }
}

/// One explicit forward adapter request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCheckpointLegacyRequestV1 {
    /// Adapter contract version the caller compiled against.
    pub adapter_schema_version: u32,
    /// Requested target family.
    pub target: ContextCheckpointLegacyTargetV1,
    /// Requested target schema version.
    pub target_schema_version: u32,
    /// Target-owned inputs; the variant must match `target`.
    pub inputs: ContextCheckpointLegacyInputsV1,
}

impl ContextCheckpointLegacyRequestV1 {
    /// Validate the version and target/payload pairing.
    pub fn validate(&self) -> Result<(), ContextCheckpointLegacyErrorV1> {
        if self.adapter_schema_version != CONTEXT_CHECKPOINT_ADAPTER_SCHEMA_VERSION {
            return Err(ContextCheckpointLegacyErrorV1::UnsupportedAdapterVersion {
                actual: self.adapter_schema_version,
                expected: CONTEXT_CHECKPOINT_ADAPTER_SCHEMA_VERSION,
            });
        }
        let inputs_target = self.inputs.target();
        if inputs_target != self.target {
            return Err(ContextCheckpointLegacyErrorV1::TargetInputsMismatch {
                target: self.target,
                inputs_target,
            });
        }
        if !self
            .target
            .accepts_schema_version(self.target_schema_version)
        {
            return Err(ContextCheckpointLegacyErrorV1::UnsupportedTargetVersion {
                target: self.target,
                requested: self.target_schema_version,
                supported: self.target.schema_version(),
            });
        }
        match &self.inputs {
            ContextCheckpointLegacyInputsV1::SessionBundle(inputs) => inputs.validate()?,
            ContextCheckpointLegacyInputsV1::HandoffBundle(inputs) => inputs.validate()?,
            ContextCheckpointLegacyInputsV1::Ctxpkg(inputs) => inputs.validate()?,
            ContextCheckpointLegacyInputsV1::ContextSnapshot(inputs) => inputs.validate()?,
        }
        Ok(())
    }
}

validated_deserialize!(ContextCheckpointLegacyRequestV1 {
    adapter_schema_version: u32,
    target: ContextCheckpointLegacyTargetV1,
    target_schema_version: u32,
    inputs: ContextCheckpointLegacyInputsV1,
});

/// The `.ctxpkg` carrier facts a checkpoint binding admitted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCheckpointCarrierAdmissionV1 {
    /// Workspace identity asserted by both the binding and the envelope.
    pub workspace_id: WorkspaceId,
    /// Carrier logical-state digest asserted by both.
    pub state_digest: Sha256Digest,
    /// Carrier envelope digest asserted by both.
    pub envelope_digest: Sha256Digest,
}
