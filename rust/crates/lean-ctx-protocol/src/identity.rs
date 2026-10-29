//! Canonical bounded identities used by the additive Context SDK contracts.

use crate::common::{ValidationError, validate_bounded_opaque_identifier};
use serde::{Deserialize, Deserializer, Serialize, de::Error as DeError};

/// Maximum length for an explicit non-path protocol reference.
pub const MAX_REFERENCE_LENGTH: usize = 1_024;

/// Exact number of hexadecimal characters in a SHA-256 digest.
pub const SHA256_HEX_LENGTH: usize = 64;

/// Maximum serialized length for a SemVer identity.
pub const MAX_SEMVER_LENGTH: usize = 128;

macro_rules! bounded_opaque_identifier {
    ($name:ident) => {
        /// Opaque, bounded identity owned by the public protocol.
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Construct an identifier after applying the protocol wire bounds.
            pub fn new(value: impl Into<String>) -> Result<Self, ValidationError> {
                let value = value.into();
                validate_bounded_opaque_identifier(&value, stringify!($name))?;
                Ok(Self(value))
            }

            /// Borrow the opaque protocol identity.
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// Consume the identifier and return its wire representation.
            pub fn into_inner(self) -> String {
                self.0
            }
        }

        impl std::str::FromStr for $name {
            type Err = ValidationError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::new(value)
            }
        }

        impl TryFrom<String> for $name {
            type Error = ValidationError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl TryFrom<&str> for $name {
            type Error = ValidationError;

            fn try_from(value: &str) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::new(value).map_err(DeError::custom)
            }
        }
    };
}

bounded_opaque_identifier!(WorkspaceId);
bounded_opaque_identifier!(RunId);
bounded_opaque_identifier!(EventId);
bounded_opaque_identifier!(ProfileId);
bounded_opaque_identifier!(KitId);
bounded_opaque_identifier!(SourceId);
bounded_opaque_identifier!(ViewId);
bounded_opaque_identifier!(ProjectContextId);
bounded_opaque_identifier!(HandoffId);
bounded_opaque_identifier!(PolicyId);
bounded_opaque_identifier!(PackageId);

/// A canonical `sha256:<lowercase-hex>` content identity.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct Sha256Digest(String);

