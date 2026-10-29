// SPDX-License-Identifier: Apache-2.0
//! Replica encryption at rest (SPEC-ENGINE §3.4.2).
//!
//! Every text column of a replica is XChaCha20-Poly1305 ciphertext under
//! the place's replica key `rk` (32 random bytes), with AAD
//! `place_id|table|row_key|column` and a random 24-byte nonce per value.
//! `rk` is stored wrapped by the machine's replica master key `mk`; `mk`
//! lives in the OS keychain (service `lean-ctx-hive`, account
//! `replica-master`) or, without one, in `<config>/hive/replica.key`
//! (0600, reported as `at_rest: file key`). `LEAN_CTX_REPLICA_KEY_FILE`
//! forces the file path (tests, staging builds: no keychain dialogs).

use std::path::{Path, PathBuf};

use chacha20poly1305::aead::{Aead, KeyInit as _, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use zeroize::Zeroizing;

use crate::core::hive::HiveError;

/// A 32-byte key that is wiped on drop.
pub type Key = Zeroizing<[u8; 32]>;

const NONCE_LEN: usize = 24;
/// AAD domain of a wrapped `rk`.
const WRAP_AAD: &str = "leanctx.hive.replica.rk.v1";

/// Where `mk` is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtRest {
    Keychain,
    FileKey,
}

impl AtRest {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Keychain => "keychain",
            Self::FileKey => "file key",
        }
    }
}

fn random_key() -> Result<Key, HiveError> {
    let mut key = Zeroizing::new([0u8; 32]);
    getrandom::fill(key.as_mut()).map_err(|e| HiveError::Local(format!("random: {e}")))?;
    Ok(key)
}

/// A fresh per-place replica key.
pub fn new_replica_key() -> Result<Key, HiveError> {
    random_key()
}

/// Encrypt `plain` under `key` bound to `aad`: `nonce ‖ ciphertext`.
pub fn seal(key: &[u8; 32], aad: &str, plain: &[u8]) -> Result<Vec<u8>, HiveError> {
    let mut nonce = [0u8; NONCE_LEN];
    getrandom::fill(&mut nonce).map_err(|e| HiveError::Local(format!("random: {e}")))?;
    let ct = XChaCha20Poly1305::new(key.into())
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: plain,
                aad: aad.as_bytes(),
            },
        )
        .map_err(|_| HiveError::Local("replica: encryption failed".into()))?;
    let mut out = nonce.to_vec();
    out.extend(ct);
    Ok(out)
}

/// Decrypt what [`seal`] made; fails on another key, AAD or a changed byte.
pub fn open(key: &[u8; 32], aad: &str, sealed: &[u8]) -> Result<Zeroizing<Vec<u8>>, HiveError> {
    if sealed.len() < NONCE_LEN {
        return Err(HiveError::Local("replica: ciphertext too short".into()));
    }
    let (nonce, ct) = sealed.split_at(NONCE_LEN);
    XChaCha20Poly1305::new(key.into())
        .decrypt(
            XNonce::from_slice(nonce),
            Payload {
                msg: ct,
                aad: aad.as_bytes(),
            },
        )
        .map(Zeroizing::new)
        .map_err(|_| HiveError::Local("replica: value does not decrypt".into()))
}

/// The AAD of one column value.
pub fn aad(place_id: &str, table: &str, row_key: &str, column: &str) -> String {
    format!("{place_id}|{table}|{row_key}|{column}")
}

/// Wrap `rk` for storage under `mk`.
pub fn wrap(mk: &[u8; 32], place_id: &str, rk: &[u8; 32]) -> Result<Vec<u8>, HiveError> {
    seal(mk, &format!("{WRAP_AAD}|{place_id}"), rk)
}

/// Unwrap a stored `rk`; `None` when `mk` does not open it
/// (`replica_key_missing`: the place is re-cloned).
pub fn unwrap(mk: &[u8; 32], place_id: &str, wrapped: &[u8]) -> Option<Key> {
    let plain = open(mk, &format!("{WRAP_AAD}|{place_id}"), wrapped).ok()?;
    let bytes: [u8; 32] = plain.as_slice().try_into().ok()?;
    Some(Zeroizing::new(bytes))
}

