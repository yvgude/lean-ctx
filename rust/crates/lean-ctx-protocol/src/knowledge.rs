//! Knowledge provenance contract.

use crate::common::{
    ExtensionsV1, ValidationError, deserialize_schema_version, validate_schema_version,
};
use serde::de::Error as DeError;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Origin category for a knowledge object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeSourceType {
    Repository,
    Documentation,
    Provider,
    UserInput,
    BuildArtifact,
    TestResult,
    Other,
}

/// Authority level used when knowledge participates in a decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeAuthority {
    Primary,
    Verified,
    Secondary,
    Derived,
    UserProvided,
}

/// Validity interval for a knowledge object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeValidityV1 {
    pub valid_from: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_until: Option<String>,
}

/// A typed pointer to the system and revision that supplied a knowledge object.
///
/// `source_type` is serialized as `type` to keep the wire representation aligned
/// with the public Knowledge Hub contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceReference {
    #[serde(rename = "type")]
    pub source_type: String,
    pub uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_sha: Option<String>,
    pub access_timestamp: String,
}

/// Authority and review metadata supplied by a local knowledge producer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuthorityMetadata {
    pub owner: String,
    pub confidence_level: f32,
    pub review_status: String,
}

/// Validity and supersession metadata for a knowledge object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidityWindow {
    pub valid_from: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_until: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
}

/// Classification level used by the portable Knowledge Hub surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClassificationLevel {
    Public,
    Internal,
    Confidential,
    Restricted,
}

/// Classification and local retention metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataClassification {
    pub level: ClassificationLevel,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention_days: Option<u32>,
}

impl DataClassification {
    fn from_legacy_value(value: &str) -> Result<Self, String> {
        let level = match value.to_ascii_lowercase().as_str() {
            "public" => ClassificationLevel::Public,
            "internal" => ClassificationLevel::Internal,
            "confidential" => ClassificationLevel::Confidential,
            "restricted" => ClassificationLevel::Restricted,
            other => return Err(format!("unknown data classification: {other}")),
        };
        Ok(Self {
            level,
            retention_days: None,
        })
    }
}

fn deserialize_optional_source_ref<'de, D>(
    deserializer: D,
) -> Result<Option<SourceReference>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    match value {
        None => Ok(None),
        Some(Value::String(uri)) => Ok(Some(SourceReference {
            source_type: "legacy".to_owned(),
            uri,
            commit_sha: None,
            access_timestamp: String::new(),
        })),
        Some(value) => serde_json::from_value(value)
            .map(Some)
            .map_err(D::Error::custom),
    }
}

fn deserialize_optional_authority<'de, D>(
    deserializer: D,
) -> Result<Option<AuthorityMetadata>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    match value {
        None => Ok(None),
        Some(Value::String(review_status)) => Ok(Some(AuthorityMetadata {
            owner: String::new(),
            confidence_level: 0.0,
            review_status,
        })),
        Some(value) => serde_json::from_value(value)
            .map(Some)
            .map_err(D::Error::custom),
    }
}

fn deserialize_optional_classification<'de, D>(
    deserializer: D,
) -> Result<Option<DataClassification>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    match value {
        None => Ok(None),
        Some(Value::String(level)) => DataClassification::from_legacy_value(&level)
            .map(Some)
            .map_err(D::Error::custom),
        Some(value) => serde_json::from_value(value)
            .map(Some)
            .map_err(D::Error::custom),
    }
}

/// Content-addressed knowledge provenance record. Unknown fields are retained.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KnowledgeObjectV1 {
    #[serde(deserialize_with = "deserialize_schema_version")]
    pub schema_version: u32,
    // The legacy V1 wire contract required the key; keep that presence
    // invariant while allowing the richer value to be null.
    #[serde(deserialize_with = "deserialize_optional_source_ref")]
    pub source_ref: Option<SourceReference>,
    pub source_type: KnowledgeSourceType,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_optional_authority"
    )]
    pub authority: Option<AuthorityMetadata>,
    pub owner: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_optional_classification"
    )]
    pub classification: Option<DataClassification>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validity: Option<ValidityWindow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<String>,
    pub content_hash: String,
    pub evidence_digest: String,
    pub policy_ref: String,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    #[serde(default, flatten)]
    pub extra: ExtensionsV1,
}

