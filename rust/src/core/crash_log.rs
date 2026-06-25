//! Persistent crash log for panic diagnosability.
//!
//! Motivated by upstream issue #378: 38 SIGABRT coredumps from a stripped
//! release binary left users (and us) with zero actionable data, because the
//! panic hook only printed to stderr — which is lost for daemon, `LaunchAgent`
//! and MCP-child processes.
//!
//! Every panic now also appends a structured entry (timestamp, version, thread,
//! location, payload, backtrace) to `~/.lean-ctx/logs/crash.log` so the *first*
//! panic of a panic→abort cascade is always recoverable after the fact.
//!
//! Constraints: the hook must never panic itself, never block meaningfully,
//! and must not leak content — the log is written with owner-only permissions.

use std::io::Write;
use std::path::{Path, PathBuf};

/// Rotate the log once it exceeds this size; one previous generation is kept.
const MAX_LOG_BYTES: u64 = 1024 * 1024;

fn crash_log_dir() -> Option<PathBuf> {
    let dir = crate::core::data_dir::lean_ctx_data_dir()
        .ok()?
        .join("logs");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

fn rotate_if_oversized(path: &Path) {
    let oversized = std::fs::metadata(path).is_ok_and(|m| m.len() > MAX_LOG_BYTES);
    if oversized {
        let _ = std::fs::rename(path, path.with_extension("log.1"));
    }
}

fn panic_payload(info: &std::panic::PanicHookInfo<'_>) -> String {
    if let Some(msg) = info.payload().downcast_ref::<&str>() {
        (*msg).to_string()
    } else if let Some(msg) = info.payload().downcast_ref::<String>() {
        msg.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
}

/// Appends one structured entry to `<dir>/crash.log`. Best-effort by design:
/// a failing crash logger must never make a crash worse.
fn write_entry_to(dir: &Path, location: &str, thread: &str, payload: &str) -> Option<PathBuf> {
    let path = dir.join("crash.log");
    rotate_if_oversized(&path);

    let backtrace = std::backtrace::Backtrace::force_capture();
    let entry = format!(
        "=== panic at {} (lean-ctx v{}) ===\nthread: {thread}\nlocation: {location}\npayload: {payload}\nbacktrace:\n{backtrace}\n\n",
        chrono::Utc::now().to_rfc3339(),
        env!("CARGO_PKG_VERSION"),
    );

    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(&path).ok()?;
    f.write_all(entry.as_bytes()).ok()?;
    Some(path)
}

/// Appends a structured crash entry for a live panic. Returns the log path on
/// success so the stderr message can point at it.
pub fn write_crash_entry(info: &std::panic::PanicHookInfo<'_>) -> Option<PathBuf> {
    let dir = crash_log_dir()?;
    let location = info
        .location()
        .map_or_else(|| "<unknown>".to_string(), ToString::to_string);
    let thread = std::thread::current()
        .name()
        .unwrap_or("<unnamed>")
        .to_string();
    write_entry_to(&dir, &location, &thread, &panic_payload(info))
}

/// Installs the process-wide panic hook: persistent crash log + the friendly
/// stderr message. Used by the binary entry point (CLI, MCP server, proxy and
/// daemon all run through it).
///
/// The hook itself must be panic-free: a panic inside the panic hook is a
/// double panic and the runtime `abort()`s the whole process. `eprintln!`
/// panics on I/O errors — background workers whose stderr is gone (terminal
/// closed, parent recycled the pipe → EPIPE) turned every ordinary panic into
/// a SIGABRT coredump (GitHub #378: 38 cores, all `abort` in the panic path).
/// Everything here is therefore best-effort writes + `catch_unwind`.
pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        use std::io::Write;

        let log_path =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| write_crash_entry(info)))
                .unwrap_or_default();

        let mut msg = String::from(
            "lean-ctx: unexpected error (your command was not affected)\n\
             \x20 Disable temporarily: lean-ctx-off\n\
             \x20 Full uninstall:      lean-ctx uninstall\n",
        );
        if let Some(m) = info.payload().downcast_ref::<&str>() {
            let _ = std::fmt::Write::write_fmt(&mut msg, format_args!("  Details: {m}\n"));
        } else if let Some(m) = info.payload().downcast_ref::<String>() {
            let _ = std::fmt::Write::write_fmt(&mut msg, format_args!("  Details: {m}\n"));
        }
        if let Some(loc) = info.location() {
            let _ = std::fmt::Write::write_fmt(
                &mut msg,
                format_args!("  Location: {}:{}\n", loc.file(), loc.line()),
            );
        }
        if let Some(p) = log_path {
            let _ = std::fmt::Write::write_fmt(
                &mut msg,
                format_args!("  Crash log: {}\n", p.display()),
            );
        }
        // Non-panicking stderr write — ignore EPIPE and friends.
        let _ = std::io::stderr().write_all(msg.as_bytes());
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_contains_thread_location_payload_and_backtrace() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_entry_to(tmp.path(), "src/foo.rs:42:7", "worker-3", "boom")
            .expect("entry written");
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("thread: worker-3"));
        assert!(content.contains("location: src/foo.rs:42:7"));
        assert!(content.contains("payload: boom"));
        assert!(content.contains("backtrace:"));
        assert!(content.contains(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn entries_append_and_rotate_when_oversized() {
        let tmp = tempfile::tempdir().unwrap();
        let p1 = write_entry_to(tmp.path(), "a.rs:1:1", "t", "first").unwrap();
        let p2 = write_entry_to(tmp.path(), "b.rs:2:2", "t", "second").unwrap();
        assert_eq!(p1, p2);
        let content = std::fs::read_to_string(&p1).unwrap();
        assert!(content.contains("first") && content.contains("second"));

        // Inflate beyond the cap → next write rotates.
        std::fs::write(&p1, vec![b'x'; (MAX_LOG_BYTES + 1) as usize]).unwrap();
        let p3 = write_entry_to(tmp.path(), "c.rs:3:3", "t", "third").unwrap();
        assert!(p3.with_extension("log.1").exists());
        let fresh = std::fs::read_to_string(&p3).unwrap();
        assert!(fresh.contains("third") && !fresh.contains("xxxx"));
    }

    #[cfg(unix)]
    #[test]
    fn crash_log_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let path = write_entry_to(tmp.path(), "a.rs:1:1", "t", "perm-check").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o077,
            0,
            "crash.log must not be group/world readable"
        );
    }
}
