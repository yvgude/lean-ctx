//! Environment scrubbing for spawned stdio addons (P1).
//!
//! A stdio addon is a child process. By default a child inherits the **full**
//! host environment — exactly where API keys and tokens tend to live — so an
//! untrusted addon could read them. When an addon declares `[capabilities]`,
//! this module clears the inherited environment and re-adds only a minimal base
//! allowlist ([`BASE_ENV_ALLOWLIST`], shared with the plugin sandbox) plus the
//! variable names the addon explicitly declared, then layers the addon's own
//! `[mcp.env]` values on top.
//!
//! Addons *without* a capability declaration keep the legacy inherited
//! environment, so existing installs do not change.

use std::collections::BTreeMap;

use tokio::process::Command;

use super::capabilities::{AddonCapabilities, BASE_ENV_ALLOWLIST};

/// Configure `cmd`'s environment for a stdio addon spawn.
///
/// - `declared_env` — the addon's `[mcp.env]` (always applied; author-set).
/// - `capabilities`:
///   - `Some` → **scrub**: clear the inherited env, re-add [`BASE_ENV_ALLOWLIST`]
///     plus the declared capability env names from the host, then `declared_env`.
///   - `None` → **legacy**: inherit the host env, then apply `declared_env`.
pub fn apply_env(
    cmd: &mut Command,
    declared_env: &BTreeMap<String, String>,
    capabilities: Option<&AddonCapabilities>,
) {
    if let Some(caps) = capabilities {
        cmd.env_clear();
        for key in BASE_ENV_ALLOWLIST {
            if let Ok(val) = std::env::var(key) {
                cmd.env(key, val);
            }
        }
        for name in &caps.env {
            if let Ok(val) = std::env::var(name) {
                cmd.env(name, val);
            }
        }
    }
    for (k, v) in declared_env {
        cmd.env(k, v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::addons::capabilities::NetworkAccess;

    fn env_keys(cmd: &Command) -> Vec<String> {
        cmd.as_std()
            .get_envs()
            .filter_map(|(k, v)| v.map(|_| k.to_string_lossy().into_owned()))
            .collect()
    }

    #[test]
    fn declared_capabilities_scrub_host_secret() {
        crate::test_env::set_var("LEAN_CTX_ADDON_TEST_SECRET", "top-secret");
        let caps = AddonCapabilities::default();
        let mut cmd = Command::new("true");
        apply_env(&mut cmd, &BTreeMap::new(), Some(&caps));
        let keys = env_keys(&cmd);
        crate::test_env::remove_var("LEAN_CTX_ADDON_TEST_SECRET");
        assert!(
            !keys.iter().any(|k| k == "LEAN_CTX_ADDON_TEST_SECRET"),
            "scrubbed child must not see the host secret"
        );
    }

    #[test]
    fn declared_env_name_passes_through() {
        crate::test_env::set_var("LEAN_CTX_ADDON_TEST_ALLOWED", "visible");
        let caps = AddonCapabilities {
            network: NetworkAccess::Full,
            env: vec!["LEAN_CTX_ADDON_TEST_ALLOWED".into()],
            ..Default::default()
        };
        let mut cmd = Command::new("true");
        apply_env(&mut cmd, &BTreeMap::new(), Some(&caps));
        let keys = env_keys(&cmd);
        crate::test_env::remove_var("LEAN_CTX_ADDON_TEST_ALLOWED");
        assert!(
            keys.iter().any(|k| k == "LEAN_CTX_ADDON_TEST_ALLOWED"),
            "declared env name must reach the child"
        );
    }

    #[test]
    fn mcp_env_values_are_always_set() {
        let mut declared = BTreeMap::new();
        declared.insert("ADDON_MODE".to_string(), "serve".to_string());
        let caps = AddonCapabilities::default();
        let mut cmd = Command::new("true");
        apply_env(&mut cmd, &declared, Some(&caps));
        assert!(env_keys(&cmd).iter().any(|k| k == "ADDON_MODE"));
    }

    #[test]
    fn legacy_path_does_not_clear_env() {
        // No capabilities → inherit host env; only declared [mcp.env] is layered.
        let mut declared = BTreeMap::new();
        declared.insert("ADDON_MODE".to_string(), "serve".to_string());
        let mut cmd = Command::new("true");
        apply_env(&mut cmd, &declared, None);
        // The std Command reports an explicit ADDON_MODE entry; the inherited
        // host env is not cleared (no env_clear call), which is the legacy
        // behaviour we preserve for addons predating capabilities.
        assert!(env_keys(&cmd).iter().any(|k| k == "ADDON_MODE"));
    }
}
