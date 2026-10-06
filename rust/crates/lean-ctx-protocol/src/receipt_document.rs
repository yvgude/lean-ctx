//! Canonical signed local evidence receipt.
//!
//! `ReceiptDocumentV1` is the immutable, digest-addressed record for one
//! Task -> Plan -> Invocation -> Receipt -> Outcome lineage.  It deliberately
//! contains metadata and references only: prompts, completions, source files,
//! credentials, headers, and other private payloads belong in separately
//! persisted evidence artifacts.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Write as _};

use serde::de::{MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::{
    AcceptanceState, CapabilityId, OutcomeId, PlanId, ProtocolReference, SemanticVersion,
    Sha256Digest, SignatureStatus, TaskId, UtcTimestamp, ValidationError,
    deserialize_schema_version, validate_bounded_opaque_identifier, validate_schema_version,
};

/// Maximum array length in the receipt wire contract.
pub const MAX_RECEIPT_ITEMS: usize = 64;

/// Maximum integer that round-trips exactly through every v1 JSON consumer.
pub const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

const MAX_ITEMS: usize = MAX_RECEIPT_ITEMS;

/// Terminal state of the recorded execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptTerminalStatusV1 {
    Succeeded,
    Failed,
    Rejected,
    Cancelled,
    TimedOut,
}

/// Provenance class of one integer observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptValueClassificationV1 {
    Measured,
    Estimated,
    Calculated,
    Reconciled,
    Unavailable,
}

/// Kind of payload referenced by a receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptEvidenceKindV1 {
    Measurement,
    Assumption,
    Formula,
    PriceTable,
    Invoice,
    Outcome,
    Runtime,
    Methodology,
}

/// Strict digest-only evidence reference; payload bytes are never embedded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptEvidenceRefV1 {
    pub kind: ReceiptEvidenceKindV1,
    pub uri: ProtocolReference,
    pub digest: Sha256Digest,
    pub media_type: String,
    pub signature_status: SignatureStatus,
}

/// One named integer fact with enough provenance to interpret its claim class.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptValueV1 {
    pub name: String,
    pub unit: String,
    pub classification: ReceiptValueClassificationV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_digests: Vec<Sha256Digest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub formula_digest: Option<Sha256Digest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price_table_digest: Option<Sha256Digest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reconciliation_digest: Option<Sha256Digest>,
}

