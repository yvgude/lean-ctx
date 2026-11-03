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

/// Detached background spawn (daemon, proxy, background Cockpit): see
/// [`crate::ipc::detached`] for the Windows job-object and handle rules
/// (GL #545, #2176).
pub use super::detached::{DetachedChild, DetachedStderr, spawn_detached};

pub use lean_ctx_fs::process::is_alive;

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
    let info = freebsd_kinfo(pid)?;
    let pid = info.ki_pid;
    let start_marker = u64::try_from(info.ki_start.tv_sec)
        .ok()?
        .checked_mul(1_000_000)?
        .checked_add(u64::try_from(info.ki_start.tv_usec).ok()?)?;
    freebsd_identity_with_path(pid, start_marker)
}

#[cfg(target_os = "freebsd")]
fn freebsd_kinfo(pid: u32) -> Option<libc::kinfo_proc> {
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
    (info.ki_pid == pid).then_some(info)
}

#[cfg(target_os = "freebsd")]
fn freebsd_identity_with_path(pid: libc::c_int, start_marker: u64) -> Option<ProcessIdentity> {
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

/// The path a running lean-ctx image was installed under. An update or
/// `dev-install` replaces the binary while services keep running the old
/// image, and the OS then reports a different path for it: Linux
/// `/proc/<pid>/exe` reads `…/lean-ctx (deleted)`, Windows reports the
/// rename-aside sidecar `lean-ctx.old.exe` or its parked form
/// `lean-ctx.old-<pid>-<nanos>.exe` (`core::updater::windows_sidecar`).
/// Exactly those shapes map back to `lean-ctx`/`lean-ctx.exe`; every other
/// path is returned unchanged.
#[must_use]
pub fn installed_executable_path(path: &str) -> String {
    let path = path.strip_suffix(" (deleted)").unwrap_or(path);
    let name_at = path.rfind(['/', '\\']).map_or(0, |i| i + 1);
    let (dir, name) = path.split_at(name_at);
    let lower = name.to_ascii_lowercase();
    let sidecar = lower
        .strip_prefix("lean-ctx.old")
        .and_then(|rest| rest.strip_suffix(".exe"))
        .is_some_and(is_parked_sidecar_suffix);
    if sidecar {
        format!("{dir}lean-ctx.exe")
    } else {
        path.to_string()
    }
}

/// `""` (`lean-ctx.old.exe`) or `-<digits>-<digits>` (a parked sidecar).
fn is_parked_sidecar_suffix(middle: &str) -> bool {
    if middle.is_empty() {
        return true;
    }
    let Some(rest) = middle.strip_prefix('-') else {
        return false;
    };
    let mut parts = rest.split('-');
    let digits =
        |p: Option<&str>| p.is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
    digits(parts.next()) && digits(parts.next()) && parts.next().is_none()
}

/// Two identities name the same process: the same creation marker and the
/// same installed executable, also when an update renamed the running image
/// in between (see [`installed_executable_path`]). Pure.
#[must_use]
pub fn same_process(expected: &ProcessIdentity, live: &ProcessIdentity) -> bool {
    if expected.start_marker != live.start_marker {
        return false;
    }
    let a = installed_executable_path(&expected.executable);
    let b = installed_executable_path(&live.executable);
    a == b || (cfg!(windows) && a.eq_ignore_ascii_case(&b))
}

/// `pid` is still the process `expected` recorded ([`same_process`]).
#[must_use]
pub fn is_same_process(pid: u32, expected: &ProcessIdentity) -> bool {
    identity(pid).is_some_and(|live| same_process(expected, &live))
}

/// True when `pid` runs as the same user as this process (effective uid on
/// Unix, token user SID on Windows). A record or a `/health` answer can name
/// any PID; a lifecycle command only ever acts on its own user's processes,
/// also when it runs elevated. Unknown means `false` (fail closed).
#[must_use]
pub fn owned_by_current_user(pid: u32) -> bool {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: geteuid has no preconditions and cannot fail.
        let me = unsafe { libc::geteuid() };
        macos_bsd_info(pid).is_some_and(|info| info.pbi_uid == me)
    }
    #[cfg(target_os = "linux")]
    {
        // SAFETY: geteuid has no preconditions and cannot fail.
        let me = unsafe { libc::geteuid() };
        std::fs::read_to_string(format!("/proc/{pid}/status"))
            .ok()
            .and_then(|status| {
                let line = status.lines().find(|l| l.starts_with("Uid:"))?;
                line.split_whitespace().nth(2)?.parse::<u32>().ok()
            })
            .is_some_and(|euid| euid == me)
    }
    #[cfg(target_os = "freebsd")]
    {
        // SAFETY: geteuid has no preconditions and cannot fail.
        let me = unsafe { libc::geteuid() };
        freebsd_kinfo(pid).is_some_and(|info| info.ki_uid == me)
    }
    #[cfg(windows)]
    {
        windows_owned_by_current_user(pid)
    }
    #[cfg(not(any(
        target_os = "macos",
        target_os = "linux",
        target_os = "freebsd",
        windows
    )))]
    {
        let _ = pid;
        false
    }
}

