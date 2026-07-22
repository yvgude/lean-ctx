use std::io::Read;
use std::process::Stdio;
use std::sync::{Arc, Mutex, atomic::AtomicBool, mpsc};
use std::time::{Duration, Instant};

const READER_RESULT_TIMEOUT: Duration = Duration::from_secs(2);

/// Prefix of the timeout notice this module appends to a killed command.
const TIMEOUT_MARKER: &str = "ERROR: command timed out after ";

/// The child's own output preceding the timeout marker, or `None` when the
/// output carries no marker. `Some("")` means the command timed out having
/// produced nothing recoverable.
///
/// Single source of truth for "was anything captured before the timeout?":
/// `call_tool` decides `isError` with it (#1086) and `ctx_shell` decides
/// whether the notice is worth archiving (#995). It lives beside the code that
/// writes the marker because both callers previously re-derived it by matching
/// the whole output, so enriching the notice broke them silently (#1173).
pub(crate) fn output_before_timeout_marker(output: &str) -> Option<&str> {
    output.find(TIMEOUT_MARKER).map(|idx| output[..idx].trim())
}

#[cfg(test)]
pub(crate) fn execute_command_in(command: &str, cwd: &str) -> (String, i32) {
    execute_command_with_env(command, cwd, &std::collections::HashMap::new(), None)
}

pub(crate) fn execute_command_with_env(
    command: &str,
    cwd: &str,
    extra_env: &std::collections::HashMap<String, String>,
    timeout_ms: Option<u64>,
) -> (String, i32) {
    execute_command_with_env_cancellable(command, cwd, extra_env, timeout_ms, None, false)
}

