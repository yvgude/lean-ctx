//! Modern, timeout-protected macOS launchd (`launchctl`) control.
//!
//! Uses the `bootstrap` / `bootout` / `enable` / `kickstart` subcommands in the
//! per-user `gui/<uid>` domain instead of the legacy `load` / `unload`. On
//! macOS 13+ the legacy verbs intermittently fail with `Input/output error`
//! and can leave a job *half-loaded*; combined with `KeepAlive` that produces a
//! launchd crash-loop which wedges every client waiting on the service (and
//! previously forced users to reboot after a binary update).
//!
//! Every invocation is wrapped in a hard timeout via
//! [`crate::ipc::process::run_with_timeout`], so a stuck `launchctl` can never
//! block a `lean-ctx` command.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

const LAUNCHCTL_TIMEOUT: Duration = Duration::from_secs(10);

fn gui_domain() -> String {
    // SAFETY: `getuid` takes no arguments, always succeeds, and only reads the
    // calling process's real UID — it cannot fail or cause undefined behaviour.
    let uid = unsafe { libc::getuid() };
    format!("gui/{uid}")
}

fn service_target(label: &str) -> String {
    format!("{}/{label}", gui_domain())
}

/// Invoke `launchctl` with the given args under a hard timeout.
/// Returns `Some(success)` when the call completed, `None` when it timed out.
fn launchctl(args: &[&str]) -> Option<bool> {
    let mut cmd = Command::new("launchctl");
    cmd.args(args);
    if let Some(out) = crate::ipc::process::run_with_timeout(cmd, LAUNCHCTL_TIMEOUT) {
        Some(out.status.success())
    } else {
        tracing::warn!("launchctl {args:?} timed out after {LAUNCHCTL_TIMEOUT:?}");
        None
    }
}

/// Stop and unregister a LaunchAgent (idempotent; a not-loaded job is fine).
/// Never hangs. Tries label-based bootout first, then path-based as a fallback.
pub(crate) fn bootout(label: &str, plist: &Path) {
    if launchctl(&["bootout", &service_target(label)]).is_some() {
        return;
    }
    let _ = launchctl(&["bootout", &gui_domain(), &plist.to_string_lossy()]);
}

/// Register and enable a LaunchAgent in the gui domain. Idempotent and
/// timeout-protected. Returns `true` if bootstrap reported success.
///
/// The job is always booted out first, so the subsequent bootstrap is a clean
/// load that honours the plist's `RunAtLoad` (proxy/daemon start immediately;
/// interval-only jobs like the auto-updater simply register). No `kickstart` is
/// issued, so this never forces an unwanted immediate run.
pub(crate) fn bootstrap(label: &str, plist: &Path) -> bool {
    // Clear any stale registration so bootstrap can't fail with
    // "service already bootstrapped".
    bootout(label, plist);
    let _ = launchctl(&["enable", &service_target(label)]);
    launchctl(&["bootstrap", &gui_domain(), &plist.to_string_lossy()]).unwrap_or(false)
}

/// The PID of the running job `label` in the gui domain (`pid = N` in
/// `launchctl print`), or `None` when it is not loaded or runs nothing.
/// Read-only and timeout-protected.
pub(crate) fn service_pid(label: &str) -> Option<u32> {
    let mut cmd = Command::new("launchctl");
    cmd.args(["print", &service_target(label)]);
    let out = crate::ipc::process::run_with_timeout(cmd, LAUNCHCTL_TIMEOUT)?;
    if !out.status.success() {
        return None;
    }
    parse_print_pid(&String::from_utf8_lossy(&out.stdout))
}

/// The top-level `pid = N` line of `launchctl print` output.
fn parse_print_pid(text: &str) -> Option<u32> {
    text.lines()
        .filter_map(|line| line.trim().strip_prefix("pid = "))
        .find_map(|value| value.trim().parse::<u32>().ok())
        .filter(|pid| *pid != 0)
}

/// Returns `true` if the service is currently registered in the gui domain.
pub(crate) fn is_loaded(label: &str) -> bool {
    launchctl(&["print", &service_target(label)]).unwrap_or(false)
}
