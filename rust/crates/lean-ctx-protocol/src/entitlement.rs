// SPDX-License-Identifier: Apache-2.0
//! Bounded, canonical entitlement wire contract, not an authorization decision.
//!
//! Callers must separately verify the signature against trusted keys, account /
//! workspace / deployment binding, revocation, and current-time policy. Plain
//! serde decoding and public-field construction do not validate the envelope.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Sha256Digest, ValidationError, deserialize_schema_version, validate_schema_version};

pub const ENTITLEMENT_SIGNATURE_DOMAIN: &[u8] = b"leanctx/entitlement/v1\0";
pub const MAX_ENTITLEMENT_BYTES: usize = 32 * 1024;
pub const MAX_ENTITLEMENT_CAPABILITIES: usize = 256;
pub const MAX_ENTITLEMENT_CAPABILITY_BYTES: usize = 128;
pub const MAX_ENTITLEMENT_IDENTIFIER_BYTES: usize = 256;
pub const MAX_ENTITLEMENT_TIMESTAMP: u64 = 9_007_199_254_740_991;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntitlementKindV1 {
    OnlineSubscription,
    OfflineEnterprise,
}

/// Canonical product tiers only; commercial SKU aliases never enter this wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntitlementPlanV1 {
    Community,
    Pro,
    Team,
    Enterprise,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntitlementDeploymentV1 {
    Hosted,
    SelfHosted,
    AirGapped,
}

/// Signed key metadata, never an embedded trust anchor or private key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntitlementSignerV1 {
    pub algorithm: String,
    pub key_id: String,
    pub public_key_digest: Sha256Digest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntitlementEnvelopeV1 {
    #[serde(deserialize_with = "deserialize_schema_version")]
    pub schema_version: u32,
    pub entitlement_id: String,
    pub kind: EntitlementKindV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    pub plan: EntitlementPlanV1,
    pub seats: u32,
    pub capabilities: Vec<String>,
    pub issued_at: u64,
    pub not_before: u64,
    pub expires_at: u64,
    pub grace_until: u64,
    pub allowed_deployments: Vec<EntitlementDeploymentV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deployment_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub org_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    pub signer: EntitlementSignerV1,
    pub signature: String,
}

impl EntitlementEnvelopeV1 {
    pub const SCHEMA_VERSION: u32 = 1;

    /// Decode one bounded canonical JSON object, rejecting duplicates at every
    /// object level, unknown fields, malformed UTF-8 and noncanonical spelling.
    /// This proves structure only, never signature authenticity or authorization.
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self, ValidationError> {
        if bytes.len() > MAX_ENTITLEMENT_BYTES {
            return Err(ValidationError::new("entitlement exceeds 32 KiB"));
        }
        // Direct typed deserialization preserves serde's duplicate-field checks;
        // an intermediate Value would silently collapse duplicate object keys.
        let envelope: Self = serde_json::from_slice(bytes)
            .map_err(|_| ValidationError::new("invalid entitlement JSON or field shape"))?;
        if envelope.canonical_bytes()? != bytes {
            return Err(ValidationError::new("entitlement JSON is not canonical"));
        }
        Ok(envelope)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ValidationError> {
        Self::from_canonical_bytes(bytes)
    }

    /// Compact UTF-8 JSON with recursively lexical object keys, signature included.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ValidationError> {
        self.validate()?;
        canonical_value(self, false)
    }

    /// Every field except signature; permits an empty signature before signing.
    pub fn unsigned_canonical_bytes(&self) -> Result<Vec<u8>, ValidationError> {
        self.validate_unsigned()?;
        canonical_value(self, true)
    }

