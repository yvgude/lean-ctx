use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Immutable OS-level process identity used to defend durable PID references
/// against PID reuse. A PID may be recycled for an unrelated process after its
/// original owner exits; the start marker makes that transition observable.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProcessIdentity {
    pub start_marker: u64,
    pub executable: String,
}

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
/// object. AI clients (OpenCode, Codex, Claude Code) run MCP servers inside
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

/// Returns a PID-reuse-safe identity when the operating system exposes one.
///
/// Callers must treat `None` as "not proven alive" for security-sensitive
/// ownership checks. The supported desktop targets use an immutable process
/// creation marker plus executable path; unsupported targets deliberately
/// fail closed rather than trusting a bare PID.
pub fn identity(pid: u32) -> Option<ProcessIdentity> {
    #[cfg(target_os = "macos")]
    {
        macos_process_identity(pid)
    }
    #[cfg(target_os = "linux")]
    {
        linux_process_identity(pid)
    }
    #[cfg(windows)]
    {
        windows_process_identity(pid)
    }
    #[cfg(target_os = "freebsd")]
    {
        freebsd_process_identity(pid)
    }
    #[cfg(not(any(
        target_os = "macos",
        target_os = "linux",
        target_os = "freebsd",
        windows
    )))]
    {
        let _ = pid;
        None
    }
}

/// #1947: `kern.proc.pid.<pid>` carries the process start time (`ki_start`),
/// which a reused PID cannot share, and `kern.proc.pathname.<pid>` the
/// executable. Without this FreeBSD fell into the fail-closed branch above,
/// and agent-bus registration refused every tool call.
#[cfg(target_os = "freebsd")]
fn freebsd_process_identity(pid: u32) -> Option<ProcessIdentity> {
    let pid = libc::c_int::try_from(pid).ok()?;

    let mut info = std::mem::MaybeUninit::<libc::kinfo_proc>::zeroed();
    let mut info_len = std::mem::size_of::<libc::kinfo_proc>();
    let mib = [libc::CTL_KERN, libc::KERN_PROC, libc::KERN_PROC_PID, pid];
    // SAFETY: `mib` holds four valid ints, `info` is a writable buffer of
    // `info_len` bytes, and no new value is written.
    let rc = unsafe {
        libc::sysctl(
            mib.as_ptr(),
            4,
            info.as_mut_ptr().cast(),
            &mut info_len,
            std::ptr::null(),
            0,
        )
    };
    if rc != 0 || info_len != std::mem::size_of::<libc::kinfo_proc>() {
        return None;
    }
    // SAFETY: sysctl filled the full structure (checked above).
    let info = unsafe { info.assume_init() };
    if info.ki_pid != pid {
        return None;
    }
    let start_marker = u64::try_from(info.ki_start.tv_sec)
        .ok()?
        .checked_mul(1_000_000)?
        .checked_add(u64::try_from(info.ki_start.tv_usec).ok()?)?;

    let mut path = [0_u8; libc::PATH_MAX as usize];
    let mut path_len = path.len();
    let mib = [
        libc::CTL_KERN,
        libc::KERN_PROC,
        libc::KERN_PROC_PATHNAME,
        pid,
    ];
    // SAFETY: `path` is a writable buffer of `path_len` bytes.
    let rc = unsafe {
        libc::sysctl(
            mib.as_ptr(),
            4,
            path.as_mut_ptr().cast(),
            &mut path_len,
            std::ptr::null(),
            0,
        )
    };
    if rc != 0 || path_len == 0 {
        return None;
    }
    // The returned length includes the trailing NUL.
    let bytes = path.get(..path_len)?;
    let bytes = bytes.strip_suffix(&[0]).unwrap_or(bytes);
    if bytes.is_empty() {
        return None;
    }
    Some(ProcessIdentity {
        start_marker,
        executable: String::from_utf8_lossy(bytes).into_owned(),
    })
}

/// True only when the live process still has the identity captured at
/// registration. This is intentionally stricter than `is_alive(pid)`.
pub fn matches_identity(pid: u32, expected: &ProcessIdentity) -> bool {
    identity(pid).as_ref() == Some(expected)
}

