// SPDX-License-Identifier: Apache-2.0

//! Legacy wire validation, canonicalization, and provenance helpers.

use super::{
    CONTEXT_CHECKPOINT_ADAPTER_SCHEMA_VERSION, CONTEXT_SNAPSHOT_V1_SCHEMA_VERSION,
    CcpSessionBundleV1, ContextCheckpointLegacyErrorV1, ContextCheckpointLegacyFieldV1,
    ContextCheckpointLegacyHashV1, ContextCheckpointLegacyPayloadV1, ContextCheckpointLegacyRoiV1,
    ContextCheckpointLegacySourceIdentityV1, ContextCheckpointLegacyTargetV1,
    ContextCheckpointProjectionLossV1, ContextCheckpointV1, ContextSnapshotV1,
    HandoffTransferBundleV1, MAX_CONTEXT_CHECKPOINT_LEGACY_PAYLOAD_BYTES,
};
use chrono::{DateTime, Utc};
use lean_ctx_protocol::UtcTimestamp;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};
use std::collections::BTreeSet;

pub(super) fn validate_carrier_envelope(
    checkpoint: &ContextCheckpointV1,
    json: &str,
) -> Result<(), ContextCheckpointLegacyErrorV1> {
    let value = parse_json_object(json)?;
    for (field, expected) in [
        ("workspace_id", checkpoint.lineage.workspace_id.as_str()),
        (
            "state_digest",
            checkpoint
                .carrier
                .as_ref()
                .map(|binding| binding.state_digest.as_str())
                .unwrap_or_default(),
        ),
        (
            "envelope_digest",
            checkpoint
                .carrier
                .as_ref()
                .map(|binding| binding.envelope_digest.as_str())
                .unwrap_or_default(),
        ),
    ] {
        if value.get(field).and_then(Value::as_str) != Some(expected) {
            return Err(ContextCheckpointLegacyErrorV1::IdentityMismatch {
                field: ContextCheckpointLegacyFieldV1::CarrierBinding,
            });
        }
    }
    Ok(())
}

pub(super) fn chrono_timestamp(
    value: &UtcTimestamp,
    field: ContextCheckpointLegacyFieldV1,
) -> Result<DateTime<Utc>, ContextCheckpointLegacyErrorV1> {
    DateTime::parse_from_rfc3339(value.as_str())
        .map(|value| value.with_timezone(&Utc))
        .map_err(|_| ContextCheckpointLegacyErrorV1::InvalidInputs { field })
}

pub(super) fn progress_percent(checkpoint: &ContextCheckpointV1) -> u8 {
    let completed = checkpoint.live_state.progress.completed_steps as u64;
    let total = checkpoint.live_state.progress.total_steps as u64;
    if total == 0 {
        return 0;
    }
    ((completed * 100) / total).min(100) as u8
}

pub(super) fn compression_rate(roi: &ContextCheckpointLegacyRoiV1) -> f64 {
    let denominator = roi.input_tokens as f64 + roi.tokens_saved as f64;
    if denominator == 0.0 {
        0.0
    } else {
        roi.tokens_saved as f64 / denominator
    }
}

pub(super) fn payload_from_value(
    checkpoint: &ContextCheckpointV1,
    target: ContextCheckpointLegacyTargetV1,
    target_schema_version: u32,
    canonical_json: String,
    losses: Vec<ContextCheckpointProjectionLossV1>,
) -> Result<ContextCheckpointLegacyPayloadV1, ContextCheckpointLegacyErrorV1> {
    let payload_content_hash = ContextCheckpointLegacyHashV1::new(crate::core::hasher::hash_hex(
        canonical_json.as_bytes(),
    ))?;
    let payload = ContextCheckpointLegacyPayloadV1 {
        adapter_schema_version: CONTEXT_CHECKPOINT_ADAPTER_SCHEMA_VERSION,
        source_identity: source_identity(checkpoint),
        target,
        target_schema_version,
        canonical_json,
        payload_content_hash,
        losses,
    };
    payload.validate()?;
    Ok(payload)
}