/// Execute a command under the normal shell policy, with an optional cooperative
/// cancellation signal used by explicit background jobs. The signal is checked
/// by the same watchdog that enforces `timeout_ms`, so cancellation kills the
/// complete Unix process group rather than only the shell leader.
///
/// `idle_keyed` turns `timeout_ms` into an *idle* budget instead of a wall-clock
/// one: the clock resets whenever the child produces new bytes (#1113/#1173). A
/// poll loop emitting a few hundred bytes over ten minutes is not the runaway
/// this watchdog protects against.
///
/// Every job managed by `background_shell` sets it, foreground runs included —
/// those detach at the soft cap and become exactly such a job. Only the
/// degraded no-session path runs strict wall-clock. Two bounds keep it honest:
/// genuinely runaway output hits the byte cap, which freezes the captured
/// length and so restarts the clock, and `LEAN_CTX_SHELL_BG_MAX_MS` (default
/// 1h) caps lifetime absolutely.
pub(crate) fn execute_command_with_env_cancellable(
    command: &str,
    cwd: &str,
    extra_env: &std::collections::HashMap<String, String>,
    timeout_ms: Option<u64>,
    cancel: Option<&AtomicBool>,
    idle_keyed: bool,
) -> (String, i32) {
    let (shell, flag) = crate::shell::shell_and_flag();
    let normalized_cmd = crate::tools::ctx_shell::normalize_command_for_shell(command);
    let dir = std::path::Path::new(cwd);
    let mut cmd = std::process::Command::new(&shell);
    if cfg!(windows) && crate::shell::platform::is_powershell(&shell) {
        cmd.args(["-NoProfile", "-ExecutionPolicy", "Bypass"]);
    }
    cmd.arg(&flag)
        .arg(&normalized_cmd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null());
    crate::shell::reentry::mark_child(&mut cmd);

    if !extra_env.contains_key("GIT_PAGER") {
        cmd.env("GIT_PAGER", "cat");
    }
    if !extra_env.contains_key("PAGER") {
        cmd.env("PAGER", "cat");
    }

    ensure_utf8_locale(&mut cmd, extra_env);
    crate::shell::platform::apply_profile_free_env(&mut cmd);

    // Auto-forward agent runtime env vars (CODEX_THREAD_ID, CLAUDE_*, …) so
    // session-aware commands run through ctx_shell can see the active session.
    //   1. From this process's own env — covers agents that pass the vars to the
    //      MCP server process.
    //   2. From the captured agent-env store — covers agents like Codex where the
    //      vars live only in the native agent shell, not the MCP server process
    //      (#370). Hooks / `lean-ctx -c` capture them; the process env wins on
    //      conflict, and explicit `extra_env` (below) wins over both.
    let mut forwarded: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (key, val) in std::env::vars() {
        if crate::core::agent_runtime_env::is_forwardable(&key) {
            cmd.env(&key, &val);
            forwarded.insert(key);
        }
    }
    for (key, val) in crate::core::agent_runtime_env::load() {
        if !forwarded.contains(&key) {
            cmd.env(&key, &val);
        }
    }

    // Explicit env vars from tool call (highest priority)
    for (key, val) in extra_env {
        cmd.env(key, val);
    }
    if dir.is_dir() {
        cmd.current_dir(dir);
    } else {
        return (
            format!("ERROR: working directory does not exist or is not a directory: {cwd}"),
            1,
        );
    }
    let cap = crate::core::limits::max_shell_bytes();

    // Isolate the shell in its own process group on Unix. A timeout must kill
    // descendants too; otherwise a child can retain a pipe write-end and make
    // the caller report an empty result despite bytes already captured (#995).
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    let mut child = match cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn() {
        Ok(c) => c,
        Err(e) => return (format!("ERROR: {e}"), 1),
    };
    // Stream each pipe into a shared, cap-bounded buffer that the main thread can
    // read at any time. Crucially this lets a timed-out wait recover the bytes
    // captured so far instead of discarding all output (#945).
    let (out_buf, out_done) = spawn_capture(child.stdout.take(), cap);
    let (err_buf, err_done) = spawn_capture(child.stderr.take(), cap);

    let timeout = command_timeout(command, timeout_ms);
    let start = Instant::now();
    let hard_deadline = idle_keyed.then(|| start + streaming_max_lifetime());
    let mut last_len = 0usize;
    let mut last_output_at = start;
    // Named on timeout so a multi-segment pipeline says *which* part hung (#1086).
    let mut still_running: Vec<String> = Vec::new();
    let (code, timed_out, cancelled) = loop {
        match child.try_wait() {
            Ok(Some(status)) => break (status.code().unwrap_or(1), false, false),
            Ok(None) => {
                if cancel.is_some_and(|signal| signal.load(std::sync::atomic::Ordering::Acquire)) {
                    kill_timed_out_child(&mut child);
                    let _ = child.wait();
                    break (130, false, true);
                }
                let idle_for = if idle_keyed {
                    let len = captured_len(&out_buf) + captured_len(&err_buf);
                    if len != last_len {
                        last_len = len;
                        last_output_at = Instant::now();
                    }
                    last_output_at.elapsed()
                } else {
                    start.elapsed()
                };
                if idle_for >= timeout || hard_deadline.is_some_and(|d| Instant::now() >= d) {
                    still_running = running_segments(&child, &normalized_cmd);
                    kill_timed_out_child(&mut child);
                    let _ = child.wait();
                    break (124, true, false);
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(_) => break (1, false, false),
        }
    };

    // Bounded grace period for the readers to reach EOF, but always read the
    // shared buffers afterwards so output is never silently lost (#945). A reader
    // that misses the deadline means something still holds the pipe open; we then
    // surface the partial capture plus an explicit note rather than nothing.
    let reader_deadline = Instant::now() + READER_RESULT_TIMEOUT;
    let out_complete = wait_for_reader(&out_done, reader_deadline);
    let err_complete = wait_for_reader(&err_done, reader_deadline);
    let (out_bytes, out_trunc) = snapshot(&out_buf);
    let (err_bytes, err_trunc) = snapshot(&err_buf);
    let reader_incomplete = !out_complete || !err_complete;

    let stdout = crate::shell::resolve_carriage_returns(&crate::shell::decode_output(&out_bytes));
    let stderr = crate::shell::resolve_carriage_returns(&crate::shell::decode_output(&err_bytes));
    // On failure both streams are labeled so the agent can attribute the error
    // (#812); success keeps the plain join.
    let mut text = crate::shell::combine_streams(&stdout, &stderr, code);

    if out_trunc || err_trunc {
        text.push_str(&format!(
            "\n[truncated: cap={}B stdout={}B stderr={}B]",
            cap,
            out_bytes.len(),
            err_bytes.len()
        ));
    }
    // The command finished but a reader never hit EOF: a leftover process is
    // holding the pipe open. We still return what was captured; flag it so the
    // agent knows the tail may be missing instead of seeing a bare exit code
    // (#945). Suppressed on timeout, which carries its own message below.
    if reader_incomplete && !timed_out {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&format!(
            "[lean-ctx: output reader still draining after {}s — a background process is likely \
             holding the pipe open; output above may be partial]",
            READER_RESULT_TIMEOUT.as_secs()
        ));
    }
    if timed_out {
        if !text.ends_with('\n') && !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&format!(
            "{TIMEOUT_MARKER}{}ms{}",
            timeout.as_millis(),
            if idle_keyed {
                " without new output"
            } else {
                ""
            }
        ));
        // #1086: for a compound command the captured output is often complete
        // for every segment but one. Name the segment(s) still alive in the
        // child's process group so the caller can fix that part instead of
        // re-running the whole pipeline.
        if !still_running.is_empty() {
            text.push_str(&format!(
                "\n[still running at timeout: {}]",
                still_running.join(" | ")
            ));
        }
    }
    if cancelled {
        if !text.ends_with('\n') && !text.is_empty() {
            text.push('\n');
        }
        text.push_str("ERROR: command cancelled");
    }

    (text, code)
}

