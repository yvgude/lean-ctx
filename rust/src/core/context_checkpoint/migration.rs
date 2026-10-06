// SPDX-License-Identifier: Apache-2.0

//! Strict, bounded migration into the canonical V1 checkpoint wire contract.

use std::fmt;

use lean_ctx_protocol::{
    ContextCheckpointMigrationV1, ContextCheckpointV1, MAX_CONTEXT_CHECKPOINT_ENCODED_BYTES,
    Sha256Digest,
};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

pub(super) const MIGRATION_SIGNATURE_DOMAIN: &[u8] = b"leanctx/context-checkpoint-migration/v1\0";

/// Exact source form accepted by the V1 migrator.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextCheckpointMigrationKindV1 {
    CanonicalV1,
    LegacyV1MissingUpdatedAt,
}

/// Content-addressed proof of a deterministic checkpoint migration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCheckpointMigrationReceiptV1 {
    schema_version: u32,
    kind: ContextCheckpointMigrationKindV1,
    source_digest: Sha256Digest,
    output_digest: Sha256Digest,
}

impl ContextCheckpointMigrationReceiptV1 {
    pub const SCHEMA_VERSION: u32 = 1;
    pub fn kind(&self) -> ContextCheckpointMigrationKindV1 {
        self.kind
    }
    pub fn source_digest(&self) -> &Sha256Digest {
        &self.source_digest
    }
    pub fn output_digest(&self) -> &Sha256Digest {
        &self.output_digest
    }
    pub fn signing_payload(&self) -> Vec<u8> {
        let mut bytes = MIGRATION_SIGNATURE_DOMAIN.to_vec();
        bytes.extend(crate::core::canonical::canonical_serialize(self));
        bytes
    }
}

/// Validated output and provenance receipt returned by a migration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextCheckpointMigrationResultV1 {
    checkpoint: ContextCheckpointV1,
    receipt: ContextCheckpointMigrationReceiptV1,
}

impl ContextCheckpointMigrationResultV1 {
    pub fn checkpoint(&self) -> &ContextCheckpointV1 {
        &self.checkpoint
    }
    pub fn receipt(&self) -> &ContextCheckpointMigrationReceiptV1 {
        &self.receipt
    }
    pub fn into_parts(self) -> (ContextCheckpointV1, ContextCheckpointMigrationReceiptV1) {
        (self.checkpoint, self.receipt)
    }
}

/// Fail-closed migration error; unsupported shapes are never guessed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContextCheckpointMigrationErrorV1 {
    EncodedLimitExceeded,
    InvalidJson(String),
    UnsupportedSchema,
    NonCanonicalSource,
    InvalidCheckpoint(String),
}

impl fmt::Display for ContextCheckpointMigrationErrorV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for ContextCheckpointMigrationErrorV1 {}

/// Stateless deterministic migrator for the only deployed V1 predecessor.
#[derive(Clone, Copy, Debug, Default)]
pub struct StrictContextCheckpointMigratorV1;

impl StrictContextCheckpointMigratorV1 {
    pub fn migrate_with_receipt(
        &self,
        bytes: &[u8],
    ) -> Result<ContextCheckpointMigrationResultV1, ContextCheckpointMigrationErrorV1> {
        if bytes.len() > MAX_CONTEXT_CHECKPOINT_ENCODED_BYTES {
            return Err(ContextCheckpointMigrationErrorV1::EncodedLimitExceeded);
        }
        let value: Value = serde_json::from_slice(bytes)
            .map_err(|error| ContextCheckpointMigrationErrorV1::InvalidJson(error.to_string()))?;
        if value.get("schema_version").and_then(Value::as_u64) != Some(1) {
            return Err(ContextCheckpointMigrationErrorV1::UnsupportedSchema);
        }
        let missing_updated_at = value.get("updated_at").is_none();
        let canonical_source = canonical_json_bytes(&value)?;
        if canonical_source != bytes {
            return Err(ContextCheckpointMigrationErrorV1::NonCanonicalSource);
        }
        let checkpoint: ContextCheckpointV1 = serde_json::from_value(value).map_err(|error| {
            ContextCheckpointMigrationErrorV1::InvalidCheckpoint(error.to_string())
        })?;
        checkpoint.validate().map_err(|error| {
            ContextCheckpointMigrationErrorV1::InvalidCheckpoint(error.to_string())
        })?;
        let kind = if missing_updated_at {
            ContextCheckpointMigrationKindV1::LegacyV1MissingUpdatedAt
        } else {
            ContextCheckpointMigrationKindV1::CanonicalV1
        };
        let source_digest =
            Sha256Digest::new(format!("sha256:{}", hex::encode(Sha256::digest(bytes)))).map_err(
                |error| ContextCheckpointMigrationErrorV1::InvalidCheckpoint(error.to_string()),
            )?;
        let output_digest = checkpoint.digest().map_err(|error| {
            ContextCheckpointMigrationErrorV1::InvalidCheckpoint(error.to_string())
        })?;
        Ok(ContextCheckpointMigrationResultV1 {
            checkpoint,
            receipt: ContextCheckpointMigrationReceiptV1 {
                schema_version: ContextCheckpointMigrationReceiptV1::SCHEMA_VERSION,
                kind,
                source_digest,
                output_digest,
            },
        })
    }
}

impl ContextCheckpointMigrationV1 for StrictContextCheckpointMigratorV1 {
    type Error = ContextCheckpointMigrationErrorV1;

    fn migrate(&self, bytes: &[u8]) -> Result<ContextCheckpointV1, Self::Error> {
        self.migrate_with_receipt(bytes)
            .map(|result| result.checkpoint)
    }
}

fn canonical_json_bytes(value: &Value) -> Result<Vec<u8>, ContextCheckpointMigrationErrorV1> {
    let mut value = value.clone();
    sort_value(&mut value);
    serde_json::to_vec(&value)
        .map_err(|error| ContextCheckpointMigrationErrorV1::InvalidJson(error.to_string()))
}

fn sort_value(value: &mut Value) {
    match value {
        Value::Object(object) => {
            let mut entries: Vec<_> = std::mem::take(object).into_iter().collect();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            for (_, child) in &mut entries {
                sort_value(child);
            }
            object.extend(entries);
        }
        Value::Array(values) => values.iter_mut().for_each(sort_value),
        _ => {}
    }
}
