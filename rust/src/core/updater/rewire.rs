// SPDX-License-Identifier: Apache-2.0
//! After an update: the one-time config migrations, LaunchAgent re-wrap,
//! proxy restart and the setup refresh (`post_update_rewire`), limited to the
//! recorded agent selection or the already-connected tools; then the daemon
//! restart on the new binary (`restart_daemon_after_update`).

use std::path::Path;
use std::time::{Duration, Instant};

/// #828: One-time migration — enable `shadow_mode` for users who never
/// explicitly set it. Without shadow mode, most harnesses silently prefer
/// native tools, negating lean-ctx's token savings. Idempotent: a state-dir
/// marker prevents repeated flips if the user disables it after migration.
fn migrate_shadow_mode_default() {
    let Ok(state) = crate::core::paths::state_dir() else {
        return;
    };
    let marker = state.join("shadow_mode_migrated");
    if marker.exists() {
        return;
    }

    // Read raw TOML to distinguish "explicitly set to false" from "never set".
    let global = crate::core::config::Config::load_global();
    let raw_toml = crate::core::config::Config::path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();

    let explicitly_set = raw_toml.lines().any(|line| {
        let trimmed = line.trim();
        trimmed.starts_with("shadow_mode") && trimmed.contains('=')
    });

    if !explicitly_set && !global.shadow_mode {
        // User never touched shadow_mode — the old default was false.
        // Set it to true in the global config so it persists.
        match crate::core::config::Config::update_global(|c| c.shadow_mode = true) {
            Ok(_) => {
                eprintln!("  \u{2139} shadow_mode enabled (recommended default since v3.9.9).");
                eprintln!("    Disable: lean-ctx config set shadow_mode false");
            }
            Err(e) => tracing::warn!("could not enable shadow_mode during update: {e}"),
        }
    } else if explicitly_set && !global.shadow_mode {
        eprintln!("  \u{2139} shadow_mode is false in your config. Since v3.9.9, true is the");
        eprintln!("    recommended default (60%+ more token savings). Enable:");
        eprintln!("    lean-ctx config set shadow_mode true");
    }

    // Write marker so this migration runs only once.
    if let Some(parent) = marker.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&marker, "migrated");
}

/// Public entry point for the shadow_mode migration, callable from
/// `cmd_dev_install` and other lifecycle commands.
pub fn migrate_shadow_mode_default_public() {
    migrate_shadow_mode_default();
}

pub(super) fn post_update_rewire(skip_rules: bool, quiet: bool, setup: &dyn SetupRefresh) {
    // #356: regenerate installed LaunchAgent plists so they adopt the new
    // deny-~/Documents seatbelt wrapper. A plist is only (re)written on install,
    // so without this an upgrade keeps the old unwrapped plist — and the TCC
    // prompt — until the next manual enable. Idempotent: install rewrites the
    // plist and re-bootstraps it.
    #[cfg(target_os = "macos")]
    rewrap_launchagents_for_tcc();

    // The persist decision reads the GLOBAL file only and writes via
    // update_global, so a project-local override is never leaked into the
    // global config (#443).
    if crate::core::config::Config::load_global()
        .proxy_enabled
        .is_none()
        && crate::proxy_autostart::is_installed()
    {
        match crate::core::config::Config::update_global(|c| c.proxy_enabled = Some(true)) {
            Ok(_) => {
                eprintln!("  \u{2139} Proxy was already active \u{2014} keeping enabled.");
                eprintln!("    Disable anytime: lean-ctx proxy disable");
            }
            Err(e) => tracing::warn!("could not persist proxy_enabled during update: {e}"),
        }
    }

    // #828: shadow_mode default changed to true. Migrate existing configs
    // that never explicitly set it (still at old default false).
    migrate_shadow_mode_default();

    // Runtime decisions use the effective (global + project-local) config.
    let cfg = crate::core::config::Config::load();
    let proxy_active = cfg.proxy_enabled == Some(true);

    // Determine whether rules should be injected during rewire.
    // CLI --skip-rules always wins. Otherwise, respect the config setting.
    let effective_skip_rules = if skip_rules {
        true
    } else {
        !cfg.setup.should_inject_rules()
    };

    // PHASE 1: Restart proxy BEFORE writing env vars.
    if proxy_active {
        restart_proxy_if_running();
        wait_for_proxy_health(crate::core::proxy_endpoint::default_port());
    }

    // PHASE 2: Run setup which writes MCP configs (always) and rules (if opted in).
    // #1026: shell hooks are refreshed here — but install_all_with_style
    // respects shell_hook_disabled, so users who opt out keep control.
    if !cfg.shell_hook_disabled_effective() {
        eprintln!("  \u{2139} Refreshing shell hooks in ~/.zshenv / ~/.bashenv.");
        eprintln!("    Set shell_hook_disabled=true to prevent this in future updates.");
    }
    // Only the recorded selection (setup.agents) or, without one, the tools
    // that are already connected: an update never connects a new tool.
    if let Err(e) = setup.refresh(proxy_active, effective_skip_rules) {
        tracing::error!("Setup refresh error: {e}");
    }
    if !quiet {
        setup.print_unconnected_tools_hint();
    }

    // The Cockpit ships with LeanCTX: an update installs it when it is missing.
    if crate::core::intelligence_runtime::cockpit::install_if_missing(true) && !quiet {
        eprintln!("  \u{2713} LeanCTX Cockpit installed (open it with: lean-ctx cockpit).");
    }
}

