// SPDX-License-Identifier: Apache-2.0

//! Versioned, fail-closed projection plans for `ContextCheckpointV1`.
//!
//! A plan is intentionally not an adapter.  The deployed Session Bundle,
//! Handoff Bundle, `.ctxpkg`, and Context Snapshot wire owners still require
//! inputs that a checkpoint does not contain (timestamps, policy identity,
//! ledger state, package contents, git/ROI state, and similar material).
//! Returning those plans makes the boundary explicit without fabricating a
//! default identity or silently discarding checkpoint semantics.

use std::collections::BTreeSet;

use serde::{Deserialize, Deserializer, Serialize};

use crate::core::contracts::{
    CCP_SESSION_BUNDLE_V1_SCHEMA_VERSION, CONTEXT_PACKAGE_V2_SCHEMA_VERSION,
    CONTEXT_SNAPSHOT_V1_SCHEMA_VERSION, HANDOFF_TRANSFER_BUNDLE_V1_SCHEMA_VERSION,
};
use lean_ctx_protocol::{
    ContextCheckpointCarrierBindingV1, ContextCheckpointIdentityV1, ContextCheckpointV1,
    ContextSessionRecoveryStateV1,
};

/// Schema version of this projection result contract.
pub const CONTEXT_CHECKPOINT_PROJECTION_SCHEMA_VERSION: u32 = 1;

/// Stable identifier for the deployed Session Bundle target.
pub const SESSION_BUNDLE_TARGET_ID: &str = "leanctx.session-bundle";
/// Stable identifier for the deployed Handoff Bundle target.
pub const HANDOFF_BUNDLE_TARGET_ID: &str = "leanctx.handoff-bundle";
/// Stable identifier for the deployed `.ctxpkg` target.
pub const CTXPKG_TARGET_ID: &str = "leanctx.ctxpkg";
/// Stable identifier for the deployed Context Snapshot target.
pub const CONTEXT_SNAPSHOT_TARGET_ID: &str = "leanctx.context-snapshot";
/// Stable identifier for the checkpoint identity/self target.
pub const CONTEXT_CHECKPOINT_IDENTITY_TARGET_ID: &str = "leanctx.context-checkpoint-identity";

/// The only target shapes covered by this slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextCheckpointProjectionTargetV1 {
    /// Existing core Session Bundle v1.
    SessionBundle,
    /// Existing core Handoff Transfer Bundle v1.
    HandoffBundle,
    /// Existing `.ctxpkg` checkpoint carrier v2.
    Ctxpkg,
    /// Existing core Context Snapshot v1.
    ContextSnapshot,
    /// The checkpoint identity/self subset.
    ContextCheckpointIdentity,
}

impl ContextCheckpointProjectionTargetV1 {
    /// Return targets in canonical output order.
    pub const fn all() -> [Self; 5] {
        [
            Self::SessionBundle,
            Self::HandoffBundle,
            Self::Ctxpkg,
            Self::ContextSnapshot,
            Self::ContextCheckpointIdentity,
        ]
    }

    /// Stable target identifier used in diagnostics and manifests.
    pub const fn target_id(self) -> &'static str {
        match self {
            Self::SessionBundle => SESSION_BUNDLE_TARGET_ID,
            Self::HandoffBundle => HANDOFF_BUNDLE_TARGET_ID,
            Self::Ctxpkg => CTXPKG_TARGET_ID,
            Self::ContextSnapshot => CONTEXT_SNAPSHOT_TARGET_ID,
            Self::ContextCheckpointIdentity => CONTEXT_CHECKPOINT_IDENTITY_TARGET_ID,
        }
    }

    /// Deployed schema version accepted for the target.
    pub const fn schema_version(self) -> u32 {
        match self {
            Self::SessionBundle => CCP_SESSION_BUNDLE_V1_SCHEMA_VERSION,
            Self::HandoffBundle => HANDOFF_TRANSFER_BUNDLE_V1_SCHEMA_VERSION,
            Self::Ctxpkg => CONTEXT_PACKAGE_V2_SCHEMA_VERSION,
            Self::ContextSnapshot => CONTEXT_SNAPSHOT_V1_SCHEMA_VERSION,
            Self::ContextCheckpointIdentity => ContextCheckpointV1::SCHEMA_VERSION,
        }
    }

    const fn is_identity(self) -> bool {
        matches!(self, Self::ContextCheckpointIdentity)
    }
}

/// Target-side input that must be supplied by a downstream wire owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextCheckpointProjectionInputV1 {
    SessionBundleExportedAt,
    SessionBundlePolicyIdentities,
    SessionBundleSessionExcerpt,
    HandoffBundleExportedAt,
    HandoffBundleLedger,
    HandoffBundleProjectIdentity,
    CtxpkgManifest,
    CtxpkgLogicalState,
    CtxpkgIntegrity,
    ContextSnapshotCreatedAt,
    ContextSnapshotGitAnchor,
    ContextSnapshotRoi,
    ContextSnapshotLineage,
    ContextSnapshotLedger,
    ContextSnapshotSession,
}

impl ContextCheckpointProjectionInputV1 {
    const fn target(self) -> ContextCheckpointProjectionTargetV1 {
        match self {
            Self::SessionBundleExportedAt
            | Self::SessionBundlePolicyIdentities
            | Self::SessionBundleSessionExcerpt => {
                ContextCheckpointProjectionTargetV1::SessionBundle
            }
            Self::HandoffBundleExportedAt
            | Self::HandoffBundleLedger
            | Self::HandoffBundleProjectIdentity => {
                ContextCheckpointProjectionTargetV1::HandoffBundle
            }
            Self::CtxpkgManifest | Self::CtxpkgLogicalState | Self::CtxpkgIntegrity => {
                ContextCheckpointProjectionTargetV1::Ctxpkg
            }
            Self::ContextSnapshotCreatedAt
            | Self::ContextSnapshotGitAnchor
            | Self::ContextSnapshotRoi
            | Self::ContextSnapshotLineage
            | Self::ContextSnapshotLedger
            | Self::ContextSnapshotSession => ContextCheckpointProjectionTargetV1::ContextSnapshot,
        }
    }

