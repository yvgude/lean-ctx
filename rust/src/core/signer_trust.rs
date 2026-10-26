//! Signer trust anchors for handoff bundles and context packages
//! (GHSA-gvg8-285q-m62g).
//!
//! A signature embedded in an artifact together with its own public key only
//! proves integrity: *someone* signed these bytes. Authenticity needs an anchor
//! that the artifact cannot supply itself. Two anchors count:
//!
//! * `<data_dir>/trusted-signers.toml` — explicit `(subject, public_key)` pins,
//!   written by hand for peers, or on first remote install of a package
//!   namespace (trust on first use, see [`pin_if_absent`]).
//! * A key held by a local agent identity (`<data_dir>/agents/…`): the private
//!   key lives on this machine, so a matching public key is ours.
//!
//! ```toml
//! [[signers]]
//! subject = "release-bot"          # handoff signer agent id, or "@ns" for packages
//! public_key = "<64 hex chars>"
//! ```

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const TRUST_FILE: &str = "trusted-signers.toml";

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct TrustedSigner {
    pub subject: String,
    pub public_key: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct TrustFile {
    #[serde(default)]
    signers: Vec<TrustedSigner>,
}

fn trust_path() -> Result<PathBuf, String> {
    Ok(crate::core::data_dir::resolve_data_dir()?.join(TRUST_FILE))
}

fn load() -> TrustFile {
    let Ok(path) = trust_path() else {
        return TrustFile::default();
    };
    match crate::core::policy::files::read(&path, false) {
        Ok(Some(text)) => toml::from_str(&text).unwrap_or_else(|_| {
            tracing::warn!("{TRUST_FILE} is invalid — no signer is trusted from it");
            TrustFile::default()
        }),
        _ => TrustFile::default(),
    }
}

fn save(file: &TrustFile) -> Result<(), String> {
    let path = trust_path()?;
    let parent = path.parent().ok_or("invalid trust store path")?;
    std::fs::create_dir_all(parent).map_err(|e| format!("create trust store dir: {e}"))?;
    let text = toml::to_string_pretty(file).map_err(|e| format!("serialize trust store: {e}"))?;
    let tmp = parent.join(format!(".{TRUST_FILE}.tmp"));
    std::fs::write(&tmp, text).map_err(|e| format!("write trust store: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("write trust store: {e}"))
}

fn normalize_key(public_key_hex: &str) -> String {
    public_key_hex.trim().to_ascii_lowercase()
}

/// Short, stable fingerprint of a hex-encoded public key for display:
/// the first 16 hex chars of SHA-256 over the raw key bytes.
pub(crate) fn key_fingerprint(public_key_hex: &str) -> String {
    let normalized = normalize_key(public_key_hex);
    let bytes = crate::core::agent_identity::hex_decode(&normalized)
        .unwrap_or_else(|_| normalized.into_bytes());
    let digest = Sha256::digest(&bytes);
    crate::core::agent_identity::hex_encode(&digest)[..16].to_string()
}

/// Whether `public_key_hex` is an anchored key for `subject`.
pub(crate) fn is_trusted(subject: &str, public_key_hex: &str) -> bool {
    let key = normalize_key(public_key_hex);
    if key.is_empty() {
        return false;
    }
    if load()
        .signers
        .iter()
        .any(|s| s.subject == subject && normalize_key(&s.public_key) == key)
    {
        return true;
    }
    crate::core::agent_identity::get_stored_public_key(subject)
        .is_ok_and(|local| crate::core::agent_identity::hex_encode(&local.to_bytes()) == key)
}

/// Outcome of [`pin_if_absent`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PinOutcome {
    /// The key was already the pinned key for this subject.
    AlreadyPinned,
    /// No key was pinned; this one is now (trust on first use).
    NewlyPinned,
    /// A *different* key is pinned — the caller must reject.
    Mismatch { pinned_fingerprint: String },
}

/// Trust-on-first-use pin: record `public_key_hex` for `subject` unless a key
/// is already pinned, in which case it must match.
pub(crate) fn pin_if_absent(
    subject: &str,
    public_key_hex: &str,
    source: &str,
) -> Result<PinOutcome, String> {
    let key = normalize_key(public_key_hex);
    let mut file = load();
    if let Some(existing) = file.signers.iter().find(|s| s.subject == subject) {
        let pinned = normalize_key(&existing.public_key);
        return Ok(if pinned == key {
            PinOutcome::AlreadyPinned
        } else {
            PinOutcome::Mismatch {
                pinned_fingerprint: key_fingerprint(&pinned),
            }
        });
    }
    file.signers.push(TrustedSigner {
        subject: subject.to_string(),
        public_key: key,
        source: source.to_string(),
    });
    save(&file)?;
    Ok(PinOutcome::NewlyPinned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pins_are_subject_bound_and_tofu_rejects_key_change() {
        let _lock = crate::core::data_dir::test_env_lock();
        let dir = tempfile::tempdir().unwrap();
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", dir.path());

        let key_a = "aa".repeat(32);
        let key_b = "bb".repeat(32);
        assert!(!is_trusted("@acme", &key_a));
        assert_eq!(
            pin_if_absent("@acme", &key_a, "tofu").unwrap(),
            PinOutcome::NewlyPinned
        );
        assert!(is_trusted("@acme", &key_a.to_ascii_uppercase()));
        assert!(!is_trusted("@other", &key_a), "pins are subject-bound");
        assert_eq!(
            pin_if_absent("@acme", &key_a, "tofu").unwrap(),
            PinOutcome::AlreadyPinned
        );
        assert!(matches!(
            pin_if_absent("@acme", &key_b, "tofu").unwrap(),
            PinOutcome::Mismatch { .. }
        ));
        assert!(!is_trusted("@acme", &key_b));
        assert!(is_trusted("@acme", &key_a), "a rejected key never replaces the pin");
        assert_eq!(key_fingerprint(&key_a).len(), 16);

        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
    }
}