/// Kill a timed-out command and every descendant that inherited its pipes.
/// The child is a process-group leader on Unix, so killing only the shell would
/// otherwise leave grandchildren alive and readers unable to reach EOF (#995).
fn kill_timed_out_child(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        let pgid = child.id() as libc::pid_t;
        if pgid > 0 {
            // SAFETY: killpg is a plain syscall; a stale group simply yields ESRCH.
            unsafe { libc::killpg(pgid, libc::SIGKILL) };
        }
    }
    let _ = child.kill();
}

/// Shared, cap-bounded capture buffer for one child pipe. The reader thread
/// appends here as bytes arrive — not only at EOF — so the caller can recover
/// whatever was read so far even if the reader never reaches EOF, e.g. when a
/// backgrounded or otherwise-inherited process keeps the pipe's write-end open
/// after the direct child exits (#945).
#[derive(Default)]
struct CaptureBuf {
    bytes: Vec<u8>,
    truncated: bool,
}

/// Spawn a reader that streams `pipe` into a shared [`CaptureBuf`] (bounded to
/// `cap` bytes) and signals completion (EOF or read error) over the returned
/// channel. The buffer is readable at any time via [`snapshot`], so a timed-out
/// wait yields partial output instead of nothing.
fn spawn_capture<R: Read + Send + 'static>(
    pipe: Option<R>,
    cap: usize,
) -> (Arc<Mutex<CaptureBuf>>, mpsc::Receiver<()>) {
    let shared = Arc::new(Mutex::new(CaptureBuf::default()));
    let (done_tx, done_rx) = mpsc::channel();
    let writer = Arc::clone(&shared);
    std::thread::spawn(move || {
        if let Some(mut r) = pipe {
            let mut buf = [0u8; 8192];
            loop {
                match r.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let mut s = writer
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        if s.bytes.len() < cap {
                            let remaining = cap - s.bytes.len();
                            let take = remaining.min(n);
                            s.bytes.extend_from_slice(&buf[..take]);
                            if take < n {
                                s.truncated = true;
                            }
                        } else {
                            s.truncated = true;
                        }
                    }
                }
            }
        }
        let _ = done_tx.send(());
    });
    (shared, done_rx)
}

/// Snapshot a capture buffer (clone bytes + truncation flag) under its lock.
/// Safe to call while the reader is still writing — yields a consistent prefix.
fn snapshot(buf: &Arc<Mutex<CaptureBuf>>) -> (Vec<u8>, bool) {
    let s = buf
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    (s.bytes.clone(), s.truncated)
}

/// Bytes captured so far, used as the liveness signal for an idle-keyed
/// timeout. Cheaper than [`snapshot`], which clones the whole buffer.
fn captured_len(buf: &Arc<Mutex<CaptureBuf>>) -> usize {
    buf.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .bytes
        .len()
}

/// Absolute ceiling on an idle-keyed (background) job, so a command that keeps
/// emitting output forever still cannot outlive the daemon's patience (#1173).
fn streaming_max_lifetime() -> Duration {
    Duration::from_millis(
        std::env::var("LEAN_CTX_SHELL_BG_MAX_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|&ms: &u64| ms > 0)
            .unwrap_or(3_600_000),
    )
}

/// Command lines still alive in the timed-out child's process group, i.e. the
/// pipeline segment(s) that did not finish (#1086). Rows still carrying the
/// *whole* command are the un-exec'd shell wrapper and say nothing specific, so
/// they are dropped — note the leader is not simply skipped by pid, because
/// `sh -c 'a; b'` execs into its final segment and so *is* the leader.
/// Best-effort: an unavailable or unparsable `ps` yields no attribution.
#[cfg(unix)]
fn running_segments(child: &std::process::Child, command: &str) -> Vec<String> {
    let pgid = child.id();
    // POSIX-portable field selection; `ps -g` differs between BSD and Linux.
    let Ok(out) = std::process::Command::new("ps")
        .args(["-A", "-o", "pid=,pgid=,args="])
        .output()
    else {
        return Vec::new();
    };
    let needle = command.split_whitespace().collect::<Vec<_>>().join(" ");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| parse_ps_row(line, pgid, &needle))
        .collect()
}

