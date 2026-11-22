// SPDX-License-Identifier: Apache-2.0
//! Landing receipt: delivery evidence that a change request landed.
//!
//! A landing service merges a change request (GitLab merge request or GitHub
//! pull request) onto its target branch after the selected checks passed, or
//! refuses it, or reverts it after landing. [`LandingReceiptV1`] records that
//! outcome as metadata only: identifiers, commit SHAs, the verdict and the
//! digest of the check selection. It never carries code, diffs, logs or
//! credentials.
//!
//! Identity: [`LandingReceiptV1::digest`] is the SHA-256 of the canonical JSON
//! (compact, object keys sorted recursively) of the receipt without its
//! `signing` block, so a signed and an unsigned copy of the same landing share
//! one id. A receipt is *hashed* without `signing`, *signed* with it: the
//! signature is Ed25519 over the canonical JSON of the receipt with only
//! `signing.signature` removed (the public key stays covered), base64
//! encoded. Verifying the signature is the consumer's job; this crate has no
//! signature dependency.
//!
//! # Canonical JSON (normative for every producer)
//!
//! Other producers (the Weave landing host, the cloud) must reproduce the
//! digest and the signed bytes byte for byte. The canonical form is:
//!
//! - UTF-8 JSON, compact: no whitespace between tokens, no trailing newline.
//! - Object keys sorted by their UTF-8 bytes, recursively (also inside
//!   `signing`).
//! - Optional fields that are absent are omitted, never `null`
//!   (`landed_sha`, `signing`).
//! - Integers in plain decimal (`schema_version`, `request`); strings with
//!   the JSON escapes only where required (`"`, `\`, control characters);
//!   non-ASCII characters are written as UTF-8, not `\u` escapes.
//! - Enum values are the snake_case names (`gitlab`, `landed`).
//! - Digest input: the receipt without `signing`. Signed bytes: the receipt
//!   with `signing` minus `signing.signature` (so `algorithm` and
//!   `public_key` are covered).
//!
//! The tests pin a literal digest and the exact signed bytes of a sample
//! receipt; a change of either is a wire break and needs a new
//! `schema_version`.
//!
//! `signing.public_key` must be the canonical (padded, standard alphabet)
//! base64 of a 32-byte Ed25519 key, so each key has one spelling.
//!
//! "Signed" means the signature verifies against the embedded key; it does
//! not say who holds that key. [`LandingReceiptV1::signer_key_id`] names the
//! key as `id:sha256:<hex of SHA-256 over the 32 key bytes>`, the `key_id`
//! form of signed reports, so a reader can compare it with a key they trust.

use std::collections::BTreeMap;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{Sha256Digest, UtcTimestamp, ValidationError, validate_bounded_string};

/// The `kind` every landing receipt carries.
pub const LANDING_RECEIPT_KIND: &str = "landing_receipt";

/// Largest receipt document accepted, in bytes.
pub const MAX_LANDING_RECEIPT_BYTES: usize = 16 * 1024;

/// The forge the change request lives on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LandingForgeV1 {
    Gitlab,
    Github,
}

impl LandingForgeV1 {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gitlab => "gitlab",
            Self::Github => "github",
        }
    }
}

/// What happened to the change request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LandingVerdictV1 {
    /// Merged onto the target branch; `landed_sha` names the new tip.
    Landed,
    /// Not merged (checks red, conflict, policy); no `landed_sha`.
    Refused,
    /// Merged, then reverted after landing; `landed_sha` names the landed commit.
    Reverted,
}

impl LandingVerdictV1 {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Landed => "landed",
            Self::Refused => "refused",
            Self::Reverted => "reverted",
        }
    }
}

/// Optional Ed25519 signature over the receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LandingSigningV1 {
    /// Always `ed25519`.
    pub algorithm: String,
    /// Base64 of the 32-byte public key.
    pub public_key: String,
    /// Base64 of the 64-byte signature.
    pub signature: String,
}

