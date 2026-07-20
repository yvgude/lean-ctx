//! Provider-authoritative tokenizer calibration evidence (CO-05).
//!
//! Local tokenizer counts remain useful estimates, but they are not provider
//! billing truth.  This module provides the bounded, deterministic report
//! envelope required before a provider-count calibration can be consumed.
//! Missing authority is deliberately rejected; callers must not silently
//! promote local reference counts to provider evidence.

use std::fmt::Write as _;

use thiserror::Error;

/// Stable schema identifier for tokenizer calibration reports.
pub const TOKEN_CALIBRATION_SCHEMA_VERSION: &str = "leanctx.token-calibration/v1";
/// Stable report version for the first calibration contract.
pub const TOKEN_CALIBRATION_REPORT_VERSION: &str = "1.0.0";
const MAX_ENTRIES: usize = 4_096;
const MAX_TEXT_FIELD_CHARS: usize = 256;
const MAX_AUTHORITY_REF_CHARS: usize = 512;

/// A single provider-count observation, identified without retaining payload.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TokenCalibrationEntry {
    /// Immutable provider/corpus reference for the measured sample.
    pub sample_ref: String,
    /// Provider-reported input token count for the sample.
    pub provider_tokens: u64,
}

/// Versioned provider-count calibration report.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TokenCalibrationReportV1 {
    pub schema_version: String,
    pub report_version: String,
    pub provider: String,
    pub model: String,
    pub tokenizer_family: String,
    /// Immutable authority/evidence reference.  `None` is never consumable.
    pub authority_ref: Option<String>,
    /// Sorted, unique provider observations.
    pub entries: Vec<TokenCalibrationEntry>,
    /// `blake3:` digest over the canonical report payload excluding this field.
    pub corpus_digest: String,
}

/// Fail-closed validation errors for calibration evidence.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TokenCalibrationError {
    #[error("unsupported calibration schema version")]
    UnsupportedSchema,
    #[error("unsupported calibration report version")]
    UnsupportedReportVersion,
    #[error("{field} is empty or exceeds its bound")]
    InvalidField { field: &'static str },
    #[error("calibration authority is missing")]
    MissingAuthority,
    #[error("calibration authority reference is invalid")]
    InvalidAuthority,
    #[error("calibration entries are empty or exceed the bound")]
    InvalidEntryCount,
    #[error("calibration entries are not sorted and unique")]
    NonCanonicalEntries,
    #[error("calibration corpus digest is malformed")]
    InvalidDigest,
    #[error("calibration corpus digest does not match entries")]
    DigestMismatch,
    #[error("calibration report serialization failed")]
    Serialization,
}

impl TokenCalibrationReportV1 {
    /// Constructs an authoritative report and computes its deterministic digest.
    pub fn new_provider_authoritative(
        provider: impl Into<String>,
        model: impl Into<String>,
        tokenizer_family: impl Into<String>,
        authority_ref: impl Into<String>,
        mut entries: Vec<TokenCalibrationEntry>,
    ) -> Result<Self, TokenCalibrationError> {
        entries.sort_by(|left, right| left.sample_ref.cmp(&right.sample_ref));
        let mut report = Self {
            schema_version: TOKEN_CALIBRATION_SCHEMA_VERSION.to_string(),
            report_version: TOKEN_CALIBRATION_REPORT_VERSION.to_string(),
            provider: provider.into(),
            model: model.into(),
            tokenizer_family: tokenizer_family.into(),
            authority_ref: Some(authority_ref.into()),
            entries,
            corpus_digest: String::new(),
        };
        report.corpus_digest = report.computed_digest();
        report.validate()?;
        Ok(report)
    }

