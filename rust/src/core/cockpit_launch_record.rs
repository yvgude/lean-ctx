// SPDX-License-Identifier: Apache-2.0
//! The Cockpit launch record: which local Cockpit server belongs to this
//! data dir.
//!
//! The server records itself in `dashboard.launch.json` in the state dir
//! (PID, process identity, port, version, foreground or background) right
//! after it bound its port and removes the record when it ends. The record
//! holds no secret: the bearer token stays in `dashboard.token` (0600).
//!
//! The record is shared state between the Cockpit server, the CLI that
//! starts and stops it, and the proxy page that links to it; the start
//! decision itself lives with the dashboard.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::ipc::process::ProcessIdentity;

/// The record file in the state dir.
pub(crate) const RECORD_FILE: &str = "dashboard.launch.json";
/// Set by a background start for its server child.
pub(crate) const DETACHED_ENV: &str = "LEAN_CTX_COCKPIT_DETACHED";

/// Where the Cockpit server of this data dir runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LaunchRecord {
    pub(crate) schema_version: u32,
    pub(crate) pid: u32,
    /// Defends the PID against reuse by an unrelated process.
    pub(crate) identity: Option<ProcessIdentity>,
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) base_path: String,
    pub(crate) version: String,
    /// Started by `lean-ctx cockpit` in the background (stoppable with
    /// `lean-ctx cockpit stop`), not in someone's terminal.
    pub(crate) detached: bool,
}

impl LaunchRecord {
    /// The record of this process, serving `host:port`.
    pub(crate) fn of_this_process(host: &str, port: u16, base_path: &str) -> Self {
        let pid = std::process::id();
        Self {
            schema_version: 1,
            pid,
            identity: crate::ipc::process::identity(pid),
            host: host.to_string(),
            port,
            base_path: base_path.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            detached: std::env::var_os(DETACHED_ENV).is_some(),
        }
    }

    /// The recorded process still runs (and is the one recorded, when the
    /// platform can tell).
    pub(crate) fn process_alive(&self) -> bool {
        if !crate::ipc::process::is_alive(self.pid) {
            return false;
        }
        match (&self.identity, crate::ipc::process::identity(self.pid)) {
            (Some(recorded), Some(current)) => *recorded == current,
            _ => true,
        }
    }
}

fn record_path() -> Option<PathBuf> {
    crate::core::paths::state_dir()
        .ok()
        .map(|dir| dir.join(RECORD_FILE))
}

pub(crate) fn read_record() -> Option<LaunchRecord> {
    let bytes = std::fs::read(record_path()?).ok()?;
    if bytes.len() > 16 * 1024 {
        return None;
    }
    serde_json::from_slice::<LaunchRecord>(&bytes)
        .ok()
        .filter(|record| record.schema_version == 1)
}

pub(crate) fn write_record(record: &LaunchRecord) {
    let Some(path) = record_path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(bytes) = serde_json::to_vec_pretty(record) {
        let _ = crate::core::updater::atomic_write_bytes(&path, &bytes);
    }
}

/// Remove the record when it names `pid` (a newer server may own it).
pub(crate) fn remove_record_of(pid: u32) {
    if read_record().is_some_and(|record| record.pid == pid)
        && let Some(path) = record_path()
    {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_record_round_trips_and_carries_no_token() {
        let record = LaunchRecord::of_this_process("127.0.0.1", 3333, "");
        let json = serde_json::to_string(&record).unwrap();
        assert!(!json.contains("token"));
        assert_eq!(serde_json::from_str::<LaunchRecord>(&json).unwrap(), record);
        assert!(record.process_alive());
    }
}
