use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};

use crate::ipc;

/// True once this process is the long-lived foreground daemon
/// (`serve --_foreground-daemon`). Sessions delegate index builds to the daemon
/// (#460); the daemon itself must never delegate to itself, so it checks this.
static IS_FOREGROUND_DAEMON: AtomicBool = AtomicBool::new(false);

/// Mark this process as the foreground daemon. Called once at daemon init.
pub fn mark_foreground_daemon() {
    IS_FOREGROUND_DAEMON.store(true, Ordering::Relaxed);
}

/// Whether this process is the long-lived foreground daemon.
#[must_use]
pub fn is_foreground_daemon() -> bool {
    IS_FOREGROUND_DAEMON.load(Ordering::Relaxed)
}

fn data_dir() -> PathBuf {
    ipc::endpoint_dir()
}

pub fn daemon_pid_path() -> PathBuf {
    data_dir().join("daemon.pid")
}

/// The daemon's process identity, next to `daemon.pid` (#2176 review #1).
/// A separate file keeps `daemon.pid` a plain PID, which earlier releases
/// (still running MCP servers during an update) read and parse.
fn daemon_identity_path() -> PathBuf {
    data_dir().join("daemon.identity.json")
}

#[derive(serde::Serialize, serde::Deserialize)]
struct DaemonIdentityFile {
    pid: u32,
    identity: ipc::process::ProcessIdentity,
}

/// The recorded daemon: the PID from `daemon.pid` and, when the identity
/// file names the same PID, the identity recorded at start. A PID file of an
/// earlier release has no identity; it is gated by command line only.
#[must_use]
pub fn read_daemon_record() -> Option<ipc::process::RecordedPid> {
    let pid = read_daemon_pid()?;
    let identity = fs::read(daemon_identity_path())
        .ok()
        .filter(|bytes| bytes.len() <= 16 * 1024)
        .and_then(|bytes| serde_json::from_slice::<DaemonIdentityFile>(&bytes).ok())
        .filter(|file| file.pid == pid)
        .map(|file| file.identity);
    Some(ipc::process::RecordedPid { pid, identity })
}

fn remove_daemon_record() {
    let _ = fs::remove_file(daemon_pid_path());
    let _ = fs::remove_file(daemon_identity_path());
}

/// The PID of the recorded daemon when it really is this user's lean-ctx
/// daemon: command line `serve --_foreground-daemon`, lean-ctx executable,
/// same user, and the recorded identity where there is one. Slower than
/// [`is_daemon_running`] (reads the command line); for `doctor` and `stop`.
#[must_use]
pub fn verified_daemon() -> Option<ipc::process::ServiceProcess> {
    let record = read_daemon_record()?;
    ipc::process::find_service_processes(
        &[record],
        ipc::process::ServiceSweep::Only(ipc::process::ServiceKind::Daemon),
    )
    .into_iter()
    .next()
}

/// Releases up to 3.12.0 kept `daemon.pid` and `daemon.sock` in the OS
/// data-local directory. A daemon such a release started is stopped before a
/// new one starts and by `stop_daemon`, so the two never run side by side. Only
/// a live `lean-ctx` process is signalled, never a reused PID. A process with a
/// directory override (a test, an isolated instance) never touches it: that
/// daemon belongs to the default install (GH #2007).
fn stop_legacy_daemon() {
    if cfg!(test) || crate::core::paths::dir_override_active() {
        return;
    }
    let legacy = ipc::legacy_endpoint_dir();
    if legacy == data_dir() {
        return;
    }
    let pid_path = legacy.join("daemon.pid");
    let Some(pid) = fs::read_to_string(&pid_path)
        .ok()
        .and_then(|contents| contents.trim().parse::<u32>().ok())
    else {
        return;
    };
    // Only when the PID still is a lean-ctx daemon of this user (command
    // line, executable, identity): a name match alone also matched MCP
    // servers on Windows, where PIDs are reused quickly (#2176 review #1).
    let report = ipc::process::stop_services(
        &[ipc::process::RecordedPid::bare(pid)],
        ipc::process::ServiceSweep::Only(ipc::process::ServiceKind::Daemon),
    );
    if !report.stopped.is_empty() {
        eprintln!("Stopped the daemon of an earlier release (PID {pid}).");
    }
    let _ = fs::remove_file(&pid_path);
    #[cfg(unix)]
    let _ = fs::remove_file(legacy.join("daemon.sock"));
}

