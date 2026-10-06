use crate::common::{
    ExtensionsV1, ValidationError, validate_bounded_string, validate_schema_version,
};
use serde::{Deserialize, Serialize};

/// Typed evidence reference (not just a string).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceRefV1 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<u32>,
    pub kind: EvidenceKind,
    pub uri: String,
    pub digest: String,
    pub signature_status: SignatureStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    #[serde(default, flatten)]
    pub extensions: ExtensionsV1,
}

const EVIDENCE_RESERVED_FIELDS: &[&str] = &[
    "schema_version",
    "kind",
    "uri",
    "digest",
    "signature_status",
    "media_type",
];

impl EvidenceRefV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        self.extensions
            .validate_reserved(EVIDENCE_RESERVED_FIELDS)?;
        if let Some(version) = self.schema_version {
            validate_schema_version(version)?;
        }
        validate_bounded_string(&self.uri, "evidence uri")?;
        validate_bounded_string(&self.digest, "evidence digest")?;
        if self.schema_version.is_some() {
            let digest = self
                .digest
                .strip_prefix("sha256:")
                .or_else(|| self.digest.strip_prefix("blake3:"))
                .unwrap_or(&self.digest);
            if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(ValidationError::new(
                    "evidence digest must contain a supported 64-digit hexadecimal digest",
                ));
            }
        }
        if let Some(media_type) = &self.media_type {
            validate_bounded_string(media_type, "evidence media_type")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EvidenceKind {
    ProviderReceipt,
    RuntimeLog,
    SignedBatch,
    QualityMeasurement,
    ExperimentOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SignatureStatus {
    Verified,
    Unverified,
    NotSigned,
}

#[cfg(test)]
mod tests {
    use crate::{EvidenceKind, EvidenceRefV1, SignatureStatus};

    #[test]
    fn serialization_round_trip() {
        let evidence = EvidenceRefV1 {
            schema_version: Some(1),
            kind: EvidenceKind::ProviderReceipt,
            uri: "urn:receipt:1".to_owned(),
            digest: format!("sha256:{}", "a".repeat(64)),
            signature_status: SignatureStatus::Verified,
            media_type: Some("application/json".to_owned()),
            extensions: Default::default(),
        };
        let json = serde_json::to_string(&evidence).expect("evidence should serialize");
        let decoded = serde_json::from_str(&json).expect("evidence should deserialize");
        assert_eq!(evidence, decoded);
    }

    #[test]
    fn versionless_legacy_digest_is_bounded_but_versioned_digest_is_strict() {
        let mut evidence = EvidenceRefV1 {
            schema_version: None,
            kind: EvidenceKind::ProviderReceipt,
            uri: "urn:legacy:receipt".to_owned(),
            digest: "sha256:legacy-receipt-id".to_owned(),
            signature_status: SignatureStatus::Verified,
            media_type: None,
            extensions: Default::default(),
        };
        evidence
            .validate()
            .expect("versionless legacy digest remains readable");

        evidence.schema_version = Some(1);
        assert!(
            evidence.validate().is_err(),
            "versioned evidence requires a canonical digest"
        );
    }
}
