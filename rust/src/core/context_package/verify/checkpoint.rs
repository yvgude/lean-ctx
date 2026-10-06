// SPDX-License-Identifier: Apache-2.0

//! Versioned checkpoint carriers share the package integrity/signing boundary.

use lean_ctx_protocol::{ContextCheckpointV1, ContextCheckpointV2, ContextCheckpointV3};

use super::super::content::{
    CHECKPOINT_PACKAGE_SCHEMA_V1, CHECKPOINT_PACKAGE_SCHEMA_V2, CHECKPOINT_PACKAGE_SCHEMA_V3,
    CHECKPOINT_PACKAGE_SCHEMA_V4, CheckpointPackageContentV1, MAX_CHECKPOINT_PACKAGE_BYTES,
};

pub(super) fn validate_checkpoint_content(
    portable: &CheckpointPackageContentV1,
    errors: &mut Vec<String>,
) {
    let Ok(encoded) = serde_json::to_string(portable) else {
        errors.push("checkpoint content cannot be encoded as JSON".into());
        return;
    };
    if encoded.len() > MAX_CHECKPOINT_PACKAGE_BYTES {
        errors.push(format!(
            "checkpoint content exceeds {MAX_CHECKPOINT_PACKAGE_BYTES} byte cap"
        ));
        return;
    }
    match portable.schema_version.as_str() {
        CHECKPOINT_PACKAGE_SCHEMA_V1 => {
            super::validate_checkpoint_object(&portable.checkpoint, errors);
            super::validate_migration_provenance(
                portable.migration_provenance.as_ref(),
                &portable.checkpoint,
                errors,
            );
        }
        CHECKPOINT_PACKAGE_SCHEMA_V2
        | CHECKPOINT_PACKAGE_SCHEMA_V3
        | CHECKPOINT_PACKAGE_SCHEMA_V4 => {
            validate_live_checkpoint(portable, errors);
        }
        _ => {
            errors.push("unsupported checkpoint package schema".into());
            return;
        }
    }
    validate_portability(portable, errors);
    if !crate::core::secret_detection::detect_secrets(&encoded).is_empty() {
        errors.push("checkpoint content contains credential-shaped material".into());
    }
}

fn validate_live_checkpoint(portable: &CheckpointPackageContentV1, errors: &mut Vec<String>) {
    // The old SDK migration evidence describes a different state universe;
    // accepting it here would invent a migration or silently discard authority.
    if portable.migration_provenance.is_some() || !portable.non_portable_fields.is_empty() {
        errors.push("live checkpoints cannot carry SDK migration or machine-local fields".into());
    }
    let Some(canonical) = canonical_live_checkpoint(portable) else {
        // Version dispatch is exact; do not echo untrusted state or demote V2.
        errors.push("invalid live checkpoint or carrier/checkpoint version mismatch".into());
        return;
    };
    // This new carrier accepts a fully expressed canonical value, not a legacy
    // shape whose defaults would change its identity during continuation.
    if !serde_json::from_slice::<serde_json::Value>(&canonical)
        .is_ok_and(|value| value == portable.checkpoint)
    {
        errors.push("live checkpoint requires its complete canonical value".into());
    }
}

fn canonical_live_checkpoint(portable: &CheckpointPackageContentV1) -> Option<Vec<u8>> {
    match portable.schema_version.as_str() {
        CHECKPOINT_PACKAGE_SCHEMA_V2 => {
            let checkpoint: ContextCheckpointV1 =
                serde_json::from_value(portable.checkpoint.clone()).ok()?;
            let bytes = checkpoint.canonical_bytes().ok()?;
            ContextCheckpointV1::from_canonical_bytes(&bytes).ok()?;
            Some(bytes)
        }
        CHECKPOINT_PACKAGE_SCHEMA_V3 => {
            let checkpoint: ContextCheckpointV2 =
                serde_json::from_value(portable.checkpoint.clone()).ok()?;
            let bytes = checkpoint.canonical_bytes().ok()?;
            ContextCheckpointV2::from_canonical_bytes(&bytes).ok()?;
            Some(bytes)
        }
        CHECKPOINT_PACKAGE_SCHEMA_V4 => {
            let checkpoint: ContextCheckpointV3 =
                serde_json::from_value(portable.checkpoint.clone()).ok()?;
            let bytes = checkpoint.canonical_bytes().ok()?;
            ContextCheckpointV3::from_canonical_bytes(&bytes).ok()?;
            Some(bytes)
        }
        _ => None,
    }
}

fn validate_portability(portable: &CheckpointPackageContentV1, errors: &mut Vec<String>) {
    let mut absolute_paths = Vec::new();
    super::collect_non_portable_paths(&portable.checkpoint, "$.checkpoint", &mut absolute_paths);
    absolute_paths.sort();
    absolute_paths.dedup();
    let mut declared = portable.non_portable_fields.clone();
    declared.sort();
    declared.dedup();
    if declared != portable.non_portable_fields
        || declared.len() > 256
        || declared
            .iter()
            .any(|item| item.is_empty() || item.len() > 1024 || item.chars().any(char::is_control))
        || declared != absolute_paths
    {
        errors.push(
            "non_portable_fields must exactly classify every machine-local absolute path".into(),
        );
    }
}
