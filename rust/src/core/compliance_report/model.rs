//! The signed compliance-report artifact (GL #677).
//!
//! [`ComplianceReportV1`] is the deliverable a CISO hands an auditor: OWASP
//! Top-10-for-Agents coverage, framework coverage (EU AI Act / ISO 42001 /
//! SOC 2), what enforcement **blocked/redacted** over a date range, and the
//! retention posture — all bound together and **Ed25519-signed**.
//!
//! Signing mirrors [`crate::core::savings_ledger::signed_batch`]: the two
//! signature fields are cleared while computing the canonical bytes, so a
//! verifier reproduces the exact signed payload from the artifact alone and
//! checks it **offline, without the audit trail or `LeanCTX`**. The embedded
//! `audit.head_hash` binds the report's counts to the precise append-only
//! audit segment that produced them.

use ed25519_dalek::{Signer, SigningKey};
use serde::{Deserialize, Serialize};

use crate::core::compliance::FrameworkReport;

pub const SCHEMA_VERSION: u32 = 1;
pub const KIND: &str = "lean-ctx.compliance-report";

/// Coverage window (inclusive RFC 3339 bounds).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Period {
    pub from: String,
    pub to: String,
}

/// One OWASP-Top-10-for-Agents row, copied from the static alignment table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwaspRow {
    pub id: String,
    pub title: String,
    /// `full` | `partial` | `minimal`.
    pub coverage: String,
}

/// OWASP alignment section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwaspSection {
    pub full: usize,
    pub partial: usize,
    pub minimal: usize,
    pub rows: Vec<OwaspRow>,
}

/// What enforcement did over the period — privacy-preserving counts only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnforcementSection {
    /// `ToolDenied` events (role / policy-pack / egress blocks).
    pub blocked: usize,
    /// `SecretDetected` events (redaction fired on output).
    pub redacted: usize,
    /// `ToolCall` events (the allowed-action denominator).
    pub tool_calls: usize,
    /// Other non-`ToolCall` security events.
    pub other_security: usize,
    /// `(event_label, count)`, sorted by label.
    pub by_event: Vec<(String, usize)>,
    /// `(tool, blocked_count)`, top rows by count.
    pub by_tool_blocked: Vec<(String, usize)>,
}

/// The audit segment that backs the enforcement counts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditSection {
    pub entries_in_period: usize,
    /// Whole-chain SHA-256 integrity at report time.
    pub chain_valid: bool,
    /// `prev_hash` of the first in-window entry.
    pub anchor_prev_hash: String,
    /// `entry_hash` of the last in-window entry.
    pub head_hash: String,
}

/// Retention posture: the pack's governance intent vs. the plan entitlement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetentionSection {
    /// Resolved pack identity (`name vX.Y.Z`) the report was assessed against.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy_pack: Option<String>,
    /// `audit_retention_days` declared by the resolved pack (governance intent).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy_audit_retention_days: Option<u32>,
    /// Effective commercial plan id (`free`, `team`, `business`, …).
    pub plan: String,
    /// Where the plan came from (`live`, `cached`, `unverified`).
    pub plan_source: String,
    /// `audit_retention_days` entitlement of the effective plan (hosted plane).
    pub plan_audit_retention_days: u32,
    /// `Some(true)` when the plan window covers the pack's declared intent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_covers_policy: Option<bool>,
}

/// Outcome of verifying a [`ComplianceReportV1`] signature — offline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportVerifyResult {
    pub signature_valid: bool,
    pub signer_public_key: Option<String>,
    pub error: Option<String>,
}

/// A signed, exportable CISO compliance report over one date range.
///
/// `signature` / `signer_public_key` are excluded from the signed payload (set
/// to `None` while computing the canonical bytes), exactly like
/// [`crate::core::savings_ledger::signed_batch::SignedSavingsBatchV1`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComplianceReportV1 {
    pub schema_version: u32,
    /// Discriminator so a verifier can refuse unrelated signed JSON.
    pub kind: String,
    pub created_at: String,
    pub lean_ctx_version: String,
    pub agent_id: String,
    pub project: String,
    pub period: Period,
    pub owasp: OwaspSection,
    pub frameworks: Vec<FrameworkReport>,
    pub enforcement: EnforcementSection,
    pub audit: AuditSection,
    pub retention: RetentionSection,
    /// Ed25519 public key (hex). `None` until signed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signer_public_key: Option<String>,
    /// Ed25519 signature over the canonical bytes (hex). `None` until signed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