    const fn field_path(self) -> &'static str {
        match self {
            Self::SessionBundleExportedAt | Self::HandoffBundleExportedAt => "target.exported_at",
            Self::SessionBundlePolicyIdentities => "target.role_profile",
            Self::SessionBundleSessionExcerpt | Self::ContextSnapshotSession => "target.session",
            Self::HandoffBundleLedger | Self::ContextSnapshotLedger => "target.ledger",
            Self::HandoffBundleProjectIdentity => "target.project",
            Self::CtxpkgManifest => "target.manifest",
            Self::CtxpkgLogicalState => "target.logical_state",
            Self::CtxpkgIntegrity => "target.integrity",
            Self::ContextSnapshotCreatedAt => "target.created_at",
            Self::ContextSnapshotGitAnchor => "target.git",
            Self::ContextSnapshotRoi => "target.roi",
            Self::ContextSnapshotLineage => "target.lineage",
        }
    }
}

/// Why one semantic field is not emitted in a target wire payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextCheckpointProjectionLossReasonV1 {
    /// The target has no semantically equivalent field.
    NotRepresentable,
    /// A target field exists, but the required downstream input is absent.
    RequiredInput,
}

/// One structured, canonical loss record.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCheckpointProjectionLossV1 {
    /// Canonical source or target field path.
    pub field_path: String,
    /// Structured reason; no free-form warning text is permitted.
    pub reason: ContextCheckpointProjectionLossReasonV1,
    /// Required input when `reason` is `required_input`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_input: Option<ContextCheckpointProjectionInputV1>,
}

impl<'de> Deserialize<'de> for ContextCheckpointProjectionLossV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            field_path: String,
            reason: ContextCheckpointProjectionLossReasonV1,
            #[serde(default)]
            required_input: Option<ContextCheckpointProjectionInputV1>,
        }

        let raw = Raw::deserialize(deserializer)?;
        let value = Self {
            field_path: raw.field_path,
            reason: raw.reason,
            required_input: raw.required_input,
        };
        validate_loss(&value, None)
            .then_some(value)
            .ok_or_else(|| serde::de::Error::custom("invalid checkpoint projection loss"))
    }
}

impl ContextCheckpointProjectionLossV1 {
    fn new(
        field_path: &'static str,
        reason: ContextCheckpointProjectionLossReasonV1,
        required_input: Option<ContextCheckpointProjectionInputV1>,
    ) -> Self {
        Self {
            field_path: field_path.to_owned(),
            reason,
            required_input,
        }
    }
}

/// Whether the emitted target subset has a proven round trip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextCheckpointProjectionFidelityV1 {
    /// Decode(Encode(subset)) equals the source subset.
    Lossless,
    /// The target is a lossy plan and requires downstream materialization.
    Lossy,
}

/// Typed plan shared by all non-identity target payloads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCheckpointProjectionPlanV1 {
    /// Source checkpoint identity; never default-filled.
    pub source_checkpoint_id: lean_ctx_protocol::ContextCheckpointIdV1,
    /// Source schema version observed by the plan.
    pub source_schema_version: u32,
    /// Inputs that the downstream target owner must supply.
    pub required_inputs: Vec<ContextCheckpointProjectionInputV1>,
    /// Source fields with a direct semantic slot in the target plan.
    pub represented_source_fields: Vec<String>,
}

impl<'de> Deserialize<'de> for ContextCheckpointProjectionPlanV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            source_checkpoint_id: lean_ctx_protocol::ContextCheckpointIdV1,
            source_schema_version: u32,
            required_inputs: Vec<ContextCheckpointProjectionInputV1>,
            represented_source_fields: Vec<String>,
        }

        let raw = Raw::deserialize(deserializer)?;
        let value = Self {
            source_checkpoint_id: raw.source_checkpoint_id,
            source_schema_version: raw.source_schema_version,
            required_inputs: raw.required_inputs,
            represented_source_fields: raw.represented_source_fields,
        };
        validate_plan(&value, None)
            .then_some(value)
            .ok_or_else(|| serde::de::Error::custom("invalid checkpoint projection plan"))
    }
}

/// `.ctxpkg` plan plus the already validated carrier binding, when present.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCheckpointCtxpkgProjectionV1 {
    pub plan: ContextCheckpointProjectionPlanV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub carrier_binding: Option<ContextCheckpointCarrierBindingV1>,
}

impl<'de> Deserialize<'de> for ContextCheckpointCtxpkgProjectionV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            plan: ContextCheckpointProjectionPlanV1,
            #[serde(default)]
            carrier_binding: Option<ContextCheckpointCarrierBindingV1>,
        }

        let raw = Raw::deserialize(deserializer)?;
        let carrier_is_represented = raw.plan.represented_source_fields == ["carrier"];
        (validate_plan(&raw.plan, Some(ContextCheckpointProjectionTargetV1::Ctxpkg))
            && raw.carrier_binding.is_some() == carrier_is_represented)
            .then_some(Self {
                plan: raw.plan,
                carrier_binding: raw.carrier_binding,
            })
            .ok_or_else(|| serde::de::Error::custom("invalid ctxpkg checkpoint projection"))
    }
}

/// Lossless identity/self projection payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCheckpointIdentityProjectionV1 {
    pub identity: ContextCheckpointIdentityV1,
}

impl<'de> Deserialize<'de> for ContextCheckpointIdentityProjectionV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            identity: ContextCheckpointIdentityV1,
        }

        let raw = Raw::deserialize(deserializer)?;
        Ok(Self {
            identity: raw.identity,
        })
    }
}

/// Typed target payload.  Non-identity variants are plans, not wire claims.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContextCheckpointProjectionPayloadV1 {
    SessionBundle(ContextCheckpointProjectionPlanV1),
    HandoffBundle(ContextCheckpointProjectionPlanV1),
    Ctxpkg(ContextCheckpointCtxpkgProjectionV1),
    ContextSnapshot(ContextCheckpointProjectionPlanV1),
    ContextCheckpointIdentity(ContextCheckpointIdentityProjectionV1),
}