    /// Validates all bounds, canonical ordering, authority, and digest fields.
    pub fn validate(&self) -> Result<(), TokenCalibrationError> {
        if self.schema_version != TOKEN_CALIBRATION_SCHEMA_VERSION {
            return Err(TokenCalibrationError::UnsupportedSchema);
        }
        if self.report_version != TOKEN_CALIBRATION_REPORT_VERSION {
            return Err(TokenCalibrationError::UnsupportedReportVersion);
        }
        for (field, value) in [
            ("provider", self.provider.as_str()),
            ("model", self.model.as_str()),
            ("tokenizer_family", self.tokenizer_family.as_str()),
        ] {
            if !valid_text(value, MAX_TEXT_FIELD_CHARS) {
                return Err(TokenCalibrationError::InvalidField { field });
            }
        }
        let Some(authority_ref) = self.authority_ref.as_deref() else {
            return Err(TokenCalibrationError::MissingAuthority);
        };
        if !valid_text(authority_ref, MAX_AUTHORITY_REF_CHARS) {
            return Err(TokenCalibrationError::InvalidAuthority);
        }
        if self.entries.is_empty() || self.entries.len() > MAX_ENTRIES {
            return Err(TokenCalibrationError::InvalidEntryCount);
        }
        let mut previous: Option<&str> = None;
        for entry in &self.entries {
            if !valid_text(&entry.sample_ref, MAX_AUTHORITY_REF_CHARS)
                || previous.is_some_and(|value| value >= entry.sample_ref.as_str())
            {
                return Err(TokenCalibrationError::NonCanonicalEntries);
            }
            previous = Some(&entry.sample_ref);
        }
        if !is_digest(&self.corpus_digest) {
            return Err(TokenCalibrationError::InvalidDigest);
        }
        if self.corpus_digest != self.computed_digest() {
            return Err(TokenCalibrationError::DigestMismatch);
        }
        Ok(())
    }

    /// Returns canonical JSON only for a fully validated report.
    pub fn canonical_json(&self) -> Result<Vec<u8>, TokenCalibrationError> {
        self.validate()?;
        serde_json::to_vec(self).map_err(|_| TokenCalibrationError::Serialization)
    }

    fn computed_digest(&self) -> String {
        let mut payload = String::new();
        append_field(&mut payload, TOKEN_CALIBRATION_SCHEMA_VERSION);
        append_field(&mut payload, TOKEN_CALIBRATION_REPORT_VERSION);
        append_field(&mut payload, &self.provider);
        append_field(&mut payload, &self.model);
        append_field(&mut payload, &self.tokenizer_family);
        append_field(&mut payload, self.authority_ref.as_deref().unwrap_or(""));
        for entry in &self.entries {
            append_field(&mut payload, &entry.sample_ref);
            let _ = write!(payload, "{}|", entry.provider_tokens);
        }
        format!("blake3:{}", blake3::hash(payload.as_bytes()).to_hex())
    }
}

fn append_field(payload: &mut String, value: &str) {
    let _ = write!(payload, "{}:", value.len());
    payload.push_str(value);
    payload.push('|');
}

fn valid_text(value: &str, max_chars: usize) -> bool {
    !value.is_empty()
        && value.chars().count() <= max_chars
        && value.chars().all(|character| !character.is_control())
}

fn is_digest(value: &str) -> bool {
    let Some(hex) = value.strip_prefix("blake3:") else {
        return false;
    };
    hex.len() == 64
        && hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> TokenCalibrationReportV1 {
        TokenCalibrationReportV1::new_provider_authoritative(
            "provider-a",
            "model-a",
            "provider-native-v1",
            "https://evidence.example/report/1",
            vec![
                TokenCalibrationEntry {
                    sample_ref: "sample:b".to_string(),
                    provider_tokens: 11,
                },
                TokenCalibrationEntry {
                    sample_ref: "sample:a".to_string(),
                    provider_tokens: 7,
                },
            ],
        )
        .expect("valid report")
    }

    #[test]
    fn constructor_sorts_entries_and_is_deterministic() {
        let first = report();
        let second = report();
        assert_eq!(first, second);
        assert_eq!(first.entries[0].sample_ref, "sample:a");
        assert_eq!(first.canonical_json(), second.canonical_json());
    }

    #[test]
    fn missing_authority_fails_closed() {
        let mut value = report();
        value.authority_ref = None;
        assert_eq!(
            value.validate(),
            Err(TokenCalibrationError::MissingAuthority)
        );
        assert_eq!(
            value.canonical_json(),
            Err(TokenCalibrationError::MissingAuthority)
        );
    }

    #[test]
    fn digest_mutation_fails_closed() {
        let mut value = report();
        value.entries[0].provider_tokens += 1;
        assert_eq!(value.validate(), Err(TokenCalibrationError::DigestMismatch));
    }

    #[test]
    fn duplicate_or_unsorted_entries_fail_closed() {
        let mut value = report();
        value.entries.swap(0, 1);
        value.corpus_digest = value.computed_digest();
        assert_eq!(
            value.validate(),
            Err(TokenCalibrationError::NonCanonicalEntries)
        );

        value.entries[0].sample_ref = value.entries[1].sample_ref.clone();
        value.corpus_digest = value.computed_digest();
        assert_eq!(
            value.validate(),
            Err(TokenCalibrationError::NonCanonicalEntries)
        );
    }
}