    /// Ed25519 input: versioned domain including its NUL, then unsigned JSON.
    pub fn signing_bytes(&self) -> Result<Vec<u8>, ValidationError> {
        let unsigned = self.unsigned_canonical_bytes()?;
        let mut bytes = Vec::with_capacity(ENTITLEMENT_SIGNATURE_DOMAIN.len() + unsigned.len());
        bytes.extend_from_slice(ENTITLEMENT_SIGNATURE_DOMAIN);
        bytes.extend_from_slice(&unsigned);
        Ok(bytes)
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        self.validate_unsigned()?;
        let bytes = self.signature.as_bytes();
        // 64 bytes have 86 meaningful RFC 4648 characters followed by ==.
        // The final sextet's low four bits must be zero (A, Q, g or w).
        if bytes.len() != 88
            || &bytes[86..] != b"=="
            || !bytes[..86]
                .iter()
                .all(|b| b.is_ascii_alphanumeric() || *b == b'+' || *b == b'/')
            || !matches!(bytes[85], b'A' | b'Q' | b'g' | b'w')
        {
            return Err(ValidationError::new(
                "entitlement signature must be canonical base64 of 64 Ed25519 bytes",
            ));
        }
        Ok(())
    }

    fn validate_unsigned(&self) -> Result<(), ValidationError> {
        validate_schema_version(self.schema_version)?;
        validate_id(
            &self.entitlement_id,
            "entitlement_id",
            MAX_ENTITLEMENT_IDENTIFIER_BYTES,
        )?;
        for (field, value) in [
            ("account_id", &self.account_id),
            ("deployment_id", &self.deployment_id),
            ("org_id", &self.org_id),
            ("workspace_id", &self.workspace_id),
        ] {
            if let Some(value) = value {
                validate_id(value, field, MAX_ENTITLEMENT_IDENTIFIER_BYTES)?;
            }
        }
        if self.seats == 0 {
            return Err(ValidationError::new("entitlement seats must be positive"));
        }
        if self.capabilities.len() > MAX_ENTITLEMENT_CAPABILITIES {
            return Err(ValidationError::new(
                "entitlement has more than 256 capabilities",
            ));
        }
        for capability in &self.capabilities {
            validate_id(capability, "capability", MAX_ENTITLEMENT_CAPABILITY_BYTES)?;
        }
        if self.capabilities.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(ValidationError::new(
                "entitlement capabilities must be sorted and unique",
            ));
        }
        if [
            self.issued_at,
            self.not_before,
            self.expires_at,
            self.grace_until,
        ]
        .into_iter()
        .any(|time| time > MAX_ENTITLEMENT_TIMESTAMP)
        {
            return Err(ValidationError::new(
                "entitlement timestamp exceeds exact JSON integer range",
            ));
        }
        if !(self.not_before <= self.issued_at
            && self.issued_at < self.expires_at
            && self.expires_at <= self.grace_until)
        {
            return Err(ValidationError::new("entitlement time window is invalid"));
        }
        if self.allowed_deployments.is_empty()
            || self.allowed_deployments.len() > 3
            || self
                .allowed_deployments
                .iter()
                .enumerate()
                .any(|(i, value)| self.allowed_deployments[..i].contains(value))
        {
            return Err(ValidationError::new(
                "allowed deployments must be nonempty and unique",
            ));
        }
        match self.kind {
            EntitlementKindV1::OnlineSubscription if self.account_id.is_none() => {
                return Err(ValidationError::new(
                    "online entitlement requires account_id",
                ));
            }
            EntitlementKindV1::OfflineEnterprise
                if self.plan != EntitlementPlanV1::Enterprise
                    || self.deployment_id.is_none()
                    || self
                        .allowed_deployments
                        .contains(&EntitlementDeploymentV1::Hosted) =>
            {
                return Err(ValidationError::new(
                    "offline entitlement requires Enterprise, deployment_id and no hosted deployment",
                ));
            }
            _ => {}
        }
        if self.signer.algorithm != "ed25519" {
            return Err(ValidationError::new(
                "entitlement signer algorithm must be ed25519",
            ));
        }
        validate_id(
            &self.signer.key_id,
            "key_id",
            MAX_ENTITLEMENT_IDENTIFIER_BYTES,
        )?;
        // The digest newtype already enforces canonical sha256:<64 lowercase hex>.
        // Reserve the exact valid signature member size even before signing.
        if canonical_value(self, true)?.len() + b",\"signature\":\"\"".len() + 88
            > MAX_ENTITLEMENT_BYTES
        {
            return Err(ValidationError::new("entitlement exceeds 32 KiB"));
        }
        Ok(())
    }
}

