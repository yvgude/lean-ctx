// SPDX-License-Identifier: Apache-2.0

//! Engine receipts v2 (C0 2.5 `schemas/receipts.schema.json`, K2 review M3).
//!
//! `POST /v1/receipts` [`ReceiptsRequestV2`] → [`ReceiptsAckV2`]: closed
//! engine receipt documents ([`EngineReceiptV2`], invocation + observation),
//! signed by the instance key (Ed25519 over [`RECEIPTS_DOMAIN`] ‖
//! canonical_json(body with `key_id`, without `signature`), as engine
//! metrics). Hive refuses v1 documents.
//!
//! No content, path or reference leaves the engine, enforced by the types:
//! input and output are [`ReceiptHmac`] values under an engine-local key
//! Hive never receives; source references are a count; failure recovery
//! references do not exist here; the only digests are the Hive-issued
//! policy digest and the item digest over this document. Every string is a
//! closed-pattern newtype or a closed enum and every struct refuses unknown
//! fields.

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::metrics_v2::{KeyId, MAX_COUNT, SignatureHex, TimeStamp, VaultId};
use super::metrics_v2::{closed_string, schema_const};
use crate::ValidationError;

pub const RECEIPTS_V2_SCHEMA: &str = "leanctx.hive.receipts/v2";
pub const RECEIPTS_V2_ACK_SCHEMA: &str = "leanctx.hive.receipts-ack/v2";
pub const ENGINE_RECEIPT_V2_SCHEMA: &str = "leanctx.engine-receipt/v2";
/// Domain of the batch signature (instance key).
pub const RECEIPTS_DOMAIN: &str = "leanctx.hive.receipts.v2\n";
/// `POST /v1/receipts`, relative to the engine API base.
pub const RECEIPTS_ROUTE: &str = "/receipts";
/// The only engine id a v2 receipt names.
pub const RECEIPT_ENGINE_ID: &str = "lean-ctx-local";
/// The only capability a v2 receipt names.
pub const RECEIPT_CAPABILITY_ID: &str = "capability://leanctx/context-optimization";
/// Prefix of a [`ReceiptHmac`].
pub const HMAC_PREFIX: &str = "hmac-sha256:";

pub const MAX_RECEIPTS_V2: usize = 256;
pub const MAX_RECEIPTS_BODY_BYTES: usize = 1 << 20;
pub const MAX_MEASUREMENTS: usize = 32;
pub const MAX_SOURCE_COUNT: u16 = 256;

fn is_lower_hex(value: &str) -> bool {
    value
        .bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn is_hmac(value: &str) -> bool {
    value
        .strip_prefix(HMAC_PREFIX)
        .is_some_and(|hex| hex.len() == 64 && is_lower_hex(hex))
}

fn is_sha256(value: &str) -> bool {
    value
        .strip_prefix("sha256:")
        .is_some_and(|hex| hex.len() == 64 && is_lower_hex(hex))
}

fn is_receipt_id(value: &str) -> bool {
    value.strip_prefix("rcpt_").is_some_and(|rest| {
        (1..=64).contains(&rest.len())
            && rest
                .bytes()
                .all(|b| b.is_ascii_digit() || b.is_ascii_lowercase())
    })
}

fn is_invocation_id(value: &str) -> bool {
    (32..=64).contains(&value.len()) && is_lower_hex(value)
}

/// `^[0-9]{1,4}\.[0-9]{1,4}\.[0-9]{1,6}(-[0-9A-Za-z.]{1,32})?$`
fn is_semver(value: &str) -> bool {
    let (core, pre) = match value.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (value, None),
    };
    let parts: Vec<&str> = core.split('.').collect();
    let digits =
        |p: &str, max: usize| (1..=max).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_digit());
    parts.len() == 3
        && digits(parts[0], 4)
        && digits(parts[1], 4)
        && digits(parts[2], 6)
        && pre.is_none_or(|p| {
            (1..=32).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.')
        })
}

/// `^[a-z0-9_-]{1,64}$` and never a run of 24+ letters or digits.
fn is_measurement_name(value: &str) -> bool {
    let shape = (1..=64).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'));
    let mut run = 0usize;
    let token_like = value.bytes().any(|b| {
        run = if b.is_ascii_alphanumeric() {
            run + 1
        } else {
            0
        };
        run >= 24
    });
    shape && !token_like
}

