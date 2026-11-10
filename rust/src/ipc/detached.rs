// SPDX-License-Identifier: Apache-2.0
//! Detached spawn of a long-lived background service (daemon, proxy,
//! background Cockpit) so it survives the caller and holds nothing of it.
//!
//! On Unix a child simply outlives its parent: stdin/stdout are null, stderr
//! is null or a log file, and the service leads its own process group
//! (`process_group(0)`). Without that it stays in the spawner's group: a
//! terminal's Ctrl-C reaches it, and every later CLI call from the same group
//! (a script, CI, an app that spawns both `serve --daemon` and `serve --stop`)
//! counts it among its own protected processes, so `serve --status` called it
//! not running and `serve --stop` could not verify it.
//!
//! On Windows a child created through `std::process::Command` inherits two
//! things a background service must not keep (#2176):
//!
//! - **Every inheritable handle of the caller.** std always passes
//!   `bInheritHandles = TRUE`, so a pipe a shell gave `lean-ctx restart` (for
//!   example cmd's stdout pipe in `cmd /c "lean-ctx restart | findstr ."`)
//!   stays open in the daemon and the reader never sees EOF. The spawn below
//!   uses `CreateProcessW` with `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`: the child
//!   inherits exactly its three std handles (NUL, NUL, NUL or the log file)
//!   and nothing else.
//! - **The caller's job object.** PowerShell's `Start-Process -Wait` and AI
//!   clients run their children in a job; a service left in it ends with the
//!   job or keeps `-Wait` waiting (GL #545, #2176). In order:
//!   1. not in a job → a plain detached spawn;
//!   2. the job allows breakaway → `CREATE_BREAKAWAY_FROM_JOB`;
//!   3. the job forbids it (PowerShell's jobs do: `LimitFlags = 0`) → the
//!      service is created with this user's Explorer of this session as its
//!      parent (`PROC_THREAD_ATTRIBUTE_PARENT_PROCESS`), the same way a
//!      program started from the Start menu runs. Explorer is accepted only
//!      when it is `%SystemRoot%\explorer.exe`, runs as this user in this
//!      session and is itself in no job. The service then gets Explorer's
//!      token: an elevated caller starts an unelevated service, which is
//!      what the service needs (same user, same data dirs);
//!   4. no such Explorer (a service session, Server Core) → the service stays
//!      in the job, and one stderr line says so instead of a silent hang.

use std::io;
use std::process::{Command, ExitStatus};

/// Where a detached service writes its stderr; stdin and stdout are always
/// the null device.
pub enum DetachedStderr {
    Null,
    File(std::fs::File),
}

/// A started background service. Dropping it neither waits for nor ends
/// the service.
pub struct DetachedChild {
    pid: u32,
    #[cfg(not(windows))]
    child: std::process::Child,
    #[cfg(windows)]
    process: win::Owned,
}

impl DetachedChild {
    #[must_use]
    pub fn id(&self) -> u32 {
        self.pid
    }

    /// The exit status once the service has ended, without waiting.
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        #[cfg(not(windows))]
        {
            self.child.try_wait()
        }
        #[cfg(windows)]
        {
            win::try_wait(&self.process)
        }
    }

    /// End the service (tests and failed starts only).
    pub fn kill(&mut self) -> io::Result<()> {
        #[cfg(not(windows))]
        {
            self.child.kill()?;
            self.child.wait().map(|_| ())
        }
        #[cfg(windows)]
        {
            win::terminate(&self.process)
        }
    }
}

/// Spawn `cmd` as a detached background service (see the module docs).
/// Only the program, arguments, environment changes and working directory of
/// `cmd` are used; its stdio settings are replaced.
pub fn spawn_detached(cmd: &mut Command, stderr: DetachedStderr) -> io::Result<DetachedChild> {
    #[cfg(not(windows))]
    {
        let stderr = match stderr {
            DetachedStderr::Null => std::process::Stdio::null(),
            DetachedStderr::File(file) => std::process::Stdio::from(file),
        };
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(stderr);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        let child = cmd.spawn()?;
        Ok(DetachedChild {
            pid: child.id(),
            child,
        })
    }
    #[cfg(windows)]
    {
        win::spawn(cmd, stderr)
    }
}

const JOB_OBJECT_LIMIT_BREAKAWAY_OK: u32 = 0x0800;
const JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK: u32 = 0x1000;