fn validate_id(value: &str, field: &str, max_bytes: usize) -> Result<(), ValidationError> {
    if value.is_empty() || value.len() > max_bytes || !value.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(ValidationError::new(format!(
            "{field} must be 1..={max_bytes} printable ASCII bytes without whitespace"
        )));
    }
    Ok(())
}

fn canonical_value(
    envelope: &EntitlementEnvelopeV1,
    unsigned: bool,
) -> Result<Vec<u8>, ValidationError> {
    let mut value = serde_json::to_value(envelope)
        .map_err(|_| ValidationError::new("cannot serialize entitlement"))?;
    if unsigned {
        let Value::Object(object) = &mut value else {
            return Err(ValidationError::new("entitlement must be an object"));
        };
        object.remove("signature");
    }
    serde_json::to_vec(&sort_json(value))
        .map_err(|_| ValidationError::new("cannot canonicalize entitlement"))
}

// Match the existing protocol canonicalization, including when Cargo feature
// unification enables serde_json/preserve_order in a consuming application.
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

#[cfg(test)]
mod tests {
    use super::{
        ENTITLEMENT_SIGNATURE_DOMAIN, EntitlementDeploymentV1, EntitlementEnvelopeV1,
        EntitlementKindV1, EntitlementPlanV1, EntitlementSignerV1, MAX_ENTITLEMENT_BYTES,
        MAX_ENTITLEMENT_TIMESTAMP,
    };
    use crate::Sha256Digest;

    fn envelope() -> EntitlementEnvelopeV1 {
        EntitlementEnvelopeV1 {
            schema_version: 1,
            entitlement_id: "ent-1".into(),
            kind: EntitlementKindV1::OnlineSubscription,
            account_id: Some("account-1".into()),
            plan: EntitlementPlanV1::Pro,
            seats: 1,
            capabilities: vec!["context.autopilot".into(), "execution.autopilot".into()],
            issued_at: 100,
            not_before: 99,
            expires_at: 200,
            grace_until: 210,
            allowed_deployments: vec![EntitlementDeploymentV1::Hosted],
            deployment_id: None,
            org_id: None,
            workspace_id: None,
            signer: EntitlementSignerV1 {
                algorithm: "ed25519".into(),
                key_id: "key-1".into(),
                public_key_digest: Sha256Digest::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
            },
            signature: "A".repeat(86) + "==",
        }
    }