impl ReceiptValueV1 {
    fn validate(
        &self,
        evidence: &BTreeMap<&str, ReceiptEvidenceKindV1>,
    ) -> Result<(), ValidationError> {
        validate_name(&self.name, "ReceiptValueV1 name")?;
        validate_name(&self.unit, "ReceiptValueV1 unit")?;
        validate_unique_digests(&self.evidence_digests, "ReceiptValueV1 evidence_digests")?;
        if self
            .evidence_digests
            .iter()
            .any(|digest| !evidence.contains_key(digest.as_str()))
        {
            return Err(ValidationError::new(
                "ReceiptValueV1 references evidence absent from the receipt",
            ));
        }

        let direct = self
            .evidence_digests
            .iter()
            .map(|digest| digest.as_str())
            .collect::<BTreeSet<_>>();
        let mut derived = BTreeSet::new();
        for (field, digest) in [
            ("formula_digest", self.formula_digest.as_ref()),
            ("price_table_digest", self.price_table_digest.as_ref()),
            ("reconciliation_digest", self.reconciliation_digest.as_ref()),
        ] {
            if let Some(digest) = digest {
                if direct.contains(digest.as_str()) || !derived.insert(digest.as_str()) {
                    return Err(ValidationError::new(format!(
                        "ReceiptValueV1 {field} must occur exactly once"
                    )));
                }
                if !evidence.contains_key(digest.as_str()) {
                    return Err(ValidationError::new(format!(
                        "ReceiptValueV1 {field} references evidence absent from the receipt"
                    )));
                }
            }
        }
        if self.value.is_some_and(|value| value > MAX_SAFE_INTEGER) {
            return Err(ValidationError::new(
                "ReceiptValueV1 value exceeds the cross-language safe integer ceiling",
            ));
        }

        let has_value = self.value.is_some();
        let has_sources = !self.evidence_digests.is_empty();
        match self.classification {
            ReceiptValueClassificationV1::Unavailable => {
                if has_value
                    || has_sources
                    || self.formula_digest.is_some()
                    || self.price_table_digest.is_some()
                    || self.reconciliation_digest.is_some()
                {
                    return Err(ValidationError::new(
                        "unavailable values must omit value and provenance",
                    ));
                }
            }
            ReceiptValueClassificationV1::Measured | ReceiptValueClassificationV1::Estimated => {
                if !has_value || !has_sources {
                    return Err(ValidationError::new(
                        "measured and estimated values require a value and evidence",
                    ));
                }
                let expected_kind = match self.classification {
                    ReceiptValueClassificationV1::Measured => ReceiptEvidenceKindV1::Measurement,
                    ReceiptValueClassificationV1::Estimated => ReceiptEvidenceKindV1::Assumption,
                    _ => unreachable!("classification branch is exhaustive"),
                };
                if !self
                    .evidence_digests
                    .iter()
                    .any(|digest| evidence.get(digest.as_str()) == Some(&expected_kind))
                {
                    return Err(ValidationError::new(
                        "measured and estimated values require matching evidence kind",
                    ));
                }
                if self.formula_digest.is_some()
                    || self.price_table_digest.is_some()
                    || self.reconciliation_digest.is_some()
                {
                    return Err(ValidationError::new(
                        "measured and estimated values cannot claim calculation or reconciliation",
                    ));
                }
            }
            ReceiptValueClassificationV1::Calculated => {
                if !has_value
                    || !has_sources
                    || self.formula_digest.is_none()
                    || self.price_table_digest.is_none()
                    || self.reconciliation_digest.is_some()
                {
                    return Err(ValidationError::new(
                        "calculated values require value, evidence, formula and price-table digests",
                    ));
                }
                if !self.evidence_digests.iter().all(|digest| {
                    matches!(
                        evidence.get(digest.as_str()),
                        Some(
                            ReceiptEvidenceKindV1::Measurement | ReceiptEvidenceKindV1::Assumption
                        )
                    )
                }) || !matches!(
                    self.formula_digest
                        .as_ref()
                        .and_then(|digest| evidence.get(digest.as_str())),
                    Some(ReceiptEvidenceKindV1::Formula)
                ) || !matches!(
                    self.price_table_digest
                        .as_ref()
                        .and_then(|digest| evidence.get(digest.as_str())),
                    Some(ReceiptEvidenceKindV1::PriceTable)
                ) {
                    return Err(ValidationError::new(
                        "calculated values require measurement/assumption, formula and price-table evidence",
                    ));
                }
            }
            ReceiptValueClassificationV1::Reconciled => {
                let Some(reconciliation) = self.reconciliation_digest.as_ref() else {
                    return Err(ValidationError::new(
                        "reconciled values require invoice evidence",
                    ));
                };
                if !has_value
                    || !has_sources
                    || self.formula_digest.is_none()
                    || self.price_table_digest.is_none()
                {
                    return Err(ValidationError::new(
                        "reconciled values require calculated provenance and listed invoice evidence",
                    ));
                }
                if !self.evidence_digests.iter().all(|digest| {
                    matches!(
                        evidence.get(digest.as_str()),
                        Some(
                            ReceiptEvidenceKindV1::Measurement | ReceiptEvidenceKindV1::Assumption
                        )
                    )
                }) || !matches!(
                    self.formula_digest
                        .as_ref()
                        .and_then(|digest| evidence.get(digest.as_str())),
                    Some(ReceiptEvidenceKindV1::Formula)
                ) || !matches!(
                    self.price_table_digest
                        .as_ref()
                        .and_then(|digest| evidence.get(digest.as_str())),
                    Some(ReceiptEvidenceKindV1::PriceTable)
                ) || evidence.get(reconciliation.as_str())
                    != Some(&ReceiptEvidenceKindV1::Invoice)
                {
                    return Err(ValidationError::new(
                        "reconciled values require invoice, formula and price-table evidence",
                    ));
                }
            }
        }
        Ok(())
    }
}

/// Version-pinned capability that participated in the invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptCapabilityLinkV1 {
    pub capability_id: CapabilityId,
    pub capability_version: SemanticVersion,
    pub invocation_ref: Sha256Digest,
}

/// Immutable identity map for the execution lineage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptLineageV1 {
    pub task_id: TaskId,
    pub task_ref: Sha256Digest,
    pub plan_id: PlanId,
    pub plan_ref: Sha256Digest,
    pub invocation_id: String,
    pub invocation_ref: Sha256Digest,
    pub identity_ref: Sha256Digest,
    pub policy_refs: Vec<Sha256Digest>,
    pub capabilities: Vec<ReceiptCapabilityLinkV1>,
}

