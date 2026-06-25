//! Trust anchors for org policy distribution (GL #674).
//!
//! A signed [`super::OrgPolicyV1`] is only honoured when its signing key is one
//! the endpoint has **pinned out-of-band** — exactly the SSH-`known_hosts` /
//! certificate-pinning model. Pinning is what makes central distribution
//! *un-bypassable*: a user cannot forge an org policy without the org's private
//! key, and cannot weaken a valid one because the runtime folds it in as a floor
//! ([`crate::core::policy::floor`]).
//!
//! Two sources, checked in order:
//! 1. `LEANCTX_ORG_TRUST_KEY` — one or more comma-separated hex public keys
//!    (managed by MDM / config-management, never written to disk by us);
//! 2. the pinned set in `<config_dir>/org-trust.toml`.
//!
//! Trust is the *separate* question from signature validity: [`super::OrgPolicyV1::verify`]
//! proves the bytes were signed by the embedded key; [`is_trusted`] proves that
//! key is one we accept. Both must hold before a policy is applied.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Env override carrying one or more trusted org public keys (hex,
/// comma-separated). Intended for MDM / fleet provisioning.
const TRUST_ENV: &str = "LEANCTX_ORG_TRUST_KEY";

/// One pinned org key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustedKey {
    /// Organisation this key signs for (informational / for `--org` selection).
    pub org: String,
    /// Ed25519 public key, hex (64 chars).
    pub public_key: String,
    /// When it was pinned (RFC 3339) — for the audit conversation, not enforcement.
    pub added_at: String,
}

/// The pinned trust set, persisted as `org-trust.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TrustStore {
    #[serde(default, rename = "trusted_key", skip_serializing_if = "Vec::is_empty")]
    pub trusted_keys: Vec<TrustedKey>,
}

/// Location of the pinned trust file (`<config_dir>/org-trust.toml`).
pub fn trust_path() -> Result<PathBuf, String> {
    Ok(crate::core::paths::config_dir()?.join("org-trust.toml"))
}

/// Load the pinned set. A missing file is the common (un-pinned) case and
/// yields an empty store, never an error.
pub fn load() -> Result<TrustStore, String> {
    let path = trust_path()?;
    if !path.exists() {
        return Ok(TrustStore::default());
    }
    let text =
        std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    toml::from_str(&text).map_err(|e| format!("parse {}: {e}", path.display()))
}

/// Persist the pinned set (creating the config dir if needed).
pub fn save(store: &TrustStore) -> Result<(), String> {
    let path = trust_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir config: {e}"))?;
    }
    let text = toml::to_string_pretty(store).map_err(|e| format!("serialize trust store: {e}"))?;
    std::fs::write(&path, text).map_err(|e| format!("write {}: {e}", path.display()))
}

/// Pin (or re-point) a trusted key for `org`. Re-pinning the same hex key for an
/// org updates its `added_at`; a different org/key pair is added. Returns
/// `true` when the set changed.
pub fn pin(org: &str, public_key: &str) -> Result<bool, String> {
    let public_key = normalize_key(public_key)?;
    let mut store = load()?;
    if let Some(existing) = store
        .trusted_keys
        .iter_mut()
        .find(|k| k.public_key == public_key)
    {
        let changed = existing.org != org;
        existing.org = org.to_string();
        existing.added_at = now();
        save(&store)?;
        return Ok(changed);
    }
    store.trusted_keys.push(TrustedKey {
        org: org.to_string(),
        public_key,
        added_at: now(),
    });
    save(&store)?;
    Ok(true)
}

/// Remove a pinned key by its hex value. Returns `true` when one was removed.
pub fn remove(public_key: &str) -> Result<bool, String> {
    let public_key = normalize_key(public_key)?;
    let mut store = load()?;
    let before = store.trusted_keys.len();
    store.trusted_keys.retain(|k| k.public_key != public_key);
    let removed = store.trusted_keys.len() != before;
    if removed {
        save(&store)?;
    }
    Ok(removed)
}

/// All trusted keys (env override first, then the pinned file). Env keys carry
/// the synthetic org name `env` so `status` can show their provenance.
#[must_use]
pub fn trusted_keys() -> Vec<TrustedKey> {
    let mut keys: Vec<TrustedKey> = env_keys()
        .into_iter()
        .map(|public_key| TrustedKey {
            org: "env".to_string(),
            public_key,
            added_at: String::new(),
        })
        .collect();
    if let Ok(store) = load() {
        for k in store.trusted_keys {
            if !keys.iter().any(|e| e.public_key == k.public_key) {
                keys.push(k);
            }
        }
    }
    keys
}

/// Whether `public_key` (hex) is pinned — via the env override or the file.
#[must_use]
pub fn is_trusted(public_key: &str) -> bool {
    let Ok(key) = normalize_key(public_key) else {
        return false;
    };
    env_keys().contains(&key)
        || load().is_ok_and(|s| s.trusted_keys.iter().any(|k| k.public_key == key))
}

/// Whether any trust anchor is configured at all (env or file). When `false`,
/// org distribution is simply not in use on this endpoint (opt-in).
#[must_use]
pub fn any_pinned() -> bool {
    !env_keys().is_empty() || load().is_ok_and(|s| !s.trusted_keys.is_empty())
}

fn env_keys() -> Vec<String> {
    std::env::var(TRUST_ENV)
        .ok()
        .into_iter()
        .flat_map(|v| {
            v.split(',')
                .filter_map(|k| normalize_key(k).ok())
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Lower-case + trim a hex key and check it is a 32-byte (64-hex-char) Ed25519
/// public key. Rejecting malformed input here keeps comparisons exact.
fn normalize_key(key: &str) -> Result<String, String> {
    let k = key.trim().to_ascii_lowercase();
    if k.len() != 64 || !k.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!(
            "invalid public key (expected 64 hex chars, got {})",
            k.len()
        ));
    }
    Ok(k)
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::data_dir::isolated_data_dir;

    fn sample_key() -> String {
        "ab".repeat(32)
    }

    #[test]
    fn normalize_rejects_bad_keys() {
        assert!(normalize_key("xyz").is_err());
        assert!(normalize_key(&"zz".repeat(32)).is_err());
        assert!(normalize_key(&sample_key()).is_ok());
    }

    #[test]
    fn pin_then_trusted_then_remove() {
        let _iso = isolated_data_dir();
        let key = sample_key();
        assert!(!is_trusted(&key));
        assert!(pin("acme", &key).unwrap());
        assert!(is_trusted(&key));
        assert!(any_pinned());
        assert!(remove(&key).unwrap());
        assert!(!is_trusted(&key));
    }

    #[test]
    fn repin_same_key_updates_org() {
        let _iso = isolated_data_dir();
        let key = sample_key();
        pin("acme", &key).unwrap();
        // same key, new org → reported as changed; still a single entry.
        assert!(pin("acme-renamed", &key).unwrap());
        assert_eq!(
            trusted_keys()
                .iter()
                .filter(|k| k.public_key == key)
                .count(),
            1
        );
    }

    #[test]
    fn env_override_is_trusted() {
        let _iso = isolated_data_dir();
        let key = sample_key();
        crate::test_env::set_var(TRUST_ENV, &key);
        assert!(is_trusted(&key));
        assert!(any_pinned());
        crate::test_env::remove_var(TRUST_ENV);
    }
}
