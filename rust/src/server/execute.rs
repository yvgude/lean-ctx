use std::io::Read;
use std::process::Stdio;
use std::sync::mpsc;
use std::time::{Duration, Instant};

const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_mins(2);
const READER_RESULT_TIMEOUT: Duration = Duration::from_secs(2);

pub fn execute_command_in(command: &str, cwd: &str) -> (String, i32) {
    let (shell, flag) = crate::shell::shell_and_flag();
    let normalized_cmd = crate::tools::ctx_shell::normalize_command_for_shell(command);
    let dir = std::path::Path::new(cwd);
    let mut cmd = std::process::Command::new(&shell);
    cmd.arg(&flag)
        .arg(&normalized_cmd)
        .env("LEAN_CTX_ACTIVE", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_PAGER", "cat")
        .env("PAGER", "cat")
        .stdin(Stdio::null());
    if dir.is_dir() {
        cmd.current_dir(dir);
    }
    let cap = crate::core::limits::max_shell_bytes();

    fn read_bounded<R: Read>(mut r: R, cap: usize) -> (Vec<u8>, bool, usize) {
        let mut kept: Vec<u8> = Vec::with_capacity(cap.min(8192));
        let mut buf = [0u8; 8192];
        let mut total = 0usize;
        let mut truncated = false;
        loop {
            match r.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    total = total.saturating_add(n);
                    if kept.len() < cap {
                        let remaining = cap - kept.len();
                        let take = remaining.min(n);
                        kept.extend_from_slice(&buf[..take]);
                        if take < n {
                            truncated = true;
                        }
                    } else {
                        truncated = true;
                    }
                }
            }
        }
        (kept, truncated, total)
    }

    let mut child = match cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn() {
        Ok(c) => c,
        Err(e) => return (format!("ERROR: {e}"), 1),
    };
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let (out_tx, out_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = stdout.map_or_else(|| (Vec::new(), false, 0), |s| read_bounded(s, cap));
        let _ = out_tx.send(result);
    });

    let (err_tx, err_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = stderr.map_or_else(|| (Vec::new(), false, 0), |s| read_bounded(s, cap));
        let _ = err_tx.send(result);
    });

    let timeout = command_timeout();
    let start = Instant::now();
    let (code, timed_out) = loop {
        match child.try_wait() {
            Ok(Some(status)) => break (status.code().unwrap_or(1), false),
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    break (124, true);
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(_) => break (1, false),
        }
    };

    let (out_bytes, out_trunc, _out_total) = out_rx
        .recv_timeout(READER_RESULT_TIMEOUT)
        .unwrap_or_default();
    let (err_bytes, err_trunc, _err_total) = err_rx
        .recv_timeout(READER_RESULT_TIMEOUT)
        .unwrap_or_default();

    let stdout = crate::shell::decode_output(&out_bytes);
    let stderr = crate::shell::decode_output(&err_bytes);
    let mut text = if stdout.is_empty() {
        stderr.clone()
    } else if stderr.is_empty() {
        stdout.clone()
    } else {
        format!("{stdout}\n{stderr}")
    };

    if out_trunc || err_trunc {
        text.push_str(&format!(
            "\n[truncated: cap={}B stdout={}B stderr={}B]",
            cap,
            out_bytes.len(),
            err_bytes.len()
        ));
    }
    if timed_out {
        if !text.ends_with('\n') && !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&format!(
            "ERROR: command timed out after {}ms",
            timeout.as_millis()
        ));
    }

    (text, code)
}

fn command_timeout() -> Duration {
    std::env::var("LEAN_CTX_SHELL_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|millis| *millis > 0)
        .map_or(DEFAULT_COMMAND_TIMEOUT, Duration::from_millis)
}

#[cfg(test)]
mod tests {
    use super::execute_command_in;

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
