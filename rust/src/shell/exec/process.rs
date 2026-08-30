use std::io::Read;
use std::process::{Child, Output};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Wait for a child process with output-size and time limits.
/// Kills the process if either limit is exceeded, returning what was
/// captured so far. Prevents unbounded memory growth on commands that
/// produce massive output (e.g. `rg -i "pattern"` over a large tree).
///
/// `kill_group` (Unix): the child was spawned into its own process group
/// (`process_group(0)`), so a timeout kill signals the whole group. Killing
/// only the direct child (a shell) leaves orphaned grandchildren holding the
/// stdout/stderr pipe write ends — the reader threads then never see EOF and
/// the join below blocks forever, wedging the caller *despite* the timeout
/// having fired (GH #720: an orphaned `rg` kept a Cursor shell session dead
/// for hours).
pub(crate) fn wait_with_limits(
    mut child: Child,
    max_bytes: usize,
    timeout: std::time::Duration,
    kill_group: bool,
) -> Output {
    const STDERR_LIMIT: usize = 512 * 1024;

    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let start = std::time::Instant::now();
    let truncated = Arc::new(AtomicBool::new(false));

    let stdout_truncated_flag = Arc::clone(&truncated);
    let stdout_handle = std::thread::spawn(move || {
        let Some(mut pipe) = stdout_pipe else {
            return (Vec::new(), false);
        };
        let mut buf = Vec::with_capacity(max_bytes.min(64 * 1024));
        let mut chunk = [0u8; 8192];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    if buf.len() + n > max_bytes {
                        let remaining = max_bytes.saturating_sub(buf.len());
                        buf.extend_from_slice(&chunk[..remaining]);
                        stdout_truncated_flag.store(true, Ordering::Relaxed);
                        return (buf, true);
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        (buf, false)
    });

    let stderr_truncated_flag = Arc::clone(&truncated);
    let stderr_handle = std::thread::spawn(move || {
        let Some(mut pipe) = stderr_pipe else {
            return (Vec::new(), false);
        };
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    if buf.len() + n > STDERR_LIMIT {
                        let remaining = STDERR_LIMIT.saturating_sub(buf.len());
                        buf.extend_from_slice(&chunk[..remaining]);
                        stderr_truncated_flag.store(true, Ordering::Relaxed);
                        return (buf, true);
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        (buf, false)
    });

    let mut timed_out = false;
    loop {
        let hit_timeout = start.elapsed() > timeout;
        if hit_timeout || truncated.load(Ordering::Relaxed) {
            kill_child(&mut child, kill_group);
            let _ = child.wait();
            timed_out = hit_timeout;
            break;
        }
        match child.try_wait() {
            Ok(Some(_)) => {
                if kill_group && (!stdout_handle.is_finished() || !stderr_handle.is_finished()) {
                    kill_child(&mut child, true);
                }
                break;
            }
            Err(_) => {
                kill_child(&mut child, kill_group);
                break;
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(50)),
        }
    }

    let (mut stdout_buf, stdout_truncated) = stdout_handle.join().unwrap_or_default();
    let (mut stderr_buf, stderr_truncated) = stderr_handle.join().unwrap_or_default();

    if timed_out || stdout_truncated {
        let notice = format!(
            "\n[lean-ctx: output truncated at {} MB / {}s limit]\n",
            max_bytes / (1024 * 1024),
            timeout.as_secs()
        );
        stdout_buf.extend_from_slice(notice.as_bytes());
    }
    if stderr_truncated {
        let notice = format!(
            "\n[lean-ctx: stderr truncated at {} KB limit]\n",
            STDERR_LIMIT / 1024
        );
        stderr_buf.extend_from_slice(notice.as_bytes());
    }

    let status = child.wait().unwrap_or_else(|_| synthetic_failure_status());

    Output {
        status,
        stdout: stdout_buf,
        stderr: stderr_buf,
    }
}

/// Kill a timed-out child — and, when it owns a process group, every
/// descendant in that group (GH #720). SIGKILL to the negative pgid reaps
/// shells' grandchildren so the captured pipes actually close.
fn kill_child(child: &mut Child, kill_group: bool) {
    #[cfg(unix)]
    if kill_group {
        // Agent Tools children inherit the Engine's dedicated process group;
        // ordinary ctx_shell children create their own. Resolve the actual
        // group so either topology terminates atomically.
        // SAFETY: getpgid only reads kernel process metadata for this live PID.
        let pgid = unsafe { libc::getpgid(child.id() as libc::pid_t) };
        if pgid > 0 {
            // SAFETY: plain syscall; a stale pgid at worst returns ESRCH.
            unsafe { libc::killpg(pgid, libc::SIGKILL) };
        }
    }
    #[cfg(not(unix))]
    let _ = kill_group;
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/T", "/PID", &child.id().to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    let _ = child.kill();
}

/// A synthetic failed `ExitStatus`, used only when `Child::wait()` itself
/// errors (e.g. the process was already reaped by another waiter) and there
/// is no real status to report. The previous fallback shelled out to
/// `Command::new("false").status()` to manufacture one, which panicked via
/// `.expect()` wherever no `false` binary exists on `PATH` — Windows, and
/// minimal/scratch containers. `ExitStatusExt::from_raw` builds the status
/// value directly, with no subprocess involved, so it can't fail.
#[cfg(unix)]
fn synthetic_failure_status() -> std::process::ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    // Raw wait(2) status encoding: low 7 bits 0 signals a normal exit
    // (`WIFEXITED`), the next byte up is the exit code (`WEXITSTATUS`) — so
    // `1 << 8` decodes as "exited normally with code 1".
    std::process::ExitStatus::from_raw(1 << 8)
}

#[cfg(not(unix))]
fn synthetic_failure_status() -> std::process::ExitStatus {
    use std::os::windows::process::ExitStatusExt;
    std::process::ExitStatus::from_raw(1)
}

#[cfg(test)]
mod tests {
    #[test]
    fn wait_with_limits_captures_output() {
        let child = std::process::Command::new("echo")
            .arg("hello")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();

        let output = super::wait_with_limits(child, 1024, std::time::Duration::from_secs(5), false);
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("hello"),
            "expected 'hello' in output: {stdout}"
        );
        assert!(output.status.success());
    }

    #[test]
    fn wait_with_limits_truncates_large_output() {
        // Generate ~100 KB of output, limit to 1 KB
        let child = std::process::Command::new("sh")
            .args(["-c", "yes 'aaaa' | head -25000"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();

        let output =
            super::wait_with_limits(child, 1024, std::time::Duration::from_secs(10), false);
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("[lean-ctx: output truncated"),
            "expected truncation notice, got len={}: ...{}",
            stdout.len(),
            &stdout[stdout.len().saturating_sub(80)..]
        );
    }

    #[test]
    fn synthetic_failure_status_is_a_failure_without_spawning_anything() {
        let status = super::synthetic_failure_status();
        assert!(!status.success());
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            assert_eq!(status.code(), Some(1));
            assert_eq!(status.signal(), None);
        }
    }

    #[test]
    fn wait_with_limits_truncates_large_stderr() {
        let child = std::process::Command::new("sh")
            .args(["-c", "yes 'aaaaaaaaaa' | head -200000 >&2"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();

        let output = super::wait_with_limits(
            child,
            1024 * 1024,
            std::time::Duration::from_secs(10),
            false,
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("[lean-ctx: stderr truncated"),
            "expected stderr truncation notice, got len={}: ...{}",
            stderr.len(),
            &stderr[stderr.len().saturating_sub(80)..]
        );
    }

    #[test]
    fn wait_with_limits_kills_promptly_on_truncation() {
        let child = std::process::Command::new("yes")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();

        let start = std::time::Instant::now();
        let output =
            super::wait_with_limits(child, 4096, std::time::Duration::from_secs(20), false);
        let elapsed = start.elapsed();

        assert!(
            elapsed < std::time::Duration::from_secs(3),
            "truncation should kill promptly, took {elapsed:?} (timeout was 20s)"
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("[lean-ctx: output truncated"));
    }

    #[test]
    fn wait_with_limits_timeout_kills_process() {
        let child = std::process::Command::new("sleep")
            .arg("60")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();

        let start = std::time::Instant::now();
        let output =
            super::wait_with_limits(child, 1024, std::time::Duration::from_millis(200), false);
        let elapsed = start.elapsed();

        assert!(
            elapsed < std::time::Duration::from_secs(3),
            "timeout should kill quickly, took {elapsed:?}"
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("[lean-ctx: output truncated"));
    }

    /// GH #720: killing only the direct child (a shell) on timeout leaves its
    /// grandchildren alive holding the stdout pipe — the reader threads never
    /// see EOF and `wait_with_limits` blocks forever even though the timeout
    /// fired. With the child in its own process group and a group kill, the
    /// whole tree dies and the call returns promptly.
    #[cfg(unix)]
    #[test]
    fn wait_with_limits_group_kill_reaps_grandchildren() {
        use std::os::unix::process::CommandExt as _;
        // The shell spawns a grandchild that inherits stdout and sleeps far
        // beyond the timeout; the shell itself also sleeps so the timeout path
        // (not natural exit) is exercised.
        let mut cmd = std::process::Command::new("sh");
        cmd.args(["-c", "sleep 30 & sleep 30"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        cmd.process_group(0);
        let child = cmd.spawn().unwrap();
        let pgid = child.id() as libc::pid_t;

        let start = std::time::Instant::now();
        let _ = super::wait_with_limits(child, 1024, std::time::Duration::from_millis(200), true);
        let elapsed = start.elapsed();

        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "group kill must unblock the reader threads, took {elapsed:?}"
        );
        // The whole group must be gone (ESRCH), not just the direct child.
        // A brief grace period lets the kernel finish reaping.
        let mut group_gone = false;
        for _ in 0..50 {
            // SAFETY: signal 0 only probes for existence.
            if unsafe { libc::killpg(pgid, 0) } == -1 {
                group_gone = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(group_gone, "process group {pgid} must be fully reaped");
    }

    /// #806: piped stdin must be forwarded to the child via Stdio::piped()
    /// and relayed, not nulled. Tests the relay pattern: write data to child
    /// stdin, close it (EOF), child reads and exits.
    #[cfg(unix)]
    #[test]
    fn stdin_relay_forwards_piped_data() {
        use std::io::Write;
        use std::os::unix::process::CommandExt as _;

        let mut cmd = std::process::Command::new("cat");
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        cmd.process_group(0);
        let mut child = cmd.spawn().expect("failed to spawn cat");

        let mut child_stdin = child.stdin.take().unwrap();
        std::thread::spawn(move || {
            child_stdin.write_all(b"hello from pipe\n").unwrap();
            drop(child_stdin);
        });

        let output = child.wait_with_output().expect("wait failed");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("hello from pipe"),
            "#806: piped stdin must reach the child, got: {stdout}"
        );
    }

    /// #806: commands that don't read stdin must still work normally
    /// when no data is piped (relay thread sees immediate EOF from parent).
    #[cfg(unix)]
    #[test]
    fn stdin_relay_no_data_does_not_hang() {
        let start = std::time::Instant::now();
        let mut cmd = std::process::Command::new("sh");
        cmd.args(["-c", "echo ok"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        use std::os::unix::process::CommandExt as _;
        cmd.process_group(0);
        let mut child = cmd.spawn().unwrap();
        // Close stdin immediately (simulates relay with empty parent pipe)
        drop(child.stdin.take());
        let output = child.wait_with_output().unwrap();
        let elapsed = start.elapsed();
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "must not hang when stdin is closed immediately, took {elapsed:?}"
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("ok"), "command output missing: {stdout}");
    }
}