/// How one spawn attempt leaves the caller's job object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))]
enum Detach {
    /// The caller is in no job.
    Plain,
    /// `CREATE_BREAKAWAY_FROM_JOB`.
    Breakaway,
    /// Created with the session's Explorer as parent, outside every job.
    Reparent,
    /// Left in the caller's job (last resort, announced on stderr).
    StayInJob,
}

/// The attempts, in order, for a caller `in_job` whose job has
/// `limit_flags`, with or without a usable Explorer broker. A breakaway the
/// innermost job allows can still be refused by an outer one, so the broker
/// and the in-job spawn follow it.
#[cfg_attr(not(windows), allow(dead_code))]
fn detach_attempts(in_job: bool, limit_flags: u32, broker: bool) -> Vec<Detach> {
    if !in_job {
        return vec![Detach::Plain];
    }
    let mut attempts = Vec::with_capacity(3);
    if limit_flags & (JOB_OBJECT_LIMIT_BREAKAWAY_OK | JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK) != 0 {
        attempts.push(Detach::Breakaway);
    }
    if broker {
        attempts.push(Detach::Reparent);
    }
    attempts.push(Detach::StayInJob);
    attempts
}

/// Append `arg` to a Windows command line so the MSVC runtime (and Rust's
/// own argument parser) reads it back unchanged.
#[cfg_attr(not(windows), allow(dead_code))]
fn append_windows_arg(out: &mut Vec<u16>, arg: &[u16]) {
    const QUOTE: u16 = b'"' as u16;
    const BACKSLASH: u16 = b'\\' as u16;
    let needs_quotes = arg.is_empty()
        || arg
            .iter()
            .any(|&c| matches!(c, 0x20 | 0x09 | 0x0a | 0x0b) || c == QUOTE);
    if !needs_quotes {
        out.extend_from_slice(arg);
        return;
    }
    out.push(QUOTE);
    let mut backslashes = 0_usize;
    for &c in arg {
        if c == BACKSLASH {
            backslashes += 1;
            continue;
        }
        if c == QUOTE {
            out.extend(std::iter::repeat_n(BACKSLASH, backslashes * 2 + 1));
        } else {
            out.extend(std::iter::repeat_n(BACKSLASH, backslashes));
        }
        out.push(c);
        backslashes = 0;
    }
    out.extend(std::iter::repeat_n(BACKSLASH, backslashes * 2));
    out.push(QUOTE);
}

/// The NUL-terminated command line for `program` and `args`. The program is
/// always quoted (paths with spaces; a program name has no escape rules).
#[cfg_attr(not(windows), allow(dead_code))]
fn windows_command_line(program: &[u16], args: &[Vec<u16>]) -> Vec<u16> {
    let mut line = Vec::with_capacity(program.len() + 2 + args.iter().map(Vec::len).sum::<usize>());
    line.push(u16::from(b'"'));
    line.extend(program.iter().copied().filter(|&c| c != u16::from(b'"')));
    line.push(u16::from(b'"'));
    for arg in args {
        line.push(u16::from(b' '));
        append_windows_arg(&mut line, arg);
    }
    line.push(0);
    line
}

/// A `CREATE_UNICODE_ENVIRONMENT` block: this process's environment with
/// the command's changes (`None` removes a variable), keys compared without
/// case as Windows does, sorted, `KEY=VALUE\0…\0`.
#[cfg_attr(not(windows), allow(dead_code))]
fn environment_block(
    base: Vec<(Vec<u16>, Vec<u16>)>,
    overrides: Vec<(Vec<u16>, Option<Vec<u16>>)>,
) -> Vec<u16> {
    let fold = |key: &[u16]| String::from_utf16_lossy(key).to_uppercase();
    let mut vars: std::collections::BTreeMap<String, (Vec<u16>, Vec<u16>)> = base
        .into_iter()
        .filter(|(key, _)| !key.is_empty())
        .map(|(key, value)| (fold(&key), (key, value)))
        .collect();
    for (key, value) in overrides {
        match value {
            Some(value) => {
                vars.insert(fold(&key), (key, value));
            }
            None => {
                vars.remove(&fold(&key));
            }
        }
    }
    let mut block = Vec::new();
    for (key, value) in vars.into_values() {
        block.extend_from_slice(&key);
        block.push(u16::from(b'='));
        block.extend_from_slice(&value);
        block.push(0);
    }
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    block
}

