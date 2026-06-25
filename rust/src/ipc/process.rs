use anyhow::Result;

/// Run a command with a hard timeout, capturing its output.
///
/// Returns `Some(output)` if the child exits within `timeout`, or `None` if it
/// had to be killed (timed out) or could not be spawned. This is the safe way
/// to invoke external control tools (`launchctl`, `systemctl`, a freshly
/// installed binary's `--version`, …) that must never be able to hang a
/// `lean-ctx` command — a wedged `launchctl` previously forced users to reboot.
///
/// Note: intended for commands with small output. The child's stdout/stderr are
/// piped; a process that writes more than the pipe buffer (~64 KiB) without
/// exiting could block. All current callers emit at most a few lines.
#[must_use]
pub fn run_with_timeout(
    mut cmd: std::process::Command,
    timeout: std::time::Duration,
) -> Option<std::process::Output> {
    use std::process::Stdio;
    use std::time::Instant;

    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;

    let start = Instant::now();
    loop {
        match child.try_wait() {
            // Process exited: pipes are at EOF, so reading output won't block.
            Ok(Some(_)) => return child.wait_with_output().ok(),
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(_) => return None,
        }
    }
}

/// Spawn a long-lived background process (proxy, daemon) detached from the
/// current process so it survives the parent's exit.
///
/// On Unix a child simply outlives its parent, so this is a plain spawn. On
/// Windows the child inherits the parent's console and — crucially — its Job
/// object. AI clients (`OpenCode`, Codex, Claude Code) run MCP servers inside
/// kill-on-close Jobs; without detachment the auto-started proxy dies the
/// moment the client recycles its MCP process, which users observe as
/// "Cannot connect to API: The socket connection was closed unexpectedly"
/// plus repeated proxy cold-starts (GL #545).
///
/// Strategy on Windows:
///  1. `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB`
///     — fully detached, escapes the parent's Job.
///  2. If the Job denies breakaway, `CreateProcess` fails with
///     `ERROR_ACCESS_DENIED`; retry without the breakaway flag (still
///     console-detached, which covers non-Job parents).
pub fn spawn_detached(cmd: &mut std::process::Command) -> std::io::Result<std::process::Child> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

        let detached = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
        match cmd
            .creation_flags(detached | CREATE_BREAKAWAY_FROM_JOB)
            .spawn()
        {
            Ok(child) => Ok(child),
            Err(_) => cmd.creation_flags(detached).spawn(),
        }
    }
    #[cfg(not(windows))]
    {
        cmd.spawn()
    }
}

/// Check whether a process with the given PID is still running.
#[must_use]
pub fn is_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // SAFETY: `kill` takes the PID and signal (0 = existence probe) by
        // value; it dereferences no pointers and reports failure via its return
        // value, so it cannot cause undefined behaviour.
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE, WAIT_TIMEOUT};
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, WaitForSingleObject,
        };

        // SAFETY: every Win32 call below takes integer args plus the local
        // `exit_code` out-pointer; the handle is null-checked and closed on
        // every return path, so no resource leaks or invalid pointers occur.
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return false;
            }
            let wait = WaitForSingleObject(handle, 0);
            if wait == WAIT_TIMEOUT {
                CloseHandle(handle);
                return true;
            }
            let mut exit_code: u32 = 0;
            GetExitCodeProcess(handle, &mut exit_code);
            CloseHandle(handle);
            exit_code == STILL_ACTIVE as u32
        }
    }
}

