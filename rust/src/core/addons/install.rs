//! Install / remove logic: wire an addon's MCP server into the global gateway
//! and record it in the installed store.
//!
//! Pure state mutation — any interactive confirmation belongs in the CLI layer
//! (so this stays unit-testable). Installation goes through
//! [`Config::update_global`], the canonical safe-persistence entry point: it
//! reads only the global config (no project-local merge) and refuses to clobber
//! an unparseable file.

use super::manifest::AddonManifest;
use super::policy::AddonsConfig;
use super::store::{ArtifactReceipt, InstalledAddon, InstalledStore};
use crate::core::config::Config;
use crate::core::gateway::GatewayServer;

/// Result of a successful [`install`].
pub struct InstallOutcome {
    pub name: String,
    pub gateway_server: String,
    /// `true` when installation flipped `gateway.enabled` from off to on.
    pub enabled_gateway: bool,
}

/// Pure pre-persist gate shared by [`install`] and the CLI. Runs every check
/// that can reject an addon *before* anything is wired or any health probe
/// spawns a process — validation, runnable endpoint, kill-switch, the org
/// install policy, and the capability-coherence gate (#1080) — and returns the
/// resolved [`GatewayServer`] so the caller can reuse it. Pure + deterministic.
pub fn preflight(
    manifest: &AddonManifest,
    addons: &AddonsConfig,
    force: bool,
) -> Result<GatewayServer, String> {
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
    let findings = super::trust::assess(manifest);
    super::policy::gate(manifest, addons, &findings)?;

    // Capability-coherence gate (#1080): an addon whose declared `[capabilities]`
    // under-state its wiring (e.g. `network = none` while launching `npx`) would
    // be silently sandbox-blocked at runtime. Refuse the install with an
    // actionable message instead of letting it fail opaquely at first use.
    enforce_capability_coherence(manifest, force)?;

    Ok(server)
}

/// Block an install whose declared capabilities under-state what the wiring
/// does (the audit's incoherence verdict), unless `force` overrides it.
fn enforce_capability_coherence(manifest: &AddonManifest, force: bool) -> Result<(), String> {
    if force {
        return Ok(());
    }
    let report = super::audit::audit(manifest);
    if report.capability_coherent {
        return Ok(());
    }
    let detail = report
        .findings
        .iter()
        .find(|f| f.code == "cap_net_underdeclared" || f.code == "cap_exec_underdeclared")
        .map_or_else(
            || "declared capabilities under-state what the wiring does".to_string(),
            |f| f.message.clone(),
        );
    Err(format!(
        "addon `{}` declares capabilities that under-state its wiring, so the OS sandbox would \
         block it at runtime:\n  {detail}\n  Fix the [capabilities] block (e.g. network = \"full\", \
         filesystem = \"read_write\" for an npx/npm server) or omit it to use `addons.sandbox`; \
         re-run with --force to install anyway.",
        manifest.addon.name
    ))
}

/// Wire `manifest` into the global gateway and record it in the store.
///
/// `source` is recorded for `addon list` (`"registry"` or `"local"`). `force`
/// bypasses the capability-coherence gate (#1080). `artifact` is the receipt
/// of a managed prebuilt binary the CLI layer installed beforehand (GH #725) —
/// like the bootstrap, the impure download runs in the CLI layer and only the
/// receipt is persisted here. Replaces any existing gateway server / store
/// entry with the same name (idempotent re-install). Returns an error if any
/// [`preflight`] check rejects the addon.
pub fn install(
    manifest: &AddonManifest,
    source: &str,
    force: bool,
    artifact: Option<ArtifactReceipt>,
) -> Result<InstallOutcome, String> {
    let cfg = Config::load();
    let server = preflight(manifest, &cfg.addons, force)?;

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
        // Record what a `[install]` block provisions so `remove` can uninstall
        // it (#1105). The bootstrap itself runs in the CLI layer before this
        // call; here we only persist the receipt, keeping `install` pure.
        install: manifest
            .install
            .is_declared()
            .then(|| manifest.install.to_receipt()),
        artifact,
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

        let out = install(&manifest("demo"), "registry", false, None).expect("install");
        assert_eq!(out.gateway_server, "demo");
        assert!(out.enabled_gateway, "gateway was off, install enables it");

        // Config now carries the server + gateway enabled.
        let cfg = Config::load();
        assert!(cfg.gateway.enabled);
        assert!(cfg.gateway.servers.iter().any(|s| s.name == "demo"));

        // Store records it.
        assert!(InstalledStore::load().get("demo").is_some());

        // Re-install is idempotent (no duplicate server).
        let out2 = install(&manifest("demo"), "registry", false, None).expect("reinstall");
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

    /// GH #725: the managed-artifact receipt the CLI layer hands over is
    /// persisted verbatim, so `doctor`/`update`/`remove` can re-verify and
    /// clean up exactly what was installed.
    #[test]
    fn artifact_receipt_is_persisted() {
        let _iso = isolated_data_dir();
        let receipt = ArtifactReceipt {
            platform: "aarch64-apple-darwin".into(),
            url: "https://example.com/demo-bin".into(),
            sha256: "c".repeat(64),
            path: "/managed/bin/demo/1.0.0/demo-bin".into(),
        };
        install(&manifest("demo"), "registry", false, Some(receipt.clone())).expect("install");
        let stored = InstalledStore::load();
        let entry = stored.get("demo").expect("stored");
        assert_eq!(entry.artifact.as_ref(), Some(&receipt));

        // Reinstall without a receipt clears it (idempotent upsert semantics).
        install(&manifest("demo"), "registry", false, None).expect("reinstall");
        assert!(
            InstalledStore::load()
                .get("demo")
                .unwrap()
                .artifact
                .is_none()
        );
    }

    #[test]
    fn under_declared_capabilities_block_install_unless_forced() {
        // #1080: a manifest that launches `npx` (needs network) but declares
        // `network = none` would be sandbox-blocked at runtime. The install gate
        // must refuse it with an actionable message — and `--force` must override.
        let _iso = isolated_data_dir();
        let incoherent = AddonManifest::from_toml(
            "[addon]\nname = \"liar\"\nversion = \"0.1.0\"\n\
             [mcp]\ntransport = \"stdio\"\ncommand = \"npx\"\nargs = [\"-y\", \"pkg@1.2.3\"]\n\
             [capabilities]\nnetwork = \"none\"\n",
        )
        .expect("parse");

        let Err(err) = install(&incoherent, "local", false, None) else {
            panic!("under-declared capabilities must block the install");
        };
        assert!(err.contains("under-state"), "got: {err}");
        assert!(
            !Config::load()
                .gateway
                .servers
                .iter()
                .any(|s| s.name == "liar"),
            "nothing is wired when the gate rejects"
        );

        // --force overrides the coherence gate.
        assert!(
            install(&incoherent, "local", true, None).is_ok(),
            "force bypasses the coherence gate"
        );
    }

    #[test]
    fn listed_only_manifest_refuses_install() {
        let _iso = isolated_data_dir();
        let listed = AddonManifest::from_toml("[addon]\nname = \"listed\"\n").expect("parse");
        assert!(install(&listed, "registry", false, None).is_err());
    }

    #[test]
    fn revoked_addon_refuses_install() {
        let _iso = isolated_data_dir();
        let mut list = super::super::revocation::RevocationList::load();
        list.revoke("demo", "kill-switch test", None);
        list.save().expect("save");
        let Err(err) = install(&manifest("demo"), "registry", false, None) else {
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