/// The setup refresh an update runs after replacing the binary. Setup sits
/// above the engine, so the updater's caller supplies it.
pub trait SetupRefresh {
    /// Re-run setup in refresh-only mode: MCP configs, shell hooks and, unless
    /// `skip_rules`, rules for the recorded or already-connected tools.
    fn refresh(&self, proxy_active: bool, skip_rules: bool) -> Result<(), String>;
    /// Print what [`SetupRefresh::refresh`] would change, without writing.
    fn print_plan(&self, proxy_active: bool, skip_rules: bool);
    /// Name installed tools the refresh leaves alone.
    fn print_unconnected_tools_hint(&self);
}

/// `update --dry-run`: the refresh an update would run, without writing.
pub(super) fn print_rewire_plan(skip_rules: bool, setup: &dyn SetupRefresh) {
    let cfg = crate::core::config::Config::load();
    let skip_rules = skip_rules || !cfg.setup.should_inject_rules();
    setup.print_plan(cfg.proxy_enabled == Some(true), skip_rules);
    setup.print_unconnected_tools_hint();
}

/// #356: rewrite every installed LaunchAgent plist (daemon, proxy, auto-updater)
/// so an upgrade re-emits them with the deny-~/Documents seatbelt wrapper.
/// plists are only generated on install, so an existing install would otherwise
/// keep the pre-wrapper plist — and the prompt — until the next manual `enable`.
/// Each `install` / `install_schedule` rewrites the plist and re-bootstraps it.
/// Another instance (directory override, sandbox HOME) rewrites none of them:
/// they belong to the default install.
#[cfg(target_os = "macos")]
fn rewrap_launchagents_for_tcc() {
    if !crate::daemon_autostart::controls_user_services() {
        return;
    }
    if crate::proxy_autostart::is_installed() {
        crate::proxy_autostart::install(crate::core::proxy_endpoint::default_port(), true);
    }
    if crate::daemon_autostart::is_installed() {
        crate::daemon_autostart::install(true);
    }
    if crate::core::update_scheduler::schedule_status().enabled {
        let hours = crate::core::config::Config::load()
            .updates
            .check_interval_hours;
        if let Err(e) = crate::core::update_scheduler::install_schedule(hours) {
            tracing::warn!("#356 re-wrap of auto-update LaunchAgent failed: {e}");
        }
    }
}