pub fn daemon_addr() -> ipc::DaemonAddr {
    ipc::DaemonAddr::default_for_current_os()
}

/// Fast check on the hot path (every MCP request): the recorded PID is
/// alive and, when the daemon recorded its identity, still that process. A
/// PID file of an earlier release has no identity; [`verified_daemon`] also
/// checks the command line.
pub fn is_daemon_running() -> bool {
    let Some(record) = read_daemon_record() else {
        return false;
    };
    if record.pid == std::process::id() {
        return true;
    }
    let pid_alive = ipc::process::is_alive(record.pid);
    match record.identity.as_ref() {
        Some(identity) if pid_alive && ipc::process::is_same_process(record.pid, identity) => {
            return true;
        }
        // A PID file without identity (an earlier release, or a PID reused
        // after a reboot): only a verified lean-ctx daemon counts (#2176 VM).
        // Rare, so the slower command-line gate is fine. A live process that
        // cannot be verified keeps its files: `daemon start`/`stop` decide.
        None if pid_alive => return legacy_record_verified(record.pid),
        Some(_) | None => {}
    }
    remove_daemon_record();
    ipc::cleanup(&daemon_addr());
    false
}

/// [`verified_daemon`] for an identity-less PID file, remembered per PID for
/// this process: the hot path must not read command lines on every request.
fn legacy_record_verified(pid: u32) -> bool {
    // Bit 33: cached; bit 32: verified; low 32 bits: the PID.
    static LAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let last = LAST.load(Ordering::Relaxed);
    if last & (1 << 33) != 0 && last as u32 == pid {
        return last & (1 << 32) != 0;
    }
    let verified = verified_daemon().is_some_and(|daemon| daemon.pid == pid);
    LAST.store(
        (1 << 33) | (u64::from(verified) << 32) | u64::from(pid),
        Ordering::Relaxed,
    );
    verified
}

pub fn read_daemon_pid() -> Option<u32> {
    let contents = fs::read_to_string(daemon_pid_path()).ok()?;
    contents.trim().parse::<u32>().ok()
}

/// Exclusive, bounded-wait lock that serializes the daemon-start critical
/// section (liveness check → spawn → PID write). Several MCP servers launching
/// at once (Claude Code + OpenCode + Cursor) would otherwise all pass the
/// `is_daemon_running()` check in the TOCTOU window and each spawn a daemon —
/// the process proliferation seen in #453. The advisory flock is tied to the
/// open fd, so it is released automatically if a holder crashes; the bounded
/// wait keeps a wedged holder from blocking startup forever (the
/// `is_daemon_running()` re-check remains the last line of defense).
fn acquire_start_lock() -> Option<fs::File> {
    use fs2::FileExt;
    let lock_path = data_dir().join("daemon.start.lock");
    if let Some(parent) = lock_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .ok()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return Some(file),
            Err(_) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(_) => return None,
        }
    }
}

pub fn start_daemon(args: &[String]) -> Result<()> {
    start_daemon_with_binary(args, None)
}

