// SPDX-License-Identifier: Apache-2.0

//! Shared input, projection, and refusal validation helpers.

use super::{
    ContextCheckpointLegacyErrorV1, ContextCheckpointLegacyFieldV1, ContextCheckpointLegacyHashV1,
    ContextCheckpointLegacyLabelV1, ContextCheckpointLegacyProjectIdentityV1,
    ContextCheckpointLegacyRefusalV1, ContextCheckpointProjectionInputV1,
    ContextCheckpointProjectionTargetV1,
};
use chrono::DateTime;
use lean_ctx_protocol::UtcTimestamp;

pub(super) fn require_capacity(
    actual: usize,
    limit: usize,
    field: ContextCheckpointLegacyFieldV1,
) -> Result<(), ContextCheckpointLegacyErrorV1> {
    if actual > limit {
        return Err(ContextCheckpointLegacyErrorV1::LegacyCapacityExceeded {
            field,
            limit: limit as u64,
            actual: actual as u64,
        });
    }
    Ok(())
}

pub(super) fn require_arity(
    actual: usize,
    expected: usize,
    field: ContextCheckpointLegacyFieldV1,
) -> Result<(), ContextCheckpointLegacyErrorV1> {
    if actual != expected {
        return Err(ContextCheckpointLegacyErrorV1::InputArityMismatch {
            field,
            expected: expected as u32,
            actual: actual as u32,
        });
    }
    Ok(())
}

pub(super) fn projection_input_field_path(
    input: ContextCheckpointProjectionInputV1,
) -> &'static str {
    match input {
        ContextCheckpointProjectionInputV1::SessionBundleExportedAt
        | ContextCheckpointProjectionInputV1::HandoffBundleExportedAt => "target.exported_at",
        ContextCheckpointProjectionInputV1::SessionBundlePolicyIdentities => "target.role_profile",
        ContextCheckpointProjectionInputV1::SessionBundleSessionExcerpt
        | ContextCheckpointProjectionInputV1::ContextSnapshotSession => "target.session",
        ContextCheckpointProjectionInputV1::HandoffBundleLedger
        | ContextCheckpointProjectionInputV1::ContextSnapshotLedger => "target.ledger",
        ContextCheckpointProjectionInputV1::HandoffBundleProjectIdentity => "target.project",
        ContextCheckpointProjectionInputV1::CtxpkgManifest => "target.manifest",
        ContextCheckpointProjectionInputV1::CtxpkgLogicalState => "target.logical_state",
        ContextCheckpointProjectionInputV1::CtxpkgIntegrity => "target.integrity",
        ContextCheckpointProjectionInputV1::ContextSnapshotCreatedAt => "target.created_at",
        ContextCheckpointProjectionInputV1::ContextSnapshotGitAnchor => "target.git",
        ContextCheckpointProjectionInputV1::ContextSnapshotRoi => "target.roi",
        ContextCheckpointProjectionInputV1::ContextSnapshotLineage => "target.lineage",
    }
}

pub(super) fn projection_input_target(
    input: ContextCheckpointProjectionInputV1,
) -> ContextCheckpointProjectionTargetV1 {
    match input {
        ContextCheckpointProjectionInputV1::SessionBundleExportedAt
        | ContextCheckpointProjectionInputV1::SessionBundlePolicyIdentities
        | ContextCheckpointProjectionInputV1::SessionBundleSessionExcerpt => {
            ContextCheckpointProjectionTargetV1::SessionBundle
        }
        ContextCheckpointProjectionInputV1::HandoffBundleExportedAt
        | ContextCheckpointProjectionInputV1::HandoffBundleLedger
        | ContextCheckpointProjectionInputV1::HandoffBundleProjectIdentity => {
            ContextCheckpointProjectionTargetV1::HandoffBundle
        }
        ContextCheckpointProjectionInputV1::CtxpkgManifest
        | ContextCheckpointProjectionInputV1::CtxpkgLogicalState
        | ContextCheckpointProjectionInputV1::CtxpkgIntegrity => {
            ContextCheckpointProjectionTargetV1::Ctxpkg
        }
        ContextCheckpointProjectionInputV1::ContextSnapshotCreatedAt
        | ContextCheckpointProjectionInputV1::ContextSnapshotGitAnchor
        | ContextCheckpointProjectionInputV1::ContextSnapshotRoi
        | ContextCheckpointProjectionInputV1::ContextSnapshotLineage
        | ContextCheckpointProjectionInputV1::ContextSnapshotLedger
        | ContextCheckpointProjectionInputV1::ContextSnapshotSession => {
            ContextCheckpointProjectionTargetV1::ContextSnapshot
        }
    }
}

impl ContextCheckpointLegacyRefusalV1 {
    /// Validate refusal identity, dependency bounds, and input pairing.
    pub fn validate(&self) -> Result<(), ContextCheckpointLegacyErrorV1> {
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
        if self.dependency.is_empty()
            || self.dependency.len() > 160
            || self.dependency.chars().any(char::is_control)
        {
            return Err(ContextCheckpointLegacyErrorV1::InvalidInputs {
                field: ContextCheckpointLegacyFieldV1::Payload,
            });
        }
        if self
            .required_inputs
            .iter()
            .any(|input| projection_input_target(*input) != self.target.projection_target())
        {
            return Err(ContextCheckpointLegacyErrorV1::TargetInputsMismatch {
                target: self.target,
                inputs_target: self.target,
            });
        }
        if self
            .required_inputs
            .windows(2)
            .any(|window| window[0] >= window[1])
        {
            return Err(ContextCheckpointLegacyErrorV1::DuplicateLegacyInput {
                field: ContextCheckpointLegacyFieldV1::Payload,
            });
        }
        Ok(())
    }
}

pub(super) fn validate_project_identity(
    project: &ContextCheckpointLegacyProjectIdentityV1,
) -> Result<(), ContextCheckpointLegacyErrorV1> {
    if let Some(hash) = &project.project_root_hash {
        ContextCheckpointLegacyHashV1::new(hash.as_str().to_owned())?;
    }
    if let Some(hash) = &project.project_identity_hash {
        ContextCheckpointLegacyHashV1::new(hash.as_str().to_owned())?;
    }
    Ok(())
}

pub(super) fn validate_optional_label(
    value: Option<&ContextCheckpointLegacyLabelV1>,
) -> Result<(), ContextCheckpointLegacyErrorV1> {
    if let Some(value) = value {
        ContextCheckpointLegacyLabelV1::new(value.as_str().to_owned())?;
    }
    Ok(())
}

pub(super) fn validate_timestamp(
    value: &UtcTimestamp,
    field: ContextCheckpointLegacyFieldV1,
) -> Result<(), ContextCheckpointLegacyErrorV1> {
    DateTime::parse_from_rfc3339(value.as_str())
        .map(|_| ())
        .map_err(|_| ContextCheckpointLegacyErrorV1::InvalidInputs { field })
}

pub(super) fn validate_sorted_unique_timestamps(
    values: &[UtcTimestamp],
    field: ContextCheckpointLegacyFieldV1,
) -> Result<(), ContextCheckpointLegacyErrorV1> {
    if values.windows(2).any(|window| window[0] >= window[1]) {
        return Err(ContextCheckpointLegacyErrorV1::DuplicateLegacyInput { field });
    }
    Ok(())
}