fn wait_for_proxy_health(port: u16) {
    let max_attempts = 20;
    for i in 0..max_attempts {
        if is_proxy_reachable(port) {
            println!("  \x1b[32m✓\x1b[0m Proxy healthy on port {port}");
            return;
        }
        if i == 0 {
            print!("  \x1b[2mWaiting for proxy to become healthy");
        }
        print!(".");
        use std::io::Write;
        std::io::stdout().flush().ok();
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    println!();
    eprintln!(
        "  \x1b[33m⚠\x1b[0m Proxy did not respond within {}s — writing env vars anyway",
        max_attempts / 2
    );
    eprintln!("    If Claude Code shows connection errors, run: lean-ctx proxy start");
}

fn restart_proxy_if_running() {
    let port = crate::core::proxy_endpoint::default_port();

    if restart_managed_proxy() {
        return;
    }

    if is_proxy_reachable(port) {
        println!(
            "  \x1b[33m⟳\x1b[0m Proxy running on port {port} — restart it to use the new binary:"
        );
        println!("    \x1b[1mlean-ctx proxy start --port={port}\x1b[0m");
    }
}

/// Restart proxy managed by launchd (macOS) or systemd (Linux).
/// Returns `true` if a managed service was found and restarted. Another
/// instance restarts nothing: `systemctl --user restart lean-ctx-proxy`
/// addresses the default install's unit by name, whatever HOME says.
fn restart_managed_proxy() -> bool {
    if !crate::daemon_autostart::controls_user_services() {
        return false;
    }
    #[cfg(target_os = "macos")]
    {
        let plist_path = dirs::home_dir()
            .unwrap_or_default()
            .join("Library/LaunchAgents/com.leanctx.proxy.plist");
        if plist_path.exists() {
            if crate::core::launchd::bootstrap("com.leanctx.proxy", &plist_path) {
                println!("  \x1b[32m✓\x1b[0m Proxy restarted (LaunchAgent)");
            } else {
                println!("  \x1b[33m⚠\x1b[0m Could not restart proxy LaunchAgent");
            }
            return true;
        }
    }

    #[cfg(target_os = "linux")]
    {
        let service_path = dirs::home_dir()
            .unwrap_or_default()
            .join(".config/systemd/user/lean-ctx-proxy.service");
        if service_path.exists() {
            let result = std::process::Command::new("systemctl")
                .args(["--user", "restart", "lean-ctx-proxy"])
                .output();
            match result {
                Ok(o) if o.status.success() => {
                    println!("  \x1b[32m✓\x1b[0m Proxy restarted (systemd)");
                }
                _ => {
                    println!("  \x1b[33m⚠\x1b[0m Could not restart proxy systemd service");
                }
            }
            return true;
        }
    }

    false
}

fn is_proxy_reachable(port: u16) -> bool {
    ureq::get(&format!("http://127.0.0.1:{port}/health"))
        .call()
        .is_ok()
}

/// How long the updater waits for the restarted daemon, as the installers do
/// (`LEAN_CTX_DAEMON_WAIT_SECS`, default 10 s, since !432).
const DAEMON_WAIT_ENV: &str = "LEAN_CTX_DAEMON_WAIT_SECS";
const DEFAULT_DAEMON_WAIT_SECS: u64 = 10;
/// Upper bound for the wait, so a stray value never hangs an update.
const MAX_DAEMON_WAIT_SECS: u64 = 300;
/// Interval between two version checks while waiting.
pub(super) const DAEMON_POLL: Duration = Duration::from_millis(500);
/// Longest single version probe; a probe never runs past the wait's deadline.
pub(super) const DAEMON_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// What restarting the daemon after an update found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DaemonRestart {
    /// No daemon ran before the update: it stays stopped.
    NotRunningBefore,
    /// The new binary is not in place yet (the deferred swap on Windows when
    /// the binary is locked): restarting now would start the old binary.
    SwapDeferred,
    /// An unattended update (`--scheduled`, `--quiet` from an older scheduler,
    /// or a launchd-standalone process on macOS) found no autostart unit to restart the daemon through. Starting
    /// it directly would put the daemon inside the updater's sandbox (the
    /// deny-~/Documents seatbelt) or its timer unit, so it is left on the old
    /// binary and the user is told to restart it.
    ManualRestartNeeded,
    /// Stopping or starting the daemon failed.
    RestartFailed(String),
    /// The daemon answers with the new version.
    Verified(String),
    /// Within `waited_secs` the daemon did not answer with `expected`;
    /// `reported` is the last version it answered with (`None`: no answer).
    VersionMismatch {
        expected: String,
        reported: Option<String>,
        waited_secs: u64,
    },
}