/// One landing outcome of one change request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LandingReceiptV1 {
    pub schema_version: u32,
    /// Always [`LANDING_RECEIPT_KIND`].
    pub kind: String,
    pub forge: LandingForgeV1,
    /// Forge project path or numeric id.
    pub project: String,
    /// Merge request / pull request number.
    pub request: u64,
    /// The landing service's own id of this landing attempt.
    pub request_id: String,
    pub target_branch: String,
    /// Target branch tip the checks ran against.
    pub base_sha: String,
    /// Change request head that was checked.
    pub head_sha: String,
    /// Commit that landed; required for `landed` and `reverted`, absent for `refused`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub landed_sha: Option<String>,
    pub verdict: LandingVerdictV1,
    /// Digest of the selected checks (names and commands), not their output.
    pub check_selection_digest: Sha256Digest,
    pub decided_at: UtcTimestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signing: Option<LandingSigningV1>,
}

fn validate_sha(value: &str, field: &str) -> Result<(), ValidationError> {
    let hex = value.len() == 40 || value.len() == 64;
    if hex
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        Ok(())
    } else {
        Err(ValidationError::new(format!(
            "{field} must be a full lowercase hex commit id (40 or 64 characters)"
        )))
    }
}

/// The 32 key bytes when `public_key` is their canonical base64.
fn signing_key_bytes(public_key: &str) -> Option<[u8; 32]> {
    let bytes = STANDARD.decode(public_key).ok()?;
    let key: [u8; 32] = bytes.try_into().ok()?;
    (STANDARD.encode(key) == public_key).then_some(key)
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
            use std::fmt::Write as _;
            let _ = write!(out, "{byte:02x}");
            out
        })
}

fn sorted(value: Value) -> Value {
    match value {
        Value::Object(object) => {
            let ordered: BTreeMap<String, Value> =
                object.into_iter().map(|(k, v)| (k, sorted(v))).collect();
            Value::Object(ordered.into_iter().collect())
        }
        Value::Array(values) => Value::Array(values.into_iter().map(sorted).collect()),
        scalar => scalar,
    }
}

impl LandingReceiptV1 {
    /// Parse and validate a receipt document.
    pub fn from_json(bytes: &[u8]) -> Result<Self, ValidationError> {
        if bytes.len() > MAX_LANDING_RECEIPT_BYTES {
            return Err(ValidationError::new(format!(
                "landing receipt exceeds {MAX_LANDING_RECEIPT_BYTES} bytes"
            )));
        }
        let receipt: Self = serde_json::from_slice(bytes)
            .map_err(|error| ValidationError::new(format!("invalid landing receipt: {error}")))?;
        receipt.validate()?;
        Ok(receipt)
    }

    /// Check every wire invariant.
    pub fn validate(&self) -> Result<(), ValidationError> {
        crate::validate_schema_version(self.schema_version)?;
        if self.kind != LANDING_RECEIPT_KIND {
            return Err(ValidationError::new(format!(
                "kind must be {LANDING_RECEIPT_KIND}"
            )));
        }
        validate_bounded_string(&self.project, "project")?;
        validate_bounded_string(&self.request_id, "request_id")?;
        validate_bounded_string(&self.target_branch, "target_branch")?;
        if self.request == 0 {
            return Err(ValidationError::new("request must be a positive number"));
        }
        validate_sha(&self.base_sha, "base_sha")?;
        validate_sha(&self.head_sha, "head_sha")?;
        match (self.verdict, &self.landed_sha) {
            (LandingVerdictV1::Refused, Some(_)) => {
                return Err(ValidationError::new("a refused landing has no landed_sha"));
            }
            (LandingVerdictV1::Landed | LandingVerdictV1::Reverted, None) => {
                return Err(ValidationError::new(format!(
                    "a {} landing needs landed_sha",
                    self.verdict.as_str()
                )));
            }
            (_, Some(sha)) => validate_sha(sha, "landed_sha")?,
            (LandingVerdictV1::Refused, None) => {}
        }
        if let Some(signing) = &self.signing {
            if signing.algorithm != "ed25519" {
                return Err(ValidationError::new("signing.algorithm must be ed25519"));
            }
            validate_bounded_string(&signing.public_key, "signing.public_key")?;
            validate_bounded_string(&signing.signature, "signing.signature")?;
            if signing_key_bytes(&signing.public_key).is_none() {
                return Err(ValidationError::new(
                    "signing.public_key must be the canonical base64 of a 32-byte Ed25519 key",
                ));
            }
        }
        Ok(())
    }