/// Source installs launch the verified installed copy, preserving the caller's CWD.
pub fn start_daemon_with_binary(args: &[String], binary: Option<&std::path::Path>) -> Result<()> {
    // Held for the whole critical section; released when `_start_lock` drops.
    let _start_lock = acquire_start_lock();

    // #2176 VM: a reused PID in daemon.pid (any live process) must not block
    // the start; only a verified lean-ctx daemon of this user does.
    if let Some(daemon) = verified_daemon() {
        anyhow::bail!(
            "Daemon already running (PID {}). Use --stop to stop it first.",
            daemon.pid
        );
    }
    if let Some(pid) = read_daemon_pid() {
        eprintln!("{}", stale_pid_message(pid));
        remove_daemon_record();
    }

    stop_legacy_daemon();
    ipc::cleanup(&daemon_addr());

    if let Ok(data_dir) = crate::core::data_dir::lean_ctx_data_dir() {
        crate::config_io::cleanup_legacy_backups(&data_dir);
    }

    let exe = binary.map_or_else(
        || std::path::PathBuf::from(crate::core::portable_binary::resolve_portable_binary()),
        std::path::Path::to_path_buf,
    );

    let mut cmd_args = vec!["serve".to_string()];
    for arg in args {
        if arg == "--daemon" || arg == "-d" {
            continue;
        }
        cmd_args.push(arg.clone());
    }
    cmd_args.push("--_foreground-daemon".to_string());

    let log_dir = data_dir();
    let _ = fs::create_dir_all(&log_dir);
    let stderr_log = log_dir.join("daemon-stderr.log");
    let stderr_file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&stderr_log);
    let stderr_cfg = match stderr_file {
        Ok(f) => ipc::process::DetachedStderr::File(f),
        Err(error) => {
            eprintln!(
                "Cannot open {}: {error}; daemon stderr disabled.",
                stderr_log.display()
            );
            ipc::process::DetachedStderr::Null
        }
    };

    let mut cmd = Command::new(&exe);
    cmd.args(&cmd_args);
    // #356: if the *spawner* is a launchd-standalone process (e.g. the proxy
    // auto-starting the daemon), the spawned daemon's ppid is the spawner — not
    // 1 — so `getppid()`-based detection would miss it and the daemon's TCC path
    // guards would stay off. Propagate the marker explicitly so the daemon
    // treats itself as standalone regardless of where it sits in the tree.
    if crate::core::pathutil::process_is_tcc_standalone() {
        cmd.env("LEAN_CTX_TCC_STANDALONE", "1");
    }
    // Detached spawn: on Windows the daemon leaves the caller's job and
    // inherits none of its handles (GL #545, #2176).
    let child = ipc::process::spawn_detached(&mut cmd, stderr_cfg)
        .with_context(|| format!("failed to spawn daemon: {}", exe.display()))?;

    let pid = child.id();
    write_pid_file(pid)?;

    std::thread::sleep(std::time::Duration::from_millis(200));

    if !ipc::process::is_alive(pid) {
        remove_daemon_record();
        let stderr_content = fs::read_to_string(&stderr_log).unwrap_or_default();
        let stderr_trimmed = stderr_content.trim();
        if stderr_trimmed.is_empty() {
            anyhow::bail!("Daemon process exited immediately. Check logs for errors.");
        }
        anyhow::bail!("Daemon process exited immediately:\n{stderr_trimmed}");
    }

    let addr = daemon_addr();
    if crate::core::protocol::meta_visible() {
        eprintln!(
            "lean-ctx daemon started (PID {pid})\n  Endpoint: {}\n  PID file: {}",
            addr.display(),
            daemon_pid_path().display()
        );
    }

    Ok(())
}

/// What `stop_daemon` says when the recorded daemon had already exited. A
/// stale PID file is normal after a reboot, a crash or an editor closing, so
/// the line is neutral rather than alarming (#2176).
fn stale_pid_message(pid: u32) -> String {
    format!("No daemon was running (stale PID file for PID {pid} removed).")
}