#[cfg(target_os = "macos")]
#[repr(C)]
struct ProcBsdInfo {
    pbi_flags: u32,
    pbi_status: u32,
    pbi_xstatus: u32,
    pbi_pid: u32,
    pbi_ppid: u32,
    pbi_uid: u32,
    pbi_gid: u32,
    pbi_ruid: u32,
    pbi_rgid: u32,
    pbi_svuid: u32,
    pbi_svgid: u32,
    rfu_1: u32,
    pbi_comm: [std::ffi::c_char; 16],
    pbi_name: [std::ffi::c_char; 32],
    pbi_nfiles: u32,
    pbi_pgid: u32,
    pbi_pjobc: u32,
    e_tdev: u32,
    e_tpgid: u32,
    pbi_nice: i32,
    pbi_start_tvsec: u64,
    pbi_start_tvusec: u64,
}

#[cfg(target_os = "macos")]
const PROC_PIDTBSDINFO: std::ffi::c_int = 3;

#[cfg(target_os = "macos")]
#[link(name = "proc")]
unsafe extern "C" {
    fn proc_pidinfo(
        pid: std::ffi::c_int,
        flavor: std::ffi::c_int,
        arg: u64,
        buffer: *mut std::ffi::c_void,
        buffersize: std::ffi::c_int,
    ) -> std::ffi::c_int;
    fn proc_pidpath(
        pid: std::ffi::c_int,
        buffer: *mut std::ffi::c_void,
        buffersize: u32,
    ) -> std::ffi::c_int;
}