impl ReceiptLineageV1 {
    fn validate(&self) -> Result<(), ValidationError> {
        validate_bounded_opaque_identifier(&self.invocation_id, "ReceiptLineageV1 invocation_id")?;
        validate_unique_digests(&self.policy_refs, "ReceiptLineageV1 policy_refs")?;
        if self.policy_refs.is_empty() {
            return Err(ValidationError::new(
                "ReceiptLineageV1 requires at least one policy reference",
            ));
        }
        if self.capabilities.is_empty() || self.capabilities.len() > MAX_ITEMS {
            return Err(ValidationError::new(
                "ReceiptLineageV1 capabilities must contain 1..=64 entries",
            ));
        }
        let mut unique = BTreeSet::new();
        for capability in &self.capabilities {
            if capability.invocation_ref != self.invocation_ref {
                return Err(ValidationError::new(
                    "ReceiptLineageV1 capability invocation_ref must bind the invocation",
                ));
            }
            if !unique.insert((
                capability.capability_id.as_str(),
                capability.capability_version.as_str(),
            )) {
                return Err(ValidationError::new(
                    "ReceiptLineageV1 capabilities must be unique",
                ));
            }
        }
        Ok(())
    }
}

/// Link to the separately persisted outcome observation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptOutcomeLinkV1 {
    pub state: AcceptanceState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome_id: Option<OutcomeId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome_ref: Option<Sha256Digest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acceptance_evidence_digest: Option<Sha256Digest>,
}

impl ReceiptOutcomeLinkV1 {
    fn validate(
        &self,
        evidence: &BTreeMap<&str, ReceiptEvidenceKindV1>,
    ) -> Result<(), ValidationError> {
        match self.state {
            AcceptanceState::Unknown => {
                if self.outcome_id.is_some()
                    || self.outcome_ref.is_some()
                    || self.acceptance_evidence_digest.is_some()
                {
                    return Err(ValidationError::new(
                        "unknown outcome must not claim an outcome or acceptance observation",
                    ));
                }
            }
            AcceptanceState::Rejected => {
                let Some(outcome_ref) = self.outcome_ref.as_ref() else {
                    return Err(ValidationError::new(
                        "rejected outcome requires outcome_id and outcome_ref",
                    ));
                };
                if self.outcome_id.is_none()
                    || evidence.get(outcome_ref.as_str()) != Some(&ReceiptEvidenceKindV1::Outcome)
                    || self.acceptance_evidence_digest.is_some()
                {
                    return Err(ValidationError::new(
                        "rejected outcome requires listed outcome evidence and no acceptance claim",
                    ));
                }
            }
            AcceptanceState::Accepted => {
                let Some(outcome_ref) = self.outcome_ref.as_ref() else {
                    return Err(ValidationError::new(
                        "accepted outcome requires outcome_id and outcome_ref",
                    ));
                };
                let Some(digest) = self.acceptance_evidence_digest.as_ref() else {
                    return Err(ValidationError::new(
                        "accepted outcome requires identified acceptance evidence",
                    ));
                };
                if self.outcome_id.is_none()
                    || evidence.get(outcome_ref.as_str()) != Some(&ReceiptEvidenceKindV1::Outcome)
                    || !evidence.contains_key(digest.as_str())
                    || outcome_ref == digest
                {
                    return Err(ValidationError::new(
                        "accepted outcome requires distinct listed outcome and acceptance evidence",
                    ));
                }
            }
        }
        Ok(())
    }
}

/// Chain predecessor covered by this receipt signature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptChainLinkV1 {
    pub chain_id: String,
    pub sequence_number: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_receipt_id: Option<Sha256Digest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_signature_digest: Option<Sha256Digest>,
}

/// Admission mode for signer keys.  No key bytes are embedded in a receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptKeyAdmissionV1 {
    ExternalTrustStore,
}

/// Explicit signer identity without an embedded trust root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptSignerV1 {
    pub algorithm: String,
    pub key_id: String,
    pub key_admission: ReceiptKeyAdmissionV1,
}

/// Canonical signed receipt document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptDocumentV1 {
    #[serde(deserialize_with = "deserialize_schema_version")]
    pub schema_version: u32,
    pub receipt_id: Sha256Digest,
    pub lineage: ReceiptLineageV1,
    pub chain: ReceiptChainLinkV1,
    pub status: ReceiptTerminalStatusV1,
    pub values: Vec<ReceiptValueV1>,
    pub outcome: ReceiptOutcomeLinkV1,
    pub evidence_refs: Vec<ReceiptEvidenceRefV1>,
    pub issued_at: UtcTimestamp,
    pub signer: ReceiptSignerV1,
    pub signature: String,
}

