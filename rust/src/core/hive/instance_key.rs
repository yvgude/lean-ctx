// SPDX-License-Identifier: Apache-2.0
//! The instance's long-lived keys: Ed25519 for token assertions and an
//! X25519 key that sealed vault keys are wrapped to.
//!
//! The X25519 key is the Montgomery form of a second, independent Ed25519
//! seed (`kx.key`), so no key ever signs and agrees at once. Seeds are kept
//! as 64 hex characters in 0600 files and zeroized in memory.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::{Signer as _, SigningKey};
use zeroize::Zeroizing;

use super::HiveError;
use super::state::{HiveStore, INSTANCE_KEY_FILE, KX_KEY_FILE};

pub struct InstanceKeys {
    signing: SigningKey,
    kx: SigningKey,
}

impl InstanceKeys {
    /// Load the instance keys, creating them on first use.
    pub fn load_or_create(store: &HiveStore) -> Result<Self, HiveError> {
        Ok(Self {
            signing: load_or_create_seed(store, INSTANCE_KEY_FILE)?,
            kx: load_or_create_seed(store, KX_KEY_FILE)?,
        })
    }

    /// Existing keys only.
    pub fn load(store: &HiveStore) -> Result<Self, HiveError> {
        Ok(Self {
            signing: load_seed(store, INSTANCE_KEY_FILE)?.ok_or(HiveError::NotConnected)?,
            kx: load_seed(store, KX_KEY_FILE)?.ok_or(HiveError::NotConnected)?,
        })
    }

    /// Ed25519 public key, base64.
    pub fn public_key_b64(&self) -> String {
        B64.encode(self.signing.verifying_key().as_bytes())
    }

    /// X25519 public key, base64.
    pub fn kx_public_key_b64(&self) -> String {
        B64.encode(self.kx_public_key())
    }

    /// X25519 public key.
    pub fn kx_public_key(&self) -> [u8; 32] {
        self.kx.verifying_key().to_montgomery().to_bytes()
    }

    /// Ed25519 signature over `message`, base64.
    pub fn sign_b64(&self, message: &[u8]) -> String {
        B64.encode(self.signing.sign(message).to_bytes())
    }

    /// X25519 secret scalar for unwrapping sealed vault keys.
    pub fn kx_secret(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(self.kx.to_scalar_bytes())
    }

    /// The Ed25519 signing key, for C0 2.0 documents signed as
    /// `domain ‖ canonical_json(..)`.
    pub(crate) fn signing_key(&self) -> &SigningKey {
        &self.signing
    }

    /// HKDF-SHA256 over the signing seed (C0 2.0 `worker_alias_key` and
    /// per-place alias seeds). Derived in memory, never stored or sent.
    pub(crate) fn derive_from_signing_seed(&self, salt: &str, info: &[u8]) -> Zeroizing<[u8; 32]> {
        derive_from_seed(&self.signing.to_bytes(), salt, info)
    }

    #[cfg(test)]
    pub(crate) fn from_seeds(signing: [u8; 32], kx: [u8; 32]) -> Self {
        Self {
            signing: SigningKey::from_bytes(&signing),
            kx: SigningKey::from_bytes(&kx),
        }
    }
}

/// `HKDF-SHA256(ikm = seed, salt, info)`, 32 bytes.
pub(crate) fn derive_from_seed(seed: &[u8; 32], salt: &str, info: &[u8]) -> Zeroizing<[u8; 32]> {
    let seed = Zeroizing::new(*seed);
    let mut out = Zeroizing::new([0u8; 32]);
    hkdf::Hkdf::<sha2::Sha256>::new(Some(salt.as_bytes()), seed.as_ref())
        .expand(info, out.as_mut())
        .expect("32 bytes is a valid HKDF-SHA256 length");
    out
}

fn load_seed(store: &HiveStore, name: &str) -> Result<Option<SigningKey>, HiveError> {
    let Some(bytes) = store.read_config_bytes(name)? else {
        return Ok(None);
    };
    let bytes = Zeroizing::new(bytes);
    let text = std::str::from_utf8(&bytes).map_err(|_| damaged(name))?;
    let mut seed = Zeroizing::new([0u8; 32]);
    hex::decode_to_slice(text.trim(), seed.as_mut()).map_err(|_| damaged(name))?;
    Ok(Some(SigningKey::from_bytes(&seed)))
}

fn load_or_create_seed(store: &HiveStore, name: &str) -> Result<SigningKey, HiveError> {
    if let Some(key) = load_seed(store, name)? {
        return Ok(key);
    }
    let mut seed = Zeroizing::new([0u8; 32]);
    getrandom::fill(seed.as_mut()).map_err(|e| HiveError::Local(format!("random: {e}")))?;
    let mut text = Zeroizing::new(hex::encode(seed.as_ref()));
    text.push('\n');
    store.write_config_bytes(name, text.as_bytes())?;
    Ok(SigningKey::from_bytes(&seed))
}

fn damaged(name: &str) -> HiveError {
    HiveError::Local(format!(
        "{name} is damaged; run `lean-ctx hive disconnect` and connect again"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signature, Verifier as _, VerifyingKey};

    #[test]
    fn keys_persist_and_sign() {
        let dir = tempfile::tempdir().unwrap();
        let store = HiveStore::at(dir.path().join("c"), dir.path().join("d"));
        let first = InstanceKeys::load_or_create(&store).unwrap();
        let again = InstanceKeys::load(&store).unwrap();
        assert_eq!(first.public_key_b64(), again.public_key_b64());
        assert_eq!(first.kx_public_key_b64(), again.kx_public_key_b64());
        assert_ne!(first.public_key_b64(), first.kx_public_key_b64());

        let public: [u8; 32] = B64
            .decode(first.public_key_b64())
            .unwrap()
            .try_into()
            .unwrap();
        let signature: [u8; 64] = B64
            .decode(first.sign_b64(b"m"))
            .unwrap()
            .try_into()
            .unwrap();
        assert!(
            VerifyingKey::from_bytes(&public)
                .unwrap()
                .verify(b"m", &Signature::from_bytes(&signature))
                .is_ok()
        );
    }
}