closed_string!(
    /// `hmac-sha256:<64 hex>` under the engine-local receipt key.
    ReceiptHmac, is_hmac, "receipt hmac"
);
closed_string!(
    /// `sha256:<64 hex>`: the Hive-issued policy digest or the item digest.
    Sha256Ref, is_sha256, "sha256 digest"
);
closed_string!(
    /// `rcpt_<1-64 of [0-9a-z]>`.
    ReceiptIdV2, is_receipt_id, "receipt id"
);
closed_string!(
    /// 32-64 lowercase hex, derived by hashing (never a path or name).
    InvocationIdV2, is_invocation_id, "invocation id"
);
closed_string!(
    /// `major.minor.patch[-pre]` as the contract allows it.
    ReceiptSemver, is_semver, "version"
);
closed_string!(
    /// A measurement name (never token-like).
    MeasurementName, is_measurement_name, "measurement name"
);

schema_const!(ReceiptsSchema, RECEIPTS_V2_SCHEMA);
schema_const!(ReceiptsAckSchema, RECEIPTS_V2_ACK_SCHEMA);
schema_const!(EngineReceiptSchema, ENGINE_RECEIPT_V2_SCHEMA);
schema_const!(EngineIdConst, RECEIPT_ENGINE_ID);
schema_const!(CapabilityIdConst, RECEIPT_CAPABILITY_ID);
schema_const!(EngineKindConst, "engine");

/// `schema_version: 1` and nothing else.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SchemaVersionOne;

impl Serialize for SchemaVersionOne {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u8(1)
    }
}