    #[test]
    fn golden_canonical_unsigned_domain_and_round_trip() {
        let envelope = envelope();
        let expected = concat!(
            "{\"account_id\":\"account-1\",\"allowed_deployments\":[\"hosted\"],",
            "\"capabilities\":[\"context.autopilot\",\"execution.autopilot\"],",
            "\"entitlement_id\":\"ent-1\",\"expires_at\":200,\"grace_until\":210,",
            "\"issued_at\":100,\"kind\":\"online_subscription\",\"not_before\":99,",
            "\"plan\":\"pro\",\"schema_version\":1,\"seats\":1,",
            "\"signer\":{\"algorithm\":\"ed25519\",\"key_id\":\"key-1\",",
            "\"public_key_digest\":\"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"}}"
        );
        assert_eq!(
            envelope.unsigned_canonical_bytes().unwrap(),
            expected.as_bytes()
        );
        let mut signing = b"leanctx/entitlement/v1\0".to_vec();
        signing.extend_from_slice(expected.as_bytes());
        assert_eq!(envelope.signing_bytes().unwrap(), signing);
        assert_eq!(ENTITLEMENT_SIGNATURE_DOMAIN.last(), Some(&0));
        let bytes = envelope.canonical_bytes().unwrap();
        assert_eq!(EntitlementEnvelopeV1::from_bytes(&bytes).unwrap(), envelope);
    }

    #[test]
    fn duplicate_unknown_nested_and_optional_fields_are_rejected() {
        let bytes = envelope().canonical_bytes().unwrap();
        let canonical = String::from_utf8(bytes).unwrap();
        for bad in [
            canonical.replacen("{", "{\"schema_version\":1,", 1),
            canonical.replacen("{", "{\"future\":true,", 1),
            canonical.replace("\"algorithm\":", "\"algorithm\":\"ed25519\",\"algorithm\":"),
            canonical.replace("\"algorithm\":", "\"unknown\":0,\"algorithm\":"),
            canonical.replacen("{", "{\"org_id\":null,\"org_id\":null,", 1),
            canonical.replacen("{", "{\"account_id\":null,", 1),
        ] {
            assert!(EntitlementEnvelopeV1::from_bytes(bad.as_bytes()).is_err());
            // The typed serde path itself rejects duplicates/unknowns, not just
            // the subsequent canonical comparison.
            assert!(serde_json::from_str::<EntitlementEnvelopeV1>(&bad).is_err());
        }
    }

    #[test]
    fn strict_decode_rejects_noncanonical_utf8_trailing_and_oversize_input() {
        let canonical = envelope().canonical_bytes().unwrap();
        let mut spaced = canonical.clone();
        spaced.push(b'\n');
        let mut trailing = canonical.clone();
        trailing.extend_from_slice(b"{}");
        for invalid in [
            spaced,
            trailing,
            vec![0xff],
            vec![b' '; MAX_ENTITLEMENT_BYTES + 1],
        ] {
            assert!(EntitlementEnvelopeV1::from_bytes(&invalid).is_err());
        }
        let text = String::from_utf8(canonical).unwrap();
        for invalid in [
            text.replace("account-1", "account\\u002d1"),
            text.replacen("{", "{\"org_id\":null,", 1),
            text.replace("\"seats\":1", "\"seats\":1.0"),
            text.replace("\"issued_at\":100", "\"issued_at\":1e2"),
            text.replace("\"issued_at\":100", "\"issued_at\":-1"),
            text.replace("\"schema_version\":1", "\"schema_version\":1.0"),
        ] {
            assert!(EntitlementEnvelopeV1::from_bytes(invalid.as_bytes()).is_err());
        }
    }

    #[test]
    fn only_four_canonical_plans_and_two_kinds_are_decoded() {
        for plan in [
            EntitlementPlanV1::Community,
            EntitlementPlanV1::Pro,
            EntitlementPlanV1::Team,
            EntitlementPlanV1::Enterprise,
        ] {
            let mut value = envelope();
            value.plan = plan;
            assert_eq!(
                EntitlementEnvelopeV1::from_bytes(&value.canonical_bytes().unwrap())
                    .unwrap()
                    .plan,
                plan
            );
        }
        let bytes = String::from_utf8(envelope().canonical_bytes().unwrap()).unwrap();
        for plan in ["business", "biz", "supporter", "sponsor", "Pro", "unknown"] {
            assert!(
                EntitlementEnvelopeV1::from_bytes(
                    bytes.replace("\"pro\"", &format!("\"{plan}\"")).as_bytes()
                )
                .is_err()
            );
        }
        assert!(
            EntitlementEnvelopeV1::from_bytes(
                bytes.replace("online_subscription", "trial").as_bytes()
            )
            .is_err()
        );
    }

    #[test]
    fn online_and_offline_bindings_are_distinct() {
        let mut value = envelope();
        value.account_id = None;
        assert!(value.validate().is_err());
        value.kind = EntitlementKindV1::OfflineEnterprise;
        value.plan = EntitlementPlanV1::Enterprise;
        value.deployment_id = Some("deployment-1".into());
        assert!(value.validate().is_err()); // hosted forbidden offline
        value.allowed_deployments = vec![
            EntitlementDeploymentV1::SelfHosted,
            EntitlementDeploymentV1::AirGapped,
        ];
        value.validate().unwrap();
        assert_eq!(
            EntitlementEnvelopeV1::from_bytes(&value.canonical_bytes().unwrap()).unwrap(),
            value
        );
        value.plan = EntitlementPlanV1::Team;
        assert!(value.validate().is_err());
        value.plan = EntitlementPlanV1::Enterprise;
        value.deployment_id = None;
        assert!(value.validate().is_err());
    }

    #[test]
    fn deployment_sets_are_nonempty_unique_and_known() {
        let mut value = envelope();
        value.allowed_deployments.clear();
        assert!(value.validate().is_err());
        value.allowed_deployments = vec![EntitlementDeploymentV1::Hosted; 2];
        assert!(value.validate().is_err());
        let bytes = String::from_utf8(envelope().canonical_bytes().unwrap()).unwrap();
        assert!(
            EntitlementEnvelopeV1::from_bytes(
                bytes.replace("\"hosted\"", "\"anything\"").as_bytes()
            )
            .is_err()
        );
    }

    #[test]
    fn identifier_and_capability_ascii_length_order_bounds() {
        for invalid in [
            "".to_string(),
            "a b".into(),
            "a\n".into(),
            "é".into(),
            "\u{7f}".into(),
            "x".repeat(257),
        ] {
            for field in 0..6 {
                let mut value = envelope();
                match field {
                    0 => value.entitlement_id = invalid.clone(),
                    1 => value.account_id = Some(invalid.clone()),
                    2 => value.deployment_id = Some(invalid.clone()),
                    3 => value.org_id = Some(invalid.clone()),
                    4 => value.workspace_id = Some(invalid.clone()),
                    _ => value.signer.key_id = invalid.clone(),
                }
                assert!(value.validate().is_err());
            }
        }
        let mut value = envelope();
        value.entitlement_id = "x".repeat(256);
        value.capabilities = vec!["x".repeat(128)];
        value.validate().unwrap();
        for invalid in [
            vec!["x".repeat(129)],
            vec!["é".into()],
            vec!["".into()],
            vec!["a b".into()],
            vec!["a".into(), "a".into()],
            vec!["z".into(), "a".into()],
        ] {
            value.capabilities = invalid;
            assert!(value.validate().is_err());
        }
        value.capabilities = (0..256).map(|i| format!("cap-{i:03}")).collect();
        value.validate().unwrap();
        value.capabilities.push("cap-256".into());
        assert!(value.validate().is_err());
        value.capabilities.clear(); // empty is structural, not a paid-feature grant
        value.validate().unwrap();
    }

    #[test]
    fn combined_byte_bound_applies_even_with_individually_valid_capabilities() {
        let mut value = envelope();
        value.capabilities = (0..256)
            .map(|i| format!("{i:03}{}", "x".repeat(125)))
            .collect();
        assert!(value.validate().is_err());
        assert!(value.signing_bytes().is_err());
        assert!(value.canonical_bytes().is_err());
    }

    #[test]
    fn exact_full_envelope_byte_boundary_includes_signature_member() {
        let mut value = envelope();
        value.capabilities = (0..256).map(|i| format!("{i:03}")).collect();
        let mut remaining = MAX_ENTITLEMENT_BYTES - value.canonical_bytes().unwrap().len();
        for capability in &mut value.capabilities {
            let extra = remaining.min(128 - capability.len());
            capability.push_str(&"x".repeat(extra));
            remaining -= extra;
        }
        assert_eq!(remaining, 0);
        let bytes = value.canonical_bytes().unwrap();
        assert_eq!(bytes.len(), MAX_ENTITLEMENT_BYTES);
        assert_eq!(EntitlementEnvelopeV1::from_bytes(&bytes).unwrap(), value);
        value
            .capabilities
            .iter_mut()
            .find(|capability| capability.len() < 128)
            .unwrap()
            .push('x');
        assert!(value.validate().is_err());
        assert!(value.unsigned_canonical_bytes().is_err());
    }

    #[test]
    fn timestamps_seats_and_schema_fail_closed() {
        let mut value = envelope();
        value.not_before = 0;
        value.issued_at = 0;
        value.expires_at = MAX_ENTITLEMENT_TIMESTAMP;
        value.grace_until = MAX_ENTITLEMENT_TIMESTAMP;
        value.validate().unwrap();
        for field in 0..4 {
            let mut invalid = envelope();
            match field {
                0 => invalid.issued_at = MAX_ENTITLEMENT_TIMESTAMP + 1,
                1 => invalid.not_before = MAX_ENTITLEMENT_TIMESTAMP + 1,
                2 => invalid.expires_at = MAX_ENTITLEMENT_TIMESTAMP + 1,
                _ => invalid.grace_until = MAX_ENTITLEMENT_TIMESTAMP + 1,
            }
            assert!(invalid.validate().is_err());
        }
        for (not_before, issued_at, expires_at, grace_until) in [
            (101, 100, 200, 210),
            (99, 100, 100, 210),
            (99, 100, 200, 199),
        ] {
            let mut invalid = envelope();
            invalid.not_before = not_before;
            invalid.issued_at = issued_at;
            invalid.expires_at = expires_at;
            invalid.grace_until = grace_until;
            assert!(invalid.validate().is_err());
        }
        value.seats = 0;
        assert!(value.validate().is_err());
        value.seats = 1;
        value.schema_version = 2;
        assert!(value.validate().is_err());
    }

    #[test]
    fn canonical_signature_length_alphabet_and_pad_bits_are_checked_without_trust_claim() {
        for signature in [
            "".to_string(),
            "A".repeat(88),
            "A".repeat(85) + "B==",
            "A".repeat(85) + "_==",
            "A".repeat(85) + "-==",
            "A".repeat(86) + "=",
            "é".repeat(43) + "==",
        ] {
            let mut invalid = envelope();
            invalid.signature = signature;
            assert!(invalid.validate().is_err());
            assert!(invalid.signing_bytes().is_ok());
        }
        for last in ['A', 'Q', 'g', 'w'] {
            let mut value = envelope();
            value.signature = format!("{}{last}==", "A".repeat(85));
            value.validate().unwrap(); // shape only; all-zero signature is not trusted
        }
        let mut value = envelope();
        value.signer.algorithm = "ed25519ph".into();
        assert!(value.validate().is_err());
    }

    #[test]
    fn signature_coverage_includes_all_other_fields() {
        let original = envelope();
        let expected = original.signing_bytes().unwrap();
        let original_json = serde_json::to_value(&original).unwrap();
        let mutations = [
            ("entitlement_id", serde_json::json!("ent-2")),
            ("account_id", serde_json::json!("account-2")),
            ("plan", serde_json::json!("team")),
            ("seats", serde_json::json!(2)),
            ("capabilities", serde_json::json!(["context.autopilot"])),
            ("issued_at", serde_json::json!(101)),
            ("not_before", serde_json::json!(98)),
            ("expires_at", serde_json::json!(201)),
            ("grace_until", serde_json::json!(211)),
            ("allowed_deployments", serde_json::json!(["self_hosted"])),
            ("deployment_id", serde_json::json!("deployment-1")),
            ("org_id", serde_json::json!("org-1")),
            ("workspace_id", serde_json::json!("workspace-1")),
        ];
        for (field, mutation) in mutations {
            let mut json = original_json.clone();
            json[field] = mutation;
            let value: EntitlementEnvelopeV1 = serde_json::from_value(json).unwrap();
            assert_ne!(value.signing_bytes().unwrap(), expected, "{field}");
        }
        for field in ["key_id", "public_key_digest"] {
            let mut json = original_json.clone();
            json["signer"][field] = if field == "key_id" {
                serde_json::json!("key-2")
            } else {
                serde_json::json!(format!("sha256:{}", "b".repeat(64)))
            };
            let value: EntitlementEnvelopeV1 = serde_json::from_value(json).unwrap();
            assert_ne!(value.signing_bytes().unwrap(), expected);
        }
        let mut value = original.clone();
        value.signature.replace_range(..1, "B");
        assert_eq!(value.signing_bytes().unwrap(), expected);
        assert_ne!(
            value.canonical_bytes().unwrap(),
            original.canonical_bytes().unwrap()
        );
    }
}