/// One `pid pgid args` row: `Some(args)` iff it belongs to `pgid` and names a
/// narrower command than the one we launched. `needle` is the whole command
/// with runs of whitespace collapsed, matching how `ps` renders `args`.
#[cfg(unix)]
fn parse_ps_row(line: &str, pgid: u32, needle: &str) -> Option<String> {
    let rest = line.trim_start();
    let (_pid, rest) = rest.split_once(char::is_whitespace)?;
    let args = rest
        .trim_start()
        .split_once(char::is_whitespace)
        .and_then(|(row_pgid, args)| {
            (row_pgid.parse::<u32>().ok()? == pgid).then_some(args.trim())
        })?;
    if args.is_empty() || args.contains(needle) {
        return None;
    }
    Some(args.chars().take(200).collect())
}

#[cfg(not(unix))]
fn running_segments(_child: &std::process::Child, _command: &str) -> Vec<String> {
    // No process groups here, so a descendant cannot be attributed to this run.
    Vec::new()
}

/// Block until the reader signals completion or `deadline` passes. Returns true
/// iff the reader completed (reached EOF) within the deadline.
fn wait_for_reader(done: &mpsc::Receiver<()>, deadline: Instant) -> bool {
    let remaining = deadline.saturating_duration_since(Instant::now());
    done.recv_timeout(remaining).is_ok()
}

fn ensure_utf8_locale(
    cmd: &mut std::process::Command,
    extra_env: &std::collections::HashMap<String, String>,
) {
    if extra_env.contains_key("LC_ALL") || extra_env.contains_key("LC_CTYPE") {
        return;
    }
    crate::shell::platform::apply_utf8_locale(cmd);
}

fn command_timeout(command: &str, timeout_ms: Option<u64>) -> Duration {
    // Single source of truth: operator env pin > per-call `timeout_ms` >
    // per-tier env/config > built-in heavy/normal ceilings. Keeps this path
    // identical to the interactive hook (`shell::exec::shell_timeout`).
    crate::shell::shell_timeout_with_override(command, timeout_ms)
}

#[cfg(test)]
mod tests {
    use super::{command_timeout, ensure_utf8_locale, execute_command_in};

    #[test]
    fn command_timeout_delegates_to_shell_timeout() {
        // `command_timeout` is a thin alias for `shell::exec::shell_timeout`
        // (full precedence coverage lives there). Smoke-test the delegation:
        // heavy beats normal, and the universal MS override pins both.
        let _lock = crate::core::data_dir::test_env_lock();
        let saved = std::env::var("LEAN_CTX_SHELL_TIMEOUT_MS").ok();
        crate::test_env::remove_var("LEAN_CTX_SHELL_TIMEOUT_MS");

        assert!(
            command_timeout("cargo install --path .", None) > command_timeout("git status", None)
        );

        crate::test_env::set_var("LEAN_CTX_SHELL_TIMEOUT_MS", "5000");
        assert_eq!(
            command_timeout("cargo install --path .", None),
            std::time::Duration::from_secs(5)
        );
        assert_eq!(
            command_timeout("git status", None),
            std::time::Duration::from_secs(5)
        );

        crate::test_env::remove_var("LEAN_CTX_SHELL_TIMEOUT_MS");
        if let Some(v) = saved {
            crate::test_env::set_var("LEAN_CTX_SHELL_TIMEOUT_MS", v);
        }
    }