pub fn stop_daemon() -> Result<()> {
    stop_legacy_daemon();

    let Some(record) = read_daemon_record() else {
        eprintln!("No daemon PID file found. Nothing to stop.");
        return Ok(());
    };
    let pid = record.pid;

    // #2176 review #1: the PID file survives a reboot or crash, and Windows
    // reuses PIDs quickly — the PID may now be an MCP server, an editor or
    // any other process. It is signalled only while it is still this user's
    // lean-ctx daemon (command line, executable, recorded identity).
    let Some(daemon) = ipc::process::find_service_processes(
        std::slice::from_ref(&record),
        ipc::process::ServiceSweep::Only(ipc::process::ServiceKind::Daemon),
    )
    .into_iter()
    .next() else {
        // Not provably the daemon: never signalled. Should the check have
        // failed for a real daemon (unreadable command line), the endpoint
        // shutdown still ends it before its endpoint files are removed.
        if ipc::process::is_alive(pid) {
            let _ = try_http_shutdown();
        }
        eprintln!("{}", stale_pid_message(pid));
        ipc::cleanup(&daemon_addr());
        remove_daemon_record();
        return Ok(());
    };

    // The HTTP shutdown goes to this instance's daemon endpoint, never to a PID.
    if try_http_shutdown() {
        for _ in 0..30 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if !daemon.is_running() {
                break;
            }
        }
    }

    if daemon.is_running() {
        daemon.signal(ipc::process::StopSignal::Terminate);
        for _ in 0..20 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if !daemon.is_running() {
                break;
            }
        }
    }

    if daemon.is_running() {
        eprintln!("Daemon (PID {pid}) did not stop gracefully, force killing.");
        daemon.signal(ipc::process::StopSignal::Kill);
        std::thread::sleep(std::time::Duration::from_millis(200));
    }

    remove_daemon_record();
    ipc::cleanup(&daemon_addr());
    eprintln!("lean-ctx daemon stopped (PID {pid}).");

    // #1292: stopping one daemon ends that daemon only. A machine-wide
    // name-based sweep here reached every other lean-ctx process — the real
    // user's daemon and proxy under a sandbox HOME, other worktrees' daemons,
    // other agents' `lean-ctx -c` commands — from `setup`, `daemon stop` and
    // `serve`. The lifecycle commands (`stop`, `restart`, `config apply`,
    // `uninstall`, `dev-install`) sweep the background services explicitly
    // with `ipc::process::stop_services`, never MCP servers (#2176).
    Ok(())
}

fn try_http_shutdown() -> bool {
    let Ok(rt) = tokio::runtime::Runtime::new() else {
        return false;
    };

    rt.block_on(async {
        crate::daemon_client::daemon_request("POST", "/v1/shutdown", "")
            .await
            .is_ok()
    })
}

pub fn daemon_status() -> String {
    let addr = daemon_addr();
    if let Some(pid) = read_daemon_pid() {
        if verified_daemon().is_some() {
            let listening = addr.is_listening();
            return format!(
                "Daemon running (PID {pid})\n  Endpoint: {} ({})\n  PID file: {}",
                addr.display(),
                if listening { "ready" } else { "missing" },
                daemon_pid_path().display()
            );
        }
        return format!("Daemon not running (stale PID file for PID {pid})");
    }
    "Daemon not running".to_string()
}

fn write_pid_file(pid: u32) -> Result<()> {
    let pid_path = daemon_pid_path();
    if let Some(parent) = pid_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("cannot create dir: {}", parent.display()))?;
    }
    let mut f = fs::File::create(&pid_path)
        .with_context(|| format!("cannot write PID file: {}", pid_path.display()))?;
    write!(f, "{pid}")?;
    // The identity defends the PID against reuse after a reboot or crash;
    // without one (unsupported platform) the PID is gated by command line.
    let identity_path = daemon_identity_path();
    match ipc::process::identity(pid) {
        Some(identity) => {
            let file = DaemonIdentityFile { pid, identity };
            if let Ok(bytes) = serde_json::to_vec(&file) {
                let _ = crate::core::updater::atomic_write_bytes(&identity_path, &bytes);
            }
        }
        None => {
            let _ = fs::remove_file(&identity_path);
        }
    }
    Ok(())
}

/// Initialize the foreground-daemon process. Commits the XDG layout pin (and
/// drains a residual `~/.lean-ctx`) *before* the daemon writes anything, so this
/// long-running, possibly launchd/systemd-autostarted writer can never
/// re-collapse config/data/state/cache onto a stray legacy dir (GL #623). The
/// MCP server pins on its own start; the daemon is the other independent entry
/// point (e.g. `serve --_foreground-daemon`), so it must heal too. `heal()` is
/// idempotent and cheap — a no-op once pinned and when no residual dir exists.
pub fn init_foreground_daemon() -> Result<()> {
    mark_foreground_daemon();
    crate::core::layout_pin::heal();
    // The daemon is the single long-lived owner for local agent-presence
    // maintenance. Starting this in every short-lived MCP process creates a
    // reaper swarm precisely when many coding agents connect at once.
    crate::core::agents::reaper::spawn_reaper();
    // The daemon starts at login: an Engine without its Cockpit (an update
    // from a release without one) gets it here, without a command.
    crate::core::intelligence_runtime::cockpit::install_if_missing_in_background();
    let pid = std::process::id();
    write_pid_file(pid)?;
    Ok(())
}