pub(super) fn encode_owner(
    value: &Value,
    parse: impl FnOnce(&str) -> Result<(), String>,
) -> Result<String, ContextCheckpointLegacyErrorV1> {
    let canonical = canonicalize_json_value(value)?;
    if canonical.len() > MAX_CONTEXT_CHECKPOINT_LEGACY_PAYLOAD_BYTES {
        return Err(ContextCheckpointLegacyErrorV1::LegacyCapacityExceeded {
            field: ContextCheckpointLegacyFieldV1::Payload,
            limit: MAX_CONTEXT_CHECKPOINT_LEGACY_PAYLOAD_BYTES as u64,
            actual: canonical.len() as u64,
        });
    }
    let json = String::from_utf8(canonical)
        .map_err(|_| ContextCheckpointLegacyErrorV1::LegacyOwnerRejected)?;
    parse(&json).map_err(|_| ContextCheckpointLegacyErrorV1::LegacyOwnerRejected)?;
    Ok(json)
}

pub(super) fn source_identity(
    checkpoint: &ContextCheckpointV1,
) -> ContextCheckpointLegacySourceIdentityV1 {
    ContextCheckpointLegacySourceIdentityV1 {
        checkpoint_id: checkpoint.identity.checkpoint_id.clone(),
        tenant_id: checkpoint.lineage.tenant_id.clone(),
        project_id: checkpoint.lineage.project_id.clone(),
        workspace_id: checkpoint.lineage.workspace_id.clone(),
        task_id: checkpoint.lineage.task_id.clone(),
        plan_id: checkpoint.lineage.plan_id.clone(),
    }
}

pub(super) fn canonicalize_json_value(
    value: &Value,
) -> Result<Vec<u8>, ContextCheckpointLegacyErrorV1> {
    let value = canonicalize_value(value.clone());
    serde_json::to_vec(&value).map_err(|_| ContextCheckpointLegacyErrorV1::LegacyOwnerRejected)
}

pub(super) fn canonicalize_value(value: Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.into_iter().map(canonicalize_value).collect()),
        Value::Object(object) => {
            let mut entries: Vec<_> = object.into_iter().collect();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            let mut sorted = Map::new();
            for (key, value) in entries {
                sorted.insert(key, canonicalize_value(value));
            }
            Value::Object(sorted)
        }
        value => value,
    }
}

pub(super) fn parse_canonical_json(json: &str) -> Result<Value, ContextCheckpointLegacyErrorV1> {
    let value: Value = serde_json::from_str(json).map_err(|_| {
        ContextCheckpointLegacyErrorV1::InvalidLegacyPayload {
            field: ContextCheckpointLegacyFieldV1::Payload,
        }
    })?;
    let canonical = canonicalize_json_value(&value)?;
    if canonical != json.as_bytes() {
        return Err(ContextCheckpointLegacyErrorV1::InvalidLegacyPayload {
            field: ContextCheckpointLegacyFieldV1::Payload,
        });
    }
    Ok(value)
}

pub(super) fn parse_json_object(json: &str) -> Result<Value, ContextCheckpointLegacyErrorV1> {
    let value: Value =
        serde_json::from_str(json).map_err(|_| ContextCheckpointLegacyErrorV1::InvalidInputs {
            field: ContextCheckpointLegacyFieldV1::Payload,
        })?;
    if !value.is_object() {
        return Err(ContextCheckpointLegacyErrorV1::InvalidInputs {
            field: ContextCheckpointLegacyFieldV1::Payload,
        });
    }
    Ok(value)
}

