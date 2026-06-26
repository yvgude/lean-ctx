//! Detached Ed25519 signing for the user-override registry (#865).
//!
//! The bundled registry is trusted by construction — it is compiled into the
//! binary. The risk surface is the **user override**
//! (`<data_dir>/addon_registry.json`): a local file that can *shadow* trusted
//! addon names with attacker-controlled wiring.
//!
//! When `addons.require_signature` is on, an override is honoured only if a
//! sidecar `addon_registry.json.sig` carries a valid signature **by a trusted
//! org key** — the same pinned-key trust anchor as the signed org-policy floor
//! ([`crate::core::policy::org::trust`]). This reuses the engine's Ed25519
//! primitives ([`crate::core::agent_identity`]); the signature covers the exact
//! file bytes, so any tampering invalidates it.

use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};

use crate::core::agent_identity::{hex_decode, hex_encode, sign_bytes_with, verify_signature};

/// A detached signature sidecar, written next to the registry file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistrySignature {
    /// Ed25519 signature over the registry file's exact bytes (hex, 128 chars).
    pub signature: String,
    /// The verifying key of the signer (hex, 64 chars) — so the artifact is
    /// self-describing; trust of this key is a *separate* check.
    pub signer_public_key: String,
}

impl RegistrySignature {
    /// Parse a `.sig` sidecar.
    pub fn from_json(text: &str) -> Result<Self, String> {
        serde_json::from_str(text).map_err(|e| format!("not a valid registry signature: {e}"))
    }

    /// Serialize to the pretty JSON sidecar.
    pub fn to_json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(self).map_err(|e| format!("serialize signature: {e}"))
    }
}

/// Sidecar path for a registry file (`<registry>.sig`).
#[must_use]
pub fn sidecar_path(registry_path: &std::path::Path) -> std::path::PathBuf {
    let mut s = registry_path.as_os_str().to_os_string();
    s.push(".sig");
    std::path::PathBuf::from(s)
}

/// Sign `content` with `key`, embedding the public key. For maintainers / the
/// `addon registry sign` path. Pure.
#[must_use]
pub fn sign_detached(content: &str, key: &SigningKey) -> RegistrySignature {
    let sig = sign_bytes_with(key, content.as_bytes());
    RegistrySignature {
        signature: hex_encode(&sig),
        signer_public_key: hex_encode(&key.verifying_key().to_bytes()),
    }
}

/// Whether `sig` is a cryptographically valid signature of `content` by the
/// embedded key. Says nothing about whether that key is *trusted*. Pure.
#[must_use]
pub fn signature_valid(content: &str, sig: &RegistrySignature) -> bool {
    let (Ok(pk), Ok(s)) = (
        hex_decode(&sig.signer_public_key),
        hex_decode(&sig.signature),
    ) else {
        return false;
    };
    verify_signature(&pk, content.as_bytes(), &s)
}

/// Outcome of gating an override file against the signature policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverrideVerdict {
    /// Honour the override (signatures not required, or signed + trusted).
    Accept,
    /// Ignore the override; carries a human-readable reason for the warning.
    Reject(String),
}

/// Decide whether to honour an override given the file `content`, its optional
/// sidecar `sig`, and whether signatures are required. `is_trusted` resolves a
/// hex public key to trust (inject [`crate::core::policy::org::trust::is_trusted`]
/// in production; a closure in tests). Pure.
#[must_use]
pub fn gate_override(
    content: &str,
    sig: Option<&RegistrySignature>,
    require_signature: bool,
    is_trusted: impl Fn(&str) -> bool,
) -> OverrideVerdict {
    if !require_signature {
        return OverrideVerdict::Accept;
    }
    let Some(sig) = sig else {
        return OverrideVerdict::Reject(
            "addons.require_signature is on but the override registry has no .sig sidecar"
                .to_string(),
        );
    };
    if !signature_valid(content, sig) {
        return OverrideVerdict::Reject(
            "override registry signature is invalid (tampered or wrong key)".to_string(),
        );
    }
    if !is_trusted(&sig.signer_public_key) {
        return OverrideVerdict::Reject(format!(
            "override registry signed by an untrusted key ({}…) — pin it with `policy org trust`",
            sig.signer_public_key.chars().take(12).collect::<String>()
        ));
    }
    OverrideVerdict::Accept
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> SigningKey {
        SigningKey::from_bytes(&[7u8; 32])
    }

    #[test]
    fn sign_then_verify_roundtrips() {
        let k = key();
        let content = "{\"addons\":[]}";
        let sig = sign_detached(content, &k);
        assert!(signature_valid(content, &sig));
    }

    #[test]
    fn tampered_content_fails() {
        let sig = sign_detached("original", &key());
        assert!(!signature_valid("tampered", &sig));
    }

    #[test]
    fn sidecar_path_appends_sig() {
        let p = sidecar_path(std::path::Path::new("/x/addon_registry.json"));
        assert_eq!(p, std::path::PathBuf::from("/x/addon_registry.json.sig"));
    }

    #[test]
    fn gate_accepts_when_not_required() {
        assert_eq!(
            gate_override("anything", None, false, |_| false),
            OverrideVerdict::Accept
        );
    }

    #[test]
    fn gate_rejects_missing_sidecar() {
        assert!(matches!(
            gate_override("x", None, true, |_| true),
            OverrideVerdict::Reject(_)
        ));
    }

    #[test]
    fn gate_rejects_invalid_signature() {
        let bad = RegistrySignature {
            signature: "00".repeat(64),
            signer_public_key: hex_encode(&key().verifying_key().to_bytes()),
        };
        assert!(matches!(
            gate_override("content", Some(&bad), true, |_| true),
            OverrideVerdict::Reject(_)
        ));
    }

    #[test]
    fn gate_rejects_untrusted_signer() {
        let content = "content";
        let sig = sign_detached(content, &key());
        assert!(matches!(
            gate_override(content, Some(&sig), true, |_| false),
            OverrideVerdict::Reject(_)
        ));
    }

    #[test]
    fn gate_accepts_signed_and_trusted() {
        let content = "content";
        let sig = sign_detached(content, &key());
        let trusted_pk = sig.signer_public_key.clone();
        assert_eq!(
            gate_override(content, Some(&sig), true, |pk| pk == trusted_pk),
            OverrideVerdict::Accept
        );
    }
}