impl ComplianceReportV1 {
    /// Deterministic bytes that get signed/verified: the whole struct with the
    /// two signature fields cleared. Identical on sign and verify.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, String> {
        let mut clone = self.clone();
        clone.signature = None;
        clone.signer_public_key = None;
        serde_json::to_vec(&clone).map_err(|e| format!("serialize for signing: {e}"))
    }

    /// Signs with the persistent machine identity (`agent_identity` keystore).
    pub fn sign(&mut self, agent_id: &str) -> Result<(), String> {
        let key = crate::core::agent_identity::get_or_create_keypair(agent_id)?;
        self.sign_with_key(&key)
    }

    /// Signs with an explicit key (used by `sign` and by hermetic tests). The
    /// public key is embedded so the artifact is self-verifying.
    pub fn sign_with_key(&mut self, key: &SigningKey) -> Result<(), String> {
        self.signature = None;
        self.signer_public_key = None;
        let canonical = self.canonical_bytes()?;
        let sig = key.sign(&canonical);
        self.signer_public_key = Some(crate::core::agent_identity::hex_encode(
            &key.verifying_key().to_bytes(),
        ));
        self.signature = Some(crate::core::agent_identity::hex_encode(&sig.to_bytes()));
        Ok(())
    }

    /// Verifies the embedded signature against the embedded public key —
    /// offline, no audit trail needed. A failure means the artifact was altered
    /// or was never validly signed.
    #[must_use]
    pub fn verify(&self) -> ReportVerifyResult {
        let fail = |msg: &str| ReportVerifyResult {
            signature_valid: false,
            signer_public_key: self.signer_public_key.clone(),
            error: Some(msg.to_string()),
        };
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
            ReportVerifyResult {
                signature_valid: true,
                signer_public_key: Some(pk_hex.clone()),
                error: None,
            }
        } else {
            fail("signature does not match payload (tampered or wrong key)")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::compliance;
    use crate::core::policy::{builtin, resolve};

    fn sample() -> ComplianceReportV1 {
        let mapping = compliance::get("soc2").unwrap();
        let resolved = resolve(&builtin::get("soc2-context").unwrap()).unwrap();
        let report = compliance::report(mapping, Some(&resolved));
        ComplianceReportV1 {
            schema_version: SCHEMA_VERSION,
            kind: KIND.to_string(),
            created_at: "2026-06-15T00:00:00+00:00".to_string(),
            lean_ctx_version: "test".to_string(),
            agent_id: "local".to_string(),
            project: "proj".to_string(),
            period: Period {
                from: "2026-05-01T00:00:00+00:00".to_string(),
                to: "2026-06-01T00:00:00+00:00".to_string(),
            },
            owasp: OwaspSection {
                full: 8,
                partial: 2,
                minimal: 0,
                rows: vec![OwaspRow {
                    id: "OWASP-AGENT-01".to_string(),
                    title: "Excessive Agency".to_string(),
                    coverage: "full".to_string(),
                }],
            },
            frameworks: vec![report],
            enforcement: EnforcementSection {
                blocked: 3,
                redacted: 5,
                tool_calls: 100,
                other_security: 0,
                by_event: vec![("tool_denied".to_string(), 3)],
                by_tool_blocked: vec![("ctx_url_read".to_string(), 3)],
            },
            audit: AuditSection {
                entries_in_period: 108,
                chain_valid: true,
                anchor_prev_hash: "genesis".to_string(),
                head_hash: "abc123".to_string(),
            },
            retention: RetentionSection {
                policy_pack: Some("soc2-context v1.0.0".to_string()),
                policy_audit_retention_days: Some(365),
                plan: "free".to_string(),
                plan_source: "unverified".to_string(),
                plan_audit_retention_days: 0,
                plan_covers_policy: Some(false),
            },
            signer_public_key: None,
            signature: None,
        }
    }

    fn key() -> SigningKey {
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).unwrap();
        SigningKey::from_bytes(&seed)
    }

    #[test]
    fn canonical_bytes_exclude_signature_fields() {
        let mut r = sample();
        let before = r.canonical_bytes().unwrap();
        r.signature = Some("deadbeef".into());
        r.signer_public_key = Some("cafe".into());
        let after = r.canonical_bytes().unwrap();
        assert_eq!(
            before, after,
            "signature fields must not affect signed bytes"
        );
    }

    #[test]
    fn sign_then_verify_roundtrips() {
        let mut r = sample();
        r.sign_with_key(&key()).unwrap();
        assert!(
            r.verify().signature_valid,
            "freshly signed report must verify"
        );
    }

    #[test]
    fn verify_detects_tampered_counts() {
        let mut r = sample();
        r.sign_with_key(&key()).unwrap();
        r.enforcement.blocked = 999;
        assert!(
            !r.verify().signature_valid,
            "edited counts must fail verification"
        );
    }

    #[test]
    fn verify_detects_tampered_audit_head() {
        let mut r = sample();
        r.sign_with_key(&key()).unwrap();
        r.audit.head_hash = "0000".into();
        assert!(
            !r.verify().signature_valid,
            "rewriting the chain head must fail"
        );
    }

    #[test]
    fn verify_rejects_unsigned() {
        assert!(!sample().verify().signature_valid);
    }

    #[test]
    fn json_roundtrip_is_byte_faithful_and_verifies() {
        let mut r = sample();
        r.sign_with_key(&key()).unwrap();
        let json = serde_json::to_string_pretty(&r).unwrap();
        let loaded: ComplianceReportV1 = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded, r, "round-trip must preserve every field");
        assert!(
            loaded.verify().signature_valid,
            "loaded artifact still verifies"
        );
    }
}