pub(super) fn validate_legacy_target_value(
    target: ContextCheckpointLegacyTargetV1,
    target_schema_version: u32,
    value: &Value,
) -> Result<(), ContextCheckpointLegacyErrorV1> {
    let schema = value
        .get("schema_version")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok());
    if matches!(
        target,
        ContextCheckpointLegacyTargetV1::SessionBundle
            | ContextCheckpointLegacyTargetV1::HandoffBundle
            | ContextCheckpointLegacyTargetV1::ContextSnapshot
    ) && schema != Some(target.schema_version())
    {
        return Err(ContextCheckpointLegacyErrorV1::InvalidLegacyPayload {
            field: ContextCheckpointLegacyFieldV1::Payload,
        });
    }
    match target {
        ContextCheckpointLegacyTargetV1::SessionBundle => {
            strict_keys(
                value,
                &[
                    "schema_version",
                    "exported_at",
                    "project",
                    "role",
                    "profile",
                    "session",
                ],
            )?;
            strict_owner_round_trip::<CcpSessionBundleV1>(value)?;
        }
        ContextCheckpointLegacyTargetV1::HandoffBundle => {
            strict_keys(
                value,
                &[
                    "schema_version",
                    "exported_at",
                    "privacy",
                    "project",
                    "ledger",
                    "artifacts",
                    "signature",
                    "signer_public_key",
                    "signer_agent_id",
                ],
            )?;
            let bundle = strict_owner_round_trip::<HandoffTransferBundleV1>(value)?;
            let expected_md5 =
                crate::core::handoff_ledger::compute_content_md5_for_ledger(&bundle.ledger);
            if bundle.ledger.content_md5 != expected_md5 {
                return Err(ContextCheckpointLegacyErrorV1::InvalidLegacyPayload {
                    field: ContextCheckpointLegacyFieldV1::Payload,
                });
            }
            let has_signature_material = bundle.signature.is_some()
                || bundle.signer_public_key.is_some()
                || bundle.signer_agent_id.is_some();
            if has_signature_material
                && crate::core::handoff_transfer_bundle::verify_bundle_signature(&bundle).is_err()
            {
                return Err(ContextCheckpointLegacyErrorV1::MalformedSignature);
            }
        }
        ContextCheckpointLegacyTargetV1::Ctxpkg => {
            let _ = target_schema_version;
            return Err(ContextCheckpointLegacyErrorV1::InvalidLegacyPayload {
                field: ContextCheckpointLegacyFieldV1::Payload,
            });
        }
        ContextCheckpointLegacyTargetV1::ContextSnapshot => {
            strict_keys(
                value,
                &[
                    "schema_version",
                    "snapshot_id",
                    "parent_id",
                    "created_at",
                    "lean_ctx_version",
                    "git",
                    "project",
                    "roi",
                    "lineage",
                    "ledger",
                    "session",
                    "signature",
                ],
            )?;
            let snapshot = strict_owner_round_trip::<ContextSnapshotV1>(value)?;
            if snapshot.schema_version != CONTEXT_SNAPSHOT_V1_SCHEMA_VERSION
                || crate::core::context_snapshot::digest::compute_id(&snapshot).map_err(|_| {
                    ContextCheckpointLegacyErrorV1::InvalidLegacyPayload {
                        field: ContextCheckpointLegacyFieldV1::Payload,
                    }
                })? != snapshot.snapshot_id
            {
                return Err(ContextCheckpointLegacyErrorV1::InvalidLegacyPayload {
                    field: ContextCheckpointLegacyFieldV1::Payload,
                });
            }
            if snapshot.signature.is_some() {
                match crate::core::context_snapshot::signing::verify_snapshot(&snapshot) {
                    Ok(true) => {}
                    Ok(false) | Err(_) => {
                        return Err(ContextCheckpointLegacyErrorV1::MalformedSignature);
                    }
                }
            }
        }
    }
    Ok(())
}

fn strict_owner_round_trip<T>(value: &Value) -> Result<T, ContextCheckpointLegacyErrorV1>
where
    T: DeserializeOwned + Serialize,
{
    let parsed = serde_json::from_value::<T>(value.clone()).map_err(|_| {
        ContextCheckpointLegacyErrorV1::InvalidLegacyPayload {
            field: ContextCheckpointLegacyFieldV1::Payload,
        }
    })?;
    let round_trip = serde_json::to_value(&parsed).map_err(|_| {
        ContextCheckpointLegacyErrorV1::InvalidLegacyPayload {
            field: ContextCheckpointLegacyFieldV1::Payload,
        }
    })?;
    if canonicalize_value(round_trip) != canonicalize_value(value.clone()) {
        return Err(ContextCheckpointLegacyErrorV1::InvalidLegacyPayload {
            field: ContextCheckpointLegacyFieldV1::Payload,
        });
    }
    Ok(parsed)
}

pub(super) fn strict_keys(
    value: &Value,
    allowed: &[&str],
) -> Result<(), ContextCheckpointLegacyErrorV1> {
    let Some(object) = value.as_object() else {
        return Err(ContextCheckpointLegacyErrorV1::InvalidLegacyPayload {
            field: ContextCheckpointLegacyFieldV1::Payload,
        });
    };
    let allowed: BTreeSet<&str> = allowed.iter().copied().collect();
    if object.keys().any(|key| !allowed.contains(key.as_str())) {
        return Err(ContextCheckpointLegacyErrorV1::InvalidLegacyPayload {
            field: ContextCheckpointLegacyFieldV1::Payload,
        });
    }
    Ok(())
}
