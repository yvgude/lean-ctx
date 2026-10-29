//! Org key trust store — file-backed pinning (ADR-023, GL #674).
//!
//! Keys are pinned to `<data_dir>/org-trust.toml`. A pinned key means the
//! endpoint trusts artifacts signed by that org. Without any pinned keys,
//! [`active_resolved`](super::active_resolved) returns `None` and no org
//! policy is enforced (fail-open by default).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TrustedKey {
    pub org: String,
    pub public_key: String,
    pub added_at: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TrustStore {
    pub keys: Vec<TrustedKey>,
}

pub fn trust_path() -> Result<PathBuf, String> {
    let dir = crate::core::paths::data_dir()?;
    Ok(dir.join("org-trust.toml"))
}

pub(crate) fn trust_path_read_only() -> Result<PathBuf, String> {
    Ok(crate::core::data_dir::resolve_data_dir()?.join("org-trust.toml"))
}

pub fn load() -> Result<TrustStore, String> {
    let path = trust_path()?;
    let Some(text) = crate::core::policy::files::read(&path, false)? else {
        return Ok(TrustStore::default());
    };
    toml::from_str(&text).map_err(|_| "org trust input is invalid".into())
}

pub fn save(store: &TrustStore) -> Result<(), String> {
    let path = trust_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    let text = toml::to_string_pretty(store).map_err(|e| format!("serialize trust store: {e}"))?;
    std::fs::write(&path, text).map_err(|e| format!("write {}: {e}", path.display()))
}

/// The trust store as it is stored, read without creating or repairing any
/// directory: for a status that only observes. No file is an empty store; a
/// file that cannot be read or parsed is an error, never an empty store.
pub(crate) fn load_read_only() -> Result<TrustStore, String> {
    let Some(text) = crate::core::policy::files::read(&trust_path_read_only()?, false)? else {
        return Ok(TrustStore::default());
    };
    toml::from_str(&text).map_err(|_| "org trust input is invalid".into())
}

/// What a request to pin `public_key` for `org` did.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PinOutcome {
    /// The key is now pinned for this organization.
    Added,
    /// The key was already pinned for exactly this organization.
    AlreadyPinned,
    /// The key is pinned for another organization. Nothing was changed: a
    /// pin is never moved or overwritten by pinning again.
    PinnedForAnother(String),
}

/// Pin `public_key` for `org`, saying which of the three things happened.
pub fn pin_for(org: &str, public_key: &str) -> Result<PinOutcome, String> {
    let mut store = load()?;
    if let Some(existing) = store.keys.iter().find(|k| k.public_key == public_key) {
        return Ok(if existing.org == org {
            PinOutcome::AlreadyPinned
        } else {
            PinOutcome::PinnedForAnother(existing.org.clone())
        });
    }
    store.keys.push(TrustedKey {
        org: org.to_string(),
        public_key: public_key.to_string(),
        added_at: chrono::Utc::now().to_rfc3339(),
    });
    save(&store)?;
    Ok(PinOutcome::Added)
}

/// [`pin_for`] as a yes/no: `true` only when a pin was added. `false` does
/// not say the key is trusted for `org` — it may be pinned for another one.
pub fn pin(org: &str, public_key: &str) -> Result<bool, String> {
    pin_for(org, public_key).map(|outcome| outcome == PinOutcome::Added)
}

/// Whether `public_key` is pinned for exactly `org` — what enforcement means
/// by trusted (`resolve_checked`). The same key pinned for another
/// organization does not count; a store that cannot be read trusts nothing.
pub fn is_trusted_for(org: &str, public_key: &str) -> bool {
    load_read_only()
        .map(|store| {
            store
                .keys
                .iter()
                .any(|key| key.public_key == public_key && key.org == org)
        })
        .unwrap_or(false)
}

/// The organization `public_key` is pinned for, if any.
pub fn pinned_organization(public_key: &str) -> Option<String> {
    load_read_only()
        .ok()?
        .keys
        .into_iter()
        .find(|key| key.public_key == public_key)
        .map(|key| key.org)
}

pub fn remove(public_key: &str) -> Result<bool, String> {
    let mut store = load()?;
    let before = store.keys.len();
    store.keys.retain(|k| k.public_key != public_key);
    if store.keys.len() < before {
        save(&store)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

pub fn trusted_keys() -> Vec<TrustedKey> {
    load().unwrap_or_default().keys
}

pub fn is_trusted(public_key: &str) -> bool {
    load()
        .map(|s| s.keys.iter().any(|k| k.public_key == public_key))
        .unwrap_or(false)
}

pub fn any_pinned() -> bool {
    load().map(|s| !s.keys.is_empty()).unwrap_or(false)
}
