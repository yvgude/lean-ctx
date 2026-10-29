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
pub(crate) struct LimitedProcessOutput {
    pub output: Output,
    pub completeness: crate::core::context_admission::OutputCompleteness,
}

/// The output alone, for tests. Product callers take the completeness with
/// it (`wait_with_limits_captured`): an incomplete capture is a decision input.
#[cfg(test)]
pub(crate) fn wait_with_limits(
    child: Child,
    max_bytes: usize,
    timeout: std::time::Duration,
    kill_group: bool,
) -> Output {
    wait_with_limits_captured(child, max_bytes, timeout, kill_group).output
}

pub(crate) fn wait_with_limits_captured(
    mut child: Child,
    max_bytes: usize,
    timeout: std::time::Duration,
    kill_group: bool,
) -> LimitedProcessOutput {
    const STDERR_LIMIT: usize = 512 * 1024;
    /// How long the readers may take to see end-of-file after the child
    /// exited before its own group is signalled.
    const DRAIN_GRACE: std::time::Duration = std::time::Duration::from_millis(250);
    /// How long the readers may take to finish once the child is gone.
    const READER_BUDGET: std::time::Duration = std::time::Duration::from_secs(2);
    /// How long an exited or killed child may take to become reapable.
    const REAP_BUDGET: std::time::Duration = std::time::Duration::from_secs(2);

    // Decided once, before anything is reaped: only a group the child leads
    // itself is ever signalled. A child that inherited its group (the
    // caller's, the host's) is killed alone.
    let own_group = owned_group(&mut child, kill_group);

    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let start = std::time::Instant::now();
    let truncated = Arc::new(AtomicBool::new(false));
    let read_failed = Arc::new(AtomicBool::new(false));

    // Each reader appends what it reads to a buffer this thread can read at
    // any time: a reader that is abandoned below still hands over everything
    // it had read by then.
    let stdout_capture = Arc::new(StreamCapture::default());
    let stderr_capture = Arc::new(StreamCapture::default());
    let stdout_handle = std::thread::spawn({
        let (capture, truncated, read_failed) = (
            Arc::clone(&stdout_capture),
            Arc::clone(&truncated),
            Arc::clone(&read_failed),
        );
        move || read_stream(stdout_pipe, max_bytes, &capture, &truncated, &read_failed)
    });
    let stderr_handle = std::thread::spawn({
        let (capture, truncated, read_failed) = (
            Arc::clone(&stderr_capture),
            Arc::clone(&truncated),
            Arc::clone(&read_failed),
        );
        move || {
            read_stream(
                stderr_pipe,
                STDERR_LIMIT,
                &capture,
                &truncated,
                &read_failed,
            );
        }
    });

    let mut timed_out = false;
    let mut wait_failed = false;
    let mut kill_failed = false;
    let mut forced_pipe_cleanup = false;
    loop {
        let hit_timeout = start.elapsed() > timeout;
        if hit_timeout || truncated.load(Ordering::Relaxed) {
            // Not reaped yet: the leader still pins its group id.
            kill_failed |= !kill_child(&mut child, own_group);
            timed_out = hit_timeout;
            break;
        }
        match exited_unreaped(&mut child) {
            Ok(true) => {
                // The leader has exited but is deliberately not reaped here:
                // as a zombie it keeps its pid, and with it the id of its own
                // group, from being reused while descendants that still hold
                // the pipes are signalled.
                if own_group.is_some()
                    && !readers_finish_within(&stdout_handle, &stderr_handle, DRAIN_GRACE)
                {
                    kill_failed |= !kill_child(&mut child, own_group);
                    forced_pipe_cleanup = true;
                }
                break;
            }
            Err(_) => {
                wait_failed = true;
                kill_failed |= !kill_child(&mut child, own_group);
                break;
            }
            Ok(false) => std::thread::sleep(std::time::Duration::from_millis(50)),
        }
    }

    // The child has exited or was killed above; reaping it is bounded all the
    // same. A child that cannot be reaped (a kill that did not take effect)
    // must not hold this call: it is reported as an incomplete capture with a
    // failure status instead.
    let reaped = reap_within(&mut child, REAP_BUDGET);

    // A descendant outside any group this call owns may keep a pipe open for
    // as long as it likes. The readers are given a budget; a reader that does
    // not finish is left behind — never joined, never waited for again — and
    // the capture is reported as incomplete. What it had read up to this
    // moment is still returned: on each stream a prefix of what was written
    // to it, within that stream's limit.
    let readers_finished = readers_finish_within(&stdout_handle, &stderr_handle, READER_BUDGET);
    let reader_join_failed =
        !readers_finished || stdout_handle.join().is_err() || stderr_handle.join().is_err();
    let (mut stdout_buf, stdout_truncated) = stdout_capture.take();
    let (mut stderr_buf, stderr_truncated) = stderr_capture.take();
    let decode_lossy =
        std::str::from_utf8(&stdout_buf).is_err() || std::str::from_utf8(&stderr_buf).is_err();
    let reader_incomplete = wait_failed
        || kill_failed
        || reaped.is_none()
        || forced_pipe_cleanup
        || read_failed.load(Ordering::Relaxed)
        || reader_join_failed;

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
    if !readers_finished {
        stdout_buf.extend_from_slice(
            b"\n[lean-ctx: output not captured: a process outside this call kept it open]\n",
        );
    }

    if reaped.is_none() {
        stdout_buf.extend_from_slice(
            b"\n[lean-ctx: the process could not be ended within the cleanup budget]\n",
        );
    }
    let status = reaped.unwrap_or_else(synthetic_failure_status);

    LimitedProcessOutput {
        output: Output {
            status,
            stdout: stdout_buf,
            stderr: stderr_buf,
        },
        completeness: crate::core::context_admission::OutputCompleteness {
            truncated: stdout_truncated || stderr_truncated || truncated.load(Ordering::Relaxed),
            timed_out,
            reader_incomplete,
            cancelled: false,
            decode_lossy,
        },
    }
}