/// The file `mk` is kept in without a keychain.
pub fn key_file(config_dir: &Path) -> PathBuf {
    std::env::var_os("LEAN_CTX_REPLICA_KEY_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| config_dir.join("replica.key"))
}

fn load_or_create_file(path: &Path) -> Result<Key, HiveError> {
    if let Ok(bytes) = std::fs::read(path) {
        let bytes = Zeroizing::new(bytes);
        let key: [u8; 32] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| HiveError::Local("replica key file is not a 32-byte key".into()))?;
        return Ok(Zeroizing::new(key));
    }
    let key = random_key()?;
    let dir = path
        .parent()
        .ok_or_else(|| HiveError::Local("replica key file has no directory".into()))?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| HiveError::Local("replica key file name".into()))?;
    crate::core::hive::state::write_private(dir, name, key.as_ref())?;
    Ok(key)
}

/// The machine's replica master key `mk` and where it is kept. The
/// keychain is used unless `LEAN_CTX_REPLICA_KEY_FILE` is set, the platform
/// has none, or `keychain_required` is false and the keychain fails;
/// `keychain_required` (workspace policy `require_keychain_at_rest`)
/// refuses the file fallback.
pub fn master_key(config_dir: &Path, keychain_required: bool) -> Result<(Key, AtRest), HiveError> {
    if std::env::var_os("LEAN_CTX_REPLICA_KEY_FILE").is_none() {
        match keychain::load_or_create() {
            Ok(key) => return Ok((key, AtRest::Keychain)),
            Err(_) if !keychain_required => {}
            Err(e) => {
                return Err(HiveError::Config(format!(
                    "keychain_required_by_policy: {e}"
                )));
            }
        }
    } else if keychain_required {
        return Err(HiveError::Config(
            "keychain_required_by_policy: a key file is set".into(),
        ));
    }
    Ok((load_or_create_file(&key_file(config_dir))?, AtRest::FileKey))
}

/// OS keychain holding `mk`. Tests never touch the real keychain.
#[cfg(all(any(target_os = "macos", windows, target_os = "linux"), not(test)))]
mod keychain {
    use super::Key;

    const SERVICE: &str = "lean-ctx-hive";
    const ACCOUNT: &str = "replica-master";

    pub(super) fn load_or_create() -> Result<Key, String> {
        let entry = keyring::Entry::new(SERVICE, ACCOUNT).map_err(|e| e.to_string())?;
        match entry.get_secret() {
            Ok(bytes) => {
                let bytes = zeroize::Zeroizing::new(bytes);
                let key: [u8; 32] = bytes
                    .as_slice()
                    .try_into()
                    .map_err(|_| "keychain entry is not a 32-byte key".to_string())?;
                Ok(zeroize::Zeroizing::new(key))
            }
            Err(keyring::Error::NoEntry) => {
                let key = super::random_key().map_err(|e| e.to_string())?;
                entry.set_secret(key.as_ref()).map_err(|e| e.to_string())?;
                Ok(key)
            }
            Err(e) => Err(e.to_string()),
        }
    }
}

#[cfg(not(all(any(target_os = "macos", windows, target_os = "linux"), not(test))))]
mod keychain {
    pub(super) fn load_or_create() -> Result<super::Key, String> {
        Err("no OS keychain here".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_are_bound_to_key_and_aad() {
        let key = [3u8; 32];
        let a = aad("p1", "nodes", "n_x", "fields");
        let ct = seal(&key, &a, b"canary statement").unwrap();
        assert_eq!(open(&key, &a, &ct).unwrap().as_slice(), b"canary statement");
        assert!(open(&[4u8; 32], &a, &ct).is_err(), "other key");
        assert!(
            open(&key, &aad("p1", "nodes", "n_y", "fields"), &ct).is_err(),
            "other row"
        );
        let mut flipped = ct.clone();
        *flipped.last_mut().unwrap() ^= 1;
        assert!(open(&key, &a, &flipped).is_err(), "changed byte");
        assert_ne!(
            seal(&key, &a, b"canary statement").unwrap(),
            ct,
            "random nonce"
        );
    }

    #[test]
    fn replica_keys_wrap_per_place() {
        let mk = [9u8; 32];
        let rk = new_replica_key().unwrap();
        let wrapped = wrap(&mk, "p1", &rk).unwrap();
        assert_eq!(unwrap(&mk, "p1", &wrapped).unwrap().as_ref(), rk.as_ref());
        assert!(unwrap(&mk, "p2", &wrapped).is_none());
        assert!(unwrap(&[1u8; 32], "p1", &wrapped).is_none());
    }

    #[test]
    fn the_file_key_is_created_once_and_private() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hive").join("replica.key");
        let first = load_or_create_file(&path).unwrap();
        let again = load_or_create_file(&path).unwrap();
        assert_eq!(first.as_ref(), again.as_ref());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "{mode:o}");
        }
    }
}