#[cfg(windows)]
mod win {
    use super::{Detach, DetachedChild, DetachedStderr};
    use std::ffi::OsStr;
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::AsRawHandle;
    use std::process::{Command, ExitStatus};
    use windows_sys::Win32::Foundation::{
        CloseHandle, DUPLICATE_CLOSE_SOURCE, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE,
        INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
    };
    use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TokenSessionId};
    use windows_sys::Win32::System::JobObjects::{
        IsProcessInJob, JOBOBJECT_BASIC_LIMIT_INFORMATION, JobObjectBasicLimitInformation,
        QueryInformationJobObject,
    };
    use windows_sys::Win32::System::Threading::{
        CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, CREATE_UNICODE_ENVIRONMENT,
        CreateProcessW, DETACHED_PROCESS, DeleteProcThreadAttributeList,
        EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess, GetExitCodeProcess,
        InitializeProcThreadAttributeList, OpenProcess, OpenProcessToken,
        PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_PARENT_PROCESS,
        PROCESS_CREATE_PROCESS, PROCESS_DUP_HANDLE, PROCESS_INFORMATION,
        PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW, STARTF_USESTDHANDLES,
        STARTUPINFOEXW, STARTUPINFOW, TerminateProcess, UpdateProcThreadAttribute,
        WaitForSingleObject,
    };

    /// A kernel handle closed on drop.
    pub(in crate::ipc) struct Owned(HANDLE);

    impl Drop for Owned {
        fn drop(&mut self) {
            if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
                // SAFETY: the handle is owned by this value and closed once.
                unsafe { CloseHandle(self.0) };
            }
        }
    }

    pub(super) fn try_wait(process: &Owned) -> io::Result<Option<ExitStatus>> {
        use std::os::windows::process::ExitStatusExt;
        // SAFETY: a valid process handle; a zero timeout never blocks.
        if unsafe { WaitForSingleObject(process.0, 0) } != WAIT_OBJECT_0 {
            return Ok(None);
        }
        let mut code = 0_u32;
        // SAFETY: valid process handle and a writable out-pointer.
        if unsafe { GetExitCodeProcess(process.0, &raw mut code) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Some(ExitStatus::from_raw(code)))
    }

    pub(super) fn terminate(process: &Owned) -> io::Result<()> {
        // SAFETY: the handle came from CreateProcessW with full access.
        if unsafe { TerminateProcess(process.0, 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn wide(s: &OsStr) -> Vec<u16> {
        s.encode_wide().collect()
    }

    pub(super) fn spawn(cmd: &Command, stderr: DetachedStderr) -> io::Result<DetachedChild> {
        let program = wide(cmd.get_program());
        let args: Vec<Vec<u16>> = cmd.get_args().map(wide).collect();
        let command_line = super::windows_command_line(&program, &args);
        let env = super::environment_block(
            std::env::vars_os()
                .map(|(k, v)| (wide(&k), wide(&v)))
                .collect(),
            cmd.get_envs()
                .map(|(k, v)| (wide(k), v.map(wide)))
                .collect(),
        );
        let cwd = cmd.get_current_dir().map(|dir| {
            let mut w = wide(dir.as_os_str());
            w.push(0);
            w
        });
        let null_device = || {
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open("NUL")
        };
        let stdin = null_device()?;
        let stdout = null_device()?;
        let stderr = match stderr {
            DetachedStderr::Null => null_device()?,
            DetachedStderr::File(file) => file,
        };
        let std_handles: [HANDLE; 3] = [
            stdin.as_raw_handle(),
            stdout.as_raw_handle(),
            stderr.as_raw_handle(),
        ];

        let (in_job, limit_flags) = job_limits();
        let broker = if in_job { session_broker() } else { None };
        let mut last_error = None;
        for attempt in super::detach_attempts(in_job, limit_flags, broker.is_some()) {
            let parent = if attempt == Detach::Reparent {
                broker.as_ref()
            } else {
                None
            };
            match create(
                &command_line,
                &env,
                cwd.as_deref(),
                std_handles,
                parent,
                attempt,
            ) {
                Ok(child) => {
                    if attempt == Detach::StayInJob {
                        eprintln!(
                            "lean-ctx: started {} (PID {}) inside the caller's job object: the job forbids breakaway and this session has no Explorer to start it from, so it ends with that job (and `Start-Process -Wait` waits for it).",
                            cmd.get_program().to_string_lossy(),
                            child.pid
                        );
                    }
                    return Ok(child);
                }
                Err(error) => last_error = Some(error),
            }
        }
        Err(last_error.unwrap_or_else(|| io::Error::other("no spawn attempt")))
    }

    /// Whether this process is in a job, and that job's limit flags (0 when
    /// they cannot be read: breakaway is then treated as forbidden).
    fn job_limits() -> (bool, u32) {
        let mut in_job = 0;
        // SAFETY: pseudo handle of this process, null job (= any job), and a
        // writable BOOL.
        let ok =
            unsafe { IsProcessInJob(GetCurrentProcess(), std::ptr::null_mut(), &raw mut in_job) };
        if ok == 0 {
            // Unknown: keep the earlier behaviour (try breakaway first).
            return (true, super::JOB_OBJECT_LIMIT_BREAKAWAY_OK);
        }
        if in_job == 0 {
            return (false, 0);
        }
        // SAFETY: plain-data struct; zeroed is a valid value.
        let mut info: JOBOBJECT_BASIC_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        let size =
            u32::try_from(std::mem::size_of::<JOBOBJECT_BASIC_LIMIT_INFORMATION>()).unwrap_or(0);
        // SAFETY: null job = this process's job; `info` is writable for `size`.
        let ok = unsafe {
            QueryInformationJobObject(
                std::ptr::null_mut(),
                JobObjectBasicLimitInformation,
                (&raw mut info).cast(),
                size,
                std::ptr::null_mut(),
            )
        };
        (true, if ok == 0 { 0 } else { info.LimitFlags })
    }

    fn token_session(process: HANDLE) -> Option<u32> {
        let mut token = std::ptr::null_mut();
        // SAFETY: `token` is a writable slot; it is closed below.
        if unsafe { OpenProcessToken(process, TOKEN_QUERY, &raw mut token) } == 0 {
            return None;
        }
        let token = Owned(token);
        let mut session = 0_u32;
        let mut len = 0_u32;
        // SAFETY: TokenSessionId writes one u32 into `session`.
        let ok = unsafe {
            GetTokenInformation(
                token.0,
                TokenSessionId,
                (&raw mut session).cast(),
                4,
                &raw mut len,
            )
        };
        (ok != 0).then_some(session)
    }

    fn image_path(process: HANDLE) -> Option<String> {
        let mut path = vec![0_u16; 32_768];
        let mut len = u32::try_from(path.len()).ok()?;
        // SAFETY: `path` is writable for `len` UTF-16 units.
        if unsafe { QueryFullProcessImageNameW(process, 0, path.as_mut_ptr(), &raw mut len) } == 0 {
            return None;
        }
        Some(String::from_utf16_lossy(path.get(..len as usize)?))
    }

    fn explorer_path() -> Option<String> {
        use windows_sys::Win32::System::SystemInformation::GetSystemWindowsDirectoryW;
        let mut buf = vec![0_u16; 1024];
        // SAFETY: `buf` is writable for its length.
        let len =
            unsafe { GetSystemWindowsDirectoryW(buf.as_mut_ptr(), u32::try_from(buf.len()).ok()?) }
                as usize;
        if len == 0 || len >= buf.len() {
            return None;
        }
        let dir = String::from_utf16_lossy(&buf[..len]);
        Some(format!("{}\\explorer.exe", dir.trim_end_matches('\\')))
    }

    /// This user's Explorer in this session, outside every job, opened with
    /// the rights a parent needs. `None` when there is none.
    fn session_broker() -> Option<Owned> {
        use windows_sys::Win32::System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
            TH32CS_SNAPPROCESS,
        };
        // SAFETY: pseudo handle of this process.
        let me = unsafe { GetCurrentProcess() };
        let my_session = token_session(me)?;
        let my_sid = crate::ipc::process::windows_token_user_sid(me)?;
        let expected = explorer_path()?;

        // SAFETY: plain flags; the snapshot is closed by `Owned`.
        let snapshot = Owned(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) });
        if snapshot.0.is_null() || snapshot.0 == INVALID_HANDLE_VALUE {
            return None;
        }
        // SAFETY: plain data; zeroed is a valid initial state.
        let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
        entry.dwSize = u32::try_from(std::mem::size_of::<PROCESSENTRY32W>()).ok()?;
        // SAFETY: valid snapshot and a sized, writable entry.
        let mut more = unsafe { Process32FirstW(snapshot.0, &raw mut entry) } != 0;
        while more {
            let len = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            let name = String::from_utf16_lossy(&entry.szExeFile[..len]);
            if name.eq_ignore_ascii_case("explorer.exe")
                && let Some(broker) =
                    usable_broker(entry.th32ProcessID, my_session, &my_sid, &expected)
            {
                return Some(broker);
            }
            // SAFETY: as above.
            more = unsafe { Process32NextW(snapshot.0, &raw mut entry) } != 0;
        }
        None
    }

    fn usable_broker(pid: u32, session: u32, sid: &[u8], expected_path: &str) -> Option<Owned> {
        let access =
            PROCESS_CREATE_PROCESS | PROCESS_DUP_HANDLE | PROCESS_QUERY_LIMITED_INFORMATION;
        // SAFETY: integers only; a null result is rejected.
        let handle = unsafe { OpenProcess(access, 0, pid) };
        if handle.is_null() {
            return None;
        }
        let handle = Owned(handle);
        let mut in_job = 1;
        // SAFETY: valid handle with query rights and a writable BOOL.
        let job_ok =
            unsafe { IsProcessInJob(handle.0, std::ptr::null_mut(), &raw mut in_job) } != 0;
        let usable = job_ok
            && in_job == 0
            && token_session(handle.0) == Some(session)
            && crate::ipc::process::windows_token_user_sid(handle.0).as_deref() == Some(sid)
            && image_path(handle.0).is_some_and(|path| path.eq_ignore_ascii_case(expected_path));
        usable.then_some(handle)
    }

    /// Copies of the std handles, inheritable, in the process that will be
    /// the child's parent; closed there when dropped.
    struct InheritedHandles {
        owner: HANDLE,
        handles: Vec<HANDLE>,
    }

    impl Drop for InheritedHandles {
        fn drop(&mut self) {
            for &handle in &self.handles {
                // SAFETY: `handle` was duplicated into `owner` and is closed
                // there exactly once; no target is requested.
                unsafe {
                    DuplicateHandle(
                        self.owner,
                        handle,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        0,
                        0,
                        DUPLICATE_CLOSE_SOURCE,
                    );
                }
            }
        }
    }

    /// An initialized `PROC_THREAD_ATTRIBUTE_LIST`, deleted on drop.
    struct AttributeList {
        buf: Vec<u64>,
    }

    impl AttributeList {
        fn new(count: u32) -> io::Result<Self> {
            let mut size = 0_usize;
            // SAFETY: a null list only reports the required size.
            unsafe {
                InitializeProcThreadAttributeList(std::ptr::null_mut(), count, 0, &raw mut size)
            };
            if size == 0 {
                return Err(io::Error::last_os_error());
            }
            let mut buf = vec![0_u64; size.div_ceil(8)];
            // SAFETY: `buf` is writable for at least `size` bytes.
            if unsafe {
                InitializeProcThreadAttributeList(buf.as_mut_ptr().cast(), count, 0, &raw mut size)
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(Self { buf })
        }

        fn ptr(&mut self) -> *mut core::ffi::c_void {
            self.buf.as_mut_ptr().cast()
        }

        /// SAFETY: `value` must stay valid and unmoved until the list is
        /// dropped (it is read by `CreateProcessW`).
        unsafe fn set(
            &mut self,
            attribute: u32,
            value: *const core::ffi::c_void,
            size: usize,
        ) -> io::Result<()> {
            // SAFETY: an initialized list; caller guarantees `value`.
            let ok = unsafe {
                UpdateProcThreadAttribute(
                    self.ptr(),
                    0,
                    attribute as usize,
                    value,
                    size,
                    std::ptr::null_mut(),
                    std::ptr::null(),
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
    }

    impl Drop for AttributeList {
        fn drop(&mut self) {
            // SAFETY: the list was initialized in `new`.
            unsafe { DeleteProcThreadAttributeList(self.ptr()) };
        }
    }

    fn create(
        command_line: &[u16],
        env: &[u16],
        cwd: Option<&[u16]>,
        std_handles: [HANDLE; 3],
        parent: Option<&Owned>,
        attempt: Detach,
    ) -> io::Result<DetachedChild> {
        // SAFETY: pseudo handle of this process; needs no close.
        let me = unsafe { GetCurrentProcess() };
        // With a parent attribute the child inherits from that parent, so
        // its handles must live in the parent's table.
        let owner = parent.map_or(me, |p| p.0);
        let mut inherited = InheritedHandles {
            owner,
            handles: Vec::with_capacity(3),
        };
        for handle in std_handles {
            let mut copy = std::ptr::null_mut();
            // SAFETY: `handle` is open in this process; `copy` is writable.
            let ok = unsafe {
                DuplicateHandle(
                    me,
                    handle,
                    owner,
                    &raw mut copy,
                    0,
                    1,
                    DUPLICATE_SAME_ACCESS,
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            inherited.handles.push(copy);
        }

        let mut attributes = AttributeList::new(if parent.is_some() { 2 } else { 1 })?;
        // SAFETY: `inherited.handles` and `parent_handle` outlive
        // `attributes` (declared before it is dropped at the end of scope,
        // and neither is moved).
        unsafe {
            attributes.set(
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
                inherited.handles.as_ptr().cast(),
                std::mem::size_of_val(inherited.handles.as_slice()),
            )?;
        }
        let parent_handle: HANDLE = parent.map_or(std::ptr::null_mut(), |p| p.0);
        if parent.is_some() {
            // SAFETY: as above; the value is a HANDLE.
            unsafe {
                attributes.set(
                    PROC_THREAD_ATTRIBUTE_PARENT_PROCESS,
                    (&raw const parent_handle).cast(),
                    std::mem::size_of::<HANDLE>(),
                )?;
            }
        }

        // SAFETY: plain data; zeroed is valid before the fields are set.
        let mut startup: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
        startup.StartupInfo.cb = u32::try_from(std::mem::size_of::<STARTUPINFOEXW>()).unwrap_or(0);
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdInput = inherited.handles[0];
        startup.StartupInfo.hStdOutput = inherited.handles[1];
        startup.StartupInfo.hStdError = inherited.handles[2];
        startup.lpAttributeList = attributes.ptr();

        let mut flags = DETACHED_PROCESS
            | CREATE_NEW_PROCESS_GROUP
            | CREATE_UNICODE_ENVIRONMENT
            | EXTENDED_STARTUPINFO_PRESENT;
        if attempt == Detach::Breakaway {
            flags |= CREATE_BREAKAWAY_FROM_JOB;
        }
        // CreateProcessW may write to the command line buffer.
        let mut line = command_line.to_vec();
        // SAFETY: zeroed out-struct.
        let mut info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: every pointer refers to live, NUL-terminated or sized data
        // owned by this frame; the handle list and parent are in `attributes`.
        let ok = unsafe {
            CreateProcessW(
                std::ptr::null(),
                line.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                1,
                flags,
                env.as_ptr().cast(),
                cwd.map_or(std::ptr::null(), <[u16]>::as_ptr),
                (&raw const startup).cast::<STARTUPINFOW>(),
                &raw mut info,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        drop(Owned(info.hThread));
        Ok(DetachedChild {
            pid: info.dwProcessId,
            process: Owned(info.hProcess),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    fn arg(s: &str) -> String {
        let mut out = Vec::new();
        append_windows_arg(&mut out, &w(s));
        String::from_utf16_lossy(&out)
    }

    #[test]
    fn windows_args_round_trip_msvc_quoting() {
        assert_eq!(arg("serve"), "serve");
        assert_eq!(arg("--port=4444"), "--port=4444");
        assert_eq!(arg(""), "\"\"");
        assert_eq!(arg("a b"), "\"a b\"");
        assert_eq!(
            arg("C:\\Program Files\\x\\"),
            "\"C:\\Program Files\\x\\\\\""
        );
        assert_eq!(arg("say \"hi\""), "\"say \\\"hi\\\"\"");
        assert_eq!(arg("a\\\"b c"), "\"a\\\\\\\"b c\"");
        assert_eq!(arg("C:\\no\\space"), "C:\\no\\space");
    }

    #[test]
    fn command_line_quotes_the_program_and_ends_with_nul() {
        let line = windows_command_line(
            &w("C:\\Program Files\\lean-ctx.exe"),
            &[w("proxy"), w("start")],
        );
        assert_eq!(line.last(), Some(&0));
        assert_eq!(
            String::from_utf16_lossy(&line[..line.len() - 1]),
            "\"C:\\Program Files\\lean-ctx.exe\" proxy start"
        );
    }

    #[test]
    fn environment_block_applies_changes_case_insensitively() {
        let block = environment_block(
            vec![
                (w("Path"), w("C:\\bin")),
                (w("SECRET"), w("x")),
                (w("b"), w("1")),
            ],
            vec![
                (w("PATH"), Some(w("D:\\bin"))),
                (w("secret"), None),
                (w("LEAN_CTX_TCC_STANDALONE"), Some(w("1"))),
            ],
        );
        let text = String::from_utf16_lossy(&block);
        assert_eq!(text, "b=1\0LEAN_CTX_TCC_STANDALONE=1\0PATH=D:\\bin\0\0");
        assert_eq!(environment_block(Vec::new(), Vec::new()), vec![0, 0]);
    }

    #[test]
    fn job_without_breakaway_uses_the_broker_then_stays_announced() {
        assert_eq!(detach_attempts(false, 0, true), vec![Detach::Plain]);
        // PowerShell's Start-Process / Start-Job job: LimitFlags = 0.
        assert_eq!(
            detach_attempts(true, 0, true),
            vec![Detach::Reparent, Detach::StayInJob]
        );
        assert_eq!(detach_attempts(true, 0, false), vec![Detach::StayInJob]);
        // sshd's job: KILL_ON_JOB_CLOSE | BREAKAWAY_OK (0x2800).
        assert_eq!(
            detach_attempts(true, 0x2800, true),
            vec![Detach::Breakaway, Detach::Reparent, Detach::StayInJob]
        );
        assert_eq!(
            detach_attempts(true, JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK, false),
            vec![Detach::Breakaway, Detach::StayInJob]
        );
    }

    /// A detached service never holds the caller's stdout (#2176 d).
    #[cfg(unix)]
    #[test]
    fn detached_spawn_does_not_hold_the_callers_stdout() {
        let mut sleeper = Command::new("sleep");
        sleeper.arg("30").stdout(std::process::Stdio::inherit());
        let mut child = spawn_detached(&mut sleeper, DetachedStderr::Null).expect("spawn sleep");
        let pid = child.id();
        let out = Command::new("lsof")
            .args(["-a", "-p", &pid.to_string(), "-d", "1", "-Fn"])
            .output();
        let _ = child.kill();
        if let Ok(out) = out
            && out.status.success()
        {
            let text = String::from_utf8_lossy(&out.stdout);
            assert!(text.contains("/dev/null"), "fd 1 of the service: {text}");
        }
    }

    /// A detached service leads its own process group, so that the caller's
    /// group (a terminal job, a script, an app) neither signals it nor counts
    /// it among the caller's protected processes.
    #[cfg(unix)]
    #[test]
    fn detached_spawn_leads_its_own_process_group() {
        let mut sleeper = Command::new("sleep");
        sleeper.arg("30");
        let mut child = spawn_detached(&mut sleeper, DetachedStderr::Null).expect("spawn sleep");
        let pid = child.id();
        let pid_t = libc::pid_t::try_from(pid).expect("pid fits pid_t");
        // SAFETY: getpgid takes a pid and only reads process state.
        let child_pgid = unsafe { libc::getpgid(pid_t) };
        // SAFETY: getpgrp() takes no arguments and cannot fail.
        let own_pgid = unsafe { libc::getpgrp() };
        let _ = child.kill();
        assert_eq!(child_pgid, pid_t, "the service leads its own group");
        assert_ne!(child_pgid, own_pgid, "the service left the caller's group");
    }

    /// The raw Windows spawn runs the program with its arguments, sets the
    /// environment change, and reports the exit code.
    #[cfg(windows)]
    #[test]
    fn windows_detached_spawn_runs_and_reports_exit() {
        let mut cmd = Command::new("cmd.exe");
        cmd.args(["/c", "if \"%LCX_T%\"==\"ok\" (exit 7) else (exit 3)"])
            .env("LCX_T", "ok");
        let mut child = spawn_detached(&mut cmd, DetachedStderr::Null).expect("spawn cmd");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            if let Some(status) = child.try_wait().expect("try_wait") {
                assert_eq!(status.code(), Some(7));
                break;
            }
            assert!(std::time::Instant::now() < deadline, "cmd did not exit");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
}
