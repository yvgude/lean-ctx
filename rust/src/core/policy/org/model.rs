//! The signed org-policy artifact (GL #674).
//!
//! [`OrgPolicyV1`] is how an organisation distributes one **central, signed**
//! policy pack to every endpoint. The admin authors a normal pack
//! ([`crate::core::policy::PolicyPack`]), wraps its TOML source in this artifact
//! and **Ed25519-signs** it; clients that have pinned the org's public key
//! ([`super::trust`]) verify the signature **offline** before the runtime folds
//! the pack in as an un-bypassable *floor* ([`crate::core::policy::floor`]).
//!
//! Signing mirrors [`crate::core::savings_ledger::signed_batch`] and the
//! compliance report: the two signature fields are cleared while computing the
//! canonical bytes, so a verifier reproduces the exact signed payload from the
//! artifact alone. The authoritative content is `pack_toml` — the verbatim pack
//! source — which every client re-parses and re-validates itself, so a tampered
//! pack body fails both validation *and* the signature.

use ed25519_dalek::{Signer, SigningKey};
use serde::{Deserialize, Serialize};

use crate::core::policy::{self, PolicyError, PolicyPack, ResolvedPolicy};

pub const SCHEMA_VERSION: u32 = 1;
pub const KIND: &str = "lean-ctx.org-policy";

/// Outcome of verifying an [`OrgPolicyV1`] signature — offline, no network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgVerifyResult {
    pub signature_valid: bool,
    pub signer_public_key: Option<String>,
    pub error: Option<String>,
}

/// A signed, centrally distributed org policy.
///
/// `signature` / `signer_public_key` are excluded from the signed payload (set
/// to `None` while computing the canonical bytes), exactly like the other
/// signed artifacts in the engine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrgPolicyV1 {
    pub schema_version: u32,
    /// Discriminator so a verifier can refuse unrelated signed JSON.
    pub kind: String,
    /// Organisation identifier (`acme`) — also selects the signing key.
    pub org: String,
    /// Admin-set distribution version (`2026.06.1`) — lets a client see which
    /// rollout it currently holds (independent of the pack's own `version`).
    pub policy_version: String,
    /// When the admin signed this rollout (RFC 3339).
    pub issued_at: String,
    /// When `true`, a client that has pinned this org's key MUST apply the pack
    /// as a floor (the runtime does; this flag is the admin's declared intent
    /// and is surfaced by `policy org status`).
    pub enforced: bool,
    /// The authoritative pack source (verbatim TOML). Re-parsed + re-validated
    /// client-side, so the body cannot be swapped without breaking validation.
    pub pack_toml: String,
    /// Ed25519 public key of the signing org key (hex). `None` until signed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signer_public_key: Option<String>,
    /// Ed25519 signature over the canonical bytes (hex). `None` until signed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

impl OrgPolicyV1 {
    /// Build an unsigned artifact from an authored pack source. The TOML is
    /// parsed + validated + resolved up front so an admin never distributes a
    /// pack that would be rejected on the endpoint.
    pub fn build(
        org: &str,
        policy_version: &str,
        enforced: bool,
        pack_toml: &str,
    ) -> Result<Self, PolicyError> {
        // Validate the body is a resolvable pack before we wrap/sign it.
        let pack = policy::parse(pack_toml)?;
        policy::resolve(&pack)?;
        Ok(Self {
            schema_version: SCHEMA_VERSION,
            kind: KIND.to_string(),
            org: org.to_string(),
            policy_version: policy_version.to_string(),
            issued_at: lean_ctx_time::UtcDateTime::now().to_rfc3339(),
            enforced,
            pack_toml: pack_toml.to_string(),
            signer_public_key: None,
            signature: None,
        })
    }

    /// The wrapped pack, re-parsed and validated from `pack_toml`.
    pub fn pack(&self) -> Result<PolicyPack, PolicyError> {
        policy::parse(&self.pack_toml)
    }

    /// The wrapped pack, fully resolved (its `extends` chain folded in).
    pub fn resolved(&self) -> Result<ResolvedPolicy, PolicyError> {
        policy::resolve(&self.pack()?)
    }

