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
    dirs::data_local_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".local/share"))
        .join("lean-ctx")
}

#[must_use]
pub fn daemon_pid_path() -> PathBuf {
    data_dir().join("daemon.pid")
}

#[must_use]
pub fn daemon_addr() -> ipc::DaemonAddr {
    ipc::DaemonAddr::default_for_current_os()
}

#[must_use]
pub fn is_daemon_running() -> bool {
    let pid_path = daemon_pid_path();
    let Ok(contents) = fs::read_to_string(&pid_path) else {
        return false;
    };
    let Ok(pid) = contents.trim().parse::<u32>() else {
        return false;
    };
    if ipc::process::is_alive(pid) {
        return true;
    }
    let _ = fs::remove_file(&pid_path);
    ipc::cleanup(&daemon_addr());
    false
}

#[must_use]
pub fn read_daemon_pid() -> Option<u32> {
    let contents = fs::read_to_string(daemon_pid_path()).ok()?;
    contents.trim().parse::<u32>().ok()
}

/// Exclusive, bounded-wait lock that serializes the daemon-start critical
/// section (liveness check → spawn → PID write). Several MCP servers launching
/// at once (Claude Code + `OpenCode` + Cursor) would otherwise all pass the
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
    // Held for the whole critical section; released when `_start_lock` drops.
    let _start_lock = acquire_start_lock();

    if is_daemon_running() {
        let pid = read_daemon_pid().unwrap_or(0);
        anyhow::bail!("Daemon already running (PID {pid}). Use --stop to stop it first.");
    }

    ipc::cleanup(&daemon_addr());

    if let Ok(data_dir) = crate::core::data_dir::lean_ctx_data_dir() {
        crate::config_io::cleanup_legacy_backups(&data_dir);
    }

    let exe_str = crate::core::portable_binary::resolve_portable_binary();
    let exe = std::path::PathBuf::from(&exe_str);

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
        Ok(f) => std::process::Stdio::from(f),
        Err(_) => std::process::Stdio::inherit(),
    };

    let mut cmd = Command::new(&exe);
    cmd.args(&cmd_args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(stderr_cfg);
    // #356: if the *spawner* is a launchd-standalone process (e.g. the proxy
    // auto-starting the daemon), the spawned daemon's ppid is the spawner — not
    // 1 — so `getppid()`-based detection would miss it and the daemon's TCC path
    // guards would stay off. Propagate the marker explicitly so the daemon
    // treats itself as standalone regardless of where it sits in the tree.
    if crate::core::pathutil::process_is_tcc_standalone() {
        cmd.env("LEAN_CTX_TCC_STANDALONE", "1");
    }
    // Detached spawn: on Windows the daemon must escape the parent's
    // console/Job so it survives AI-client MCP process recycling (GL #545).
    let child = ipc::process::spawn_detached(&mut cmd)
        .with_context(|| format!("failed to spawn daemon: {}", exe.display()))?;

    let pid = child.id();
    write_pid_file(pid)?;

    std::thread::sleep(std::time::Duration::from_millis(200));

    if !ipc::process::is_alive(pid) {
        let _ = fs::remove_file(daemon_pid_path());
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

pub fn stop_daemon() -> Result<()> {
    let pid_path = daemon_pid_path();

    let Some(pid) = read_daemon_pid() else {
        eprintln!("No daemon PID file found. Nothing to stop.");
        return Ok(());
    };

    if !ipc::process::is_alive(pid) {
        eprintln!("Daemon (PID {pid}) is not running. Cleaning up stale files.");
        ipc::cleanup(&daemon_addr());
        let _ = fs::remove_file(&pid_path);
        return Ok(());
    }

    let http_shutdown_ok = try_http_shutdown();

    if http_shutdown_ok {
        for _ in 0..30 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if !ipc::process::is_alive(pid) {
                break;
            }
        }
    }

    if ipc::process::is_alive(pid) {
        let _ = ipc::process::terminate_gracefully(pid);
        for _ in 0..20 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if !ipc::process::is_alive(pid) {
                break;
            }
        }
    }

    if ipc::process::is_alive(pid) {
        eprintln!("Daemon (PID {pid}) did not stop gracefully, force killing.");
        let _ = ipc::process::force_kill(pid);
        std::thread::sleep(std::time::Duration::from_millis(200));
    }

    let _ = fs::remove_file(&pid_path);
    ipc::cleanup(&daemon_addr());
    eprintln!("lean-ctx daemon stopped (PID {pid}).");

    let orphans = ipc::process::find_pids_by_name("lean-ctx");
    if !orphans.is_empty() {
        eprintln!("  Cleaning up {} orphan process(es)…", orphans.len());
        let _ = ipc::process::kill_all_by_name("lean-ctx");
    }

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

#[must_use]
pub fn daemon_status() -> String {
    let addr = daemon_addr();
    if let Some(pid) = read_daemon_pid() {
        if ipc::process::is_alive(pid) {
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
    let pid = std::process::id();
    write_pid_file(pid)?;
    Ok(())
}

/// Cleanup PID file and IPC endpoint on shutdown.
pub fn cleanup_daemon_files() {
    let _ = fs::remove_file(daemon_pid_path());
    ipc::cleanup(&daemon_addr());
}
