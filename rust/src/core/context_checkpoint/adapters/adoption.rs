// SPDX-License-Identifier: Apache-2.0

//! Authenticated reverse adoption.

use super::{
    ContextCheckpointLegacyAdoptionV1, ContextCheckpointLegacyErrorV1,
    ContextCheckpointLegacyFieldV1, ContextCheckpointLegacyMaterializationV1,
    ContextCheckpointLegacyPayloadV1, ContextCheckpointLegacyRefusalReasonV1,
    ContextCheckpointLegacyRefusalV1, ContextCheckpointLegacyRequestV1,
    ContextCheckpointLegacyTargetV1, ContextCheckpointV1, Value, materialize_checkpoint_legacy,
    source_identity,
};

/// Verify a legacy payload against an authenticated anchor without importing
/// fields the checkpoint cannot own.  Adoption is intentionally idempotent:
/// a payload is accepted only when it is exactly the bytes this adapter would
/// emit for the same source/request and it carries no residual loss.
pub fn adopt_legacy_payload(
    anchor: &ContextCheckpointV1,
    request: &ContextCheckpointLegacyRequestV1,
    payload: &ContextCheckpointLegacyPayloadV1,
) -> Result<ContextCheckpointLegacyAdoptionV1, ContextCheckpointLegacyErrorV1> {
    request.validate()?;
    payload.validate()?;
    if payload.target != request.target
        || payload.target_schema_version != request.target_schema_version
    {
        return Err(ContextCheckpointLegacyErrorV1::TargetInputsMismatch {
            target: request.target,
            inputs_target: payload.target,
        });
    }
    if payload.source_identity != source_identity(anchor) {
        return Err(ContextCheckpointLegacyErrorV1::IdentityMismatch {
            field: ContextCheckpointLegacyFieldV1::SourceIdentity,
        });
    }
    if !payload.losses.is_empty() {
        return Ok(ContextCheckpointLegacyAdoptionV1::Refused(
            ContextCheckpointLegacyRefusalV1 {
                target: request.target,
                target_schema_version: request.target_schema_version,
                reason: ContextCheckpointLegacyRefusalReasonV1::ReverseNotRepresentable,
                dependency: "ContextCheckpointV1::lossless_reverse_contract".to_owned(),
                required_inputs: Vec::new(),
            },
        ));
    }
    let materialized = materialize_checkpoint_legacy(anchor, request)?;
    let ContextCheckpointLegacyMaterializationV1::Materialized(expected) = materialized else {
        return Ok(ContextCheckpointLegacyAdoptionV1::Refused(
            match materialized {
                ContextCheckpointLegacyMaterializationV1::Refused(refusal) => refusal,
                ContextCheckpointLegacyMaterializationV1::Materialized(_) => unreachable!(),
            },
        ));
    };
    if expected.canonical_json != payload.canonical_json
        || expected.payload_content_hash != payload.payload_content_hash
    {
        return Err(identity_or_overwrite_error(
            request.target,
            &expected.canonical_json,
            &payload.canonical_json,
        ));
    }
    Ok(ContextCheckpointLegacyAdoptionV1::Adopted(Box::new(
        anchor.clone(),
    )))
}

/// Compatibility alias with an explicit V1 suffix.
pub fn adopt_legacy_payload_v1(
    anchor: &ContextCheckpointV1,
    request: &ContextCheckpointLegacyRequestV1,
    payload: &ContextCheckpointLegacyPayloadV1,
) -> Result<ContextCheckpointLegacyAdoptionV1, ContextCheckpointLegacyErrorV1> {
    adopt_legacy_payload(anchor, request, payload)
}

pub(super) fn identity_or_overwrite_error(
    target: ContextCheckpointLegacyTargetV1,
    expected: &str,
    actual: &str,
) -> ContextCheckpointLegacyErrorV1 {
    let expected_value: Value = serde_json::from_str(expected).unwrap_or(Value::Null);
    let actual_value: Value = serde_json::from_str(actual).unwrap_or(Value::Null);
    if expected_value.get("project") != actual_value.get("project") {
        return ContextCheckpointLegacyErrorV1::IdentityMismatch {
            field: ContextCheckpointLegacyFieldV1::Payload,
        };
    }
    let session_id_differs = match target {
        ContextCheckpointLegacyTargetV1::SessionBundle => expected_value
            .pointer("/session/id")
            .ne(&actual_value.pointer("/session/id")),
        ContextCheckpointLegacyTargetV1::HandoffBundle => expected_value
            .pointer("/ledger/session/id")
            .ne(&actual_value.pointer("/ledger/session/id")),
        ContextCheckpointLegacyTargetV1::ContextSnapshot => expected_value
            .pointer("/session/session_id")
            .ne(&actual_value.pointer("/session/session_id")),
        ContextCheckpointLegacyTargetV1::Ctxpkg => false,
    };
    if session_id_differs {
        ContextCheckpointLegacyErrorV1::IdentityMismatch {
            field: ContextCheckpointLegacyFieldV1::SessionId,
        }
    } else {
        ContextCheckpointLegacyErrorV1::SemanticOverwrite {
            field: ContextCheckpointLegacyFieldV1::Payload,
        }
    }
}