/// Versioned result returned by every target projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCheckpointProjectionResultV1 {
    pub projection_schema_version: u32,
    pub target: ContextCheckpointProjectionTargetV1,
    pub target_schema_version: u32,
    pub payload: ContextCheckpointProjectionPayloadV1,
    /// Sorted and unique by `(field_path, reason, required_input)`.
    pub losses: Vec<ContextCheckpointProjectionLossV1>,
    pub fidelity: ContextCheckpointProjectionFidelityV1,
}

impl<'de> Deserialize<'de> for ContextCheckpointProjectionResultV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            projection_schema_version: u32,
            target: ContextCheckpointProjectionTargetV1,
            target_schema_version: u32,
            payload: ContextCheckpointProjectionPayloadV1,
            losses: Vec<ContextCheckpointProjectionLossV1>,
            fidelity: ContextCheckpointProjectionFidelityV1,
        }

        let raw = Raw::deserialize(deserializer)?;
        let value = Self {
            projection_schema_version: raw.projection_schema_version,
            target: raw.target,
            target_schema_version: raw.target_schema_version,
            payload: raw.payload,
            losses: raw.losses,
            fidelity: raw.fidelity,
        };
        validate_result(&value)
            .then_some(value)
            .ok_or_else(|| serde::de::Error::custom("invalid checkpoint projection result"))
    }
}

/// State that this projection slice refuses to reinterpret.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextCheckpointProjectionUnsupportedStateV1 {
    InspectOnly,
    Corrupt,
}

/// Typed fail-closed projection errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ContextCheckpointProjectionErrorV1 {
    UnsupportedSourceVersion {
        actual: u32,
        expected: u32,
    },
    UnsupportedTargetVersion {
        target: ContextCheckpointProjectionTargetV1,
        requested: u32,
        supported: u32,
    },
    InvalidSource,
    UnsupportedState {
        target: ContextCheckpointProjectionTargetV1,
        state: ContextCheckpointProjectionUnsupportedStateV1,
    },
    InvalidProjectionResult,
}

/// Borrowed projection façade over one validated checkpoint.
#[derive(Debug)]
pub struct ContextCheckpointProjections<'a> {
    checkpoint: &'a ContextCheckpointV1,
}

impl<'a> ContextCheckpointProjections<'a> {
    /// Validate the source before exposing the projection façade.
    pub fn new(
        checkpoint: &'a ContextCheckpointV1,
    ) -> Result<Self, ContextCheckpointProjectionErrorV1> {
        validate_source(checkpoint)?;
        Ok(Self { checkpoint })
    }

    /// Project one explicit target/version pair.
    pub fn project(
        &self,
        target: ContextCheckpointProjectionTargetV1,
        target_schema_version: u32,
    ) -> Result<ContextCheckpointProjectionResultV1, ContextCheckpointProjectionErrorV1> {
        validate_source(self.checkpoint)?;
        validate_target_version(target, target_schema_version)?;
        validate_target_state(self.checkpoint, target)?;

        let result = build_result(self.checkpoint, target)?;
        validate_result(&result)
            .then_some(result)
            .ok_or(ContextCheckpointProjectionErrorV1::InvalidProjectionResult)
    }

    /// Project every target in canonical target order.
    pub fn project_all(
        &self,
    ) -> Vec<Result<ContextCheckpointProjectionResultV1, ContextCheckpointProjectionErrorV1>> {
        ContextCheckpointProjectionTargetV1::all()
            .into_iter()
            .map(|target| self.project(target, target.schema_version()))
            .collect()
    }
}

/// Convenience entry point for one projection.
pub fn project_checkpoint(
    checkpoint: &ContextCheckpointV1,
    target: ContextCheckpointProjectionTargetV1,
    target_schema_version: u32,
) -> Result<ContextCheckpointProjectionResultV1, ContextCheckpointProjectionErrorV1> {
    ContextCheckpointProjections::new(checkpoint)?.project(target, target_schema_version)
}

fn validate_source(
    checkpoint: &ContextCheckpointV1,
) -> Result<(), ContextCheckpointProjectionErrorV1> {
    if checkpoint.schema_version != ContextCheckpointV1::SCHEMA_VERSION {
        return Err(
            ContextCheckpointProjectionErrorV1::UnsupportedSourceVersion {
                actual: checkpoint.schema_version,
                expected: ContextCheckpointV1::SCHEMA_VERSION,
            },
        );
    }
    checkpoint
        .validate()
        .map_err(|_| ContextCheckpointProjectionErrorV1::InvalidSource)
}

fn validate_target_version(
    target: ContextCheckpointProjectionTargetV1,
    requested: u32,
) -> Result<(), ContextCheckpointProjectionErrorV1> {
    let supported = target.schema_version();
    if requested != supported {
        return Err(
            ContextCheckpointProjectionErrorV1::UnsupportedTargetVersion {
                target,
                requested,
                supported,
            },
        );
    }
    Ok(())
}

fn validate_target_state(
    checkpoint: &ContextCheckpointV1,
    target: ContextCheckpointProjectionTargetV1,
) -> Result<(), ContextCheckpointProjectionErrorV1> {
    if target.is_identity() {
        return Ok(());
    }
    if let Some(session) = &checkpoint.live_state.session_state {
        let state = match session.state.recovery_state {
            ContextSessionRecoveryStateV1::InspectOnly => {
                Some(ContextCheckpointProjectionUnsupportedStateV1::InspectOnly)
            }
            ContextSessionRecoveryStateV1::Corrupt => {
                Some(ContextCheckpointProjectionUnsupportedStateV1::Corrupt)
            }
            _ => None,
        };
        if let Some(state) = state {
            return Err(ContextCheckpointProjectionErrorV1::UnsupportedState { target, state });
        }
    }
    Ok(())
}

