use std::io::Read;
use std::process::Stdio;
use std::sync::mpsc;
use std::time::{Duration, Instant};

const READER_RESULT_TIMEOUT: Duration = Duration::from_secs(2);

#[cfg(test)]
pub(crate) fn execute_command_in(command: &str, cwd: &str) -> (String, i32) {
    execute_command_with_env(command, cwd, &std::collections::HashMap::new())
}

pub(crate) fn execute_command_with_env(
    command: &str,
    cwd: &str,
    extra_env: &std::collections::HashMap<String, String>,
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

    let timeout = command_timeout(command);
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

fn ensure_utf8_locale(
    cmd: &mut std::process::Command,
    extra_env: &std::collections::HashMap<String, String>,
) {
    if extra_env.contains_key("LC_ALL") || extra_env.contains_key("LC_CTYPE") {
        return;
    }
    crate::shell::platform::apply_utf8_locale(cmd);
}

fn command_timeout(command: &str) -> Duration {
    // Single source of truth: env (MS / per-tier SECS) > config > built-in
    // heavy/normal ceilings. Keeps this path identical to `ctx_shell` and the
    // interactive hook (`shell::exec::shell_timeout`).
    crate::shell::shell_timeout(command)
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

        assert!(command_timeout("cargo install --path .") > command_timeout("git status"));

        crate::test_env::set_var("LEAN_CTX_SHELL_TIMEOUT_MS", "5000");
        assert_eq!(
            command_timeout("cargo install --path ."),
            std::time::Duration::from_secs(5)
        );
        assert_eq!(
            command_timeout("git status"),
            std::time::Duration::from_secs(5)
        );

        crate::test_env::remove_var("LEAN_CTX_SHELL_TIMEOUT_MS");
        if let Some(v) = saved {
            crate::test_env::set_var("LEAN_CTX_SHELL_TIMEOUT_MS", v);
        }
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