const KNOWLEDGE_RESERVED_FIELDS: &[&str] = &[
    "schema_version",
    "source_ref",
    "source_type",
    "authority",
    "owner",
    "classification",
    "validity",
    "supersedes",
    "content_hash",
    "evidence_digest",
    "policy_ref",
    "evidence_refs",
];

impl KnowledgeObjectV1 {
    /// Validate schema invariants for a knowledge object.
    pub fn validate(&self) -> Result<(), ValidationError> {
        self.extra.validate_reserved(KNOWLEDGE_RESERVED_FIELDS)?;
        validate_schema_version(self.schema_version)
    }

    /// Stable local-store identifier for this content-addressed object.
    pub fn object_id(&self) -> &str {
        &self.content_hash
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialization_round_trip() {
        let knowledge = KnowledgeObjectV1 {
            schema_version: 1,
            source_ref: Some(SourceReference {
                source_type: "documentation".to_owned(),
                uri: "docs:architecture".to_owned(),
                commit_sha: None,
                access_timestamp: "2026-08-09T00:00:00Z".to_owned(),
            }),
            source_type: KnowledgeSourceType::Documentation,
            authority: Some(AuthorityMetadata {
                owner: "team-platform".to_owned(),
                confidence_level: 0.95,
                review_status: "verified".to_owned(),
            }),
            owner: "team-platform".to_owned(),
            classification: Some(DataClassification {
                level: ClassificationLevel::Internal,
                retention_days: Some(365),
            }),
            validity: Some(ValidityWindow {
                valid_from: "2026-08-09T00:00:00Z".to_owned(),
                valid_until: None,
                superseded_by: None,
            }),
            supersedes: Some("docs:architecture-old".to_owned()),
            content_hash: "sha256:content".to_owned(),
            evidence_digest: "sha256:evidence".to_owned(),
            policy_ref: "policy:knowledge".to_owned(),
            evidence_refs: vec!["evidence:architecture".to_owned()],
            extra: {
                let mut extra = ExtensionsV1::default();
                extra
                    .insert("extension", Value::from("kept"))
                    .expect("extension should be valid");
                extra
            },
        };
        let json = serde_json::to_string(&knowledge).expect("knowledge should serialize");
        let decoded: KnowledgeObjectV1 =
            serde_json::from_str(&json).expect("knowledge should deserialize");
        assert_eq!(knowledge, decoded);
        knowledge
            .validate()
            .expect("knowledge should satisfy invariants");
    }

    #[test]
    fn legacy_scalar_metadata_deserializes_into_optional_fields() {
        let json = r#"{
            "schema_version": 1,
            "source_ref": "docs:architecture",
            "source_type": "documentation",
            "authority": "verified",
            "owner": "team-platform",
            "classification": "Internal",
            "validity": {"valid_from": "2026-08-09T00:00:00Z"},
            "content_hash": "sha256:content",
            "evidence_digest": "sha256:evidence",
            "policy_ref": "policy:knowledge"
        }"#;

        let knowledge: KnowledgeObjectV1 = serde_json::from_str(json).expect("legacy JSON parses");
        assert_eq!(
            knowledge
                .source_ref
                .as_ref()
                .map(|source| source.uri.as_str()),
            Some("docs:architecture")
        );
        assert_eq!(
            knowledge
                .authority
                .as_ref()
                .map(|authority| authority.review_status.as_str()),
            Some("verified")
        );
        assert_eq!(
            knowledge
                .classification
                .as_ref()
                .map(|classification| classification.level),
            Some(ClassificationLevel::Internal)
        );
        assert_eq!(knowledge.evidence_refs, Vec::<String>::new());
    }
}
