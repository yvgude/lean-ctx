// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx daemon status --json` (`daemon.status`): whether this
//! Instance's daemon runs, its PID and socket, and whether the autostart
//! service is installed — so a reader never parses the text status.
//! Read only: it starts, stops and repairs nothing.

use serde::Serialize;

use super::Failure;

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct DaemonStatus {
    /// A verified LeanCTX daemon owns the recorded PID.
    pub running: bool,
    /// The PID of the running daemon, else `null`.
    pub pid: Option<u32>,
    /// The daemon socket (named pipe on Windows) while it listens, else `null`.
    pub socket: Option<String>,
    /// `enabled` | `disabled` | `unknown` (no autostart service on this OS).
    pub autostart: &'static str,
    /// The autostart service's name (`com.leanctx.daemon`,
    /// `lean-ctx-daemon`), `null` where there is none.
    pub service_name: Option<&'static str>,
}

/// The `autostart` value: `unknown` where the OS has no autostart service.
pub(crate) fn autostart_state(supported: bool, installed: bool) -> &'static str {
    match (supported, installed) {
        (false, _) => "unknown",
        (true, true) => "enabled",
        (true, false) => "disabled",
    }
}

pub(crate) fn build(_args: &[String]) -> Result<DaemonStatus, Failure> {
    // The identity gate, not a bare liveness check: a reused PID in
    // daemon.pid is not "running". Served by the daemon itself, it is the
    // running daemon its record names (the sweep excludes its own PID).
    let pid = lean_ctx::daemon::running_daemon_pid();
    let running = pid.is_some();
    let addr = lean_ctx::daemon::daemon_addr();
    let socket = (running && addr.is_listening()).then(|| addr.display());
    let supported = lean_ctx::daemon_autostart::is_supported();
    let installed = supported && lean_ctx::daemon_autostart::is_installed();
    Ok(DaemonStatus {
        running,
        pid,
        socket,
        autostart: autostart_state(supported, installed),
        service_name: lean_ctx::daemon_autostart::service_name(),
    })
}
