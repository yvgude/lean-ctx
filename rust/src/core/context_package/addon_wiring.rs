//! Gateway wiring for `kind=addon` packages.
//!
//! An addon that declares `[mcp]` is asking lean-ctx to run an external MCP
//! server on its behalf. That is a bigger ask than storing a sandboxed WASM
//! module: the server is an ordinary process with the user's own privileges.
//!
//! Three deliberate limits, all of them things the pre-3.9.20 channel did and
//! this one does not:
//!
//! 1. **lean-ctx never installs the server.** No `uv tool install`, no `npx`,
//!    no download. The manifest says how to *run* something; putting it on the
//!    machine stays the user's step, where their package manager's own trust
//!    model applies.
//! 2. **The exact command is shown before consent**, not summarised. A user who
//!    is about to let a process spawn should read its argv.
//! 3. **`[gateway]` stays global-only and opt-in.** Adding a server does not
//!    enable the gateway; an addon cannot switch it on for you.

use crate::core::config::Config;
use crate::core::mcp_catalog::config::GatewayServer;

use super::addon_manifest::AddonManifest;

/// What `register` did, so the caller can tell the user precisely.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Wired {
    /// The addon declares no `[mcp]` table.
    NothingToWire,
    /// A new gateway entry was added.
    Added(String),
    /// An entry with this name already existed and was replaced (re-install or
    /// upgrade). Replacing rather than duplicating keeps the catalog namespace
    /// unambiguous — two servers named `mdcast` would make `mdcast::tool`
    /// mean two things.
    Replaced(String),
}

/// Add (or replace) the addon's gateway entry in the **global** config.
pub(crate) fn register(manifest: &AddonManifest) -> Result<Wired, String> {
    let Some(wiring) = manifest.mcp.as_ref() else {
        return Ok(Wired::NothingToWire);
    };
    let name = manifest.addon.name.clone();
    let server: GatewayServer = wiring.to_gateway_server(&name);

    let mut cfg = Config::load();
    let existed = cfg.gateway.servers.iter().any(|s| s.name == name);
    cfg.gateway.servers.retain(|s| s.name != name);
    cfg.gateway.servers.push(server);
    cfg.save().map_err(|e| format!("save config: {e}"))?;

    Ok(if existed {
        Wired::Replaced(name)
    } else {
        Wired::Added(name)
    })
}

/// Drop the addon's gateway entry. Returns whether one was there.
///
/// Removal is by name and unconditional: leaving a gateway entry behind after
/// `addon remove` would keep spawning a process for an addon the user believes
/// is gone.
pub(crate) fn unregister(name: &str) -> Result<bool, String> {
    let mut cfg = Config::load();
    let before = cfg.gateway.servers.len();
    cfg.gateway.servers.retain(|s| s.name != name);
    if cfg.gateway.servers.len() == before {
        return Ok(false);
    }
    cfg.save().map_err(|e| format!("save config: {e}"))?;
    Ok(true)
}

/// Whether the gateway itself is on. An addon can be wired while the gateway
/// is off; the CLI says so rather than letting the user think it is running.
pub(crate) fn gateway_enabled() -> bool {
    Config::load().gateway.enabled
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::context_package::addon_manifest;

    const STDIO: &str = r#"
[addon]
name = "mdcast"
[mcp]
command = "mdcast"
args = ["mcp", "serve"]
"#;

    #[test]
    fn a_wasm_only_addon_wires_nothing() {
        let m = addon_manifest::parse("[addon]\nname = \"squeeze\"\n").unwrap();
        assert_eq!(register(&m).unwrap(), Wired::NothingToWire);
    }

    #[test]
    fn registering_adds_then_replaces_rather_than_duplicating() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let m = addon_manifest::parse(STDIO).unwrap();

        assert_eq!(register(&m).unwrap(), Wired::Added("mdcast".into()));
        let cfg = Config::load();
        let entry = cfg
            .gateway
            .servers
            .iter()
            .find(|s| s.name == "mdcast")
            .expect("wired");
        assert_eq!(entry.command, "mdcast");
        assert_eq!(entry.args, vec!["mcp", "serve"]);

        // Re-install must not leave two servers claiming the same namespace.
        assert_eq!(register(&m).unwrap(), Wired::Replaced("mdcast".into()));
        assert_eq!(
            Config::load()
                .gateway
                .servers
                .iter()
                .filter(|s| s.name == "mdcast")
                .count(),
            1
        );
    }

    #[test]
    fn unregister_removes_the_entry_and_reports_whether_it_was_there() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let m = addon_manifest::parse(STDIO).unwrap();
        register(&m).unwrap();

        assert!(unregister("mdcast").unwrap(), "it was wired");
        assert!(
            !Config::load()
                .gateway
                .servers
                .iter()
                .any(|s| s.name == "mdcast"),
            "a removed addon must not keep spawning a process"
        );
        assert!(!unregister("mdcast").unwrap(), "second call is a no-op");
    }

    /// Wiring a server must not silently switch the gateway on — that is a
    /// separate, deliberate decision by the user.
    #[test]
    fn registering_does_not_enable_the_gateway() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let before = Config::load().gateway.enabled;
        register(&addon_manifest::parse(STDIO).unwrap()).unwrap();
        assert_eq!(
            Config::load().gateway.enabled,
            before,
            "an addon may not turn the gateway on for the user"
        );
    }
}