    /// #1113/#1173: a poll loop that emits a short line every so often must
    /// outlive the wall-clock budget — the cap protects against runaway output,
    /// not against a long-lived monitor. Wall-clock mode still kills it.
    #[test]
    #[cfg_attr(windows, ignore)]
    fn idle_keyed_timeout_survives_a_trickling_loop() {
        let _lock = crate::core::data_dir::test_env_lock();
        crate::test_env::remove_var("LEAN_CTX_SHELL_TIMEOUT_MS");
        let env = std::collections::HashMap::new();
        // Emits every 100ms for ~900ms under a 400ms budget.
        let cmd = "for i in 1 2 3 4 5 6 7 8 9; do printf T; sleep 0.1; done; printf DONE";

        let (idle_out, idle_code) =
            super::execute_command_with_env_cancellable(cmd, ".", &env, Some(400), None, true);
        assert_eq!(
            idle_code, 0,
            "output kept resetting the idle clock: {idle_out}"
        );
        assert!(idle_out.contains("DONE"));

        let (wall_out, wall_code) =
            super::execute_command_with_env_cancellable(cmd, ".", &env, Some(400), None, false);
        assert_eq!(wall_code, 124, "wall-clock mode must still enforce the cap");
        assert!(wall_out.contains("timed out"));
    }

    /// #1086: a timed-out pipeline names the segment that was still running,
    /// so the caller fixes that part instead of re-running everything.
    #[test]
    #[cfg_attr(windows, ignore)]
    fn timeout_names_the_still_running_segment() {
        let _lock = crate::core::data_dir::test_env_lock();
        crate::test_env::remove_var("LEAN_CTX_SHELL_TIMEOUT_MS");
        let env = std::collections::HashMap::new();
        let (out, code) = super::execute_command_with_env_cancellable(
            "printf FIRST_OK; sleep 47",
            ".",
            &env,
            Some(600),
            None,
            false,
        );
        assert_eq!(code, 124);
        assert!(
            out.contains("FIRST_OK"),
            "partial output must survive: {out}"
        );
        assert!(
            out.contains("still running at timeout") && out.contains("47"),
            "the hung segment must be named: {out}"
        );
    }

    #[test]
    #[cfg(unix)]
    fn ps_row_names_segments_and_drops_the_whole_command() {
        let whole = "printf hi; grep -rn needle /repo";
        assert_eq!(
            super::parse_ps_row(" 4242  4200 grep -rn needle /repo", 4200, whole).as_deref(),
            Some("grep -rn needle /repo"),
            "a segment must be named"
        );
        // The un-exec'd shell wrapper still carries the whole command.
        assert_eq!(
            super::parse_ps_row(
                "4200 4200 /bin/sh -c printf hi; grep -rn needle /repo",
                4200,
                whole
            ),
            None
        );
        // Another job's process group.
        assert_eq!(super::parse_ps_row("4242 9999 sleep 5", 4200, whole), None);
        assert_eq!(super::parse_ps_row("garbage", 4200, whole), None);
    }

    #[test]
    fn ensure_utf8_locale_sets_fallback_when_none_inherited() {
        let empty: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        let mut cmd = std::process::Command::new("true");

        // Temporarily unset locale vars to test fallback
        let saved = (
            std::env::var("LC_ALL").ok(),
            std::env::var("LC_CTYPE").ok(),
            std::env::var("LANG").ok(),
        );
        crate::test_env::remove_var("LC_ALL");
        crate::test_env::remove_var("LC_CTYPE");
        crate::test_env::remove_var("LANG");

        ensure_utf8_locale(&mut cmd, &empty);

        // Restore
        if let Some(v) = saved.0 {
            crate::test_env::set_var("LC_ALL", v);
        }
        if let Some(v) = saved.1 {
            crate::test_env::set_var("LC_CTYPE", v);
        }
        if let Some(v) = saved.2 {
            crate::test_env::set_var("LANG", v);
        }

        // Command internal env isn't inspectable, but we verify the fn doesn't panic
        // and the real integration test below checks byte-level correctness.
    }

    #[test]
    fn ensure_utf8_locale_skips_when_extra_env_has_lc_all() {
        let mut extra = std::collections::HashMap::new();
        extra.insert("LC_ALL".to_string(), "C".to_string());
        let mut cmd = std::process::Command::new("true");
        ensure_utf8_locale(&mut cmd, &extra);
        // Should not panic or override
    }

    #[test]
    #[cfg_attr(windows, ignore)]
    fn utf8_bytes_survive_shell_roundtrip() {
        let (output, code) = execute_command_in(
            "printf '\\xD0\\x9F\\xD1\\x80\\xD0\\xB8\\xD0\\xB2\\xD0\\xB5\\xD1\\x82'",
            ".",
        );
        assert_eq!(code, 0, "printf failed: {output}");
        assert_eq!(output, "Привет", "Cyrillic bytes must survive roundtrip");
    }