/// Ask a process to terminate gracefully (SIGTERM on Unix, nothing on Windows
/// since we prefer HTTP shutdown; the caller should have already tried that).
pub fn terminate_gracefully(pid: u32) -> Result<()> {
    #[cfg(unix)]
    {
        // SAFETY: `kill` takes the PID and signal by value; no pointer is
        // dereferenced and errors surface via the return value.
        let ret = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
        if ret != 0 {
            anyhow::bail!(
                "Failed to send SIGTERM to PID {pid}: {}",
                std::io::Error::last_os_error()
            );
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        force_kill(pid)
    }
}

/// Unconditionally kill a process.
pub fn force_kill(pid: u32) -> Result<()> {
    #[cfg(unix)]
    {
        // SAFETY: `kill` takes the PID and signal by value; no pointer is
        // dereferenced and errors surface via the return value.
        let ret = unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
        if ret != 0 {
            anyhow::bail!(
                "Failed to send SIGKILL to PID {pid}: {}",
                std::io::Error::last_os_error()
            );
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_TERMINATE, TerminateProcess,
        };

        // SAFETY: the Win32 calls take integer args only; the handle is
        // null-checked and closed before returning on every path.
        unsafe {
            let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
            if handle.is_null() {
                anyhow::bail!(
                    "Failed to open PID {pid} for termination: {}",
                    std::io::Error::last_os_error()
                );
            }
            let ok = TerminateProcess(handle, 1);
            CloseHandle(handle);
            if ok == 0 {
                anyhow::bail!(
                    "Failed to terminate PID {pid}: {}",
                    std::io::Error::last_os_error()
                );
            }
            Ok(())
        }
    }
}

/// Find all PIDs of processes whose executable name matches `name`.
/// Excludes the current process.
#[must_use]
pub fn find_pids_by_name(name: &str) -> Vec<u32> {
    let my_pid = std::process::id();
    let mut pids = Vec::new();

    #[cfg(unix)]
    {
        // Exact name match first
        if let Ok(output) = std::process::Command::new("pgrep")
            .arg("-x")
            .arg(name)
            .output()
        {
            collect_pids(&output.stdout, my_pid, &mut pids);
        }

        // Also find processes where the full command line contains the binary path
        // (catches processes launched via absolute path, e.g. /Users/x/.local/bin/lean-ctx)
        if let Ok(output) = std::process::Command::new("pgrep")
            .arg("-f")
            .arg(format!("/{name}(\\s|$)"))
            .output()
        {
            collect_pids(&output.stdout, my_pid, &mut pids);
        }

        pids.sort_unstable();
        pids.dedup();
    }

    #[cfg(windows)]
    {
        if let Ok(output) = std::process::Command::new("tasklist")
            .args([
                "/FI",
                &format!("IMAGENAME eq {name}.exe"),
                "/FO",
                "CSV",
                "/NH",
            ])
            .output()
        {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines() {
                let parts: Vec<&str> = line.split(',').collect();
                if parts.len() >= 2 {
                    let pid_str = parts[1].trim().trim_matches('"');
                    if let Ok(pid) = pid_str.parse::<u32>() {
                        if pid != my_pid {
                            pids.push(pid);
                        }
                    }
                }
            }
        }
    }

    pids
}

#[cfg(unix)]
fn collect_pids(stdout: &[u8], exclude_pid: u32, out: &mut Vec<u32>) {
    let text = String::from_utf8_lossy(stdout);
    for line in text.lines() {
        if let Ok(pid) = line.trim().parse::<u32>()
            && pid != exclude_pid
        {
            out.push(pid);
        }
    }
}

/// Returns PIDs that are NOT MCP stdio servers (safe to kill during `lean-ctx stop`).
/// MCP servers are child processes of the IDE and must not be killed — the IDE
/// will immediately respawn them, causing a kill loop that requires a reboot.
#[must_use]
pub fn find_killable_pids(name: &str) -> Vec<u32> {
    let all = find_pids_by_name(name);
    let mcp_pids = find_mcp_server_pids(name);
    all.into_iter().filter(|p| !mcp_pids.contains(p)).collect()
}

#[cfg(unix)]
fn find_mcp_server_pids(name: &str) -> Vec<u32> {
    find_pids_by_name(name)
        .into_iter()
        .filter(|&pid| is_mcp_stdio_process(pid))
        .collect()
}

#[cfg(not(unix))]
fn find_mcp_server_pids(_name: &str) -> Vec<u32> {
    Vec::new()
}

#[cfg(unix)]
fn is_mcp_stdio_process(pid: u32) -> bool {
    if let Ok(output) = std::process::Command::new("ps")
        .args(["-o", "ppid=,command=", "-p", &pid.to_string()])
        .output()
    {
        let text = String::from_utf8_lossy(&output.stdout);
        let t = text.trim();
        if t.contains("Cursor") || t.contains("cursor") || t.contains("code") {
            return true;
        }
        let parts: Vec<&str> = t.split_whitespace().collect();
        if let Some(ppid_str) = parts.first()
            && let Ok(ppid) = ppid_str.parse::<u32>()
            && let Ok(pp_out) = std::process::Command::new("ps")
                .args(["-o", "command=", "-p", &ppid.to_string()])
                .output()
        {
            let pp_cmd = String::from_utf8_lossy(&pp_out.stdout);
            if pp_cmd.contains("Cursor") || pp_cmd.contains("cursor") || pp_cmd.contains("code") {
                return true;
            }
        }
        let cmd_part = parts.get(1..).map(|p| p.join(" ")).unwrap_or_default();
        // MCP stdio servers: bare `lean-ctx` with no subcommand (or just `mcp`)
        if (cmd_part.ends_with("/lean-ctx") || cmd_part == "lean-ctx")
            && !cmd_part.contains("proxy")
            && !cmd_part.contains("dashboard")
            && !cmd_part.contains("daemon")
            && !cmd_part.contains("stop")
            && !cmd_part.contains("hook")
        {
            return true;
        }
        // Hook observer/rewriter processes spawned by IDE
        if cmd_part.contains("hook observe")
            || cmd_part.contains("hook rewrite")
            || cmd_part.contains("hook redirect")
        {
            return true;
        }
    }
    false
}

/// Kill non-MCP processes matching `name` (SIGTERM then SIGKILL).
/// Returns count of killed processes.
#[must_use]
pub fn kill_all_by_name(name: &str) -> usize {
    let pids = find_killable_pids(name);
    if pids.is_empty() {
        return 0;
    }

    for &pid in &pids {
        let _ = terminate_gracefully(pid);
    }

    std::thread::sleep(std::time::Duration::from_millis(500));

    let mut killed = 0;
    for &pid in &pids {
        if is_alive(pid) {
            let _ = force_kill(pid);
        }
        killed += 1;
    }

    std::thread::sleep(std::time::Duration::from_millis(200));

    killed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_process_is_alive() {
        assert!(is_alive(std::process::id()));
    }

    #[test]
    fn bogus_pid_is_not_alive() {
        assert!(!is_alive(u32::MAX - 42));
    }

    #[cfg(unix)]
    #[test]
    fn run_with_timeout_returns_output_for_fast_command() {
        let mut cmd = std::process::Command::new("echo");
        cmd.arg("hello");
        let out = run_with_timeout(cmd, std::time::Duration::from_secs(5))
            .expect("fast command should complete");
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hello");
    }

    #[cfg(unix)]
    #[test]
    fn run_with_timeout_kills_slow_command() {
        let mut cmd = std::process::Command::new("sleep");
        cmd.arg("30");
        let start = std::time::Instant::now();
        let result = run_with_timeout(cmd, std::time::Duration::from_millis(300));
        assert!(result.is_none(), "slow command must time out");
        assert!(
            start.elapsed() < std::time::Duration::from_secs(5),
            "timeout must not wait for the full command"
        );
    }
}