impl ReceiptDocumentV1 {
    /// Canonical bytes for the complete document, including signature.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ValidationError> {
        canonical_without(self, &[])
    }

    /// Return canonical JSON as UTF-8 text without changing its bytes.
    pub fn canonical_json(&self) -> Result<String, ValidationError> {
        String::from_utf8(self.canonical_bytes()?)
            .map_err(|error| ValidationError::new(format!("canonical receipt UTF-8: {error}")))
    }

    /// Canonical bytes used to derive the receipt ID.
    pub fn identity_bytes(&self) -> Result<Vec<u8>, ValidationError> {
        canonical_without(self, &["receipt_id", "signature"])
    }

    /// Canonical bytes covered by the Ed25519 signature.
    pub fn signing_bytes(&self) -> Result<Vec<u8>, ValidationError> {
        canonical_without(self, &["signature"])
    }

    /// Alias used by verifiers when requesting signature coverage bytes.
    pub fn signature_bytes(&self) -> Result<Vec<u8>, ValidationError> {
        self.signing_bytes()
    }

    /// Derive the canonical SHA-256 receipt identity.
    pub fn derived_receipt_id(&self) -> Result<Sha256Digest, ValidationError> {
        digest_bytes(&self.identity_bytes()?)
    }

    /// Decode exactly canonical UTF-8 JSON and validate all protocol joins.
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self, ValidationError> {
        let value = strict_json_value(bytes)?;
        let document = serde_json::from_value::<Self>(value)
            .map_err(|error| ValidationError::new(format!("decode receipt document: {error}")))?;
        if document.canonical_bytes()? != bytes {
            return Err(ValidationError::new(
                "receipt document JSON is not canonical UTF-8, compactness or key order",
            ));
        }
        document.validate()?;
        Ok(document)
    }

    /// Compatibility alias for callers that name the wire input `from_bytes`.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ValidationError> {
        Self::from_canonical_bytes(bytes)
    }

    /// Compatibility alias for callers that name the wire input JSON.
    pub fn from_canonical_json(bytes: &[u8]) -> Result<Self, ValidationError> {
        Self::from_canonical_bytes(bytes)
    }

    /// Validate the full signed lineage and its content identity.
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_schema_version(self.schema_version)?;
        self.lineage.validate()?;
        validate_bounded_opaque_identifier(&self.chain.chain_id, "ReceiptChainLinkV1 chain_id")?;
        validate_key_id(&self.signer.key_id)?;
        if self.signer.algorithm != "ed25519" {
            return Err(ValidationError::new(
                "ReceiptSignerV1 algorithm must be ed25519",
            ));
        }
        if !valid_base64_signature(&self.signature) {
            return Err(ValidationError::new(
                "ReceiptDocumentV1 signature must be canonical base64 Ed25519 bytes",
            ));
        }
        if self.chain.sequence_number == 0 || self.chain.sequence_number > MAX_SAFE_INTEGER {
            return Err(ValidationError::new(
                "ReceiptChainLinkV1 sequence_number must be 1..=9007199254740991",
            ));
        }
        let predecessor_pair = (
            self.chain.previous_receipt_id.is_some(),
            self.chain.previous_signature_digest.is_some(),
        );
        if (self.chain.sequence_number == 1 && predecessor_pair != (false, false))
            || (self.chain.sequence_number > 1 && predecessor_pair != (true, true))
        {
            return Err(ValidationError::new(
                "ReceiptChainLinkV1 predecessor fields must be absent only at genesis",
            ));
        }
        if self.chain.previous_receipt_id.as_ref() == Some(&self.receipt_id) {
            return Err(ValidationError::new(
                "ReceiptChainLinkV1 cannot reference the current receipt",
            ));
        }
        if self.evidence_refs.len() > MAX_ITEMS || self.values.len() > MAX_ITEMS {
            return Err(ValidationError::new(
                "ReceiptDocumentV1 collection limit exceeded",
            ));
        }

        let mut evidence = BTreeMap::new();
        for reference in &self.evidence_refs {
            validate_media_type(&reference.media_type)?;
            validate_safe_evidence_uri(reference.uri.as_str())?;
            if evidence
                .insert(reference.digest.as_str(), reference.kind)
                .is_some()
            {
                return Err(ValidationError::new(
                    "ReceiptDocumentV1 evidence digests must be unique",
                ));
            }
        }
        let mut value_names = BTreeSet::new();
        for value in &self.values {
            if !value_names.insert(value.name.as_str()) {
                return Err(ValidationError::new(
                    "ReceiptDocumentV1 value names must be unique",
                ));
            }
            value.validate(&evidence)?;
        }
        self.outcome.validate(&evidence)?;
        match (self.status, self.outcome.state) {
            (ReceiptTerminalStatusV1::Rejected, AcceptanceState::Rejected)
            | (ReceiptTerminalStatusV1::Succeeded, AcceptanceState::Accepted) => {}
            (ReceiptTerminalStatusV1::Rejected, _) => {
                return Err(ValidationError::new(
                    "rejected terminal status requires a rejected outcome",
                ));
            }
            (_, AcceptanceState::Unknown) => {}
            (_, AcceptanceState::Rejected) => {
                return Err(ValidationError::new(
                    "rejected outcome requires a rejected terminal status",
                ));
            }
            (_, AcceptanceState::Accepted) => {
                return Err(ValidationError::new(
                    "accepted outcome requires a succeeded terminal status",
                ));
            }
        }
        if self.derived_receipt_id()? != self.receipt_id {
            return Err(ValidationError::new(
                "ReceiptDocumentV1 receipt_id does not match canonical content",
            ));
        }
        Ok(())
    }
}