fn build_result(
    checkpoint: &ContextCheckpointV1,
    target: ContextCheckpointProjectionTargetV1,
) -> Result<ContextCheckpointProjectionResultV1, ContextCheckpointProjectionErrorV1> {
    let represented_source_fields = represented_source_fields(target, checkpoint);
    let required_inputs = required_inputs(target);
    let plan = ContextCheckpointProjectionPlanV1 {
        source_checkpoint_id: checkpoint.identity.checkpoint_id.clone(),
        source_schema_version: checkpoint.schema_version,
        required_inputs,
        represented_source_fields,
    };
    let (payload, fidelity) = match target {
        ContextCheckpointProjectionTargetV1::SessionBundle => (
            ContextCheckpointProjectionPayloadV1::SessionBundle(plan),
            ContextCheckpointProjectionFidelityV1::Lossy,
        ),
        ContextCheckpointProjectionTargetV1::HandoffBundle => (
            ContextCheckpointProjectionPayloadV1::HandoffBundle(plan),
            ContextCheckpointProjectionFidelityV1::Lossy,
        ),
        ContextCheckpointProjectionTargetV1::Ctxpkg => (
            ContextCheckpointProjectionPayloadV1::Ctxpkg(ContextCheckpointCtxpkgProjectionV1 {
                plan,
                carrier_binding: checkpoint.carrier.clone(),
            }),
            ContextCheckpointProjectionFidelityV1::Lossy,
        ),
        ContextCheckpointProjectionTargetV1::ContextSnapshot => (
            ContextCheckpointProjectionPayloadV1::ContextSnapshot(plan),
            ContextCheckpointProjectionFidelityV1::Lossy,
        ),
        ContextCheckpointProjectionTargetV1::ContextCheckpointIdentity => {
            let identity = checkpoint.identity.clone();
            // The protocol identity type has strict serde validation.  This
            // round trip is the proof required before claiming losslessness.
            let encoded = serde_json::to_vec(&identity)
                .map_err(|_| ContextCheckpointProjectionErrorV1::InvalidProjectionResult)?;
            let decoded: ContextCheckpointIdentityV1 = serde_json::from_slice(&encoded)
                .map_err(|_| ContextCheckpointProjectionErrorV1::InvalidProjectionResult)?;
            debug_assert_eq!(decoded, identity);
            (
                ContextCheckpointProjectionPayloadV1::ContextCheckpointIdentity(
                    ContextCheckpointIdentityProjectionV1 { identity },
                ),
                ContextCheckpointProjectionFidelityV1::Lossless,
            )
        }
    };

    let losses = canonicalize_losses(losses_for(target, &payload));
    Ok(ContextCheckpointProjectionResultV1 {
        projection_schema_version: CONTEXT_CHECKPOINT_PROJECTION_SCHEMA_VERSION,
        target,
        target_schema_version: target.schema_version(),
        payload,
        losses,
        fidelity,
    })
}

fn required_inputs(
    target: ContextCheckpointProjectionTargetV1,
) -> Vec<ContextCheckpointProjectionInputV1> {
    let mut inputs: Vec<_> = ContextCheckpointProjectionInputV1::all()
        .into_iter()
        .filter(|input| input.target() == target)
        .collect();
    inputs.sort();
    inputs
}

impl ContextCheckpointProjectionInputV1 {
    const fn all() -> [Self; 15] {
        [
            Self::SessionBundleExportedAt,
            Self::SessionBundlePolicyIdentities,
            Self::SessionBundleSessionExcerpt,
            Self::HandoffBundleExportedAt,
            Self::HandoffBundleLedger,
            Self::HandoffBundleProjectIdentity,
            Self::CtxpkgManifest,
            Self::CtxpkgLogicalState,
            Self::CtxpkgIntegrity,
            Self::ContextSnapshotCreatedAt,
            Self::ContextSnapshotGitAnchor,
            Self::ContextSnapshotRoi,
            Self::ContextSnapshotLineage,
            Self::ContextSnapshotLedger,
            Self::ContextSnapshotSession,
        ]
    }
}

const SOURCE_FIELDS: &[&str] = &[
    "carrier",
    "created_at",
    "encryption_metadata",
    "engine_version",
    "identity.branch_id",
    "identity.checkpoint_id",
    "identity.device_id",
    "identity.device_sequence",
    "identity.parent_branch_id",
    "identity.parent_checkpoint_id",
    "identity.parent_device_id",
    "identity.parent_device_sequence",
    "lineage.context_ir_digest",
    "lineage.evidence_refs",
    "lineage.gotcha_refs",
    "lineage.hosted_index_digest",
    "lineage.knowledge_refs",
    "lineage.plan_id",
    "lineage.project_id",
    "lineage.receipt_ids",
    "lineage.snapshot_refs",
    "lineage.task_id",
    "lineage.tenant_id",
    "lineage.workspace_id",
    "live_state.decisions[*].decision_id",
    "live_state.decisions[*].evidence_refs",
    "live_state.decisions[*].rationale",
    "live_state.decisions[*].statement",
    "live_state.decisions[*].status",
    "live_state.files[*].content_digest",
    "live_state.files[*].role",
    "live_state.files[*].source_id",
    "live_state.findings",
    "live_state.handoff_summary",
    "live_state.learning_state",
    "live_state.next_steps",
    "live_state.package_pins",
    "live_state.policy_pins",
    "live_state.profile_id",
    "live_state.progress.completed_steps",
    "live_state.progress.confidence_milliunits",
    "live_state.progress.summary",
    "live_state.progress.total_steps",
    "live_state.schema_version",
    "live_state.session_state",
    "live_state.task.plan_id",
    "live_state.task.status",
    "live_state.task.task_id",
    "live_state.task.title",
    "schema_version",
    "updated_at",
];

fn represented_source_fields(
    target: ContextCheckpointProjectionTargetV1,
    checkpoint: &ContextCheckpointV1,
) -> Vec<String> {
    let mut fields: Vec<&str> = match target {
        ContextCheckpointProjectionTargetV1::SessionBundle => vec![
            "live_state.decisions[*].rationale",
            "live_state.decisions[*].statement",
            "live_state.findings",
            "live_state.next_steps",
            "live_state.progress.summary",
            "live_state.task.title",
        ],
        ContextCheckpointProjectionTargetV1::HandoffBundle => vec![
            "lineage.evidence_refs",
            "live_state.decisions[*].statement",
            "live_state.findings",
            "live_state.next_steps",
            "live_state.task.title",
        ],
        ContextCheckpointProjectionTargetV1::Ctxpkg => {
            if checkpoint.carrier.is_some() {
                vec!["carrier"]
            } else {
                Vec::new()
            }
        }
        ContextCheckpointProjectionTargetV1::ContextSnapshot => vec![
            "live_state.decisions[*].statement",
            "live_state.progress.summary",
            "live_state.task.title",
        ],
        ContextCheckpointProjectionTargetV1::ContextCheckpointIdentity => vec![
            "identity.branch_id",
            "identity.checkpoint_id",
            "identity.device_id",
            "identity.device_sequence",
            "identity.parent_branch_id",
            "identity.parent_checkpoint_id",
            "identity.parent_device_id",
            "identity.parent_device_sequence",
        ],
    };
    fields.sort_unstable();
    fields.dedup();
    fields.into_iter().map(str::to_owned).collect()
}

