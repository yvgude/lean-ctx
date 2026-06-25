//! Addon integrity pinning + local re-verify (P2 — the lockfile half).
//!
//! `installed.json` is the lockfile: at install time we pin a content hash of
//! the exact gateway wiring an addon installed (transport, command, args, env,
//! url, headers, capabilities). [`verify_all`] re-computes that hash from the
//! live `[[gateway.servers]]` config and reports any drift — so a swapped
//! command, an added arg, or a widened capability after install is caught,
//! complementing the [`super::revocation`] deny-list with a positive integrity
//! check.
//!
//! (Pulling a newer *signed* version — the "updater" — is registry-server work
//! that reuses the ctxpkg remote rails; this module is the local lock + verify
//! it builds on.)

use crate::core::gateway::GatewayServer;

/// Stable content hash of a gateway server's wiring. Deterministic: the struct
/// serialises in field order with sorted `BTreeMap`s, so the same wiring always
/// hashes the same (provider prompt-cache friendly, #498).
#[must_use]
pub fn wiring_hash(server: &GatewayServer) -> String {
    let json = serde_json::to_string(server).unwrap_or_default();
    crate::core::hasher::hash_str(&json)
}

/// The per-addon verdict of a re-verify.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntegrityStatus {
    /// Live wiring matches the pinned hash.
    Ok,
    /// Live wiring differs from the pinned hash (possible tampering / drift).
    Drift,
    /// Installed, but no live `[[gateway.servers]]` entry exists.
    Missing,
    /// Installed before integrity pinning — no hash recorded to check against.
    Unpinned,
}

impl IntegrityStatus {
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Drift => "DRIFT",
            Self::Missing => "missing",
            Self::Unpinned => "unpinned",
        }
    }
}

/// One addon's re-verify result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrityFinding {
    pub name: String,
    pub status: IntegrityStatus,
}

/// Re-verify every installed addon against the live gateway config. Pure over
/// its two inputs so it is unit-testable without disk.
#[must_use]
pub fn verify(
    installed: &[&super::store::InstalledAddon],
    servers: &[GatewayServer],
) -> Vec<IntegrityFinding> {
    let mut out: Vec<IntegrityFinding> = installed
        .iter()
        .map(|addon| {
            let live = servers.iter().find(|s| s.name == addon.gateway_server);
            let status = match (&addon.content_hash, live) {
                (None, _) => IntegrityStatus::Unpinned,
                (Some(_), None) => IntegrityStatus::Missing,
                (Some(pinned), Some(server)) => {
                    if *pinned == wiring_hash(server) {
                        IntegrityStatus::Ok
                    } else {
                        IntegrityStatus::Drift
                    }
                }
            };
            IntegrityFinding {
                name: addon.name.clone(),
                status,
            }
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Re-verify against the on-disk store + global config.
#[must_use]
pub fn verify_all() -> Vec<IntegrityFinding> {
    let store = super::store::InstalledStore::load();
    let cfg = crate::core::config::Config::load();
    verify(&store.list(), &cfg.gateway.servers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::addons::store::InstalledAddon;

    fn server(name: &str, command: &str) -> GatewayServer {
        GatewayServer {
            name: name.into(),
            command: command.into(),
            ..Default::default()
        }
    }

    fn installed(name: &str, hash: Option<String>) -> InstalledAddon {
        InstalledAddon {
            name: name.into(),
            version: "1.0.0".into(),
            source: "registry".into(),
            gateway_server: name.into(),
            granted_capabilities: None,
            content_hash: hash,
        }
    }

    #[test]
    fn hash_is_deterministic_and_wiring_sensitive() {
        let a = wiring_hash(&server("x", "cmd"));
        let b = wiring_hash(&server("x", "cmd"));
        assert_eq!(a, b, "same wiring → same hash");
        let c = wiring_hash(&server("x", "other"));
        assert_ne!(a, c, "different command → different hash");
    }

    #[test]
    fn matching_hash_is_ok_drift_is_detected() {
        let srv = server("demo", "demo-mcp");
        let pinned = wiring_hash(&srv);
        let addon = installed("demo", Some(pinned));

        // Unchanged wiring → Ok.
        let findings = verify(&[&addon], std::slice::from_ref(&srv));
        assert_eq!(findings[0].status, IntegrityStatus::Ok);

        // Tampered wiring → Drift.
        let tampered = server("demo", "evil-mcp");
        let findings = verify(&[&addon], &[tampered]);
        assert_eq!(findings[0].status, IntegrityStatus::Drift);
    }

    #[test]
    fn missing_and_unpinned_are_reported() {
        let pinned = wiring_hash(&server("gone", "x"));
        let missing = installed("gone", Some(pinned));
        assert_eq!(verify(&[&missing], &[])[0].status, IntegrityStatus::Missing);

        let legacy = installed("old", None);
        assert_eq!(
            verify(&[&legacy], &[server("old", "x")])[0].status,
            IntegrityStatus::Unpinned
        );
    }

    #[test]
    fn findings_are_name_sorted() {
        let b = installed("b", None);
        let a = installed("a", None);
        let findings = verify(&[&b, &a], &[]);
        assert_eq!(findings[0].name, "a");
        assert_eq!(findings[1].name, "b");
    }
}