fn canonical_without<T: Serialize>(
    value: &T,
    omitted: &[&str],
) -> Result<Vec<u8>, ValidationError> {
    let mut value = serde_json::to_value(value)
        .map_err(|error| ValidationError::new(format!("serialize receipt document: {error}")))?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| ValidationError::new("receipt document must serialize as an object"))?;
    for field in omitted {
        object.remove(*field);
    }
    serde_json::to_vec(&sort_json(value))
        .map_err(|error| ValidationError::new(format!("canonicalize receipt document: {error}")))
}

pub(crate) fn sort_json(value: Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .into_iter()
                .map(|(key, value)| (key, sort_json(value)))
                .collect::<BTreeMap<_, _>>()
                .into_iter()
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.into_iter().map(sort_json).collect()),
        scalar => scalar,
    }
}

/// A serde JSON visitor that rejects duplicate object keys at every depth.
struct StrictJsonValue(Value);

impl<'de> Deserialize<'de> for StrictJsonValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(StrictJsonVisitor)
    }
}

struct StrictJsonVisitor;

impl<'de> Visitor<'de> for StrictJsonVisitor {
    type Value = StrictJsonValue;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value with unique object keys")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(StrictJsonValue(Value::Bool(value)))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(StrictJsonValue(Value::Number(value.into())))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(StrictJsonValue(Value::Number(value.into())))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        let _ = value;
        Err(E::custom(
            "floating-point JSON numbers are not canonical receipt values",
        ))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(StrictJsonValue(Value::String(value.to_owned())))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(StrictJsonValue(Value::String(value)))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(StrictJsonValue(Value::Null))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(StrictJsonValue(Value::Null))
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(value) = seq.next_element::<StrictJsonValue>()? {
            values.push(value.0);
        }
        Ok(StrictJsonValue(Value::Array(values)))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut object = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if object.contains_key(&key) {
                return Err(serde::de::Error::custom(format!(
                    "duplicate JSON object key {key:?}"
                )));
            }
            let value = map.next_value::<StrictJsonValue>()?;
            object.insert(key, value.0);
        }
        Ok(StrictJsonValue(Value::Object(object)))
    }
}

fn strict_json_value(bytes: &[u8]) -> Result<Value, ValidationError> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let value = StrictJsonValue::deserialize(&mut deserializer)
        .map_err(|error| ValidationError::new(format!("decode canonical JSON: {error}")))?
        .0;
    deserializer
        .end()
        .map_err(|error| ValidationError::new(format!("trailing canonical JSON: {error}")))?;
    Ok(value)
}

pub(crate) fn digest_bytes(bytes: &[u8]) -> Result<Sha256Digest, ValidationError> {
    let mut value = String::with_capacity(71);
    value.push_str("sha256:");
    for byte in Sha256::digest(bytes) {
        write!(&mut value, "{byte:02x}")
            .map_err(|_| ValidationError::new("digest encoding failed"))?;
    }
    Sha256Digest::new(value)
}

fn validate_name(value: &str, field: &str) -> Result<(), ValidationError> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err(ValidationError::new(format!(
            "{field} must be 1..=64 lowercase ASCII letters, digits or underscores"
        )));
    }
    Ok(())
}

fn validate_media_type(value: &str) -> Result<(), ValidationError> {
    let mut parts = value.split('/');
    let Some(major) = parts.next() else {
        return Err(ValidationError::new("media type must contain one slash"));
    };
    let Some(subtype) = parts.next() else {
        return Err(ValidationError::new("media type must contain one slash"));
    };
    if parts.next().is_some()
        || value.is_empty()
        || value.len() > 128
        || major.is_empty()
        || subtype.is_empty()
        || !major.bytes().all(is_media_token)
        || !subtype.bytes().all(is_media_token)
    {
        return Err(ValidationError::new(
            "ReceiptEvidenceRefV1 media_type must be a bounded RFC token type/subtype",
        ));
    }
    Ok(())
}