/// The signal [`signal_same_process`] delivers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopSignal {
    /// SIGTERM on Unix; Windows has no graceful signal for a detached
    /// process, so this terminates as well (callers try HTTP shutdown first).
    Terminate,
    /// SIGKILL / `TerminateProcess`.
    Kill,
}

/// Deliver `signal` to `pid` only while it is still the process `expected`
/// describes. True when the signal was delivered.
///
/// The check and the signal are bound to one process object wherever the OS
/// allows it, so a PID recycled in between is never hit:
/// - Windows: one handle opened with query and terminate rights; the
///   identity is read through that handle and `TerminateProcess` uses it.
/// - Linux: a pidfd is opened first, the identity checked, then
///   `pidfd_send_signal` targets the pidfd (kernel ≥ 5.3; older kernels fall
///   back to the re-check + `kill` below).
/// - macOS and other Unix: the identity is re-checked immediately before
///   `kill`; the remaining window is microseconds.
pub fn signal_same_process(pid: u32, expected: &ProcessIdentity, signal: StopSignal) -> bool {
    if pid == 0 || pid == std::process::id() {
        return false;
    }
    #[cfg(windows)]
    {
        // Windows has one way to stop a detached process; see StopSignal.
        let _ = signal;
        windows_terminate_same_process(pid, expected)
    }
    #[cfg(unix)]
    {
        let sig = match signal {
            StopSignal::Terminate => libc::SIGTERM,
            StopSignal::Kill => libc::SIGKILL,
        };
        let Ok(raw_pid) = libc::pid_t::try_from(pid) else {
            return false;
        };
        #[cfg(target_os = "linux")]
        if let Some(delivered) = linux_pidfd_signal(raw_pid, expected, sig) {
            return delivered;
        }
        // SAFETY: `kill` takes the PID and signal by value; no pointer is
        // dereferenced and errors surface via the return value.
        is_same_process(pid, expected) && unsafe { libc::kill(raw_pid, sig) } == 0
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (expected, signal);
        false
    }
}

/// `Some(delivered)` when pidfds are available, `None` to fall back.
#[cfg(target_os = "linux")]
fn linux_pidfd_signal(
    pid: libc::pid_t,
    expected: &ProcessIdentity,
    sig: libc::c_int,
) -> Option<bool> {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    // SAFETY: pidfd_open takes a PID and flags by value and returns a new fd
    // or -1; no pointer is involved.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    if fd < 0 {
        return match std::io::Error::last_os_error().raw_os_error() {
            // The process is gone: nothing to signal.
            Some(libc::ESRCH) => Some(false),
            // ENOSYS (kernel < 5.3), EPERM under seccomp, …: fall back.
            _ => None,
        };
    }
    let fd = libc::c_int::try_from(fd).ok()?;
    // SAFETY: `fd` was just returned by pidfd_open and is owned here only.
    let pidfd = unsafe { OwnedFd::from_raw_fd(fd) };
    // The pidfd pins the process that held `pid` when it was opened. If that
    // process is the expected one now, the signal below reaches it and no
    // successor; if it already exited, pidfd_send_signal fails with ESRCH.
    if !is_same_process(u32::try_from(pid).ok()?, expected) {
        return Some(false);
    }
    // SAFETY: valid pidfd, plain signal number, null siginfo, zero flags.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            pidfd.as_raw_fd(),
            sig,
            std::ptr::null::<libc::siginfo_t>(),
            0,
        )
    };
    Some(rc == 0)
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
fn macos_bsd_info(pid: u32) -> Option<ProcBsdInfo> {
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
    Some(unsafe { info.assume_init() })
}

#[cfg(target_os = "macos")]
fn macos_process_identity(pid: u32) -> Option<ProcessIdentity> {
    let info = macos_bsd_info(pid)?;
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

/// A process handle closed on drop.
#[cfg(windows)]
struct ProcessHandle(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl ProcessHandle {
    fn open(pid: u32, access: u32) -> Option<Self> {
        use windows_sys::Win32::System::Threading::OpenProcess;
        // SAFETY: OpenProcess takes integers only; a null result is rejected.
        let handle = unsafe { OpenProcess(access, 0, pid) };
        (!handle.is_null()).then_some(Self(handle))
    }
}

#[cfg(windows)]
impl Drop for ProcessHandle {
    fn drop(&mut self) {
        // SAFETY: the handle came from OpenProcess and is closed exactly once.
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0) };
    }
}

