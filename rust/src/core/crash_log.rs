//! Persistent crash log for panic diagnosability.
//!
//! Motivated by upstream issue #378: 38 SIGABRT coredumps from a stripped
//! release binary left users (and us) with zero actionable data, because the
//! panic hook only printed to stderr — which is lost for daemon, LaunchAgent
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
    // A panic message can quote the value it failed on (a parsed header, a
    // config line, a request fragment): it gets the same redaction as every
    // other log line before it reaches disk.
    let payload = crate::core::redaction::redact_text(payload);

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

/// True when a panic payload describes a peer that closed the pipe.
///
/// A host closing stdout/stderr is normal for an MCP server — a restart, an
/// aborted tool call, an IDE reload — and the resulting write panic is not a
/// crash worth logging. The wording is platform-specific, which is exactly what
/// #1781 exposed: the original guard matched only the POSIX phrasing, so every
/// Windows instance recorded these as crashes (ten-plus entries in one report).
///
/// - POSIX: `Broken pipe`
/// - Windows: `The pipe is being closed. (os error 232)` — `ERROR_NO_DATA`
/// - Windows: `os error 109` — `ERROR_BROKEN_PIPE`
pub(crate) fn is_broken_pipe_payload(payload: &str) -> bool {
    payload.contains("Broken pipe")
        || payload.contains("The pipe is being closed")
        || payload.contains("os error 232")
        || payload.contains("os error 109")
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
/// Everything here is therefore best-effort writes + catch_unwind.
pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        use std::io::Write;

        // #826/#1781: a peer closing the pipe is normal; don't log it as a crash.
        let payload = info
            .payload()
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| info.payload().downcast_ref::<&str>().copied())
            .unwrap_or_default();
        if is_broken_pipe_payload(payload) {
            return;
        }

        let log_path =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| write_crash_entry(info)))
                .unwrap_or_default();

        let mut msg = String::from(
            "lean-ctx: unexpected error (your command was not affected)\n\
             \x20 Disable temporarily: lean-ctx-off\n\
             \x20 Full uninstall:      lean-ctx uninstall\n",
        );
        if !payload.is_empty() {
            // stderr of a daemon or LaunchAgent is often a file: redacted too.
            let details = std::panic::catch_unwind(|| crate::core::redaction::redact_text(payload))
                .unwrap_or_default();
            let _ = std::fmt::Write::write_fmt(&mut msg, format_args!("  Details: {details}\n"));
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

    /// #1781: the guard matched only the POSIX wording, so every Windows
    /// instance logged a routine pipe close as a crash — ten-plus entries in
    /// the report that prompted this. The Windows payloads cannot be produced
    /// on this platform, so the wordings are pinned here instead.
    #[test]
    fn broken_pipe_payloads_are_recognised_on_every_platform() {
        assert!(is_broken_pipe_payload(
            "failed printing to stderr: Broken pipe (os error 32)"
        ));
        assert!(is_broken_pipe_payload(
            "failed printing to stderr: The pipe is being closed. (os error 232)"
        ));
        assert!(is_broken_pipe_payload("write failed: os error 109"));
    }

    #[test]
    fn unrelated_panics_are_still_logged_as_crashes() {
        assert!(!is_broken_pipe_payload(
            "index out of bounds: the len is 3 but the index is 7"
        ));
        assert!(!is_broken_pipe_payload(
            "called `Option::unwrap()` on a `None` value"
        ));
        // A different OS error must not be swallowed just because it is an OS error.
        assert!(!is_broken_pipe_payload("permission denied (os error 13)"));
    }

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

    /// E07: the payload was written raw, so a panic quoting a credential put
    /// it in `logs/crash.log`; it is redacted like every other log line.
    #[test]
    fn a_credential_in_the_panic_payload_never_reaches_the_crash_log() {
        let tmp = tempfile::tempdir().unwrap();
        let key = concat!("AK", "IAIOSFODNN7EXAMPLE");
        let path = write_entry_to(
            tmp.path(),
            "a.rs:1:1",
            "t",
            &format!("invalid header: aws_access_key_id={key}"),
        )
        .unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("payload: invalid header"), "{content}");
        assert!(!content.contains(key), "{content}");
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