/// What one reader has read of its stream so far, shared with the waiting
/// thread: the bytes (never more than the stream's limit) and whether the
/// limit cut them short.
#[derive(Default)]
struct StreamCapture {
    bytes: std::sync::Mutex<Vec<u8>>,
    truncated: AtomicBool,
}

impl StreamCapture {
    /// The bytes read up to now and whether the limit was reached. A reader
    /// that is still running keeps its lock only for one append, so this
    /// never waits on a pipe.
    fn take(&self) -> (Vec<u8>, bool) {
        let bytes = std::mem::take(
            &mut *self
                .bytes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        (bytes, self.truncated.load(Ordering::Relaxed))
    }
}

/// Reads `pipe` to its end or to `limit` bytes into `capture`, chunk by
/// chunk, so that the capture holds at every moment a prefix of the stream.
/// `any_truncated` tells the waiting thread to end the child.
fn read_stream(
    pipe: Option<impl Read>,
    limit: usize,
    capture: &StreamCapture,
    any_truncated: &AtomicBool,
    read_failed: &AtomicBool,
) {
    let Some(mut pipe) = pipe else {
        read_failed.store(true, Ordering::Relaxed);
        return;
    };
    let mut chunk = [0u8; 8192];
    loop {
        match pipe.read(&mut chunk) {
            Ok(0) => return,
            Ok(n) => {
                let mut bytes = capture
                    .bytes
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if bytes.len() + n > limit {
                    let remaining = limit.saturating_sub(bytes.len());
                    bytes.extend_from_slice(&chunk[..remaining]);
                    capture.truncated.store(true, Ordering::Relaxed);
                    any_truncated.store(true, Ordering::Relaxed);
                    return;
                }
                bytes.extend_from_slice(&chunk[..n]);
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => {
                read_failed.store(true, Ordering::Relaxed);
                return;
            }
        }
    }
}

/// The process group this call may signal: the child's own, and only when the
/// child leads it (it was spawned with `process_group(0)`). Read while the
/// child is certainly not reaped, so the answer is about this child.
///
/// A child that has already exited cannot be asked on every system (macOS
/// reports no group for a zombie). Its unreaped pid still names it alone, and
/// a group with that number is either the one it led or does not exist: no
/// other process can have created a group under a pid that is still taken.
/// So the number is safe to signal, and a call whose leader finished before
/// the wait began still ends what it left behind.
#[cfg(unix)]
fn owned_group(child: &mut Child, kill_group: bool) -> Option<i32> {
    if !kill_group {
        return None;
    }
    let pid = child.id() as libc::pid_t;
    // SAFETY: getpgid only reads kernel process metadata; the pid is ours and
    // unreaped (alive or zombie), so it cannot name another process.
    let group = unsafe { libc::getpgid(pid) };
    if group == pid {
        return Some(pid);
    }
    (group == -1 && matches!(exited_unreaped(child), Ok(true))).then_some(pid)
}

#[cfg(not(unix))]
fn owned_group(_: &mut Child, _: bool) -> Option<i32> {
    None
}

/// Whether the child has exited, WITHOUT reaping it on Unix (`WNOWAIT`).
#[cfg(unix)]
pub(crate) fn exited_unreaped(child: &mut Child) -> std::io::Result<bool> {
    // SAFETY: an all-zero siginfo_t is a valid out-parameter for waitid.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // SAFETY: waitid on our own unreaped child; WNOWAIT leaves it waitable.
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            child.id() as libc::id_t,
            &raw mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result == -1 {
        let error = std::io::Error::last_os_error();
        return if error.kind() == std::io::ErrorKind::Interrupted {
            Ok(false)
        } else {
            Err(error)
        };
    }
    // SAFETY: waitid succeeded; with WNOHANG si_pid stays 0 while running.
    Ok(unsafe { info.si_pid() } != 0)
}

#[cfg(not(unix))]
fn exited_unreaped(child: &mut Child) -> std::io::Result<bool> {
    child.try_wait().map(|status| status.is_some())
}

fn readers_finish_within<A, B>(
    first: &std::thread::JoinHandle<A>,
    second: &std::thread::JoinHandle<B>,
    budget: std::time::Duration,
) -> bool {
    let deadline = std::time::Instant::now() + budget;
    loop {
        if first.is_finished() && second.is_finished() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// Kill the child — and, when it leads its own process group, every
/// descendant in that group (GH #720). SIGKILL to the group ends a shell's
/// grandchildren so the captured pipes actually close. A group the child
/// merely inherited is never signalled: that would end the caller and its
/// unrelated siblings.
///
/// Returns `false` when the platform's tree kill did not confirm (Windows:
/// `taskkill` failed, or did not finish within its own budget). The direct
/// child is killed in every case.
fn kill_child(child: &mut Child, own_group: Option<i32>) -> bool {
    #[cfg(unix)]
    if let Some(group) = own_group {
        // SAFETY: plain syscall on a group id pinned by our unreaped child.
        unsafe { libc::killpg(group, libc::SIGKILL) };
    }
    #[cfg(not(unix))]
    let _ = own_group;
    #[cfg(windows)]
    let tree_killed = {
        const TREE_KILL_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);
        match std::process::Command::new("taskkill")
            .args(["/F", "/T", "/PID", &child.id().to_string()])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(mut tree_kill) => match reap_within(&mut tree_kill, TREE_KILL_BUDGET) {
                Some(status) => status.success(),
                None => {
                    let _ = tree_kill.kill();
                    let _ = reap_within(&mut tree_kill, TREE_KILL_BUDGET);
                    false
                }
            },
            Err(_) => false,
        }
    };
    #[cfg(not(windows))]
    let tree_killed = true;
    let _ = child.kill();
    tree_killed
}

/// Reap a child that has exited or was just killed, within `budget`.
pub(crate) fn reap_within(
    child: &mut Child,
    budget: std::time::Duration,
) -> Option<std::process::ExitStatus> {
    let deadline = std::time::Instant::now() + budget;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Ok(None) | Err(_) => return None,
        }
    }
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
    const OUTPUT_HELPER_ENV: &str = "LEAN_CTX_TEST_OUTPUT_HELPER";
    const OUTPUT_HELPER_TEST: &str = "shell::exec::process::tests::wait_with_limits_output_helper";