fn losses_for(
    target: ContextCheckpointProjectionTargetV1,
    payload: &ContextCheckpointProjectionPayloadV1,
) -> Vec<ContextCheckpointProjectionLossV1> {
    let represented: BTreeSet<&str> = match payload {
        ContextCheckpointProjectionPayloadV1::SessionBundle(plan)
        | ContextCheckpointProjectionPayloadV1::HandoffBundle(plan)
        | ContextCheckpointProjectionPayloadV1::ContextSnapshot(plan) => plan
            .represented_source_fields
            .iter()
            .map(String::as_str)
            .collect(),
        ContextCheckpointProjectionPayloadV1::Ctxpkg(projection) => projection
            .plan
            .represented_source_fields
            .iter()
            .map(String::as_str)
            .collect(),
        ContextCheckpointProjectionPayloadV1::ContextCheckpointIdentity(_) => [
            "identity.branch_id",
            "identity.checkpoint_id",
            "identity.device_id",
            "identity.device_sequence",
            "identity.parent_branch_id",
            "identity.parent_checkpoint_id",
            "identity.parent_device_id",
            "identity.parent_device_sequence",
        ]
        .into_iter()
        .collect(),
    };

    let mut losses = SOURCE_FIELDS
        .iter()
        .filter(|field| !represented.contains(*field))
        .map(|field| {
            ContextCheckpointProjectionLossV1::new(
                field,
                ContextCheckpointProjectionLossReasonV1::NotRepresentable,
                None,
            )
        })
        .collect::<Vec<_>>();

    for input in required_inputs(target) {
        losses.push(ContextCheckpointProjectionLossV1::new(
            input.field_path(),
            ContextCheckpointProjectionLossReasonV1::RequiredInput,
            Some(input),
        ));
    }
    losses
}

fn canonicalize_losses(
    mut losses: Vec<ContextCheckpointProjectionLossV1>,
) -> Vec<ContextCheckpointProjectionLossV1> {
    losses.sort();
    losses.dedup();
    losses
}

fn validate_result(result: &ContextCheckpointProjectionResultV1) -> bool {
    let expected_fidelity = if result.target.is_identity() {
        ContextCheckpointProjectionFidelityV1::Lossless
    } else {
        ContextCheckpointProjectionFidelityV1::Lossy
    };
    let payload_matches = match &result.payload {
        ContextCheckpointProjectionPayloadV1::SessionBundle(plan) => validate_plan(
            plan,
            Some(ContextCheckpointProjectionTargetV1::SessionBundle),
        ),
        ContextCheckpointProjectionPayloadV1::HandoffBundle(plan) => validate_plan(
            plan,
            Some(ContextCheckpointProjectionTargetV1::HandoffBundle),
        ),
        ContextCheckpointProjectionPayloadV1::Ctxpkg(projection) => validate_plan(
            &projection.plan,
            Some(ContextCheckpointProjectionTargetV1::Ctxpkg),
        ),
        ContextCheckpointProjectionPayloadV1::ContextSnapshot(plan) => validate_plan(
            plan,
            Some(ContextCheckpointProjectionTargetV1::ContextSnapshot),
        ),
        ContextCheckpointProjectionPayloadV1::ContextCheckpointIdentity(_) => {
            result.target == ContextCheckpointProjectionTargetV1::ContextCheckpointIdentity
        }
    } && payload_target(&result.payload) == result.target;
    let expected_losses = canonicalize_losses(losses_for(result.target, &result.payload));

    result.projection_schema_version == CONTEXT_CHECKPOINT_PROJECTION_SCHEMA_VERSION
        && result.target_schema_version == result.target.schema_version()
        && result.fidelity == expected_fidelity
        && payload_matches
        && result.losses == expected_losses
        && result
            .losses
            .iter()
            .all(|loss| validate_loss(loss, Some(result.target)))
}

fn payload_target(
    payload: &ContextCheckpointProjectionPayloadV1,
) -> ContextCheckpointProjectionTargetV1 {
    match payload {
        ContextCheckpointProjectionPayloadV1::SessionBundle(_) => {
            ContextCheckpointProjectionTargetV1::SessionBundle
        }
        ContextCheckpointProjectionPayloadV1::HandoffBundle(_) => {
            ContextCheckpointProjectionTargetV1::HandoffBundle
        }
        ContextCheckpointProjectionPayloadV1::Ctxpkg(_) => {
            ContextCheckpointProjectionTargetV1::Ctxpkg
        }
        ContextCheckpointProjectionPayloadV1::ContextSnapshot(_) => {
            ContextCheckpointProjectionTargetV1::ContextSnapshot
        }
        ContextCheckpointProjectionPayloadV1::ContextCheckpointIdentity(_) => {
            ContextCheckpointProjectionTargetV1::ContextCheckpointIdentity
        }
    }
}

fn validate_loss(
    loss: &ContextCheckpointProjectionLossV1,
    target: Option<ContextCheckpointProjectionTargetV1>,
) -> bool {
    !loss.field_path.is_empty()
        && loss.field_path.len() <= 128
        && loss.field_path.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'.' | b'_' | b'[' | b']' | b'*')
        })
        && match loss.reason {
            ContextCheckpointProjectionLossReasonV1::NotRepresentable => {
                loss.required_input.is_none()
                    && SOURCE_FIELDS
                        .binary_search(&loss.field_path.as_str())
                        .is_ok()
            }
            ContextCheckpointProjectionLossReasonV1::RequiredInput => {
                loss.required_input.is_some_and(|input| {
                    loss.field_path == input.field_path()
                        && target.is_none_or(|expected| input.target() == expected)
                })
            }
        }
}

