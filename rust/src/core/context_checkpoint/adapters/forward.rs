// SPDX-License-Identifier: Apache-2.0

//! Forward checkpoint-to-legacy dispatch and projection planning.

use super::{
    ContextCheckpointLegacyErrorV1, ContextCheckpointLegacyInputsV1,
    ContextCheckpointLegacyMaterializationV1, ContextCheckpointLegacyRefusalReasonV1,
    ContextCheckpointLegacyRefusalV1, ContextCheckpointLegacyRequestV1,
    ContextCheckpointLegacyTargetV1, ContextCheckpointProjectionErrorV1,
    ContextCheckpointProjectionInputV1, ContextCheckpointProjectionLossReasonV1,
    ContextCheckpointProjectionLossV1, ContextCheckpointProjectionResultV1,
    ContextCheckpointProjectionTargetV1, ContextCheckpointProjections, ContextCheckpointV1,
    materialize_context_snapshot, materialize_ctxpkg, materialize_handoff_bundle,
    materialize_session_bundle,
};

/// Materialize one validated checkpoint into a deployed legacy wire family.
///
/// The returned payload contains canonical compact JSON parsed once by the
/// deployed wire owner.  Refused is a successful, typed outcome: no target
/// bytes were emitted when an owner-side dependency is absent.
pub fn materialize_checkpoint_legacy(
    checkpoint: &ContextCheckpointV1,
    request: &ContextCheckpointLegacyRequestV1,
) -> Result<ContextCheckpointLegacyMaterializationV1, ContextCheckpointLegacyErrorV1> {
    request.validate()?;
    let plan = projection_plan(checkpoint, request.target)?;

    match (&request.target, &request.inputs) {
        (
            ContextCheckpointLegacyTargetV1::SessionBundle,
            ContextCheckpointLegacyInputsV1::SessionBundle(inputs),
        ) => materialize_session_bundle(checkpoint, request, inputs, &plan),
        (
            ContextCheckpointLegacyTargetV1::HandoffBundle,
            ContextCheckpointLegacyInputsV1::HandoffBundle(inputs),
        ) => materialize_handoff_bundle(checkpoint, request, inputs, &plan),
        (
            ContextCheckpointLegacyTargetV1::Ctxpkg,
            ContextCheckpointLegacyInputsV1::Ctxpkg(inputs),
        ) => materialize_ctxpkg(checkpoint, request, inputs, &plan),
        (
            ContextCheckpointLegacyTargetV1::ContextSnapshot,
            ContextCheckpointLegacyInputsV1::ContextSnapshot(inputs),
        ) => materialize_context_snapshot(checkpoint, request, inputs, &plan),
        (target, inputs) => Err(ContextCheckpointLegacyErrorV1::TargetInputsMismatch {
            target: *target,
            inputs_target: inputs.target(),
        }),
    }
}

/// Compatibility alias with an explicit V1 suffix.
pub fn materialize_checkpoint_legacy_v1(
    checkpoint: &ContextCheckpointV1,
    request: &ContextCheckpointLegacyRequestV1,
) -> Result<ContextCheckpointLegacyMaterializationV1, ContextCheckpointLegacyErrorV1> {
    materialize_checkpoint_legacy(checkpoint, request)
}

pub(super) fn projection_plan(
    checkpoint: &ContextCheckpointV1,
    target: ContextCheckpointLegacyTargetV1,
) -> Result<ContextCheckpointProjectionResultV1, ContextCheckpointLegacyErrorV1> {
    let projections =
        ContextCheckpointProjections::new(checkpoint).map_err(map_projection_error)?;
    projections
        .project(
            target.projection_target(),
            target.projection_target().schema_version(),
        )
        .map_err(map_projection_error)
}

pub(super) fn map_projection_error(
    error: ContextCheckpointProjectionErrorV1,
) -> ContextCheckpointLegacyErrorV1 {
    match error {
        ContextCheckpointProjectionErrorV1::UnsupportedSourceVersion { actual, expected } => {
            ContextCheckpointLegacyErrorV1::UnsupportedSourceVersion { actual, expected }
        }
        ContextCheckpointProjectionErrorV1::UnsupportedTargetVersion {
            target,
            requested,
            supported,
        } => ContextCheckpointLegacyErrorV1::UnsupportedTargetVersion {
            target: legacy_target(target),
            requested,
            supported,
        },
        ContextCheckpointProjectionErrorV1::InvalidSource
        | ContextCheckpointProjectionErrorV1::UnsupportedState { .. } => {
            ContextCheckpointLegacyErrorV1::InvalidSource
        }
        ContextCheckpointProjectionErrorV1::InvalidProjectionResult => {
            ContextCheckpointLegacyErrorV1::LegacyOwnerRejected
        }
    }
}

pub(super) fn legacy_target(
    target: ContextCheckpointProjectionTargetV1,
) -> ContextCheckpointLegacyTargetV1 {
    match target {
        ContextCheckpointProjectionTargetV1::SessionBundle
        | ContextCheckpointProjectionTargetV1::ContextCheckpointIdentity => {
            ContextCheckpointLegacyTargetV1::SessionBundle
        }
        ContextCheckpointProjectionTargetV1::HandoffBundle => {
            ContextCheckpointLegacyTargetV1::HandoffBundle
        }
        ContextCheckpointProjectionTargetV1::Ctxpkg => ContextCheckpointLegacyTargetV1::Ctxpkg,
        ContextCheckpointProjectionTargetV1::ContextSnapshot => {
            ContextCheckpointLegacyTargetV1::ContextSnapshot
        }
    }
}

pub(super) fn residual_losses(
    plan: &ContextCheckpointProjectionResultV1,
) -> Vec<ContextCheckpointProjectionLossV1> {
    plan.losses
        .iter()
        .filter(|loss| loss.reason == ContextCheckpointProjectionLossReasonV1::NotRepresentable)
        .cloned()
        .collect()
}

pub(super) fn required_inputs_from_plan(
    plan: &ContextCheckpointProjectionResultV1,
) -> Vec<ContextCheckpointProjectionInputV1> {
    let mut inputs = plan
        .losses
        .iter()
        .filter_map(|loss| loss.required_input)
        .collect::<Vec<_>>();
    inputs.sort_unstable();
    inputs.dedup();
    inputs
}

pub(super) fn refusal(
    request: &ContextCheckpointLegacyRequestV1,
    reason: ContextCheckpointLegacyRefusalReasonV1,
    dependency: &str,
    required_inputs: Vec<ContextCheckpointProjectionInputV1>,
) -> Result<ContextCheckpointLegacyMaterializationV1, ContextCheckpointLegacyErrorV1> {
    let value = ContextCheckpointLegacyRefusalV1 {
        target: request.target,
        target_schema_version: request.target_schema_version,
        reason,
        dependency: dependency.to_owned(),
        required_inputs,
    };
    value
        .validate()
        .map_err(|_| ContextCheckpointLegacyErrorV1::LegacyOwnerRejected)?;
    Ok(ContextCheckpointLegacyMaterializationV1::Refused(value))
}