impl<'de> Deserialize<'de> for SchemaVersionOne {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        (u64::deserialize(deserializer)? == 1)
            .then_some(Self)
            .ok_or_else(|| serde::de::Error::custom("schema_version must be 1"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeasurementUnit {
    Tokens,
    Bytes,
    Ms,
    Count,
    Lines,
    RatioPpm,
}

impl MeasurementUnit {
    /// The unit a local measurement names, `None` outside the closed set.
    pub fn of(unit: &str) -> Option<Self> {
        Some(match unit {
            "tokens" => Self::Tokens,
            "bytes" => Self::Bytes,
            "ms" => Self::Ms,
            "count" => Self::Count,
            "lines" => Self::Lines,
            "ratio_ppm" => Self::RatioPpm,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Classification {
    Measured,
    Estimated,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdmissionDecision {
    Admitted,
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationStatus {
    Succeeded,
    Degraded,
    Rejected,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureCode {
    PolicyRejected,
    SourceUnavailable,
    SourceIntegrityMismatch,
    ResourceLimit,
    UnsupportedOperation,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptEngineV2 {
    pub engine_id: EngineIdConst,
    pub engine_version: ReceiptSemver,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptOperationV2 {
    pub capability_id: CapabilityIdConst,
    pub capability_version: ReceiptSemver,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyAdmissionV2 {
    pub decision: AdmissionDecision,
    /// The effective-policy digest Hive issued, or `null`.
    pub policy_digest: Option<Sha256Ref>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptInvocationV2 {
    pub schema_version: SchemaVersionOne,
    pub invocation_id: InvocationIdV2,
    pub engine: ReceiptEngineV2,
    pub operation: ReceiptOperationV2,
    pub input_hmac: ReceiptHmac,
    /// 1-256.
    pub source_count: u16,
    pub policy_admission: PolicyAdmissionV2,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeasurementV2 {
    pub name: MeasurementName,
    pub unit: MeasurementUnit,
    pub classification: Classification,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FailureV2 {
    pub code: FailureCode,
    pub retryable_by_host: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptObservationV2 {
    pub schema_version: SchemaVersionOne,
    pub invocation_id: InvocationIdV2,
    pub status: ObservationStatus,
    pub output_hmac: Option<ReceiptHmac>,
    /// 0-256.
    pub source_count: u16,
    pub measurements: Vec<MeasurementV2>,
    pub failure: Option<FailureV2>,
}

/// The receipt artifact as uploaded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineReceiptV2 {
    pub schema: EngineReceiptSchema,
    pub invocation: ReceiptInvocationV2,
    pub observation: ReceiptObservationV2,
}

impl EngineReceiptV2 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        let (i, o) = (&self.invocation, &self.observation);
        if i.invocation_id != o.invocation_id {
            return Err(ValidationError::new("receipt: invocation_id differs"));
        }
        if !(1..=MAX_SOURCE_COUNT).contains(&i.source_count) || o.source_count > MAX_SOURCE_COUNT {
            return Err(ValidationError::new("receipt: source_count out of range"));
        }
        if o.measurements.len() > MAX_MEASUREMENTS
            || o.measurements
                .iter()
                .any(|m| m.value.is_some_and(|v| v > MAX_COUNT))
        {
            return Err(ValidationError::new("receipt: measurements"));
        }
        Ok(())
    }

    /// `sha256:` of canonical_json(document).
    pub fn digest(&self) -> Result<Sha256Ref, ValidationError> {
        use sha2::Digest as _;
        let value = serde_json::to_value(self)
            .map_err(|e| ValidationError::new(format!("receipt: encode: {e}")))?;
        let bytes = super::canonical_json(&value)?;
        let hex: String = sha2::Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        Sha256Ref::new(&format!("sha256:{hex}"))
            .ok_or_else(|| ValidationError::new("receipt: digest"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptItemV2 {
    pub receipt_id: ReceiptIdV2,
    pub kind: EngineKindConst,
    pub vault_id: VaultId,
    pub created_at: TimeStamp,
    pub digest: Sha256Ref,
    pub document: EngineReceiptV2,
}

impl ReceiptItemV2 {
    /// An item over `document`, its digest computed here.
    pub fn new(
        receipt_id: ReceiptIdV2,
        vault_id: VaultId,
        created_at: TimeStamp,
        document: EngineReceiptV2,
    ) -> Result<Self, ValidationError> {
        document.validate()?;
        Ok(Self {
            receipt_id,
            kind: EngineKindConst,
            vault_id,
            created_at,
            digest: document.digest()?,
            document,
        })
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        self.document.validate()?;
        if self.document.digest()? != self.digest {
            return Err(ValidationError::new(
                "receipt: digest does not bind the document",
            ));
        }
        Ok(())
    }
}

/// The signed part of a batch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptsBodyV2 {
    pub schema: ReceiptsSchema,
    pub receipts: Vec<ReceiptItemV2>,
}

impl ReceiptsBodyV2 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.receipts.is_empty() || self.receipts.len() > MAX_RECEIPTS_V2 {
            return Err(ValidationError::new("receipts: 1-256 per batch"));
        }
        let mut ids = BTreeSet::new();
        if !self
            .receipts
            .iter()
            .all(|r| ids.insert(r.receipt_id.as_str()))
        {
            return Err(ValidationError::new("receipts: duplicate receipt_id"));
        }
        self.receipts.iter().try_for_each(ReceiptItemV2::validate)
    }

    /// The bytes the signature covers: the batch with `key_id` and without
    /// `signature`.
    pub fn signing_input(&self, key_id: &KeyId) -> Result<Vec<u8>, ValidationError> {
        let mut value = serde_json::to_value(self)
            .map_err(|e| ValidationError::new(format!("receipts: encode: {e}")))?;
        value["key_id"] = serde_json::Value::String(key_id.as_str().to_string());
        super::signing_input(RECEIPTS_DOMAIN, &value)
    }

    pub fn signed(self, key_id: KeyId, signature: SignatureHex) -> ReceiptsRequestV2 {
        ReceiptsRequestV2 {
            schema: self.schema,
            receipts: self.receipts,
            key_id,
            signature,
        }
    }
}

/// `POST /v1/receipts` body, at most 1 MiB.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptsRequestV2 {
    pub schema: ReceiptsSchema,
    pub receipts: Vec<ReceiptItemV2>,
    pub key_id: KeyId,
    pub signature: SignatureHex,
}

impl ReceiptsRequestV2 {
    pub fn body(&self) -> ReceiptsBodyV2 {
        ReceiptsBodyV2 {
            schema: self.schema,
            receipts: self.receipts.clone(),
        }
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        self.body().validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptsAckV2 {
    pub schema: ReceiptsAckSchema,
    pub accepted: u16,
    pub duplicates: u16,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_strings_follow_the_contract_patterns() {
        assert!(ReceiptHmac::new(&format!("hmac-sha256:{}", "a".repeat(64))).is_some());
        for bad in [
            format!("sha256:{}", "a".repeat(64)),
            format!("hmac-sha256:{}", "A".repeat(64)),
            "hmac-sha256:".to_string(),
        ] {
            assert!(ReceiptHmac::new(&bad).is_none(), "{bad}");
        }
        assert!(ReceiptIdV2::new("rcpt_01jb8z5").is_some());
        assert!(ReceiptIdV2::new("engine-receipt-1").is_none());
        assert!(InvocationIdV2::new(&"0".repeat(32)).is_some());
        assert!(InvocationIdV2::new(&"0".repeat(31)).is_none());
        assert!(InvocationIdV2::new("src/main.rs").is_none());
        for good in ["3.12.0", "1.0.0", "3.12.0-rc.1"] {
            assert!(ReceiptSemver::new(good).is_some(), "{good}");
        }
        for bad in ["3.12", "3.12.0-", "3.12.0-a b", "v3.12.0"] {
            assert!(ReceiptSemver::new(bad).is_none(), "{bad}");
        }
        assert!(MeasurementName::new("tokens_baseline").is_some());
        for bad in ["skprojabcdefghijklmnopqrstuvwxyz", "/etc/passwd", "A", ""] {
            assert!(MeasurementName::new(bad).is_none(), "{bad}");
        }
    }
}
