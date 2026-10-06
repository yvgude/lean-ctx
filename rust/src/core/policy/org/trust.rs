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

pub fn pin(org: &str, public_key: &str) -> Result<bool, String> {
    let mut store = load()?;
    if store.keys.iter().any(|k| k.public_key == public_key) {
        return Ok(false);
    }
    store.keys.push(TrustedKey {
        org: org.to_string(),
        public_key: public_key.to_string(),
        added_at: chrono::Utc::now().to_rfc3339(),
    });
    save(&store)?;
    Ok(true)
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