fn is_media_token(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"!#$&^_.+-".contains(&byte)
}

fn validate_safe_evidence_uri(value: &str) -> Result<(), ValidationError> {
    if value.is_empty()
        || value.len() > 1_024
        || !value.is_ascii()
        || value
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
        || value.contains(['?', '#', '@'])
    {
        return Err(ValidationError::new(
            "evidence URI must be an ASCII privacy-safe locator without query, fragment or userinfo",
        ));
    }
    let allowed = [
        "artifact://",
        "evidence://",
        "bundle://",
        "source://",
        "urn:",
    ];
    let Some(prefix) = allowed.iter().find(|prefix| value.starts_with(*prefix)) else {
        return Err(ValidationError::new(
            "evidence URI must use artifact, evidence, bundle, source or urn scheme",
        ));
    };
    if value.len() == prefix.len() || value[prefix.len()..].starts_with('/') {
        return Err(ValidationError::new(
            "evidence URI must include a non-path locator",
        ));
    }
    if value.contains("..") || value.starts_with("file:") {
        return Err(ValidationError::new(
            "evidence URI must not contain path traversal or local file paths",
        ));
    }
    Ok(())
}

fn validate_key_id(value: &str) -> Result<(), ValidationError> {
    if value.is_empty()
        || value.len() > 128
        || !value.is_ascii()
        || !value.as_bytes()[0].is_ascii_alphanumeric()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:/-".contains(&byte))
        || value.starts_with("base64:")
        || value.starts_with("hex:")
    {
        return Err(ValidationError::new(
            "ReceiptSignerV1 key_id must be a bounded external key identifier, not key material",
        ));
    }
    Ok(())
}

fn validate_unique_digests(values: &[Sha256Digest], field: &str) -> Result<(), ValidationError> {
    if values.len() > MAX_ITEMS {
        return Err(ValidationError::new(format!("{field} exceeds 64 entries")));
    }
    let mut unique = BTreeSet::new();
    if values.iter().any(|value| !unique.insert(value.as_str())) {
        return Err(ValidationError::new(format!("{field} must be unique")));
    }
    Ok(())
}

fn valid_base64_signature(value: &str) -> bool {
    if value.len() != 88 || !value.ends_with("==") {
        return false;
    }
    let bytes = value.as_bytes();
    if !bytes[..86]
        .iter()
        .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'+' || *byte == b'/')
    {
        return false;
    }
    // 64 bytes encode as 86 meaningful base64 characters plus `==`.  The
    // low four bits of the final sextet are padding and must be zero; without
    // this check multiple encodings would verify the same signature bytes.
    base64_value(bytes[85]).is_some_and(|last| last & 0x0f == 0)
}