/// PIDs of the background services this instance (data dir) recorded: the
/// daemon PID file, the proxy that answers `/health` on the configured port,
/// and the Cockpit launch record. Nothing else is ever a stop candidate —
/// MCP stdio servers, `lean-ctx -c` runners, hooks and other instances'
/// services have no record here (#2176).
#[must_use]
pub fn owned_service_pids() -> Vec<ipc::process::RecordedPid> {
    let mut pids = Vec::new();
    pids.extend(read_daemon_record());
    pids.extend(crate::proxy_autostart::recorded_proxy_pid().map(ipc::process::RecordedPid::bare));
    // #2176 review #6: the Cockpit record carries the identity it started
    // with; a record whose PID was reused is not a candidate.
    pids.extend(crate::core::cockpit_launch_record::read_record().map(|r| {
        ipc::process::RecordedPid {
            pid: r.pid,
            identity: r.identity,
        }
    }));
    pids
}

/// Stop this instance's recorded background services that `sweep` includes
/// (see [`owned_service_pids`]); every PID is gated again by command line,
/// user and process identity in `ipc::process::stop_services`.
pub fn stop_owned_services(sweep: ipc::process::ServiceSweep) -> ipc::process::ServiceStopReport {
    ipc::process::stop_services(&owned_service_pids(), sweep)
}

/// lean-ctx background services of this user that this instance has no
/// record of. Listed by `stop`, `uninstall` and `doctor`, never stopped
/// (owner decision 02.11.).
#[must_use]
pub fn unrecorded_services() -> Vec<ipc::process::ServiceProcess> {
    let recorded: Vec<u32> = owned_service_pids().iter().map(|r| r.pid).collect();
    ipc::process::find_unrecorded_services(&recorded)
}

/// The two lines `stop`/`uninstall`/`doctor` print for
/// [`unrecorded_services`] — what still runs, and how to stop it — or `None`.
#[must_use]
pub fn unrecorded_services_warning() -> Option<(String, String)> {
    ipc::process::unrecorded_services_lines(&unrecorded_services(), cfg!(windows))
}

/// Start a proxy that `restart`/`config apply` stopped again when no
/// autostart service brought it back (#2176 review #4): detached, with the
/// arguments it ran with, by the installed binary. `None` when there was
/// nothing to restore; otherwise the PID or the spawn error.
pub fn restore_stopped_proxy(
    report: &ipc::process::ServiceStopReport,
    autostart_restarted: bool,
) -> Option<std::io::Result<u32>> {
    let args = ipc::process::proxy_respawn_args(&report.stopped, autostart_restarted)?;
    let binary = crate::core::portable_binary::resolve_portable_binary();
    let mut cmd = Command::new(binary);
    cmd.args(&args);
    Some(
        ipc::process::spawn_detached(&mut cmd, ipc::process::DetachedStderr::Null)
            .map(|child| child.id()),
    )
}

/// Cleanup PID file and IPC endpoint on shutdown.
pub fn cleanup_daemon_files() {
    remove_daemon_record();
    ipc::cleanup(&daemon_addr());
}

#[cfg(test)]
mod tests {
    /// #2176: a stale PID file on `restart` read as an alarm ("is not
    /// running. Cleaning up stale files."); it is a neutral note now.
    #[test]
    fn stale_pid_message_is_neutral() {
        let line = super::stale_pid_message(36492);
        assert_eq!(
            line,
            "No daemon was running (stale PID file for PID 36492 removed)."
        );
        assert!(!line.contains("Cleaning up"));
    }
}
