//! Install / remove logic: wire an addon's MCP server into the global gateway
//! and record it in the installed store.
//!
//! Pure state mutation — any interactive confirmation belongs in the CLI layer
//! (so this stays unit-testable). Installation goes through
//! [`Config::update_global`], the canonical safe-persistence entry point: it
//! reads only the global config (no project-local merge) and refuses to clobber
//! an unparseable file.

use super::manifest::AddonManifest;
use super::store::{InstalledAddon, InstalledStore};
use crate::core::config::Config;

/// Result of a successful [`install`].
pub struct InstallOutcome {
    pub name: String,
    pub gateway_server: String,
    /// `true` when installation flipped `gateway.enabled` from off to on.
    pub enabled_gateway: bool,
}

/// Wire `manifest` into the global gateway and record it in the store.
///
/// `source` is recorded for `addon list` (`"registry"` or `"local"`). Replaces
/// any existing gateway server / store entry with the same name (idempotent
/// re-install). Returns an error if the addon has no runnable MCP endpoint.
pub fn install(manifest: &AddonManifest, source: &str) -> Result<InstallOutcome, String> {
    manifest.validate()?;
    let server = manifest.to_gateway_server();
    server.resolve().map_err(|e| {
        format!(
            "addon `{}` has no runnable MCP endpoint: {e}",
            manifest.addon.name
        )
    })?;

    // Kill-switch (P2): a revoked addon never installs.
    if let Some(reason) =
        super::revocation::install_block(&manifest.addon.name, &manifest.addon.version)
    {
        return Err(format!(
            "addon `{}` is revoked and cannot be installed: {reason}",
            manifest.addon.name
        ));
    }

    // Security floor (#865): enforce the global-only install policy before any
    // gateway mutation, so a blocked addon never touches config.
    let cfg = Config::load();
    let findings = super::trust::assess(manifest);
    super::policy::gate(manifest, &cfg.addons, &findings)?;

    let name = manifest.addon.name.clone();
    let server_name = server.name.clone();
    let mut enabled_gateway = false;

    Config::update_global(|cfg| {
        if !cfg.gateway.enabled {
            cfg.gateway.enabled = true;
            enabled_gateway = true;
        }
        cfg.gateway.servers.retain(|s| s.name != server_name);
        cfg.gateway.servers.push(server.clone());
    })
    .map_err(|e| e.to_string())?;

    let mut store = InstalledStore::load();
    store.upsert(InstalledAddon {
        name: name.clone(),
        version: manifest.addon.version.clone(),
        source: source.to_string(),
        gateway_server: server_name.clone(),
        granted_capabilities: manifest.capabilities.clone(),
        content_hash: Some(super::integrity::wiring_hash(&server)),
    });
    store.save()?;

    crate::core::gateway::catalog::invalidate();

    Ok(InstallOutcome {
        name,
        gateway_server: server_name,
        enabled_gateway,
    })
}

/// Result of a successful [`remove`].
pub struct RemoveOutcome {
    pub name: String,
    pub gateway_server: String,
    /// `true` when no addons remain installed afterwards.
    pub last_removed: bool,
}

/// Unwire an installed addon: drop its gateway server and store entry.
pub fn remove(name: &str) -> Result<RemoveOutcome, String> {
    let mut store = InstalledStore::load();
    let entry = store
        .get(name)
        .cloned()
        .ok_or_else(|| format!("addon `{name}` is not installed"))?;
    let server_name = entry.gateway_server.clone();

    Config::update_global(|cfg| {
        cfg.gateway.servers.retain(|s| s.name != server_name);
    })
    .map_err(|e| e.to_string())?;

    store.remove(name);
    let last_removed = store.addons.is_empty();
    store.save()?;

    crate::core::gateway::catalog::invalidate();

    Ok(RemoveOutcome {
        name: name.to_string(),
        gateway_server: server_name,
        last_removed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::data_dir::isolated_data_dir;

    fn manifest(name: &str) -> AddonManifest {
        AddonManifest::from_toml(&format!(
            "[addon]\nname = \"{name}\"\nversion = \"0.1.0\"\n\
             [mcp]\ntransport = \"stdio\"\ncommand = \"{name}-mcp\"\n"
        ))
        .expect("parse")
    }

    #[test]
    fn install_then_remove_round_trip() {
        let _iso = isolated_data_dir();

        let out = install(&manifest("demo"), "registry").expect("install");
        assert_eq!(out.gateway_server, "demo");
        assert!(out.enabled_gateway, "gateway was off, install enables it");

        // Config now carries the server + gateway enabled.
        let cfg = Config::load();
        assert!(cfg.gateway.enabled);
        assert!(cfg.gateway.servers.iter().any(|s| s.name == "demo"));

        // Store records it.
        assert!(InstalledStore::load().get("demo").is_some());

        // Re-install is idempotent (no duplicate server).
        let out2 = install(&manifest("demo"), "registry").expect("reinstall");
        assert!(!out2.enabled_gateway, "already enabled");
        let cfg = Config::load();
        assert_eq!(
            cfg.gateway
                .servers
                .iter()
                .filter(|s| s.name == "demo")
                .count(),
            1
        );

        // Remove unwinds both config + store.
        let rm = remove("demo").expect("remove");
        assert!(rm.last_removed);
        let cfg = Config::load();
        assert!(!cfg.gateway.servers.iter().any(|s| s.name == "demo"));
        assert!(InstalledStore::load().get("demo").is_none());
    }

    #[test]
    fn remove_unknown_is_error() {
        let _iso = isolated_data_dir();
        assert!(remove("nope").is_err());
    }

    #[test]
    fn listed_only_manifest_refuses_install() {
        let _iso = isolated_data_dir();
        let listed = AddonManifest::from_toml("[addon]\nname = \"listed\"\n").expect("parse");
        assert!(install(&listed, "registry").is_err());
    }

    #[test]
    fn revoked_addon_refuses_install() {
        let _iso = isolated_data_dir();
        let mut list = super::super::revocation::RevocationList::load();
        list.revoke("demo", "kill-switch test", None);
        list.save().expect("save");
        let Err(err) = install(&manifest("demo"), "registry") else {
            panic!("revoked addon must refuse to install");
        };
        assert!(err.contains("revoked"), "got: {err}");
        // Nothing was wired.
        assert!(
            !Config::load()
                .gateway
                .servers
                .iter()
                .any(|s| s.name == "demo")
        );
        assert!(InstalledStore::load().get("demo").is_none());
    }
}