fn base64_value(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const C: &str = "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
    const D: &str = "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";

    fn digest(value: &str) -> Sha256Digest {
        Sha256Digest::new(value).expect("valid digest")
    }

    fn evidence(
        kind: ReceiptEvidenceKindV1,
        digest_value: &str,
        uri: &str,
    ) -> ReceiptEvidenceRefV1 {
        ReceiptEvidenceRefV1 {
            kind,
            uri: ProtocolReference::new(uri).unwrap(),
            digest: digest(digest_value),
            media_type: "application/json".to_owned(),
            signature_status: SignatureStatus::Verified,
        }
    }

    fn document() -> ReceiptDocumentV1 {
        let mut document = ReceiptDocumentV1 {
            schema_version: 1,
            receipt_id: digest(B),
            lineage: ReceiptLineageV1 {
                task_id: TaskId::new("task-1").unwrap(),
                task_ref: digest(A),
                plan_id: PlanId::new("plan-1").unwrap(),
                plan_ref: digest(B),
                invocation_id: "invocation-1".to_owned(),
                invocation_ref: digest(C),
                identity_ref: digest(A),
                policy_refs: vec![digest(B)],
                capabilities: vec![ReceiptCapabilityLinkV1 {
                    capability_id: CapabilityId::new("capability://leanctx/context").unwrap(),
                    capability_version: SemanticVersion::new("1.0.0").unwrap(),
                    invocation_ref: digest(C),
                }],
            },
            chain: ReceiptChainLinkV1 {
                chain_id: "chain-1".to_owned(),
                sequence_number: 1,
                previous_receipt_id: None,
                previous_signature_digest: None,
            },
            status: ReceiptTerminalStatusV1::Succeeded,
            values: vec![ReceiptValueV1 {
                name: "input_tokens".to_owned(),
                unit: "token".to_owned(),
                classification: ReceiptValueClassificationV1::Measured,
                value: Some(42),
                evidence_digests: vec![digest(A)],
                formula_digest: None,
                price_table_digest: None,
                reconciliation_digest: None,
            }],
            outcome: ReceiptOutcomeLinkV1 {
                state: AcceptanceState::Unknown,
                outcome_id: None,
                outcome_ref: None,
                acceptance_evidence_digest: None,
            },
            evidence_refs: vec![evidence(
                ReceiptEvidenceKindV1::Measurement,
                A,
                "artifact://usage",
            )],
            issued_at: UtcTimestamp::new("2026-08-23T12:00:00Z").unwrap(),
            signer: ReceiptSignerV1 {
                algorithm: "ed25519".to_owned(),
                key_id: "test-key".to_owned(),
                key_admission: ReceiptKeyAdmissionV1::ExternalTrustStore,
            },
            signature: "A".repeat(86) + "==",
        };
        document.receipt_id = document.derived_receipt_id().unwrap();
        document
    }

    #[test]
    fn canonical_identity_and_signature_bytes_are_deterministic() {
        let document = document();
        document.validate().unwrap();
        assert_eq!(
            document.identity_bytes().unwrap(),
            document.identity_bytes().unwrap()
        );
        assert_ne!(
            document.identity_bytes().unwrap(),
            document.signing_bytes().unwrap()
        );
        assert_ne!(
            document.signing_bytes().unwrap(),
            document.canonical_bytes().unwrap()
        );
    }

    #[test]
    fn canonical_decoder_rejects_duplicates_order_whitespace_and_invalid_utf8() {
        let document = document();
        let canonical = document.canonical_bytes().unwrap();
        let mut duplicate = canonical.clone();
        duplicate.pop();
        duplicate.extend_from_slice(b",\"schema_version\":1}");
        assert!(ReceiptDocumentV1::from_canonical_bytes(&duplicate).is_err());

        let mut whitespace = Vec::from(b" \n".as_slice());
        whitespace.extend_from_slice(&canonical);
        assert!(ReceiptDocumentV1::from_canonical_bytes(&whitespace).is_err());
        assert!(ReceiptDocumentV1::from_canonical_bytes(&[0xff]).is_err());
        assert!(strict_json_value(br#"{"value":1.0}"#).is_err());
    }

    #[test]
    fn canonical_decoder_round_trips_golden_value() {
        let document = document();
        let bytes = document.canonical_bytes().unwrap();
        let decoded = ReceiptDocumentV1::from_canonical_bytes(&bytes).unwrap();
        assert_eq!(decoded, document);
        let golden =
            include_bytes!("../../../../docs/contracts/receipt-document/v1/valid-structure.json");
        let golden = golden.strip_suffix(b"\n").unwrap_or(golden);
        assert_eq!(
            ReceiptDocumentV1::from_canonical_bytes(golden).unwrap(),
            document
        );
        let identity = include_bytes!(
            "../../../../docs/contracts/receipt-document/v1/canonical-identity.json"
        );
        let identity = identity.strip_suffix(b"\n").unwrap_or(identity);
        assert_eq!(document.identity_bytes().unwrap(), identity);
        let signing =
            include_bytes!("../../../../docs/contracts/receipt-document/v1/canonical-signing.json");
        let signing = signing.strip_suffix(b"\n").unwrap_or(signing);
        assert_eq!(document.signing_bytes().unwrap(), signing);
        let invalid = include_bytes!(
            "../../../../docs/contracts/receipt-document/v1/invalid-unknown-field.json"
        );
        let invalid = invalid.strip_suffix(b"\n").unwrap_or(invalid);
        assert!(ReceiptDocumentV1::from_canonical_bytes(invalid).is_err());
        let unicode =
            include_bytes!("../../../../docs/contracts/receipt-document/v1/canonical-unicode.json");
        let unicode = unicode.strip_suffix(b"\n").unwrap_or(unicode);
        let unicode_document = ReceiptDocumentV1::from_canonical_bytes(unicode).unwrap();
        assert_eq!(unicode_document.lineage.task_id.as_str(), "tâsk-✓");
        assert_eq!(unicode_document.canonical_bytes().unwrap(), unicode);
        let escaped = include_bytes!(
            "../../../../docs/contracts/receipt-document/v1/invalid-unicode-escape.json"
        );
        let escaped = escaped.strip_suffix(b"\n").unwrap_or(escaped);
        assert!(ReceiptDocumentV1::from_canonical_bytes(escaped).is_err());
    }

    #[test]
    fn integer_ceiling_and_timestamp_are_cross_language_strict() {
        let mut document = document();
        document.values[0].value = Some(MAX_SAFE_INTEGER + 1);
        document.receipt_id = document.derived_receipt_id().unwrap();
        assert!(document.validate().is_err());
        assert!(UtcTimestamp::new("2026-08-23T12:00:00.000Z").is_err());
        assert!(UtcTimestamp::new("2026-08-23T14:00:00+02:00").is_err());
    }

    #[test]
    fn bounded_opaque_ids_enforce_utf8_byte_ceiling() {
        assert!(TaskId::new("é".repeat(128)).is_ok());
        assert!(TaskId::new("é".repeat(129)).is_err());
    }

    #[test]
    fn base64_and_key_id_are_strict_without_embedded_trust() {
        assert!(valid_base64_signature(&("A".repeat(86) + "==")));
        assert!(!valid_base64_signature(&("A".repeat(85) + "B==")));
        let mut alias = document();
        alias.signature = "A".repeat(85) + "B==";
        assert!(alias.validate().is_err());
        assert!(validate_media_type("/json").is_err());
        assert!(validate_media_type("application/json").is_ok());
        assert!(validate_safe_evidence_uri("artifact://").is_err());
        assert!(validate_safe_evidence_uri("artifact:///tmp/private").is_err());
        assert!(validate_key_id("key://runtime/ed25519").is_ok());
        assert!(validate_key_id("base64:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=").is_err());
        assert!(validate_key_id("bad key").is_err());
    }

    #[test]
    fn calculated_and_reconciled_values_require_typed_evidence() {
        let mut calculated = document();
        calculated.evidence_refs.extend([
            evidence(ReceiptEvidenceKindV1::Formula, B, "artifact://formula"),
            evidence(ReceiptEvidenceKindV1::PriceTable, C, "artifact://price"),
        ]);
        calculated.values[0].classification = ReceiptValueClassificationV1::Calculated;
        calculated.values[0].formula_digest = Some(digest(B));
        calculated.values[0].price_table_digest = Some(digest(C));
        calculated.receipt_id = calculated.derived_receipt_id().unwrap();
        calculated.validate().unwrap();
        calculated.values[0].price_table_digest = Some(digest(B));
        calculated.receipt_id = calculated.derived_receipt_id().unwrap();
        assert!(calculated.validate().is_err());
        calculated.values[0].price_table_digest = Some(digest(C));

        let mut reconciled = calculated;
        reconciled.evidence_refs.push(evidence(
            ReceiptEvidenceKindV1::Invoice,
            D,
            "artifact://invoice",
        ));
        reconciled.values[0].classification = ReceiptValueClassificationV1::Reconciled;
        reconciled.values[0].reconciliation_digest = Some(digest(D));
        reconciled.values[0].evidence_digests.push(digest(D));
        reconciled.receipt_id = reconciled.derived_receipt_id().unwrap();
        assert!(reconciled.validate().is_err());
        reconciled.values[0].evidence_digests.pop();
        reconciled.receipt_id = reconciled.derived_receipt_id().unwrap();
        reconciled.validate().unwrap();
    }

    #[test]
    fn rejected_outcome_requires_outcome_evidence_and_terminal_status() {
        let mut document = document();
        document.status = ReceiptTerminalStatusV1::Rejected;
        document.receipt_id = document.derived_receipt_id().unwrap();
        assert!(document.validate().is_err());

        document.evidence_refs.push(evidence(
            ReceiptEvidenceKindV1::Outcome,
            D,
            "artifact://outcome",
        ));
        document.outcome = ReceiptOutcomeLinkV1 {
            state: AcceptanceState::Rejected,
            outcome_id: Some(OutcomeId::new("outcome-1").unwrap()),
            outcome_ref: Some(digest(D)),
            acceptance_evidence_digest: None,
        };
        document.receipt_id = document.derived_receipt_id().unwrap();
        document.validate().unwrap();
        document.outcome.acceptance_evidence_digest = Some(digest(A));
        document.receipt_id = document.derived_receipt_id().unwrap();
        assert!(document.validate().is_err());
    }

    #[test]
    fn duplicate_provenance_is_rejected() {
        let mut document = document();
        document.evidence_refs.push(evidence(
            ReceiptEvidenceKindV1::Runtime,
            A,
            "artifact://runtime",
        ));
        document.receipt_id = document.derived_receipt_id().unwrap();
        assert!(document.validate().is_err());
    }

    #[test]
    fn strict_decoder_rejects_unknown_fields() {
        let mut value = serde_json::to_value(document()).unwrap();
        value["unknown"] = Value::Bool(true);
        assert!(serde_json::from_value::<ReceiptDocumentV1>(value).is_err());
    }
}