impl Sha256Digest {
    /// Parse a canonical SHA-256 digest identity.
    pub fn new(value: impl Into<String>) -> Result<Self, ValidationError> {
        let value = value.into();
        let Some(hex) = value.strip_prefix("sha256:") else {
            return Err(ValidationError::new(
                "Sha256Digest must start with the canonical sha256: prefix",
            ));
        };
        if hex.len() != SHA256_HEX_LENGTH {
            return Err(ValidationError::new(format!(
                "Sha256Digest must contain exactly {SHA256_HEX_LENGTH} lowercase hexadecimal characters"
            )));
        }
        if !hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(ValidationError::new(
                "Sha256Digest must contain lowercase hexadecimal characters only",
            ));
        }
        Ok(Self(value))
    }

    /// Borrow the canonical digest including the algorithm prefix.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Borrow the raw lowercase hexadecimal digest bytes.
    pub fn hex(&self) -> &str {
        self.0
            .strip_prefix("sha256:")
            .expect("Sha256Digest is constructed with a sha256: prefix")
    }

    /// Consume the digest and return its canonical wire representation.
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl std::str::FromStr for Sha256Digest {
    type Err = ValidationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<String> for Sha256Digest {
    type Error = ValidationError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<&str> for Sha256Digest {
    type Error = ValidationError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<Sha256Digest> for String {
    fn from(value: Sha256Digest) -> Self {
        value.0
    }
}

impl<'de> Deserialize<'de> for Sha256Digest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(DeError::custom)
    }
}

/// A strict SemVer 2.0.0 identity used for pinned protocol artifacts.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct SemanticVersion(String);

impl SemanticVersion {
    /// Parse a bounded SemVer 2.0.0 version without accepting loose aliases.
    pub fn new(value: impl Into<String>) -> Result<Self, ValidationError> {
        let value = value.into();
        if value.is_empty() || value.len() > MAX_SEMVER_LENGTH || !value.is_ascii() {
            return Err(ValidationError::new(format!(
                "SemanticVersion must be nonempty ASCII and at most {MAX_SEMVER_LENGTH} bytes"
            )));
        }

        let (without_build, build) = split_optional(&value, '+')?;
        let (core, prerelease) = split_optional(without_build, '-')?;
        let core_components = core.split('.').collect::<Vec<_>>();
        if core_components.len() != 3 || !core_components.iter().all(valid_numeric) {
            return Err(ValidationError::new(
                "SemanticVersion must have major.minor.patch numeric core components",
            ));
        }
        if let Some(prerelease) = prerelease {
            validate_identifier_components(prerelease, true, "prerelease")?;
        }
        if let Some(build) = build {
            validate_identifier_components(build, false, "build metadata")?;
        }
        Ok(Self(value))
    }

    /// Borrow the canonical SemVer representation.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume the version and return its wire representation.
    pub fn into_inner(self) -> String {
        self.0
    }
}

fn split_optional(value: &str, separator: char) -> Result<(&str, Option<&str>), ValidationError> {
    let Some((before, after)) = value.split_once(separator) else {
        return Ok((value, None));
    };
    if before.is_empty() || after.is_empty() || (separator == '+' && after.contains(separator)) {
        return Err(ValidationError::new(format!(
            "SemanticVersion contains an invalid {separator} segment"
        )));
    }
    Ok((before, Some(after)))
}

fn valid_numeric(value: &&str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && (value.len() == 1 || !value.starts_with('0'))
}

fn validate_identifier_components(
    value: &str,
    reject_leading_zero_numeric: bool,
    field: &str,
) -> Result<(), ValidationError> {
    if value.split('.').any(|part| {
        part.is_empty()
            || !part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            || (reject_leading_zero_numeric
                && part.len() > 1
                && part.bytes().all(|byte| byte.is_ascii_digit())
                && part.starts_with('0'))
    }) {
        return Err(ValidationError::new(format!(
            "SemanticVersion {field} must contain dot-separated ASCII alphanumeric or hyphen identifiers"
        )));
    }
    Ok(())
}

impl std::str::FromStr for SemanticVersion {
    type Err = ValidationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<String> for SemanticVersion {
    type Error = ValidationError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<&str> for SemanticVersion {
    type Error = ValidationError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<SemanticVersion> for String {
    fn from(value: SemanticVersion) -> Self {
        value.0
    }
}

impl<'de> Deserialize<'de> for SemanticVersion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(DeError::custom)
    }
}

/// Bounded non-path reference to provenance, activation, or policy material.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ProtocolReference(String);

impl ProtocolReference {
    /// Construct a bounded, nonempty protocol reference.
    pub fn new(value: impl Into<String>) -> Result<Self, ValidationError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(ValidationError::new("ProtocolReference must not be empty"));
        }
        if value.len() > MAX_REFERENCE_LENGTH {
            return Err(ValidationError::new(format!(
                "ProtocolReference exceeds the {MAX_REFERENCE_LENGTH} byte limit"
            )));
        }
        if value.chars().any(char::is_control) {
            return Err(ValidationError::new(
                "ProtocolReference must not contain control characters",
            ));
        }
        Ok(Self(value))
    }

    /// Borrow the wire reference.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume the reference and return its wire representation.
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl std::str::FromStr for ProtocolReference {
    type Err = ValidationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<String> for ProtocolReference {
    type Error = ValidationError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<&str> for ProtocolReference {
    type Error = ValidationError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<ProtocolReference> for String {
    fn from(value: ProtocolReference) -> Self {
        value.0
    }
}

impl<'de> Deserialize<'de> for ProtocolReference {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(DeError::custom)
    }
}

/// Canonical UTC timestamp with second precision for deterministic session metadata.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct UtcTimestamp(String);

impl UtcTimestamp {
    /// Parse `YYYY-MM-DDTHH:MM:SSZ` and reject invalid calendar values.
    pub fn new(value: impl Into<String>) -> Result<Self, ValidationError> {
        let value = value.into();
        let bytes = value.as_bytes();
        if bytes.len() != 20
            || bytes[4] != b'-'
            || bytes[7] != b'-'
            || bytes[10] != b'T'
            || bytes[13] != b':'
            || bytes[16] != b':'
            || bytes[19] != b'Z'
            || [0..4, 5..7, 8..10, 11..13, 14..16, 17..19]
                .iter()
                .any(|range| !bytes[range.clone()].iter().all(u8::is_ascii_digit))
        {
            return Err(ValidationError::new(
                "UtcTimestamp must use canonical YYYY-MM-DDTHH:MM:SSZ syntax",
            ));
        }
        let year = decimal(&bytes[0..4]);
        let month = decimal(&bytes[5..7]);
        let day = decimal(&bytes[8..10]);
        let hour = decimal(&bytes[11..13]);
        let minute = decimal(&bytes[14..16]);
        let second = decimal(&bytes[17..19]);
        if year == 0
            || !(1..=12).contains(&month)
            || day == 0
            || day > days_in_month(year, month)
            || hour > 23
            || minute > 59
            || second > 59
        {
            return Err(ValidationError::new(
                "UtcTimestamp contains an invalid calendar or clock value",
            ));
        }
        Ok(Self(value))
    }

    /// Borrow the canonical timestamp.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume the timestamp and return its wire representation.
    pub fn into_inner(self) -> String {
        self.0
    }
}

fn decimal(bytes: &[u8]) -> u32 {
    bytes
        .iter()
        .fold(0, |value, byte| value * 10 + u32::from(*byte - b'0'))
}

fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year.is_multiple_of(400) || (year.is_multiple_of(4) && !year.is_multiple_of(100)) => {
            29
        }
        2 => 28,
        _ => unreachable!("month range has already been validated"),
    }
}

impl std::str::FromStr for UtcTimestamp {
    type Err = ValidationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<String> for UtcTimestamp {
    type Error = ValidationError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<&str> for UtcTimestamp {
    type Error = ValidationError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<UtcTimestamp> for String {
    fn from(value: UtcTimestamp) -> Self {
        value.0
    }
}

impl<'de> Deserialize<'de> for UtcTimestamp {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(DeError::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    #[test]
    fn sha256_digest_requires_the_canonical_wire_form() {
        assert_eq!(
            Sha256Digest::new(DIGEST).expect("valid digest").hex(),
            "a".repeat(64)
        );
        for invalid in [
            "sha256:abc",
            "SHA256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "sha256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        ] {
            assert!(Sha256Digest::new(invalid).is_err(), "{invalid} must fail");
        }
    }

    #[test]
    fn semantic_versions_reject_loose_or_ambiguous_inputs() {
        for valid in ["1.2.3", "1.2.3-rc.1", "1.2.3+build.7", "1.2.3-rc.1+build.7"] {
            assert!(SemanticVersion::new(valid).is_ok(), "{valid} must parse");
        }
        for invalid in ["v1.2.3", "1.2", "01.2.3", "1.2.3-01", "1.2.3+", "1.2.3++x"] {
            assert!(
                SemanticVersion::new(invalid).is_err(),
                "{invalid} must fail"
            );
        }
    }

    #[test]
    fn opaque_identifiers_and_references_are_bounded() {
        assert!(ProfileId::new("profile-1").is_ok());
        assert!(ProfileId::new("\n").is_err());
        assert!(ProfileId::new("a".repeat(crate::MAX_IDENTIFIER_LENGTH + 1)).is_err());
        assert!(ProtocolReference::new("source:fixture").is_ok());
        assert!(ProtocolReference::new("\u{0000}").is_err());
    }

    #[test]
    fn timestamps_are_canonical_and_calendar_checked() {
        assert!(UtcTimestamp::new("2028-02-29T23:59:59Z").is_ok());
        assert!(UtcTimestamp::new("2027-02-29T23:59:59Z").is_err());
        assert!(UtcTimestamp::new("2026-01-01T00:00:00+00:00").is_err());
    }
}
