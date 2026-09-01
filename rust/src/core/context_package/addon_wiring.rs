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
//!
//! And one rule that only shows up on the second install: an upgrade replaces
//! what the *author* declares and keeps what the *user* configured — their
//! credentials and their per-server off switch. See [`register`].

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
///
/// A replace is an upgrade, not a fresh install, so it must not clobber the
/// parts of the entry the *user* owns rather than the author:
///
/// - `secret_env` / `secret_headers` are memento references to credentials. A
///   manifest cannot carry them by design, so a wholesale replace would silently
///   drop the token the user configured and the server would simply stop
///   authenticating, with nothing saying why.
/// - `enabled = false` is a deliberate decision to keep an addon installed but
///   idle. Re-enabling it on upgrade would override that silently, which is the
///   one thing a per-server switch exists to prevent.
///
/// Everything else — transport, command, args, url, pin, integration — is the
/// author's to change between versions, and is taken from the new manifest.
pub(crate) fn register(manifest: &AddonManifest) -> Result<Wired, String> {
    let Some(wiring) = manifest.mcp.as_ref() else {
        return Ok(Wired::NothingToWire);
    };
    let name = manifest.addon.name.clone();
    let mut server: GatewayServer = wiring.to_gateway_server(&name);

    let mut cfg = Config::load();
    let previous = cfg.gateway.servers.iter().find(|s| s.name == name).cloned();
    if let Some(prev) = &previous {
        server.secret_env = prev.secret_env.clone();
        server.secret_headers = prev.secret_headers.clone();
        server.enabled = prev.enabled;
    }

    cfg.gateway.servers.retain(|s| s.name != name);
    cfg.gateway.servers.push(server);
    cfg.save().map_err(|e| format!("save config: {e}"))?;

    Ok(if previous.is_some() {
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

    /// An upgrade must not silently undo the user's own configuration. The
    /// credential is the sharp case: a manifest cannot carry `secret_headers`
    /// by design, so replacing the entry wholesale would drop the token the
    /// user configured and the server would just stop authenticating, with
    /// nothing in the output explaining why.
    #[test]
    fn an_upgrade_keeps_the_users_secrets_and_their_off_switch() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let m = addon_manifest::parse(STDIO).unwrap();
        register(&m).unwrap();

        // The user configures a credential and turns the server off.
        let mut cfg = Config::load();
        {
            let entry = cfg
                .gateway
                .servers
                .iter_mut()
                .find(|s| s.name == "mdcast")
                .expect("wired");
            entry.secret_env.insert(
                "API_TOKEN".to_string(),
                crate::core::mcp_catalog::config::SecretMementoRef {
                    id: "memento-1".to_string(),
                    format: String::new(),
                },
            );
            entry.enabled = false;
        }
        cfg.save().unwrap();

        // A newer version of the same addon is installed over it.
        let upgraded = addon_manifest::parse(
            "[addon]\nname = \"mdcast\"\n[mcp]\ncommand = \"mdcast\"\nargs = [\"mcp\", \"v2\"]\n",
        )
        .unwrap();
        assert_eq!(
            register(&upgraded).unwrap(),
            Wired::Replaced("mdcast".into())
        );

        let entry = Config::load()
            .gateway
            .servers
            .into_iter()
            .find(|s| s.name == "mdcast")
            .expect("still wired");
        assert_eq!(entry.args, vec!["mcp", "v2"], "the author's change applies");
        assert!(
            entry.secret_env.contains_key("API_TOKEN"),
            "the user's credential survived the upgrade"
        );
        assert!(
            !entry.enabled,
            "a deliberate off switch is not flipped back on by an upgrade"
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