    /// Deterministic bytes that get signed/verified: the whole struct with the
    /// two signature fields cleared. Identical on sign and verify.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, String> {
        let mut clone = self.clone();
        clone.signature = None;
        clone.signer_public_key = None;
        serde_json::to_vec(&clone).map_err(|e| format!("serialize for signing: {e}"))
    }

    /// Sign with the org signing key from the keystore (created on first use).
    pub fn sign(&mut self) -> Result<(), String> {
        let key =
            crate::core::agent_identity::get_or_create_keypair(&super::org_key_id(&self.org))?;
        self.sign_with_key(&key);
        Ok(())
    }

    /// Sign with an explicit key (used by `sign` and by hermetic tests). The
    /// public key is embedded so the artifact is self-verifying.
    pub fn sign_with_key(&mut self, key: &SigningKey) {
        self.signature = None;
        self.signer_public_key = None;
        // `canonical_bytes` cannot fail here: the struct is a plain value with
        // both signature fields cleared. Fall back to an empty payload on the
        // impossible serialize error rather than panicking in the hot CLI path.
        let canonical = self.canonical_bytes().unwrap_or_default();
        let sig = key.sign(&canonical);
        self.signer_public_key = Some(crate::core::agent_identity::hex_encode(
            &key.verifying_key().to_bytes(),
        ));
        self.signature = Some(crate::core::agent_identity::hex_encode(&sig.to_bytes()));
    }

    /// Verify the embedded signature against the embedded public key — offline,
    /// no audit trail, no network. A failure means the artifact was altered or
    /// was never validly signed. Trust (is this key *ours*?) is a separate
    /// check in [`super::trust`].
    #[must_use]
    pub fn verify(&self) -> OrgVerifyResult {
        let fail = |msg: &str| OrgVerifyResult {
            signature_valid: false,
            signer_public_key: self.signer_public_key.clone(),
            error: Some(msg.to_string()),
        };
        if self.kind != KIND {
            return fail("not an org-policy artifact");
        }
        let (Some(sig_hex), Some(pk_hex)) = (&self.signature, &self.signer_public_key) else {
            return fail("artifact is not signed");
        };
        let (Ok(sig_bytes), Ok(pk_bytes)) = (
            crate::core::agent_identity::hex_decode(sig_hex),
            crate::core::agent_identity::hex_decode(pk_hex),
        ) else {
            return fail("malformed signature or public key hex");
        };
        let canonical = match self.canonical_bytes() {
            Ok(c) => c,
            Err(e) => return fail(&e),
        };
        if crate::core::agent_identity::verify_signature(&pk_bytes, &canonical, &sig_bytes) {
            OrgVerifyResult {
                signature_valid: true,
                signer_public_key: Some(pk_hex.clone()),
                error: None,
            }
        } else {
            fail("signature does not match payload (tampered or wrong key)")
        }
    }

    /// Serialize to the pretty JSON artifact written to disk / distributed.
    pub fn to_json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(self).map_err(|e| format!("serialize org policy: {e}"))
    }

    /// Parse an artifact, rejecting unrelated JSON by `kind`.
    pub fn from_json(text: &str) -> Result<Self, String> {
        let parsed: Self = serde_json::from_str(text)
            .map_err(|e| format!("not a valid org-policy artifact: {e}"))?;
        if parsed.kind != KIND {
            return Err(format!(
                "wrong artifact kind '{}' (expected '{KIND}')",
                parsed.kind
            ));
        }
        Ok(parsed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PACK: &str = r#"
name = "acme-floor"
version = "1.0.0"
description = "ACME org floor"
extends = "strict-redaction"

[context]
deny_tools = ["ctx_url_read"]
"#;

    fn key() -> SigningKey {
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).unwrap();
        SigningKey::from_bytes(&seed)
    }

    #[test]
    fn build_rejects_invalid_pack() {
        let err = OrgPolicyV1::build("acme", "1", true, "not = valid = toml");
        assert!(err.is_err());
    }

    #[test]
    fn sign_then_verify_roundtrips() {
        let mut a = OrgPolicyV1::build("acme", "2026.06.1", true, PACK).unwrap();
        a.sign_with_key(&key());
        assert!(a.verify().signature_valid);
    }

    #[test]
    fn verify_detects_tampered_pack_body() {
        let mut a = OrgPolicyV1::build("acme", "1", true, PACK).unwrap();
        a.sign_with_key(&key());
        a.pack_toml = a.pack_toml.replace("ctx_url_read", "ctx_read");
        assert!(
            !a.verify().signature_valid,
            "editing the pack body must break the signature"
        );
    }

    #[test]
    fn verify_detects_flipped_enforced_flag() {
        let mut a = OrgPolicyV1::build("acme", "1", true, PACK).unwrap();
        a.sign_with_key(&key());
        a.enforced = false;
        assert!(!a.verify().signature_valid);
    }

    #[test]
    fn json_roundtrip_preserves_and_verifies() {
        let mut a = OrgPolicyV1::build("acme", "1", true, PACK).unwrap();
        a.sign_with_key(&key());
        let json = a.to_json().unwrap();
        let loaded = OrgPolicyV1::from_json(&json).unwrap();
        assert_eq!(loaded, a);
        assert!(loaded.verify().signature_valid);
    }

    #[test]
    fn from_json_rejects_foreign_kind() {
        let json = r#"{"schema_version":1,"kind":"something-else","org":"x","policy_version":"1","issued_at":"t","enforced":false,"pack_toml":""}"#;
        assert!(OrgPolicyV1::from_json(json).is_err());
    }

    #[test]
    fn resolved_folds_extends_chain() {
        let a = OrgPolicyV1::build("acme", "1", true, PACK).unwrap();
        let r = a.resolved().unwrap();
        // strict-redaction lineage + the pack's own deny accumulate.
        assert!(r.deny_tools.contains(&"ctx_url_read".to_string()));
        assert!(!r.redaction.is_empty());
    }
}