    /// Canonical JSON (compact, keys sorted) of the full receipt.
    #[must_use]
    pub fn canonical_json(&self) -> Vec<u8> {
        let value = serde_json::to_value(self).expect("receipt serializes");
        serde_json::to_vec(&sorted(value)).expect("JSON values serialize")
    }

    /// Content id: SHA-256 of the canonical JSON without `signing`.
    #[must_use]
    pub fn digest(&self) -> Sha256Digest {
        let unsigned = Self {
            signing: None,
            ..self.clone()
        };
        let hex = Sha256::digest(unsigned.canonical_json()).iter().fold(
            String::with_capacity(64),
            |mut out, byte| {
                use std::fmt::Write as _;
                let _ = write!(out, "{byte:02x}");
                out
            },
        );
        Sha256Digest::new(format!("sha256:{hex}")).expect("hex digest is canonical")
    }

    /// The bytes an Ed25519 signer signs: the canonical JSON of the receipt
    /// with `signing.signature` removed (public key and algorithm stay).
    #[must_use]
    pub fn signing_payload(&self) -> Vec<u8> {
        let mut value = serde_json::to_value(self).expect("receipt serializes");
        if let Some(signing) = value.get_mut("signing").and_then(Value::as_object_mut) {
            signing.remove("signature");
        }
        serde_json::to_vec(&sorted(value)).expect("JSON values serialize")
    }

    /// Id of the signing key, `id:sha256:<hex>` over the 32 public-key
    /// bytes (the `key_id` form of signed reports). `None` for a hashed
    /// receipt or a key that is not canonical base64 of 32 bytes.
    #[must_use]
    pub fn signer_key_id(&self) -> Option<String> {
        let key = signing_key_bytes(&self.signing.as_ref()?.public_key)?;
        Some(format!("id:sha256:{}", hex(&Sha256::digest(key))))
    }