fn validate_plan(
    plan: &ContextCheckpointProjectionPlanV1,
    expected_target: Option<ContextCheckpointProjectionTargetV1>,
) -> bool {
    let Some(first_input) = plan.required_inputs.first().copied() else {
        return false;
    };
    let target = first_input.target();
    if expected_target.is_some_and(|expected| expected != target)
        || plan.source_schema_version != ContextCheckpointV1::SCHEMA_VERSION
        || plan.required_inputs != required_inputs(target)
        || !plan
            .represented_source_fields
            .windows(2)
            .all(|window| window[0] < window[1])
        || plan.represented_source_fields.len() > SOURCE_FIELDS.len()
        || !plan
            .represented_source_fields
            .iter()
            .all(|field| field.len() <= 128 && SOURCE_FIELDS.binary_search(&field.as_str()).is_ok())
    {
        return false;
    }

    let expected_fields = match target {
        ContextCheckpointProjectionTargetV1::SessionBundle => [
            "live_state.decisions[*].rationale",
            "live_state.decisions[*].statement",
            "live_state.findings",
            "live_state.next_steps",
            "live_state.progress.summary",
            "live_state.task.title",
        ]
        .as_slice(),
        ContextCheckpointProjectionTargetV1::HandoffBundle => [
            "lineage.evidence_refs",
            "live_state.decisions[*].statement",
            "live_state.findings",
            "live_state.next_steps",
            "live_state.task.title",
        ]
        .as_slice(),
        ContextCheckpointProjectionTargetV1::ContextSnapshot => [
            "live_state.decisions[*].statement",
            "live_state.progress.summary",
            "live_state.task.title",
        ]
        .as_slice(),
        ContextCheckpointProjectionTargetV1::Ctxpkg => {
            return plan.represented_source_fields.is_empty()
                || plan.represented_source_fields == ["carrier"];
        }
        ContextCheckpointProjectionTargetV1::ContextCheckpointIdentity => return false,
    };
    plan.represented_source_fields
        .iter()
        .map(String::as_str)
        .eq(expected_fields.iter().copied())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lean_ctx_protocol::{
        ContextCheckpointBranchIdV1, ContextCheckpointDecisionStatusV1,
        ContextCheckpointDecisionV1, ContextCheckpointDeviceIdV1, ContextCheckpointFileRoleV1,
        ContextCheckpointFileV1, ContextCheckpointIdV1, ContextCheckpointIdentityV1,
        ContextCheckpointLineageV1, ContextCheckpointLiveStateV1, ContextCheckpointProgressV1,
        ContextCheckpointSessionStateV1, ContextCheckpointTaskStatusV1, ContextCheckpointTaskV1,
        ContextCheckpointTextV1, ContextSessionPhaseV1, ContextSessionRecoveryStateV1,
        ContextSessionStateV1, DecisionId, PlanId, ProtocolReference, SemanticVersion,
        SessionIdentityV1, Sha256Digest, SourceId, TaskId, UtcTimestamp,
    };

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).expect("fixture identity is valid")
    }

    fn reference(value: &str) -> ProtocolReference {
        ProtocolReference::new(value).expect("fixture reference is valid")
    }

    fn digest(value: &str) -> Sha256Digest {
        Sha256Digest::new(format!("sha256:{value:0<64}")).expect("fixture digest is valid")
    }

    fn checkpoint() -> ContextCheckpointV1 {
        let task_id: TaskId = id("task-1");
        let plan_id: PlanId = id("plan-1");
        let lineage = ContextCheckpointLineageV1 {
            project_id: id("project-1"),
            workspace_id: id("550e8400-e29b-41d4-a716-446655440000"),
            tenant_id: id("tenant-1"),
            task_id: task_id.clone(),
            plan_id: Some(plan_id.clone()),
            receipt_ids: vec![id("receipt-1")],
            context_ir_digest: Some(digest("a")),
            hosted_index_digest: Some(digest("b")),
            evidence_refs: vec![reference("evidence:one")],
            knowledge_refs: vec![reference("knowledge:one")],
            gotcha_refs: vec![reference("gotcha:one")],
            snapshot_refs: vec![reference("snapshot:one")],
        };
        let live_state = ContextCheckpointLiveStateV1 {
            schema_version: 1,
            task: ContextCheckpointTaskV1 {
                task_id,
                title: ContextCheckpointTextV1::new("typed projection fixture").unwrap(),
                status: ContextCheckpointTaskStatusV1::InProgress,
                plan_id: Some(plan_id),
            },
            progress: ContextCheckpointProgressV1 {
                completed_steps: 1,
                total_steps: 2,
                confidence_milliunits: 750,
                summary: ContextCheckpointTextV1::new("half complete").unwrap(),
            },
            decisions: vec![ContextCheckpointDecisionV1 {
                decision_id: id::<DecisionId>("decision-1"),
                statement: ContextCheckpointTextV1::new("retain typed source").unwrap(),
                rationale: ContextCheckpointTextV1::new("one semantic owner").unwrap(),
                status: ContextCheckpointDecisionStatusV1::Accepted,
                evidence_refs: vec![reference("evidence:one")],
            }],
            findings: vec![ContextCheckpointTextV1::new("finding one").unwrap()],
            next_steps: vec![ContextCheckpointTextV1::new("verify gates").unwrap()],
            handoff_summary: ContextCheckpointTextV1::new("continue projection").unwrap(),
            files: vec![ContextCheckpointFileV1 {
                source_id: id::<SourceId>("source-1"),
                role: ContextCheckpointFileRoleV1::Modified,
                content_digest: digest("c"),
            }],
            profile_id: Some(id("profile-1")),
            policy_pins: vec![],
            package_pins: vec![],
            session_state: Some(
                ContextCheckpointSessionStateV1::try_new(
                    SessionIdentityV1 {
                        session_id: id("session-1"),
                        task_id: id("task-1"),
                        run_id: id("run-1"),
                        trace_id: id("trace-1"),
                        parent_task_id: None,
                        agent_id: id("agent-1"),
                        project_id: id("project-1"),
                        workspace_id: Some(id("550e8400-e29b-41d4-a716-446655440000")),
                        tenant_id: Some(id("tenant-1")),
                        project_root_ref: reference("project:root"),
                        project_revision_ref: None,
                        created_at: UtcTimestamp::new("2026-01-01T00:00:00Z").unwrap(),
                    },
                    ContextSessionStateV1 {
                        phase: ContextSessionPhaseV1::Executing,
                        revision: 1,
                        next_event_sequence: 2,
                        active_plan_id: Some(id("plan-1")),
                        receipt_id: None,
                        last_checkpoint_digest: None,
                        recovery_state: ContextSessionRecoveryStateV1::Resumable,
                        abort_reason: None,
                    },
                )
                .unwrap(),
            ),
            learning_state: None,
        };
        let identity = ContextCheckpointIdentityV1::try_new(
            id::<ContextCheckpointIdV1>("550e8400-e29b-41d4-a716-446655440001"),
            None,
            None,
            None,
            None,
            id::<ContextCheckpointBranchIdV1>("main"),
            id::<ContextCheckpointDeviceIdV1>("device-1"),
            1,
        )
        .unwrap();
        ContextCheckpointV1::try_new(
            identity,
            lineage,
            live_state,
            None,
            SemanticVersion::new("1.0.0").unwrap(),
            UtcTimestamp::new("2026-01-01T00:00:00Z").unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn all_five_targets_are_explicit_and_versioned() {
        let results = ContextCheckpointProjections::new(&checkpoint())
            .unwrap()
            .project_all();
        assert_eq!(results.len(), 5);
        for (target, result) in ContextCheckpointProjectionTargetV1::all()
            .into_iter()
            .zip(results)
        {
            let result = result.unwrap();
            assert_eq!(result.target, target);
            assert_eq!(result.target_schema_version, target.schema_version());
            assert_eq!(result.projection_schema_version, 1);
        }
    }

    #[test]
    fn identity_projection_is_lossless_only_for_identity_subset() {
        let source = checkpoint();
        let result = project_checkpoint(
            &source,
            ContextCheckpointProjectionTargetV1::ContextCheckpointIdentity,
            1,
        )
        .unwrap();
        assert_eq!(
            result.fidelity,
            ContextCheckpointProjectionFidelityV1::Lossless
        );
        assert!(result.losses.iter().all(|loss| {
            !loss.field_path.starts_with("identity.")
                && loss.reason == ContextCheckpointProjectionLossReasonV1::NotRepresentable
        }));
        match result.payload {
            ContextCheckpointProjectionPayloadV1::ContextCheckpointIdentity(payload) => {
                assert_eq!(payload.identity, source.identity);
            }
            _ => panic!("wrong identity payload"),
        }
    }

    #[test]
    fn losses_are_golden_sorted_and_unique() {
        let source = checkpoint();
        let result = project_checkpoint(
            &source,
            ContextCheckpointProjectionTargetV1::SessionBundle,
            1,
        )
        .unwrap();
        let paths: Vec<_> = result
            .losses
            .iter()
            .map(|loss| loss.field_path.as_str())
            .collect();
        assert!(result.losses.windows(2).all(|window| window[0] < window[1]));
        assert_eq!(paths[0], "carrier");
        assert_eq!(paths[1], "created_at");
        assert_eq!(paths.last(), Some(&"updated_at"));
        assert_eq!(result.losses.len(), {
            let mut unique = paths.clone();
            unique.sort_unstable();
            unique.dedup();
            unique.len()
        });
    }

    #[test]
    fn target_version_fails_closed() {
        let error = project_checkpoint(
            &checkpoint(),
            ContextCheckpointProjectionTargetV1::Ctxpkg,
            1,
        )
        .unwrap_err();
        assert_eq!(
            error,
            ContextCheckpointProjectionErrorV1::UnsupportedTargetVersion {
                target: ContextCheckpointProjectionTargetV1::Ctxpkg,
                requested: 1,
                supported: 2,
            }
        );
    }

    #[test]
    fn source_version_fails_closed() {
        let mut source = checkpoint();
        source.schema_version = 2;
        assert_eq!(
            ContextCheckpointProjections::new(&source).unwrap_err(),
            ContextCheckpointProjectionErrorV1::UnsupportedSourceVersion {
                actual: 2,
                expected: 1,
            }
        );
    }

    #[test]
    fn required_input_absence_is_typed_and_explicit() {
        let result = project_checkpoint(
            &checkpoint(),
            ContextCheckpointProjectionTargetV1::ContextSnapshot,
            1,
        )
        .unwrap();
        let required: Vec<_> = result
            .losses
            .iter()
            .filter_map(|loss| loss.required_input)
            .collect();
        assert_eq!(required.len(), 6);
        assert!(required.contains(&ContextCheckpointProjectionInputV1::ContextSnapshotRoi));
        assert!(required.contains(&ContextCheckpointProjectionInputV1::ContextSnapshotLedger));
    }

    #[test]
    fn result_is_deterministic_and_source_is_not_mutated() {
        let source = checkpoint();
        let before = source.clone();
        let first = project_checkpoint(
            &source,
            ContextCheckpointProjectionTargetV1::HandoffBundle,
            1,
        )
        .unwrap();
        let second = project_checkpoint(
            &source,
            ContextCheckpointProjectionTargetV1::HandoffBundle,
            1,
        )
        .unwrap();
        assert_eq!(first, second);
        assert_eq!(source, before);
    }

    #[test]
    fn duplicate_losses_are_eliminated_and_unknown_fields_reject() {
        let duplicate = ContextCheckpointProjectionLossV1::new(
            "live_state.findings",
            ContextCheckpointProjectionLossReasonV1::NotRepresentable,
            None,
        );
        assert_eq!(
            canonicalize_losses(vec![duplicate.clone(), duplicate]).len(),
            1
        );

        let result = project_checkpoint(
            &checkpoint(),
            ContextCheckpointProjectionTargetV1::ContextSnapshot,
            1,
        )
        .unwrap();
        let mut encoded = serde_json::to_string(&result).unwrap();
        encoded.pop();
        encoded.push_str(",\"unknown\":1}");
        assert!(serde_json::from_str::<ContextCheckpointProjectionResultV1>(&encoded).is_err());
    }

    #[test]
    fn public_projection_deserialization_rejects_inconsistent_wire_state() {
        let result = project_checkpoint(
            &checkpoint(),
            ContextCheckpointProjectionTargetV1::SessionBundle,
            1,
        )
        .unwrap();
        let encoded = serde_json::to_value(&result).unwrap();

        for mutation in [
            ("/projection_schema_version", serde_json::json!(2)),
            ("/target", serde_json::json!("context_snapshot")),
            ("/target_schema_version", serde_json::json!(2)),
            ("/fidelity", serde_json::json!("lossless")),
            (
                "/payload/SessionBundle/source_schema_version",
                serde_json::json!(2),
            ),
            (
                "/payload/SessionBundle/required_inputs/0",
                serde_json::json!("ctxpkg_manifest"),
            ),
            (
                "/payload/SessionBundle/represented_source_fields/0",
                serde_json::json!("arbitrary.unbounded.field"),
            ),
        ] {
            let mut invalid = encoded.clone();
            *invalid.pointer_mut(mutation.0).expect("mutation pointer") = mutation.1;
            assert!(
                serde_json::from_value::<ContextCheckpointProjectionResultV1>(invalid).is_err(),
                "mutation at {} must fail closed",
                mutation.0
            );
        }

        let mut unsorted_inputs = encoded.clone();
        unsorted_inputs["payload"]["SessionBundle"]["required_inputs"]
            .as_array_mut()
            .unwrap()
            .reverse();
        assert!(
            serde_json::from_value::<ContextCheckpointProjectionResultV1>(unsorted_inputs).is_err()
        );

        let mut missing_loss = encoded;
        missing_loss["losses"].as_array_mut().unwrap().pop();
        assert!(
            serde_json::from_value::<ContextCheckpointProjectionResultV1>(missing_loss).is_err()
        );

        let ContextCheckpointProjectionPayloadV1::SessionBundle(plan) = result.payload else {
            unreachable!()
        };
        let mut invalid_plan = serde_json::to_value(plan).unwrap();
        invalid_plan["required_inputs"]
            .as_array_mut()
            .unwrap()
            .clear();
        assert!(serde_json::from_value::<ContextCheckpointProjectionPlanV1>(invalid_plan).is_err());

        let invalid_loss = serde_json::json!({
            "field_path": "target.exported_at",
            "reason": "not_representable",
            "required_input": "session_bundle_exported_at"
        });
        assert!(serde_json::from_value::<ContextCheckpointProjectionLossV1>(invalid_loss).is_err());

        for invalid_loss in [
            serde_json::json!({
                "field_path": "not_a_canonical_field",
                "reason": "not_representable"
            }),
            serde_json::json!({
                "field_path": "target.created_at",
                "reason": "required_input",
                "required_input": "session_bundle_exported_at"
            }),
        ] {
            assert!(
                serde_json::from_value::<ContextCheckpointProjectionLossV1>(invalid_loss).is_err()
            );
        }

        let mut checkpoint_with_carrier = checkpoint();
        checkpoint_with_carrier.carrier = Some(
            ContextCheckpointCarrierBindingV1::try_new(
                id("550e8400-e29b-41d4-a716-446655440000"),
                digest("d"),
                digest("e"),
            )
            .unwrap(),
        );
        let ctxpkg = project_checkpoint(
            &checkpoint_with_carrier,
            ContextCheckpointProjectionTargetV1::Ctxpkg,
            CONTEXT_PACKAGE_V2_SCHEMA_VERSION,
        )
        .unwrap();
        let ContextCheckpointProjectionPayloadV1::Ctxpkg(ctxpkg) = ctxpkg.payload else {
            unreachable!();
        };
        let encoded_ctxpkg = serde_json::to_value(ctxpkg).unwrap();

        let mut carrier_not_represented = encoded_ctxpkg.clone();
        carrier_not_represented["plan"]["represented_source_fields"] = serde_json::json!([]);
        assert!(
            serde_json::from_value::<ContextCheckpointCtxpkgProjectionV1>(carrier_not_represented)
                .is_err()
        );

        let mut represented_without_carrier = encoded_ctxpkg;
        represented_without_carrier["carrier_binding"] = serde_json::Value::Null;
        assert!(
            serde_json::from_value::<ContextCheckpointCtxpkgProjectionV1>(
                represented_without_carrier
            )
            .is_err()
        );
    }

    #[test]
    fn corrupt_session_state_is_rejected_for_wire_targets() {
        let mut source = checkpoint();
        source
            .live_state
            .session_state
            .as_mut()
            .unwrap()
            .state
            .recovery_state = ContextSessionRecoveryStateV1::Corrupt;
        source
            .live_state
            .session_state
            .as_mut()
            .unwrap()
            .state
            .phase = ContextSessionPhaseV1::Closed;
        // The source remains valid: this is an explicit state policy failure,
        // not an attempt to reinterpret corrupt continuation state.
        let error = ContextCheckpointProjections::new(&source)
            .unwrap()
            .project(ContextCheckpointProjectionTargetV1::SessionBundle, 1)
            .unwrap_err();
        assert_eq!(
            error,
            ContextCheckpointProjectionErrorV1::UnsupportedState {
                target: ContextCheckpointProjectionTargetV1::SessionBundle,
                state: ContextCheckpointProjectionUnsupportedStateV1::Corrupt,
            }
        );
    }

    #[test]
    fn checkpoint_identity_is_the_only_semantic_owner() {
        let source = checkpoint();
        let result = project_checkpoint(
            &source,
            ContextCheckpointProjectionTargetV1::SessionBundle,
            1,
        )
        .unwrap();
        match result.payload {
            ContextCheckpointProjectionPayloadV1::SessionBundle(plan) => {
                assert_eq!(plan.source_checkpoint_id, source.identity.checkpoint_id);
                assert!(
                    plan.represented_source_fields
                        .iter()
                        .all(|field| { field.starts_with("live_state.") })
                );
            }
            _ => panic!("wrong session payload"),
        }
    }
}