impl DaemonRestart {
    /// Whether the version check ran, so a warning about the wait value
    /// (`LEAN_CTX_DAEMON_WAIT_SECS`) is relevant.
    pub(super) fn waited(&self) -> bool {
        matches!(self, Self::Verified(_) | Self::VersionMismatch { .. })
    }
}

/// The daemon operations the restart needs, so the decision logic is tested
/// without starting a real daemon.
pub(super) trait DaemonControl {
    /// Whether this instance's autostart unit (LaunchAgent, systemd user
    /// unit) is installed, so a restart goes through the service manager.
    fn supervised(&self) -> bool;
    /// Stop this instance's daemon and start it again on the new binary.
    /// Not covered by the version-check deadline: the restart is bounded by
    /// the stop ladder (endpoint shutdown, TERM, KILL) and the service
    /// manager's own timeouts (`systemctl --user stop/start` waits up to the
    /// unit's `TimeoutStopSec`/`TimeoutStartSec`).
    fn restart(&mut self) -> Result<(), String>;
    /// The version the running daemon reports within `timeout`, `None` when
    /// it does not answer in time.
    fn reported_version(&mut self, timeout: Duration) -> Option<String>;
    /// The current time, so the wait is bounded by a deadline.
    fn now(&self) -> Instant;
    /// Wait before the next check.
    fn pause(&mut self, duration: Duration);
}

/// What the updater knows about the daemon and its own run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct RestartContext {
    /// This instance's daemon ran before the swap.
    pub(super) was_running: bool,
    /// The new binary is in place (not deferred to the Windows helper).
    pub(super) swap_landed: bool,
    /// An unattended run on a platform whose daemon belongs under a service
    /// unit (see [`unattended_run`]).
    pub(super) unattended: bool,
    /// The setup refresh already restarted the daemon through its unit (see
    /// [`restarted_by_rewrap`]).
    pub(super) already_restarted: bool,
}

/// The wait for the restarted daemon from `LEAN_CTX_DAEMON_WAIT_SECS`: whole
/// seconds, at most 300 s, the default 10 s when unset. An invalid value
/// (`1.5`, `-3`, `abc`) also falls back to the default and a capped one to
/// 300 s; both come with a warning for the user.
pub(super) fn daemon_wait(raw: Option<&str>) -> (Duration, Option<String>) {
    let default = Duration::from_secs(DEFAULT_DAEMON_WAIT_SECS);
    let Some(value) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return (default, None);
    };
    let shown: String = value.chars().take(32).collect();
    if !value.bytes().all(|b| b.is_ascii_digit()) {
        return (
            default,
            Some(format!(
                "  \x1b[33m⚠\x1b[0m Ignoring {DAEMON_WAIT_ENV}={shown:?}: not a whole number of seconds; waiting {DEFAULT_DAEMON_WAIT_SECS}s."
            )),
        );
    }
    match value.parse::<u64>() {
        Ok(secs) if secs <= MAX_DAEMON_WAIT_SECS => (Duration::from_secs(secs), None),
        _ => (
            Duration::from_secs(MAX_DAEMON_WAIT_SECS),
            Some(format!(
                "  \x1b[33m⚠\x1b[0m {DAEMON_WAIT_ENV}={shown} is above the limit; waiting {MAX_DAEMON_WAIT_SECS}s."
            )),
        ),
    }
}

/// Whether this update runs unattended on `os` in the sense that matters for
/// the daemon: an automatic run (`--scheduled`, or `--quiet` alone as the
/// schedulers written before #335 still invoke it, the same definition as
/// the auto-update gate) or a launchd-standalone process. Only on macOS and
/// Linux, where a directly started daemon would inherit the updater's
/// deny-~/Documents seatbelt (macOS) or die with the oneshot timer unit's
/// cgroup (Linux, `KillMode=control-group`). Windows has no unit: the daemon
/// always starts directly there.
pub(super) fn unattended_run(os: &str, scheduled: bool, quiet: bool, tcc_standalone: bool) -> bool {
    matches!(os, "macos" | "linux") && (scheduled || quiet || tcc_standalone)
}