    /// One deterministic summary line. A signed receipt names its key's
    /// fingerprint: the signature is self-consistent, not trusted by itself.
    #[must_use]
    pub fn summary_line(&self) -> String {
        let landed = self.landed_sha.as_deref().map_or("-", |sha| &sha[..12]);
        format!(
            "{} {} {}!{} {} -> {} base {} landed {} checks {} [{}]",
            self.decided_at.as_str(),
            self.verdict.as_str(),
            self.forge.as_str(),
            self.request,
            self.project,
            self.target_branch,
            &self.base_sha[..12],
            landed,
            &self.check_selection_digest.hex()[..12],
            self.signer_key_id().map_or_else(
                || "hashed".to_string(),
                |id| format!("signed key {}", &id["id:sha256:".len()..][..16]),
            ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> LandingReceiptV1 {
        LandingReceiptV1 {
            schema_version: 1,
            kind: LANDING_RECEIPT_KIND.to_string(),
            forge: LandingForgeV1::Gitlab,
            project: "group/app".to_string(),
            request: 42,
            request_id: "land-42-1".to_string(),
            target_branch: "main".to_string(),
            base_sha: "a".repeat(40),
            head_sha: "b".repeat(40),
            landed_sha: Some("c".repeat(40)),
            verdict: LandingVerdictV1::Landed,
            check_selection_digest: Sha256Digest::new(format!("sha256:{}", "d".repeat(64)))
                .unwrap(),
            decided_at: UtcTimestamp::new("2026-11-18T10:00:00Z").unwrap(),
            signing: None,
        }
    }

    #[test]
    fn landing_receipt_round_trips_through_json() {
        let receipt = sample();
        let bytes = serde_json::to_vec_pretty(&receipt).unwrap();
        let parsed = LandingReceiptV1::from_json(&bytes).unwrap();
        assert_eq!(parsed, receipt);
        assert_eq!(parsed.digest(), receipt.digest());
        assert_eq!(parsed.canonical_json(), receipt.canonical_json());
    }

    /// Canonical JSON of [`sample`]; any drift is a wire break.
    const SAMPLE_CANONICAL: &str = concat!(
        r#"{"base_sha":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","#,
        r#""check_selection_digest":"sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd","#,
        r#""decided_at":"2026-11-18T10:00:00Z","forge":"gitlab","#,
        r#""head_sha":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","kind":"landing_receipt","#,
        r#""landed_sha":"cccccccccccccccccccccccccccccccccccccccc","project":"group/app","#,
        r#""request":42,"request_id":"land-42-1","schema_version":1,"#,
        r#""target_branch":"main","verdict":"landed"}"#
    );
    const SAMPLE_DIGEST: &str =
        "sha256:df35a7e09a04f3a077c3a1f06672423d4360b03a9008820646cf192c903448dd";

    #[test]
    fn landing_receipt_canonical_bytes_and_digest_are_literal() {
        let receipt = sample();
        assert_eq!(
            String::from_utf8(receipt.canonical_json()).unwrap(),
            SAMPLE_CANONICAL
        );
        assert_eq!(receipt.digest().as_str(), SAMPLE_DIGEST);
        // The public key is the bytes 0..32, so the pin stays reproducible.
        let key = STANDARD.encode(std::array::from_fn::<u8, 32, _>(|i| i as u8));
        let signed = LandingReceiptV1 {
            signing: Some(LandingSigningV1 {
                algorithm: "ed25519".into(),
                public_key: key.clone(),
                signature: "c2ln".into(),
            }),
            ..receipt
        };
        signed.validate().unwrap();
        let expected_payload = SAMPLE_CANONICAL.replace(
            r#""schema_version":1,"#,
            &format!(
                r#""schema_version":1,"signing":{{"algorithm":"ed25519","public_key":"{key}"}},"#
            ),
        );
        assert_eq!(
            String::from_utf8(signed.signing_payload()).unwrap(),
            expected_payload
        );
        assert_eq!(signed.digest().as_str(), SAMPLE_DIGEST);
        assert_eq!(
            signed.signer_key_id().as_deref(),
            Some("id:sha256:630dcd2966c4336691125448bbb25b4ff412a49c732db2c8abc1b8581bd710dd")
        );
        assert!(
            signed
                .summary_line()
                .ends_with("[signed key 630dcd2966c43366]"),
            "{}",
            signed.summary_line()
        );
        assert!(sample().summary_line().ends_with("[hashed]"));
        // One spelling per key: unpadded, short or non-32-byte keys are refused.
        for bad in [key.trim_end_matches('='), "cHVi", "AAEC"] {
            let mut receipt = signed.clone();
            receipt.signing.as_mut().unwrap().public_key = bad.to_string();
            assert!(receipt.validate().is_err(), "{bad}");
            assert_eq!(receipt.signer_key_id(), None);
        }
    }

    #[test]
    fn landing_receipt_digest_is_pinned_and_ignores_signing() {
        let receipt = sample();
        let digest = receipt.digest();
        assert_eq!(digest.as_str(), SAMPLE_DIGEST);
        let signed = LandingReceiptV1 {
            signing: Some(LandingSigningV1 {
                algorithm: "ed25519".into(),
                public_key: "cHVi".into(),
                signature: "c2ln".into(),
            }),
            ..receipt.clone()
        };
        assert_eq!(signed.digest(), digest);
        let other = LandingReceiptV1 {
            request: 43,
            ..receipt
        };
        assert_ne!(other.digest(), digest);
        let payload = String::from_utf8(signed.signing_payload()).unwrap();
        assert!(payload.contains("\"public_key\":\"cHVi\""));
        assert!(!payload.contains("c2ln"));
    }

    #[test]
    fn landing_receipt_refuses_inconsistent_verdicts_and_unknown_fields() {
        let refused_with_sha = LandingReceiptV1 {
            verdict: LandingVerdictV1::Refused,
            ..sample()
        };
        assert!(refused_with_sha.validate().is_err());
        let landed_without_sha = LandingReceiptV1 {
            landed_sha: None,
            ..sample()
        };
        assert!(landed_without_sha.validate().is_err());
        let refused = LandingReceiptV1 {
            verdict: LandingVerdictV1::Refused,
            landed_sha: None,
            ..sample()
        };
        refused.validate().unwrap();
        assert!(refused.summary_line().contains("refused gitlab!42"));
        let short_sha = LandingReceiptV1 {
            base_sha: "abc".into(),
            ..sample()
        };
        assert!(short_sha.validate().is_err());
        let mut value = serde_json::to_value(sample()).unwrap();
        value["diff"] = Value::String("--- a/x".into());
        assert!(LandingReceiptV1::from_json(&serde_json::to_vec(&value).unwrap()).is_err());
    }
}
