//! Per-addon / per-tool usage metering (P5 — discovery & observability).
//!
//! Every gateway proxy call ([`crate::core::gateway::proxy`]) is attributed to
//! its owning server and tool, and counted in a local ledger
//! (`<data_dir>/addons/usage.json`). This is the foundation for marketplace
//! analytics, builder dashboards and usage-metered billing (Track B) — without
//! it there is no honest basis to pay a builder or show "most-used" tools.
//!
//! Local-only and side-channel: metering writes to a state file, never to a tool
//! output body, so it cannot perturb output determinism (#498). Controlled by
//! `addons.metering` (default on).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// Call counters for a single downstream tool.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolStat {
    /// Total proxied calls (success + error).
    pub calls: u64,
    /// Subset that returned an error (transport failure or `is_error`).
    pub errors: u64,
}

/// Aggregated usage for one gateway server (= one addon).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerUsage {
    pub calls: u64,
    pub errors: u64,
    /// Per-tool breakdown, keyed by tool name.
    #[serde(default)]
    pub tools: BTreeMap<String, ToolStat>,
}

/// The on-disk usage ledger (`<data_dir>/addons/usage.json`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageLedger {
    /// Per-server usage, keyed by gateway server name (the addon slug).
    #[serde(default)]
    pub servers: BTreeMap<String, ServerUsage>,
}

/// Serialises read-modify-write so concurrent proxy calls in one process don't
/// clobber each other's increments.
static WRITE_LOCK: Mutex<()> = Mutex::new(());

fn ledger_path() -> Result<PathBuf, String> {
    Ok(crate::core::data_dir::lean_ctx_data_dir()?
        .join("addons")
        .join("usage.json"))
}

impl UsageLedger {
    /// Load the ledger, or an empty one if it does not exist / is unreadable.
    #[must_use]
    pub fn load() -> Self {
        let Ok(path) = ledger_path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(raw) if !raw.trim().is_empty() => serde_json::from_str(&raw).unwrap_or_default(),
            _ => Self::default(),
        }
    }

    /// Persist the ledger (creating the `addons/` dir as needed).
    pub fn save(&self) -> Result<(), String> {
        let path = ledger_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&path, json).map_err(|e| e.to_string())
    }

    /// Apply one call to the in-memory ledger. Pure — the unit-testable core of
    /// [`record`].
    pub fn record_into(&mut self, server: &str, tool: &str, ok: bool) {
        let su = self.servers.entry(server.to_string()).or_default();
        su.calls += 1;
        let ts = su.tools.entry(tool.to_string()).or_default();
        ts.calls += 1;
        if !ok {
            su.errors += 1;
            ts.errors += 1;
        }
    }

    /// Servers sorted by total calls (descending) — the "most-used" ordering for
    /// discovery / dashboards. Ties broken by name for determinism.
    #[must_use]
    pub fn by_usage(&self) -> Vec<(&String, &ServerUsage)> {
        let mut v: Vec<_> = self.servers.iter().collect();
        v.sort_by(|a, b| b.1.calls.cmp(&a.1.calls).then_with(|| a.0.cmp(b.0)));
        v
    }
}

/// Record a single proxied call for `server::tool`. No-op when
/// `addons.metering` is off or the data dir is unavailable. Best-effort: a
/// metering write failure never affects the proxied call's result.
pub fn record(server: &str, tool: &str, ok: bool) {
    if !crate::core::config::Config::load().addons.metering {
        return;
    }
    let Ok(_guard) = WRITE_LOCK.lock() else {
        return;
    };
    let mut ledger = UsageLedger::load();
    ledger.record_into(server, tool, ok);
    if let Err(e) = ledger.save() {
        tracing::debug!("[addon-meter] could not persist usage: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::data_dir::isolated_data_dir;

    #[test]
    fn record_into_counts_calls_and_errors() {
        let mut l = UsageLedger::default();
        l.record_into("git", "commit", true);
        l.record_into("git", "commit", false);
        l.record_into("git", "status", true);

        let git = &l.servers["git"];
        assert_eq!(git.calls, 3);
        assert_eq!(git.errors, 1);
        assert_eq!(git.tools["commit"].calls, 2);
        assert_eq!(git.tools["commit"].errors, 1);
        assert_eq!(git.tools["status"].calls, 1);
        assert_eq!(git.tools["status"].errors, 0);
    }

    #[test]
    fn by_usage_is_descending_and_deterministic() {
        let mut l = UsageLedger::default();
        for _ in 0..5 {
            l.record_into("busy", "t", true);
        }
        l.record_into("quiet", "t", true);
        let order: Vec<&str> = l.by_usage().iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(order, vec!["busy", "quiet"]);
    }

    #[test]
    fn round_trips_through_disk() {
        let _iso = isolated_data_dir();
        record("demo", "tool", true);
        record("demo", "tool", false);
        let reloaded = UsageLedger::load();
        assert_eq!(reloaded.servers["demo"].calls, 2);
        assert_eq!(reloaded.servers["demo"].errors, 1);
    }
}