/// Whether the setup refresh already restarted the daemon on `os`: on macOS
/// `post_update_rewire` re-wraps an installed daemon LaunchAgent and
/// re-bootstraps it, which restarts the daemon that launchd runs. That holds
/// only when the daemon found before the swap (`daemon_pid`) was the
/// LaunchAgent's job (`launchd_pid`); a daemon started by hand next to an
/// installed plist is still stopped and restarted explicitly.
pub(super) fn restarted_by_rewrap(
    os: &str,
    unit_installed: bool,
    daemon_pid: Option<u32>,
    launchd_pid: Option<u32>,
) -> bool {
    os == "macos" && unit_installed && daemon_pid.is_some() && daemon_pid == launchd_pid
}

fn normalize_version(version: &str) -> &str {
    version.trim().trim_start_matches('v')
}

/// Restart the daemon only when it ran before the update and the new binary
/// is in place, then check until a deadline `wait` after the restart that it
/// reports `expected`. Every probe is cut to the time left, so the whole wait
/// ends by the deadline (the very first probe gets at least one poll
/// interval, so a wait of 0 s still asks once). The deadline bounds only the
/// version check, not [`DaemonControl::restart`] before it. A daemon that ran
/// before is never left without a report: the outcome says whether it came
/// back on the new version.
pub(super) fn restart_and_verify(
    control: &mut dyn DaemonControl,
    context: RestartContext,
    expected: &str,
    wait: Duration,
) -> DaemonRestart {
    if !context.was_running {
        return DaemonRestart::NotRunningBefore;
    }
    if !context.swap_landed {
        return DaemonRestart::SwapDeferred;
    }
    if !context.already_restarted {
        if context.unattended && !control.supervised() {
            return DaemonRestart::ManualRestartNeeded;
        }
        if let Err(error) = control.restart() {
            return DaemonRestart::RestartFailed(error);
        }
    }
    let expected = normalize_version(expected).to_string();
    let start = control.now();
    let deadline = start + wait;
    let mut reported = None;
    let mut first = true;
    loop {
        let remaining = deadline.saturating_duration_since(control.now());
        let budget = if first {
            remaining.max(DAEMON_POLL)
        } else {
            remaining
        };
        first = false;
        if let Some(version) = control.reported_version(DAEMON_PROBE_TIMEOUT.min(budget)) {
            let version = normalize_version(&version).to_string();
            if version == expected {
                return DaemonRestart::Verified(version);
            }
            reported = Some(version);
        }
        let remaining = deadline.saturating_duration_since(control.now());
        if remaining.is_zero() {
            break;
        }
        control.pause(DAEMON_POLL.min(remaining));
    }
    DaemonRestart::VersionMismatch {
        expected,
        reported,
        waited_secs: control.now().saturating_duration_since(start).as_secs(),
    }
}

/// What the updater prints for `outcome`: `(warning, lines)`, where a warning
/// goes to stderr even in quiet mode. `None` when there is nothing to say.
pub(super) fn daemon_restart_report(outcome: &DaemonRestart) -> Option<(bool, Vec<String>)> {
    const CHECK: &str =
        "    Check it with: lean-ctx daemon status; restart it with: lean-ctx daemon restart";
    match outcome {
        DaemonRestart::NotRunningBefore => None,
        DaemonRestart::Verified(version) => Some((
            false,
            vec![format!(
                "  \x1b[32m✓\x1b[0m Daemon running on the new binary (v{version})"
            )],
        )),
        DaemonRestart::SwapDeferred => Some((
            true,
            vec![
                "  \x1b[33m⚠\x1b[0m The binary swap finishes after this process exits; the daemon was not restarted.".to_string(),
                "    Once the swap is done, run: lean-ctx daemon restart".to_string(),
            ],
        )),
        DaemonRestart::ManualRestartNeeded => Some((
            true,
            vec![
                "  \x1b[33m⚠\x1b[0m The daemon still runs the old binary: an unattended update does not start a daemon outside its autostart unit.".to_string(),
                "    To move it to the new binary, run: lean-ctx daemon restart".to_string(),
            ],
        )),
        DaemonRestart::RestartFailed(error) => Some((
            true,
            vec![
                format!("  \x1b[33m⚠\x1b[0m Could not restart the daemon on the new binary: {error}"),
                CHECK.to_string(),
            ],
        )),
        DaemonRestart::VersionMismatch {
            expected,
            reported,
            waited_secs,
        } => {
            let first = match reported {
                Some(version) => format!(
                    "  \x1b[33m⚠\x1b[0m The restarted daemon reports v{version}, not v{expected}."
                ),
                None => format!(
                    "  \x1b[33m⚠\x1b[0m The daemon did not come back on v{expected} within {waited_secs}s after the restart."
                ),
            };
            Some((
                true,
                vec![
                    first,
                    CHECK.to_string(),
                    "    The update itself is installed; to go back: lean-ctx update --rollback"
                        .to_string(),
                ],
            ))
        }
    }
}