    #[test]
    #[cfg_attr(windows, ignore)] // ReadToEnd() blocks indefinitely on Windows CI
    fn execute_command_closes_stdin() {
        let command = "sh -c 'if read -t 1 line; then echo 67890; else echo 12345; fi'";
        let (output, code) = execute_command_in(command, ".");
        assert_eq!(code, 0, "command failed: {output}");
        assert!(
            output.contains("12345"),
            "child process should receive EOF on stdin, got: {output}"
        );
    }

    /// #945: a command that finishes but leaves a process holding the stdout
    /// pipe open (here a backgrounded `sleep`) must NOT lose the foreground
    /// output. The old `recv_timeout(...).unwrap_or_default()` discarded
    /// everything on the reader timeout; now the captured prefix survives and
    /// the still-draining reader is flagged instead of returning a bare exit.
    #[test]
    #[cfg_attr(windows, ignore)] // POSIX backgrounding (`&`) + sleep
    fn background_pipe_holder_keeps_foreground_output() {
        let (output, code) = execute_command_in("echo REPRO_CANARY_945; sleep 4 &", ".");
        assert_eq!(code, 0, "command should succeed: {output:?}");
        assert!(
            output.contains("REPRO_CANARY_945"),
            "foreground stdout must survive a lingering background pipe holder, got: {output:?}"
        );
        assert!(
            output.contains("output reader still draining"),
            "an incomplete reader must be flagged, got: {output:?}"
        );
    }

    #[test]
    #[cfg_attr(windows, ignore)]
    fn forwards_captured_agent_runtime_env() {
        // #370: the MCP server process lacks CODEX_THREAD_ID; a hook captured it
        // from the agent shell. ctx_shell must still forward it to the child.
        let _lock = crate::core::data_dir::test_env_lock();
        let dir = std::env::temp_dir().join("lean_ctx_exec_runtime_env");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", &dir);

        // Simulate a hook capturing the var from the native agent environment.
        crate::test_env::remove_var("CODEX_THREAD_ID");
        crate::test_env::set_var("CODEX_THREAD_ID", "thread-from-hook");
        crate::core::agent_runtime_env::capture();
        // The MCP server process itself does not carry the var.
        crate::test_env::remove_var("CODEX_THREAD_ID");

        let (output, code) = execute_command_in("printf 'TID=%s' \"$CODEX_THREAD_ID\"", ".");

        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(code, 0, "command failed: {output}");
        assert!(
            output.contains("TID=thread-from-hook"),
            "captured agent runtime var must be forwarded, got: {output}"
        );
    }

    /// Per-call `timeout_ms` must reach the kill loop: a command that sleeps
    /// past its 200ms budget is killed with the timeout exit code and message.
    #[test]
    #[cfg_attr(windows, ignore)] // POSIX sleep
    fn per_call_timeout_kills_long_command() {
        let (output, code) = super::execute_command_with_env(
            "sleep 3",
            ".",
            &std::collections::HashMap::new(),
            Some(200),
        );
        assert_eq!(code, 124, "timed-out command must exit 124: {output}");
        assert!(
            output.contains("timed out after 200ms"),
            "timeout message must carry the per-call budget, got: {output}"
        );
    }

    /// #995: bytes emitted before a timeout remain visible; a timeout notice is
    /// additive rather than a replacement for useful subprocess output.
    #[test]
    #[cfg_attr(windows, ignore)] // POSIX sleep
    fn per_call_timeout_preserves_partial_output() {
        let (output, code) = super::execute_command_with_env(
            "printf TIMEOUT_PARTIAL_995; sleep 3",
            ".",
            &std::collections::HashMap::new(),
            Some(200),
        );
        assert_eq!(code, 124, "timed-out command must exit 124: {output}");
        assert!(
            output.contains("TIMEOUT_PARTIAL_995"),
            "stdout emitted before timeout must be preserved: {output:?}"
        );
        assert!(
            output.contains("timed out after 200ms"),
            "timeout notice must remain explicit: {output:?}"
        );
    }

    #[test]
    fn git_version_returns_when_git_is_available() {
        let git_available = std::process::Command::new("git")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok();
        if !git_available {
            return;
        }

        let (output, code) = execute_command_in("git --version", ".");
        assert_eq!(code, 0, "git command failed: {output}");
        assert!(
            output.to_ascii_lowercase().contains("git version"),
            "unexpected git output: {output}"
        );
    }
}