#[cfg(windows)]
fn windows_process_identity(pid: u32) -> Option<ProcessIdentity> {
    use windows_sys::Win32::System::Threading::PROCESS_QUERY_LIMITED_INFORMATION;
    let handle = ProcessHandle::open(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
    windows_identity_of(&handle)
}

/// `TerminateProcess` through the same handle the identity was verified on:
/// the handle pins the process object, so a recycled PID cannot be hit.
#[cfg(windows)]
fn windows_terminate_same_process(pid: u32, expected: &ProcessIdentity) -> bool {
    use windows_sys::Win32::System::Threading::{
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE, TerminateProcess,
    };
    let Some(handle) =
        ProcessHandle::open(pid, PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE)
    else {
        return false;
    };
    if !windows_identity_of(&handle).is_some_and(|live| same_process(expected, &live)) {
        return false;
    }
    // SAFETY: valid handle opened with PROCESS_TERMINATE above.
    unsafe { TerminateProcess(handle.0, 1) != 0 }
}

/// The user SID of `process`'s token, as bytes.
#[cfg(windows)]
pub(crate) fn windows_token_user_sid(
    process: windows_sys::Win32::Foundation::HANDLE,
) -> Option<Vec<u8>> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::Security::{
        GetLengthSid, GetTokenInformation, IsValidSid, TOKEN_QUERY, TOKEN_USER, TokenUser,
    };
    use windows_sys::Win32::System::Threading::OpenProcessToken;

    let mut token = std::ptr::null_mut();
    // SAFETY: `token` is a writable handle slot; it is closed below.
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &raw mut token) } == 0 {
        return None;
    }
    let sid = (|| {
        let mut needed = 0_u32;
        // SAFETY: a zero-length query only writes the required size.
        unsafe { GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &raw mut needed) };
        if needed < u32::try_from(std::mem::size_of::<TOKEN_USER>()).ok()? || needed > 4096 {
            return None;
        }
        let mut buf = vec![0_u64; (needed as usize).div_ceil(8)];
        let buf_bytes = buf.len() * 8;
        // SAFETY: `buf` is writable for at least `needed` bytes.
        let ok = unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                buf.as_mut_ptr().cast(),
                needed,
                &raw mut needed,
            )
        };
        if ok == 0 {
            return None;
        }
        // SAFETY: on success the aligned buffer starts with a TOKEN_USER.
        let sid = unsafe { &*buf.as_ptr().cast::<TOKEN_USER>() }.User.Sid;
        // SAFETY: the SID pointer comes from the successful query.
        if sid.is_null() || unsafe { IsValidSid(sid) } == 0 {
            return None;
        }
        // SAFETY: valid SID (checked above).
        let len = unsafe { GetLengthSid(sid) } as usize;
        let (start, base) = (sid as usize, buf.as_ptr() as usize);
        if start < base || start + len > base + buf_bytes {
            return None;
        }
        // SAFETY: the SID lies inside `buf` (bounds checked above).
        Some(unsafe { std::slice::from_raw_parts(sid.cast::<u8>(), len) }.to_vec())
    })();
    // SAFETY: `token` came from OpenProcessToken and is still open.
    unsafe { CloseHandle(token) };
    sid
}

#[cfg(windows)]
fn windows_owned_by_current_user(pid: u32) -> bool {
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    let Some(handle) = ProcessHandle::open(pid, PROCESS_QUERY_LIMITED_INFORMATION) else {
        return false;
    };
    // SAFETY: GetCurrentProcess returns a pseudo handle that needs no close.
    let mine = windows_token_user_sid(unsafe { GetCurrentProcess() });
    let theirs = windows_token_user_sid(handle.0);
    mine.is_some() && mine == theirs
}

#[cfg(windows)]
fn windows_identity_of(handle: &ProcessHandle) -> Option<ProcessIdentity> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::{GetProcessTimes, QueryFullProcessImageNameW};

    let handle = handle.0;
    let mut created = FILETIME::default();
    let mut exited = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: all output pointers refer to initialized local storage.
    let times_ok = unsafe {
        GetProcessTimes(
            handle,
            &raw mut created,
            &raw mut exited,
            &raw mut kernel,
            &raw mut user,
        )
    } != 0;
    let mut path = vec![0_u16; 32_768];
    let mut path_len = u32::try_from(path.len()).ok()?;
    // SAFETY: `path` is a writable UTF-16 buffer and `path_len` describes it.
    let path_ok =
        unsafe { QueryFullProcessImageNameW(handle, 0, path.as_mut_ptr(), &raw mut path_len) != 0 };
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
        // Only this user's processes: `stop` and `uninstall` act for the caller
        // and never reach another account's lean-ctx, even when run as root.
        // SAFETY: getuid has no preconditions and cannot fail.
        let uid = unsafe { libc::getuid() }.to_string();

        // Exact name match first
        if let Ok(output) = std::process::Command::new("pgrep")
            .args(["-U", &uid, "-x", name])
            .output()
        {
            collect_pids(&output.stdout, &protected, &mut pids);
        }

        // Also find processes where the full command line contains the binary path
        // (catches processes launched via absolute path, e.g. /Users/x/.local/bin/lean-ctx)
        if let Ok(output) = std::process::Command::new("pgrep")
            .args(["-U", &uid])
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

mod services;
pub use services::*;

#[cfg(test)]
mod tests;