#[cfg(target_os = "macos")]
fn macos_process_identity(pid: u32) -> Option<ProcessIdentity> {
    let mut info = std::mem::MaybeUninit::<ProcBsdInfo>::zeroed();
    let size = i32::try_from(std::mem::size_of::<ProcBsdInfo>()).ok()?;
    // SAFETY: `info` points to a suitably sized writable C-compatible buffer.
    let read = unsafe {
        proc_pidinfo(
            i32::try_from(pid).ok()?,
            PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if read != size {
        return None;
    }
    // SAFETY: `proc_pidinfo` returned the full structure size above.
    let info = unsafe { info.assume_init() };
    let mut path = [0_u8; libc::PATH_MAX as usize];
    // SAFETY: `path` is a writable byte buffer of the length supplied.
    let path_len = unsafe {
        proc_pidpath(
            i32::try_from(pid).ok()?,
            path.as_mut_ptr().cast(),
            u32::try_from(path.len()).ok()?,
        )
    };
    if path_len <= 0 {
        return None;
    }
    let path_len = usize::try_from(path_len).ok()?;
    let executable = String::from_utf8_lossy(path.get(..path_len)?).into_owned();
    let start_marker = info
        .pbi_start_tvsec
        .checked_mul(1_000_000)?
        .checked_add(info.pbi_start_tvusec)?;
    Some(ProcessIdentity {
        start_marker,
        executable,
    })
}

#[cfg(target_os = "linux")]
fn linux_process_identity(pid: u32) -> Option<ProcessIdentity> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (_, fields) = stat.rsplit_once(')')?;
    let start_marker = fields.split_whitespace().nth(19)?.parse().ok()?;
    let executable = std::fs::read_link(format!("/proc/{pid}/exe"))
        .ok()?
        .to_string_lossy()
        .into_owned();
    Some(ProcessIdentity {
        start_marker,
        executable,
    })
}

#[cfg(windows)]
fn windows_process_identity(pid: u32) -> Option<ProcessIdentity> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
    };

    // SAFETY: the returned handle is checked and closed on every path below.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return None;
    }
    let mut created = FILETIME::default();
    let mut exited = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: all output pointers refer to initialized local storage.
    let times_ok =
        unsafe { GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) } != 0;
    let mut path = vec![0_u16; 32_768];
    let mut path_len = u32::try_from(path.len()).ok()?;
    // SAFETY: `path` is a writable UTF-16 buffer and `path_len` describes it.
    let path_ok =
        unsafe { QueryFullProcessImageNameW(handle, 0, path.as_mut_ptr(), &mut path_len) != 0 };
    // SAFETY: `handle` was returned by OpenProcess and has not been closed yet.
    unsafe { CloseHandle(handle) };
    if !times_ok || !path_ok {
        return None;
    }
    let executable = std::ffi::OsString::from_wide(path.get(..usize::try_from(path_len).ok()?)?)
        .to_string_lossy()
        .into_owned();
    let start_marker = (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
    Some(ProcessIdentity {
        start_marker,
        executable,
    })
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

/// PIDs this process must never signal: itself, its ancestor chain, and every
/// member of its own process group.
///
/// The ancestor chain matters whenever `lean-ctx stop`/`dev-install` runs
/// *under* lean-ctx itself — the shell hook routes commands through a
/// `lean-ctx -c` wrapper, so the process tree is
/// `lean-ctx -c … → sh → lean-ctx dev-install`. Excluding only `getpid()`
/// SIGTERMed the wrapper parent, which took the whole pipeline down mid-run
/// (exit 143) before autostart was re-enabled (#714).
///
/// The process *group* matters because agent harnesses (Cursor's shell) can
/// reparent intermediaries to PID 1 mid-run — the `ps ppid` walk then stops
/// before reaching the outer wrapper, but the wrapper still shares the
/// foreground pgid; signalling it kills the pipeline all the same (#714
/// follow-up, reproduced twice on the first fix).
fn protected_self_pids() -> std::collections::HashSet<u32> {
    let mut protected = std::collections::HashSet::new();
    protected.insert(std::process::id());
    #[cfg(unix)]
    {
        let mut pid = std::process::id();
        for _ in 0..16 {
            let Ok(output) = std::process::Command::new("ps")
                .args(["-o", "ppid=", "-p", &pid.to_string()])
                .output()
            else {
                break;
            };
            let Ok(ppid) = String::from_utf8_lossy(&output.stdout)
                .trim()
                .parse::<u32>()
            else {
                break;
            };
            if ppid <= 1 || !protected.insert(ppid) {
                break;
            }
            pid = ppid;
        }

        // SAFETY: getpgrp() takes no arguments and cannot fail.
        let own_pgid = unsafe { libc::getpgrp() };
        if own_pgid > 0 {
            protected.extend(process_group_pids(own_pgid));
        }
    }
    protected
}

#[cfg(unix)]
fn process_group_pids(pgid: libc::pid_t) -> Vec<u32> {
    std::process::Command::new("pgrep")
        .args(["-g", &pgid.to_string()])
        .output()
        .ok()
        .map(|output| {
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter_map(|line| line.trim().parse::<u32>().ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Find all PIDs of processes whose executable name matches `name`.
/// Excludes the current process and its ancestor chain (#714).
pub fn find_pids_by_name(name: &str) -> Vec<u32> {
    let protected = protected_self_pids();
    let mut pids = Vec::new();

    #[cfg(unix)]
    {
        // Exact name match first
        if let Ok(output) = std::process::Command::new("pgrep")
            .arg("-x")
            .arg(name)
            .output()
        {
            collect_pids(&output.stdout, &protected, &mut pids);
        }

        // Also find processes where the full command line contains the binary path
        // (catches processes launched via absolute path, e.g. /Users/x/.local/bin/lean-ctx)
        if let Ok(output) = std::process::Command::new("pgrep")
            .arg("-f")
            .arg(format!("/{name}(\\s|$)"))
            .output()
        {
            collect_pids(&output.stdout, &protected, &mut pids);
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
                        if !protected.contains(&pid) {
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
fn collect_pids(stdout: &[u8], protected: &std::collections::HashSet<u32>, out: &mut Vec<u32>) {
    let text = String::from_utf8_lossy(stdout);
    for line in text.lines() {
        if let Ok(pid) = line.trim().parse::<u32>()
            && !protected.contains(&pid)
        {
            out.push(pid);
        }
    }
}

/// Returns PIDs that are safe to kill during `lean-ctx stop`: neither MCP stdio
/// servers nor in-flight command runners.
///
/// MCP servers are child processes of the IDE and must not be killed — the IDE
/// will immediately respawn them, causing a kill loop that requires a reboot.
/// Command runners (`lean-ctx -c <cmd>`, `exec`, `--track`) are executing
/// someone's command right now — another agent's `cargo test`, a commit hook.
/// Killing one ends that work with SIGTERM/SIGKILL and holds nothing lean-ctx
/// owns, so even a deliberate "stop everything" leaves them to finish (#1292).
pub fn find_killable_pids(name: &str) -> Vec<u32> {
    let mut protected = find_mcp_server_pids(name);
    protected.extend(find_command_runner_pids(name));
    killable_excluding_mcp(find_pids_by_name(name), &protected)
}

/// Pure set-difference: every PID in `all` that is not a protected PID (MCP
/// server or command runner). Split out from [`find_killable_pids`] so the
/// protection invariant — a protected process is never returned as killable
/// (#1036, #1292) — is unit-testable without spawning real processes.
fn killable_excluding_mcp(all: Vec<u32>, protected: &[u32]) -> Vec<u32> {
    all.into_iter().filter(|p| !protected.contains(p)).collect()
}

/// `lean-ctx` processes that are running a user's command (#1292).
#[cfg(unix)]
fn find_command_runner_pids(name: &str) -> Vec<u32> {
    find_pids_by_name(name)
        .into_iter()
        .filter(|&pid| {
            std::process::Command::new("ps")
                .args(["-o", "command=", "-p", &pid.to_string()])
                .output()
                .is_ok_and(|out| is_command_runner_cmdline(&String::from_utf8_lossy(&out.stdout)))
        })
        .collect()
}

/// Windows keeps the running `.exe` locked, so `dev-install` and `uninstall`
/// must still be able to stop a runner there before replacing the binary.
#[cfg(not(unix))]
fn find_command_runner_pids(_name: &str) -> Vec<u32> {
    Vec::new()
}

/// True when `command` is a `lean-ctx` invocation that runs a user command:
/// `-c`/`exec` (the shell wrapper) or `-t`/`--track`.
// Only the Unix runner lookup calls this; the unit test runs on every target,
// so the function is not gated, only its dead-code lint off Unix.
#[cfg_attr(not(unix), allow(dead_code))]
fn is_command_runner_cmdline(command: &str) -> bool {
    let mut words = command.split_whitespace();
    let Some(argv0) = words.next() else {
        return false;
    };
    if argv0 != "lean-ctx" && !argv0.ends_with("/lean-ctx") {
        return false;
    }
    matches!(words.next(), Some("-c" | "exec" | "-t" | "--track"))
}

#[cfg(unix)]
fn find_mcp_server_pids(name: &str) -> Vec<u32> {
    find_pids_by_name(name)
        .into_iter()
        .filter(|&pid| is_mcp_stdio_process(pid))
        .collect()
}

#[cfg(windows)]
fn find_mcp_server_pids(name: &str) -> Vec<u32> {
    find_pids_by_name(name)
        .into_iter()
        .filter(|&pid| is_mcp_stdio_process_win(pid))
        .collect()
}

#[cfg(windows)]
fn is_mcp_stdio_process_win(pid: u32) -> bool {
    // On Windows, check if the parent process is an IDE (Cursor, VS Code).
    // Uses `wmic` which is available on all supported Windows versions.
    let Ok(output) = std::process::Command::new("wmic")
        .args([
            "process",
            "where",
            &format!("ProcessId={pid}"),
            "get",
            "ParentProcessId,CommandLine",
            "/FORMAT:CSV",
        ])
        .output()
    else {
        return false;
    };

    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines() {
        let parts: Vec<&str> = line.split(',').collect();
        if parts.len() < 3 {
            continue;
        }
        let cmd_line = parts[1].to_lowercase();
        let ppid_str = parts.last().unwrap_or(&"").trim();

        // Direct child of IDE
        if cmd_line.contains("cursor") || cmd_line.contains("code.exe") {
            return true;
        }

        // Check parent process name
        if let Ok(ppid) = ppid_str.parse::<u32>() {
            if let Ok(pp_out) = std::process::Command::new("wmic")
                .args([
                    "process",
                    "where",
                    &format!("ProcessId={ppid}"),
                    "get",
                    "Name",
                    "/FORMAT:CSV",
                ])
                .output()
            {
                let pp_text = String::from_utf8_lossy(&pp_out.stdout).to_lowercase();
                if pp_text.contains("cursor") || pp_text.contains("code.exe") {
                    return true;
                }
            }
        }

        // MCP stdio server heuristic: lean-ctx.exe with no subcommand
        if (cmd_line.ends_with("lean-ctx.exe") || cmd_line.ends_with("lean-ctx.exe\""))
            && !cmd_line.contains("proxy")
            && !cmd_line.contains("dashboard")
            && !cmd_line.contains("daemon")
            && !cmd_line.contains("stop")
            && !cmd_line.contains("hook")
        {
            return true;
        }
    }
    false
}

#[cfg(not(any(unix, windows)))]
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
    fn current_process_identity_is_stable_and_matches() {
        let pid = std::process::id();
        let identity = identity(pid).expect("supported platform exposes process identity");
        assert!(!identity.executable.is_empty());
        assert!(matches_identity(pid, &identity));
    }

    #[test]
    fn killable_pids_exclude_active_mcp_stdio_servers() {
        assert_eq!(
            killable_excluding_mcp(vec![11, 17, 23], &[11, 23]),
            vec![17]
        );
    }

    /// #1292: `lean-ctx stop` SIGKILLed other agents' in-flight commands
    /// (`lean-ctx -c "cargo test"`, a commit hook) as "orphans". A command
    /// runner is never killable; daemons, proxies and dashboards still are.
    #[test]
    fn in_flight_command_runners_are_never_killable() {
        for runner in [
            "lean-ctx -c cargo test --lib",
            "/Users/me/.local/bin/lean-ctx -c git commit -m x",
            "/usr/local/bin/lean-ctx exec make",
            "lean-ctx --track npm test",
            "lean-ctx -t pytest",
        ] {
            assert!(
                is_command_runner_cmdline(runner),
                "must be protected: {runner}"
            );
        }
        for stoppable in [
            "lean-ctx serve --_foreground-daemon",
            "/Users/me/.local/bin/lean-ctx proxy start --port 4444",
            "lean-ctx dashboard",
            "lean-ctx",
            "/usr/bin/grep -c lean-ctx",
            "",
        ] {
            assert!(
                !is_command_runner_cmdline(stoppable),
                "must stay stoppable: {stoppable}"
            );
        }
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

    #[test]
    fn killable_excludes_mcp_pids() {
        // #1036: the IDE-owned MCP stdio server PID must never be returned as
        // killable, so `cmd_dev_install`'s force-kill cannot drop the editor's
        // MCP connection.
        let killable = killable_excluding_mcp(vec![1, 2, 3, 4], &[2, 4]);
        assert_eq!(killable, vec![1, 3]);
        assert!(!killable.contains(&2));
        assert!(!killable.contains(&4));
    }

    #[test]
    fn killable_with_no_mcp_returns_all() {
        let all = vec![10, 20, 30];
        assert_eq!(killable_excluding_mcp(all.clone(), &[]), all);
    }

    /// #714 follow-up: agent harnesses reparent intermediaries to PID 1, so
    /// the ppid walk alone misses the outer wrapper — the shared foreground
    /// process group must be protected too.
    #[cfg(unix)]
    #[test]
    fn protected_pids_cover_own_process_group() {
        // SAFETY: getpgrp() takes no arguments and cannot fail.
        let pgid = unsafe { libc::getpgrp() };
        // Bracket `protected_self_pids` with two group snapshots. A single
        // snapshot is racy under `cargo test` parallelism: sibling tests spawn
        // short-lived subprocesses (git/ps/pgrep) that transiently join this
        // foreground process group, so a member seen once may already be gone
        // when `protected_self_pids` runs — or appear only afterwards. Only PIDs
        // present in BOTH snapshots were alive across the whole window and must
        // therefore be covered by the protected set.
        let members_before = process_group_pids(pgid);
        let protected = protected_self_pids();
        let members_after = process_group_pids(pgid);
        for pid in members_before {
            if !members_after.contains(&pid) {
                continue; // transient foreign subprocess from a parallel test
            }
            assert!(
                protected.contains(&pid),
                "stable group member {pid} missing from protected set"
            );
        }
    }

    /// #714: `stop`/`dev-install` running *under* a lean-ctx shell wrapper
    /// (`lean-ctx -c … → sh → lean-ctx dev-install`) must not SIGTERM its own
    /// ancestor chain — that killed the pipeline mid-run (exit 143) before
    /// autostart was re-enabled.
    #[test]
    fn protected_pids_cover_self_and_ancestors() {
        let protected = protected_self_pids();
        assert!(protected.contains(&std::process::id()));
        #[cfg(unix)]
        {
            // The direct parent (cargo's test runner) must be protected too.
            let out = std::process::Command::new("ps")
                .args(["-o", "ppid=", "-p", &std::process::id().to_string()])
                .output()
                .expect("ps runs");
            if let Ok(ppid) = String::from_utf8_lossy(&out.stdout).trim().parse::<u32>()
                && ppid > 1
            {
                assert!(
                    protected.contains(&ppid),
                    "parent {ppid} missing from {protected:?}"
                );
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn find_pids_never_reports_own_process_tree() {
        // Regardless of what matches by name, the returned set must be
        // disjoint from the protected self/ancestor set (#714).
        let protected = protected_self_pids();
        for pid in find_pids_by_name("lean-ctx") {
            assert!(!protected.contains(&pid), "own tree pid {pid} reported");
        }
    }
}