/// The version in a `GET /v1/local/capabilities` document (`"version"`).
pub(super) fn version_from_capabilities(body: &str) -> Option<String> {
    let doc: serde_json::Value = serde_json::from_str(body).ok()?;
    doc.get("version")?
        .as_str()
        .map(normalize_version)
        .filter(|version| !version.is_empty())
        .map(str::to_string)
}

/// The daemon of this instance through the existing daemon and service API,
/// the same channel as `lean-ctx daemon restart`: `stop_daemon` stops this
/// data dir's daemon (and its launchd/systemd supervisor only when that runs
/// it); the autostart unit starts it again where one is installed (macOS
/// LaunchAgent, Linux systemd user unit), otherwise the new binary starts it
/// directly (Windows, or no unit). Never `lean-ctx stop`.
struct ServiceDaemon<'a> {
    binary: &'a Path,
    supervised: bool,
    quiet: bool,
    runtime: Option<tokio::runtime::Runtime>,
}

impl DaemonControl for ServiceDaemon<'_> {
    fn supervised(&self) -> bool {
        self.supervised
    }

    fn restart(&mut self) -> Result<(), String> {
        if !self.quiet {
            println!("  \x1b[2mRestarting the lean-ctx daemon on the new binary …\x1b[0m");
        }
        let stopped = if self.quiet {
            crate::daemon::stop_daemon_quietly()
        } else {
            crate::daemon::stop_daemon()
        };
        stopped.map_err(|e| format!("stop: {e}"))?;
        if self.supervised {
            // A refused start is reported now, not after the whole wait.
            return if crate::daemon_autostart::start() {
                Ok(())
            } else {
                Err("start: the service manager did not start the autostart unit".to_string())
            };
        }
        crate::daemon::start_daemon_with_binary(&[], Some(self.binary))
            .map_err(|e| format!("start: {e}"))
    }

    fn reported_version(&mut self, timeout: Duration) -> Option<String> {
        if self.runtime.is_none() {
            self.runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .ok();
        }
        let runtime = self.runtime.as_ref()?;
        let body = runtime.block_on(async {
            tokio::time::timeout(
                timeout,
                crate::daemon_client::daemon_request("GET", "/v1/local/capabilities", ""),
            )
            .await
            .ok()?
            .ok()
        })?;
        version_from_capabilities(&body)
    }

    fn now(&self) -> Instant {
        Instant::now()
    }

    fn pause(&mut self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// After a successful update: restart the daemon that ran before the swap on
/// the new binary (`binary`) and confirm it reports `target_version`, as the
/// installers do since !432. The rollback state is not touched: a daemon that
/// does not come back is reported, the update stays installed and
/// `lean-ctx update --rollback` remains available.
pub(super) fn restart_daemon_after_update(
    context: RestartContext,
    target_version: &str,
    binary: &Path,
    quiet: bool,
) -> DaemonRestart {
    let (wait, wait_warning) = daemon_wait(std::env::var(DAEMON_WAIT_ENV).ok().as_deref());
    let mut control = ServiceDaemon {
        binary,
        supervised: crate::daemon_autostart::is_installed(),
        quiet,
        runtime: None,
    };
    let outcome = restart_and_verify(&mut control, context, target_version, wait);
    // The wait value matters only when a wait actually ran.
    if let Some(warning) = wait_warning.filter(|_| outcome.waited()) {
        eprintln!("{warning}");
    }
    if let Some((warning, lines)) = daemon_restart_report(&outcome) {
        for line in lines {
            if warning {
                eprintln!("{line}");
            } else if !quiet {
                println!("{line}");
            }
        }
    }
    outcome
}