    #[test]
    #[ignore = "subprocess helper for wait_with_limits tests"]
    fn wait_with_limits_output_helper() {
        use std::io::Write as _;

        match std::env::var(OUTPUT_HELPER_ENV).as_deref() {
            Ok("stdout-small") => {
                std::io::stdout().write_all(b"hello\n").unwrap();
            }
            // A producer that never ends on its own. Once the reader drops the
            // pipe, keep the process alive until the parent kills it: exiting
            // here would race that kill against the instrumented binary's
            // at-exit coverage profile write and leave a truncated .profraw
            // that breaks the Coverage job's report.
            Ok("stdout-forever") => {
                let chunk = [b'a'; 8192];
                let mut stream = std::io::stdout().lock();
                while stream.write_all(&chunk).is_ok() {}
                std::thread::sleep(std::time::Duration::from_mins(1));
            }
            Ok("stderr-forever") => {
                let chunk = [b'a'; 8192];
                let mut stream = std::io::stderr().lock();
                while stream.write_all(&chunk).is_ok() {}
                std::thread::sleep(std::time::Duration::from_mins(1));
            }
            Ok("sleep") => {
                std::thread::sleep(std::time::Duration::from_mins(1));
            }
            #[cfg(unix)]
            Ok("guardian") => guardian(),
            Ok(other) => panic!("unknown output helper mode: {other}"),
            Err(error) => panic!("missing {OUTPUT_HELPER_ENV}: {error}"),
        }
    }

