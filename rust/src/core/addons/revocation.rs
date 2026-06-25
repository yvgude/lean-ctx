//! Central addon revocation / kill-switch (P2).
//!
//! A revocation immediately **blocks an addon from running** — at three points:
//!
//! 1. **install** ([`super::install`]) — a revoked addon refuses to install,
//! 2. **gateway catalog build** ([`crate::core::gateway::catalog`]) — a revoked
//!    server is dropped from the catalog with a surfaced error (its tools
//!    disappear), and
//! 3. **every proxy call** ([`crate::core::gateway`]) — a call to a revoked
//!    server is refused.
//!
//! This is the platform's emergency brake: a compromised or malicious addon can
//! be neutralised without waiting for the user to uninstall it. Unlike `remove`
//! (which the user must run), a revocation takes effect on the next gateway use.
//!
//! Sources (highest precedence last):
//! 1. the **local** list `<data_dir>/addons/revocations.json`, managed by the
//!    operator via `lean-ctx addon revoke`.
//! 2. an **org feed** layered in through the same signed-override trust anchor as
//!    the registry ([`super::signing`]) — verified before it can block, so a
//!    revocation feed cannot itself be used to disable security tooling. (The
//!    network sync that fetches the feed reuses the ctxpkg remote rails; this
//!    module is the local enforcement core it feeds.)

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// A single revocation entry, keyed by addon slug in [`RevocationList`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Revocation {
    /// Human-readable reason, shown wherever the block surfaces.
    pub reason: String,
    /// When set, only this exact addon version is revoked; otherwise every
    /// version of the slug is blocked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// The on-disk revocation list (`<data_dir>/addons/revocations.json`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RevocationList {
    #[serde(default)]
    pub revocations: BTreeMap<String, Revocation>,
}

fn list_path() -> Result<PathBuf, String> {
    Ok(crate::core::data_dir::lean_ctx_data_dir()?
        .join("addons")
        .join("revocations.json"))
}

impl RevocationList {
    /// Load the list, or an empty one if it does not exist / is unreadable.
    #[must_use]
    pub fn load() -> Self {
        let Ok(path) = list_path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(raw) if !raw.trim().is_empty() => serde_json::from_str(&raw).unwrap_or_default(),
            _ => Self::default(),
        }
    }

    /// Persist the list (creating the `addons/` dir as needed).
    pub fn save(&self) -> Result<(), String> {
        let path = list_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&path, json).map_err(|e| e.to_string())
    }

    /// Add/replace a revocation. `version = None` blocks every version.
    pub fn revoke(&mut self, name: &str, reason: &str, version: Option<String>) {
        self.revocations.insert(
            name.to_string(),
            Revocation {
                reason: reason.to_string(),
                version,
            },
        );
    }

    /// Lift a revocation. Returns the removed entry, if any.
    pub fn unrevoke(&mut self, name: &str) -> Option<Revocation> {
        self.revocations.remove(name)
    }

    /// Pure verdict: is `name` (at `installed_version`, if known) revoked?
    /// Returns the reason when blocked. A version-pinned revocation only blocks
    /// the matching version; an unpinned one blocks regardless of version.
    #[must_use]
    pub fn verdict(&self, name: &str, installed_version: Option<&str>) -> Option<String> {
        let entry = self.revocations.get(name)?;
        match &entry.version {
            None => Some(entry.reason.clone()),
            Some(pinned) => match installed_version {
                Some(v) if v == pinned => Some(entry.reason.clone()),
                _ => None,
            },
        }
    }
}

/// Runtime block check for a gateway server name: consults the local list and
/// the installed-addon version. Returns the reason when the server must not run.
#[must_use]
pub fn blocked_reason(server_name: &str) -> Option<String> {
    let installed_version = super::store::InstalledStore::load()
        .get(server_name)
        .map(|a| a.version.clone());
    RevocationList::load().verdict(server_name, installed_version.as_deref())
}

/// Install-time block check: the manifest version is known directly.
#[must_use]
pub fn install_block(name: &str, version: &str) -> Option<String> {
    RevocationList::load().verdict(name, Some(version))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::data_dir::isolated_data_dir;

    #[test]
    fn unpinned_revocation_blocks_all_versions() {
        let mut list = RevocationList::default();
        list.revoke("evil", "supply-chain compromise", None);
        assert_eq!(
            list.verdict("evil", Some("9.9.9")).as_deref(),
            Some("supply-chain compromise")
        );
        assert_eq!(
            list.verdict("evil", None).as_deref(),
            Some("supply-chain compromise")
        );
        assert!(list.verdict("clean", Some("1.0.0")).is_none());
    }

    #[test]
    fn version_pinned_revocation_blocks_only_match() {
        let mut list = RevocationList::default();
        list.revoke("tool", "bad release", Some("1.2.3".into()));
        assert!(list.verdict("tool", Some("1.2.3")).is_some());
        assert!(list.verdict("tool", Some("1.2.4")).is_none());
        assert!(list.verdict("tool", None).is_none());
    }

    #[test]
    fn round_trips_through_disk_and_unrevoke() {
        let _iso = isolated_data_dir();
        let mut list = RevocationList::load();
        assert!(list.revocations.is_empty());
        list.revoke("evil", "malware", None);
        list.save().expect("save");

        let reloaded = RevocationList::load();
        assert!(reloaded.verdict("evil", None).is_some());

        let mut reloaded = reloaded;
        assert!(reloaded.unrevoke("evil").is_some());
        reloaded.save().expect("save");
        assert!(RevocationList::load().verdict("evil", None).is_none());
    }

    #[test]
    fn blocked_reason_uses_installed_version() {
        let _iso = isolated_data_dir();
        // Revoke a specific installed version.
        let mut list = RevocationList::load();
        list.revoke("demo", "pinned bad version", Some("1.0.0".into()));
        list.save().expect("save");

        let mut store = super::super::store::InstalledStore::load();
        store.upsert(super::super::store::InstalledAddon {
            name: "demo".into(),
            version: "1.0.0".into(),
            source: "registry".into(),
            gateway_server: "demo".into(),
            granted_capabilities: None,
            content_hash: None,
        });
        store.save().expect("save");

        assert!(
            blocked_reason("demo").is_some(),
            "installed 1.0.0 is revoked"
        );
        assert!(install_block("demo", "1.0.0").is_some());
        assert!(install_block("demo", "1.0.1").is_none());
    }
}
