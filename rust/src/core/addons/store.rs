//! Installed-addon state: `<data_dir>/addons/installed.json`.
//!
//! Records which addons are installed and the gateway server each one owns, so
//! `remove` can cleanly unwire exactly what `add` wired. State only — config
//! (the live `[[gateway.servers]]`) remains the single source of truth for what
//! actually runs.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::bootstrap::InstallReceipt;
use super::capabilities::AddonCapabilities;

/// Receipt of a managed prebuilt binary installed from a `[artifacts]` entry
/// (GH #724/#725, Phase 1): what was downloaded, where it lives, and the
/// SHA-256 the gateway pins at spawn. `doctor` re-verifies it; `remove` and
/// `update` clean up exactly what was installed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactReceipt {
    /// Rust target triple the asset was resolved for.
    pub platform: String,
    /// Download URL the binary came from.
    pub url: String,
    /// SHA-256 pin (also mirrored into the gateway server's `binary_sha256`).
    pub sha256: String,
    /// Absolute managed path the gateway spawns.
    pub path: String,
}

/// One installed addon and the gateway server it owns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledAddon {
    pub name: String,
    pub version: String,
    /// Where it came from: `"registry"` or `"local"`.
    pub source: String,
    /// The `[[gateway.servers]]` entry this addon installed.
    pub gateway_server: String,
    /// The capabilities the user consented to at install (P1). `None` for
    /// addons installed before the capability model / without a declaration —
    /// a record of the granted permissions, for audit and later re-prompting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub granted_capabilities: Option<AddonCapabilities>,
    /// Integrity lock (P2): content hash of the gateway wiring pinned at install.
    /// `None` for addons installed before integrity pinning. Re-verified by
    /// [`super::integrity::verify_all`] to detect post-install drift.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    /// Bootstrap receipt (#1105): the package a `[install]` block provisioned,
    /// so `remove` can uninstall exactly what `add` installed. `None` for addons
    /// with no bootstrap (ephemeral runners or already-present binaries).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install: Option<InstallReceipt>,
    /// Managed-binary receipt (GH #725): the prebuilt artifact `add` installed
    /// into the managed bin dir. `None` for addons resolved via `PATH`,
    /// bootstrap, or an ephemeral runner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<ArtifactReceipt>,
}

/// The on-disk installed-addons index.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InstalledStore {
    #[serde(default)]
    pub addons: BTreeMap<String, InstalledAddon>,
}

fn store_path() -> Result<PathBuf, String> {
    Ok(crate::core::data_dir::lean_ctx_data_dir()?
        .join("addons")
        .join("installed.json"))
}

impl InstalledStore {
    /// Load the store, or an empty one if it does not exist / is unreadable.
    pub fn load() -> Self {
        let Ok(path) = store_path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(raw) if !raw.trim().is_empty() => serde_json::from_str(&raw).unwrap_or_default(),
            _ => Self::default(),
        }
    }

    /// Persist the store (creating the `addons/` dir as needed).
    pub fn save(&self) -> Result<(), String> {
        let path = store_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&path, json).map_err(|e| e.to_string())
    }

    pub fn get(&self, name: &str) -> Option<&InstalledAddon> {
        self.addons.get(name)
    }

    /// Installed addons, sorted by name (`BTreeMap` iteration order).
    pub fn list(&self) -> Vec<&InstalledAddon> {
        self.addons.values().collect()
    }

    pub fn upsert(&mut self, addon: InstalledAddon) {
        self.addons.insert(addon.name.clone(), addon);
    }

    pub fn remove(&mut self, name: &str) -> Option<InstalledAddon> {
        self.addons.remove(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::data_dir::isolated_data_dir;

    fn sample(name: &str) -> InstalledAddon {
        InstalledAddon {
            name: name.to_string(),
            version: "1.0.0".into(),
            source: "registry".into(),
            gateway_server: name.to_string(),
            granted_capabilities: None,
            content_hash: None,
            install: None,
            artifact: None,
        }
    }

    #[test]
    fn round_trips_through_disk() {
        let _data = isolated_data_dir();
        assert!(InstalledStore::load().list().is_empty());

        let mut store = InstalledStore::default();
        store.upsert(sample("alpha"));
        store.upsert(sample("beta"));
        store.save().expect("save");

        let reloaded = InstalledStore::load();
        assert_eq!(reloaded.list().len(), 2);
        assert!(reloaded.get("alpha").is_some());

        let mut reloaded = reloaded;
        assert!(reloaded.remove("alpha").is_some());
        reloaded.save().expect("save");
        assert!(InstalledStore::load().get("alpha").is_none());
        assert!(InstalledStore::load().get("beta").is_some());
    }
}