    fn spawn_output_helper(mode: &str) -> std::process::Child {
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([OUTPUT_HELPER_TEST, "--exact", "--ignored", "--nocapture"])
            .env(OUTPUT_HELPER_ENV, mode)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap()
    }

    #[test]
    fn wait_with_limits_captures_output() {
        let child = spawn_output_helper("stdout-small");

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
        let child = spawn_output_helper("stdout-forever");

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
        let child = spawn_output_helper("stderr-forever");

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
        let child = spawn_output_helper("stdout-forever");

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
        let child = spawn_output_helper("sleep");

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

    #[cfg(unix)]
    fn alive(pid: u32) -> bool {
        // SAFETY: signal 0 only probes for existence.
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }

    /// Body of the guardian process (its own process group, created by the
    /// test below). Everything that could be signalled by mistake lives in
    /// this group: the guardian itself and a sentinel that belongs to no call.
    #[cfg(unix)]
    fn guardian() {
        use std::os::unix::process::CommandExt as _;
        use std::process::{Command, Stdio};
        let piped = |script: &str| {
            let mut command = Command::new("sh");
            command
                .args(["-c", script])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            command
        };
        let mut sentinel = Command::new("sleep")
            .arg("60")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        // SAFETY: getpgrp has no preconditions.
        let own = unsafe { libc::getpgrp() };
        assert_eq!(own, std::process::id() as libc::pid_t, "guardian leads");
        // Whatever happens below, the fixture ends its own group — itself
        // included — while it still leads it, so nothing outlives the test
        // and no group id is signalled after it could have been reused.
        struct EndOwnGroup(libc::pid_t);
        impl Drop for EndOwnGroup {
            fn drop(&mut self) {
                use std::io::Write as _;
                let _ = std::io::stdout().flush();
                // SAFETY: this process leads the group and is alive.
                unsafe { libc::killpg(self.0, libc::SIGKILL) };
            }
        }
        let _end = EndOwnGroup(own);
        // SAFETY: reads metadata of our live child.
        assert_eq!(unsafe { libc::getpgid(sentinel.id() as libc::pid_t) }, own);

        // 1. A call that INHERITED this group runs into its timeout while a
        // grandchild holds its pipes. Only the call's direct child is killed;
        // sentinel and guardian live; the capture is bounded and incomplete.
        let child = piped("sleep 60 & sleep 60").spawn().unwrap();
        let start = std::time::Instant::now();
        let captured = super::wait_with_limits_captured(
            child,
            1024,
            std::time::Duration::from_millis(200),
            true,
        );
        assert!(start.elapsed() < std::time::Duration::from_secs(8));
        assert!(captured.completeness.timed_out);
        assert!(captured.completeness.reader_incomplete);
        assert!(
            alive(sentinel.id()),
            "inherited group must not be signalled"
        );

        // 2. The same with the output limit instead of the timeout.
        let child = piped("sleep 60 & yes").spawn().unwrap();
        let captured =
            super::wait_with_limits_captured(child, 1024, std::time::Duration::from_secs(30), true);
        assert!(captured.completeness.truncated);
        assert!(
            alive(sentinel.id()),
            "inherited group must not be signalled"
        );

        // 3. A call with its OWN group whose leader exits at once while its
        // child keeps the pipes: the group is found although the leader is
        // gone, the whole call tree ends, nothing else does.
        let mut command = piped("sleep 60 & echo started");
        command.process_group(0);
        let child = command.spawn().unwrap();
        let group = child.id() as libc::pid_t;
        let start = std::time::Instant::now();
        let captured =
            super::wait_with_limits_captured(child, 1024, std::time::Duration::from_secs(30), true);
        assert!(
            start.elapsed() < std::time::Duration::from_secs(5),
            "leader gone, pipe holder alive: must not wait for it"
        );
        assert!(!captured.completeness.timed_out);
        assert!(captured.completeness.reader_incomplete);
        assert!(String::from_utf8_lossy(&captured.output.stdout).contains("started"));
        let mut group_gone = false;
        for _ in 0..100 {
            // SAFETY: signal 0 only probes for existence.
            if unsafe { libc::killpg(group, 0) } == -1 {
                group_gone = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(group_gone, "the call's own group must be gone");
        assert!(
            alive(sentinel.id()),
            "a foreign group must not be signalled"
        );

        // 4. A command writes a line and exits, and leaves a process outside
        // any group this call owns holding its pipes (the call inherited
        // this group, so nothing is signalled). The readers are abandoned
        // after their budget; what they had read is returned, in front of
        // the notice, and the capture is flagged incomplete.
        let child = piped("echo written; echo problem >&2; sleep 60 &")
            .spawn()
            .unwrap();
        let start = std::time::Instant::now();
        let captured =
            super::wait_with_limits_captured(child, 1024, std::time::Duration::from_secs(30), true);
        assert!(start.elapsed() < std::time::Duration::from_secs(8));
        assert!(captured.output.status.success());
        assert!(captured.completeness.reader_incomplete);
        assert!(!captured.completeness.timed_out && !captured.completeness.truncated);
        assert_eq!(
            String::from_utf8_lossy(&captured.output.stdout),
            "written\n\n[lean-ctx: output not captured: a process outside this call kept it open]\n"
        );
        assert_eq!(captured.output.stderr, b"problem\n");
        assert!(
            alive(sentinel.id()),
            "inherited group must not be signalled"
        );

        // 5. Control: a command that leaves nothing is returned as it always
        // was — its bytes and nothing else, complete.
        let child = piped("echo written; echo problem >&2").spawn().unwrap();
        let captured =
            super::wait_with_limits_captured(child, 1024, std::time::Duration::from_secs(30), true);
        assert_eq!(captured.output.stdout, b"written\n");
        assert_eq!(captured.output.stderr, b"problem\n");
        assert!(captured.completeness.is_complete());

        // The sentinel survived every case; end and reap it ourselves.
        sentinel.kill().unwrap();
        sentinel.wait().unwrap();
        println!("GUARDIAN-COMPLETE");
    }

    /// Q13: a timeout or output limit never signals a process group the call
    /// did not create, and a leader that exited before its pipe-holding child
    /// does not hide the call's own group. Runs under a guardian in its own
    /// group, so a regression can only kill the fixture, never the test run.
    #[cfg(unix)]
    #[test]
    fn limits_signal_only_a_group_the_call_owns() {
        use std::os::unix::process::CommandExt as _;
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([OUTPUT_HELPER_TEST, "--exact", "--ignored", "--nocapture"])
            .env(OUTPUT_HELPER_ENV, "guardian")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        command.process_group(0);
        let child = command.spawn().unwrap();
        // The guardian ends its own group as its last act, so its exit status
        // is a kill signal by design; the marker line is the verdict.
        let captured = super::wait_with_limits_captured(
            child,
            64 * 1024,
            std::time::Duration::from_mins(1),
            true,
        );
        let stdout = String::from_utf8_lossy(&captured.output.stdout);
        let stderr = String::from_utf8_lossy(&captured.output.stderr);
        assert!(
            stdout.contains("GUARDIAN-COMPLETE"),
            "guardian status {:?}\n{stdout}\n{stderr}",
            captured.output.status
        );
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
