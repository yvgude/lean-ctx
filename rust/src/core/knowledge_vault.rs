//! Zero-knowledge Personal-Cloud knowledge vault (GL #467) — the E2E
//! envelope for `/api/sync/knowledge`.
//!
//! Same construction as the hosted index bundles (XChaCha20-Poly1305,
//! HKDF-SHA256 from the stable account API key) but with **domain-separated
//! key material** (`knowledge-vault-v1` HKDF info): a leaked index-bundle key
//! can never open a knowledge vault and vice versa.
//!
//! The vault is a whole-account snapshot, last-writer-wins — knowledge stores
//! are small (≤ a few thousand entries) and device-generated, so blob-level
//! replacement is the same consistency model the index bundles already use.
//! Contract: `docs/contracts/personal-cloud-encryption-v1.md`.

use serde::{Deserialize, Serialize};

use super::index_bundle::{BundleError, decrypt, encrypt};

/// Envelope version inside the ciphertext.
pub const VAULT_VERSION: u32 = 1;

/// Plaintext payload of a sealed vault.
#[derive(Debug, Serialize, Deserialize)]
pub struct VaultEnvelope {
    /// Envelope format version ([`VAULT_VERSION`]).
    pub v: u32,
    /// Knowledge entries exactly as the legacy JSON push sent them
    /// (`{category, key, value}` objects) — the local stores stay the
    /// source of truth for richer fields.
    pub entries: Vec<serde_json::Value>,
}

/// Derive the vault key from the account API key. Distinct HKDF `info` keeps
/// this key domain-separated from `index_bundle::derive_key`.
#[must_use]
pub fn derive_vault_key(api_key: &str) -> [u8; 32] {
    derive_key(api_key, b"knowledge-vault-v1")
}

/// The gotcha vault key — same construction, own HKDF domain
/// (`gotcha-vault-v1`): a leaked knowledge-vault key can never open the
/// gotcha vault and vice versa.
#[must_use]
pub fn derive_gotcha_vault_key(api_key: &str) -> [u8; 32] {
    derive_key(api_key, b"gotcha-vault-v1")
}

fn derive_key(api_key: &str, info: &[u8]) -> [u8; 32] {
    let hk = hkdf::Hkdf::<sha2::Sha256>::new(Some(b"leanctx"), api_key.as_bytes());
    let mut okm = [0u8; 32];
    hk.expand(info, &mut okm)
        .expect("32 bytes is a valid HKDF-SHA256 output length");
    okm
}

/// Serialize + encrypt the entries into a vault blob (`nonce || ciphertext`).
pub fn seal(entries: &[serde_json::Value], key: &[u8; 32]) -> Result<Vec<u8>, BundleError> {
    let envelope = VaultEnvelope {
        v: VAULT_VERSION,
        entries: entries.to_vec(),
    };
    let plain = serde_json::to_vec(&envelope)
        .map_err(|e| BundleError::Corrupt(format!("vault serialize: {e}")))?;
    encrypt(&plain, key)
}

/// Decrypt + parse a vault blob back into its entries.
pub fn open(blob: &[u8], key: &[u8; 32]) -> Result<Vec<serde_json::Value>, BundleError> {
    let plain = decrypt(blob, key)?;
    let envelope: VaultEnvelope = serde_json::from_slice(&plain)
        .map_err(|e| BundleError::Corrupt(format!("vault parse: {e}")))?;
    if envelope.v != VAULT_VERSION {
        return Err(BundleError::Corrupt(format!(
            "unsupported vault version {}",
            envelope.v
        )));
    }
    Ok(envelope.entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_entries() -> Vec<serde_json::Value> {
        vec![
            serde_json::json!({"category": "decision", "key": "db", "value": "postgres"}),
            serde_json::json!({"category": "gotcha", "key": "launchd respawn", "value": "lean-ctx stop first"}),
        ]
    }

    #[test]
    fn seal_open_roundtrip_preserves_entries() {
        let key = derive_vault_key("lc_test_api_key");
        let blob = seal(&sample_entries(), &key).unwrap();
        let out = open(&blob, &key).unwrap();
        assert_eq!(out, sample_entries());
    }

    #[test]
    fn tampered_blob_and_wrong_key_fail_closed() {
        let key = derive_vault_key("lc_test_api_key");
        let mut blob = seal(&sample_entries(), &key).unwrap();

        // Flip one ciphertext byte → AEAD must reject.
        let last = blob.len() - 1;
        blob[last] ^= 0x01;
        assert!(matches!(open(&blob, &key), Err(BundleError::Decrypt)));

        // Wrong account key → reject.
        let blob_ok = seal(&sample_entries(), &key).unwrap();
        let other = derive_vault_key("different_api_key");
        assert!(matches!(open(&blob_ok, &other), Err(BundleError::Decrypt)));
    }

    /// The whole point of the domain separation: index-bundle key material
    /// must never open a knowledge vault, even for the same account.
    #[test]
    fn vault_key_is_domain_separated_from_index_bundle_key() {
        let api_key = "lc_same_account_key";
        let vault_key = derive_vault_key(api_key);
        let index_key = crate::core::index_bundle::derive_key(api_key);
        assert_ne!(vault_key, index_key);

        let blob = seal(&sample_entries(), &vault_key).unwrap();
        assert!(matches!(open(&blob, &index_key), Err(BundleError::Decrypt)));
    }

    /// Gotcha vault keys are their own domain: neither the knowledge-vault
    /// key nor the index-bundle key can open a gotcha vault.
    #[test]
    fn gotcha_vault_key_is_domain_separated() {
        let api_key = "lc_same_account_key";
        let gotcha_key = derive_gotcha_vault_key(api_key);
        assert_ne!(gotcha_key, derive_vault_key(api_key));
        assert_ne!(gotcha_key, crate::core::index_bundle::derive_key(api_key));

        let blob = seal(&sample_entries(), &gotcha_key).unwrap();
        assert!(matches!(
            open(&blob, &derive_vault_key(api_key)),
            Err(BundleError::Decrypt)
        ));
        assert_eq!(open(&blob, &gotcha_key).unwrap(), sample_entries());
    }
}
