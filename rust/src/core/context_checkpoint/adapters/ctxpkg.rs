// SPDX-License-Identifier: Apache-2.0

//! Context package materializer.

use super::{
    ContextCheckpointCtxpkgInputsV1, ContextCheckpointLegacyErrorV1,
    ContextCheckpointLegacyMaterializationV1, ContextCheckpointLegacyRefusalReasonV1,
    ContextCheckpointLegacyRequestV1, ContextCheckpointProjectionInputV1,
    ContextCheckpointProjectionResultV1, ContextCheckpointV1, refusal, required_inputs_from_plan,
    validate_carrier_envelope,
};

pub(super) fn materialize_ctxpkg(
    checkpoint: &ContextCheckpointV1,
    request: &ContextCheckpointLegacyRequestV1,
    inputs: &ContextCheckpointCtxpkgInputsV1,
    plan: &ContextCheckpointProjectionResultV1,
) -> Result<ContextCheckpointLegacyMaterializationV1, ContextCheckpointLegacyErrorV1> {
    if let Some(envelope_json) = inputs.carrier_envelope_json.as_ref() {
        validate_carrier_envelope(checkpoint, envelope_json)?;
    }
    if checkpoint.carrier.is_none() {
        return refusal(
            request,
            ContextCheckpointLegacyRefusalReasonV1::MissingCarrierBinding,
            "lean_ctx_protocol::ContextCheckpointCarrierBindingV1",
            vec![ContextCheckpointProjectionInputV1::CtxpkgManifest],
        );
    }
    refusal(
        request,
        ContextCheckpointLegacyRefusalReasonV1::TargetOwnerDependency,
        "crate::core::context_package::{builder,export,manifest,verify}",
        required_inputs_from_plan(plan),
    )
}
