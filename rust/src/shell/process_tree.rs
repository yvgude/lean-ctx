// SPDX-License-Identifier: Apache-2.0
//! Whole-tree ownership of a spawned command (#1920).
//!
//! A timeout or cancel has to end every process the command started, not just
//! the shell leader. On Unix callers already isolate the child in its own
//! process group, and `killpg` reaches every descendant. Windows has no such
//! group: `Child::kill` ends the shell alone, and a `python -` it started keeps
//! running detached — spinning a core for hours when its script loops.
//!
//! A job object is the Windows equivalent. The child is spawned suspended,
//! assigned to a private job and only then resumed, so nothing it starts can
//! escape. `TerminateJobObject` ends the whole tree at once, and
//! `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` ends it when lean-ctx itself dies before
//! it could clean up.

use std::process::{Child, Command};

/// The process tree of one spawned command. Create it with [`ProcessTree::start`]
/// right after spawning a command prepared by [`ProcessTree::prepare`].
pub(crate) struct ProcessTree {
    #[cfg(windows)]
    job: Option<job::ProcessJob>,
}

impl ProcessTree {
    /// Configure `cmd` so its tree can be contained. On Windows this spawns the
    /// child suspended — [`ProcessTree::start`] must follow every successful
    /// spawn. It sets the creation flags, so callers must not set their own.
    pub(crate) fn prepare(cmd: &mut Command) {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(windows_sys::Win32::System::Threading::CREATE_SUSPENDED);
        }
        #[cfg(not(windows))]
        let _ = cmd;
    }

    /// Take ownership of the tree of a child spawned from a prepared command
    /// and let it run.
    ///
    /// A job that cannot be created or assigned is not fatal: the command still
    /// runs, and [`ProcessTree::kill`] falls back to `taskkill /T`. A child that
    /// cannot be resumed is killed and reported, never left suspended.
    pub(crate) fn start(child: &mut Child) -> Result<Self, String> {
        #[cfg(windows)]
        {
            let job = match job::ProcessJob::assign(child) {
                Ok(job) => Some(job),
                Err(error) => {
                    tracing::warn!("process tree not contained, falling back to taskkill: {error}");
                    None
                }
            };
            if let Err(error) = job::resume(child.id()) {
                if let Some(job) = &job {
                    job.terminate();
                }
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
            Ok(Self { job })
        }
        #[cfg(not(windows))]
        {
            let _ = child;
            Ok(Self {})
        }
    }

    /// Start `command` as one contained call (B7): on Windows its tree goes
    /// into a private kill-on-close job before its first instruction runs
    /// (spawned suspended, assigned, resumed). The caller holds the returned
    /// tree for the call and drops it afterwards, which ends whatever the
    /// call left running; the tree ends as well should this process die
    /// first. A job that cannot be created leaves the previous behaviour
    /// (`taskkill /T` on a limit). A child that cannot be resumed was killed
    /// without having run, and the error says so. (Unix calls start behind
    /// their guard instead, `shell::containment::start_behind_guard`.)
    #[cfg(not(unix))]
    pub(crate) fn spawn_contained(command: &mut Command) -> std::io::Result<(Child, Self)> {
        Self::prepare(command);
        let mut child = command.spawn()?;
        let tree = Self::start(&mut child).map_err(std::io::Error::other)?;
        Ok((child, tree))
    }

    /// Kill the command and every descendant it started. Killing only the
    /// leader would leave grandchildren alive and the caller's pipe readers
    /// unable to reach EOF (#995).
    ///
    /// On Unix the child must lead its own process group (`setsid` or
    /// `process_group(0)`), which makes its pid the group id.
    pub(crate) fn kill(&self, child: &mut Child) {
        #[cfg(unix)]
        {
            let pgid = child.id() as libc::pid_t;
            if pgid > 0 {
                // SAFETY: killpg is a plain syscall; a stale group yields ESRCH.
                unsafe { libc::killpg(pgid, libc::SIGKILL) };
            }
        }
        #[cfg(windows)]
        match &self.job {
            Some(job) => job.terminate(),
            None => {
                let _ = std::process::Command::new("taskkill")
                    .args(["/F", "/T", "/PID", &child.id().to_string()])
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status();
            }
        }
        let _ = child.kill();
    }

    /// Hand the tree back after the command exited on its own.
    ///
    /// A process it deliberately left running (`start /b server`) survives, as
    /// it does on Unix, where an exited leader leaves its group alone. Dropping
    /// a `ProcessTree` without this — lean-ctx unwinding or dying mid-command —
    /// ends the whole tree instead.
    pub(crate) fn release(self) {
        #[cfg(windows)]
        if let Some(job) = &self.job {
            job.release();
        }
    }
}

/// The output of a started shell, wherever its pipes were made.
type ShellPipe = Option<Box<dyn std::io::Read + Send>>;

fn boxed(pipe: impl std::io::Read + Send + 'static) -> Box<dyn std::io::Read + Send> {
    Box::new(pipe)
}

/// A started MCP shell: the process, its output, and what the caller has to
/// show in front of the result, if anything.
pub(crate) struct StartedShell {
    pub(crate) child: ShellChild,
    pub(crate) stdout: ShellPipe,
    pub(crate) stderr: ShellPipe,
    /// A fixed, content-free line (see `shell::containment`).
    pub(crate) notice: Option<&'static str>,
}

/// The shell of one MCP shell call, whoever its parent is.
///
/// On Unix the parent is this process's spawn helper (`shell::containment`):
/// it holds the shell's session and ends it should this process die. Where
/// there is no helper — Windows, a host that is not `lean-ctx`, a helper that
/// could not be started — this process is the parent, as it always was.
pub(crate) enum ShellChild {
    Direct {
        child: Child,
        tree: ProcessTree,
    },
    #[cfg(unix)]
    Helped(crate::shell::containment::HelpedChild),
}

impl ShellChild {
    /// Starts the shell that `cmd` describes, in a session of its own on
    /// Unix, with its output piped. `cmd` must have been through
    /// [`ProcessTree::prepare`] and must not set its own standard streams.
    pub(crate) fn start(cmd: &mut Command) -> Result<StartedShell, String> {
        #[cfg(unix)]
        let notice = {
            use crate::shell::containment::{HelperSpawn, spawn_shell};
            match spawn_shell(cmd) {
                HelperSpawn::Helped {
                    child,
                    stdout,
                    stderr,
                } => {
                    return Ok(StartedShell {
                        child: Self::Helped(child),
                        stdout: Some(boxed(stdout)),
                        stderr: Some(boxed(stderr)),
                        notice: None,
                    });
                }
                HelperSpawn::Direct(notice) => notice,
            }
        };
        #[cfg(not(unix))]
        let notice = None;
        #[cfg(unix)]
        crate::shell::containment::lead_new_session(cmd);
        let mut child = cmd
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|error| error.to_string())?;
        let tree = ProcessTree::start(&mut child)?;
        let stdout = child.stdout.take().map(boxed);
        let stderr = child.stderr.take().map(boxed);
        Ok(StartedShell {
            child: Self::Direct { child, tree },
            stdout,
            stderr,
            notice,
        })
    }

    /// The shell's process id; on Unix also the id of the group it leads.
    pub(crate) fn id(&self) -> u32 {
        match self {
            Self::Direct { child, .. } => child.id(),
            #[cfg(unix)]
            Self::Helped(child) => child.id(),
        }
    }

    /// The exit status, if the shell has exited. Reaps a direct child.
    pub(crate) fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        match self {
            Self::Direct { child, .. } => child.try_wait(),
            #[cfg(unix)]
            Self::Helped(child) => child.try_wait(),
        }
    }

    /// Ends the shell and everything it started ([`ProcessTree::kill`]). The
    /// group is signalled by the shell's parent, while the shell is unreaped.
    pub(crate) fn kill(&mut self) {
        match self {
            Self::Direct { child, tree } => tree.kill(child),
            #[cfg(unix)]
            Self::Helped(child) => child.kill(),
        }
    }

    /// Waits for a shell that was just killed to be reaped.
    pub(crate) fn wait(&mut self) {
        match self {
            Self::Direct { child, .. } => {
                let _ = child.wait();
            }
            #[cfg(unix)]
            Self::Helped(child) => child.wait(),
        }
    }

    /// Hands the tree back after the command exited on its own
    /// ([`ProcessTree::release`]): what it left running stays.
    ///
    /// `Some`: the fixed notice for a release that could not be handed to
    /// the shell's helper — whether what the command left running is still
    /// running is then not known. The caller shows it with the result.
    pub(crate) fn release(self) -> Option<&'static str> {
        match self {
            Self::Direct { tree, .. } => {
                tree.release();
                None
            }
            #[cfg(unix)]
            Self::Helped(child) => {
                (!child.release()).then_some(crate::shell::containment::RELEASE_NOTICE)
            }
        }
    }

    /// The fixed notice for a shell whose helper went away — before it said
    /// whether the command was started, or before the shell's exit was
    /// known. The call's outcome is then not known in full. Never the case
    /// for a direct child.
    pub(crate) fn lost_notice(&self) -> Option<&'static str> {
        match self {
            Self::Direct { .. } => None,
            #[cfg(unix)]
            Self::Helped(child) => child.notice(),
        }
    }
}

/// Session containment on Windows (Q13/B7), the counterpart of the Unix
/// session starter (`shell::containment`).
///
/// An SDK asks for it with `LEAN_CTX_SESSION_CONTAINMENT=1` on an `engine …`
/// start. There is no starter process here: the Engine puts itself into a
/// kill-on-close job before it does anything else. From then on everything it
/// starts belongs to that job, and all of it ends when the Engine process
/// ends — on its own, because the SDK ended it, or because its input closed
/// after the SDK went away. Only then is the acknowledgement written, the
/// same first line as on Unix, so an SDK reports containment exactly when it
/// exists. A job that cannot be created or joined (a host job that forbids
/// nesting) writes nothing: the Engine runs as before and the SDK reports
/// `unavailable`.
///
/// The request is taken out of the environment first, as the starter does, so
/// no process the Engine starts sees or repeats it.
#[cfg(windows)]
pub(crate) fn contain_requested_session() {
    use std::io::Write;
    const SESSION_ENV: &str = "LEAN_CTX_SESSION_CONTAINMENT";
    const SESSION_ACK: &[u8] = b"{\"leanctx_session_containment\":1}\n";

    let mut args = std::env::args_os();
    let _program = args.next();
    if args.next().is_none_or(|first| first != "engine")
        || std::env::var_os(SESSION_ENV).is_none_or(|value| value != "1")
    {
        return;
    }
    // SAFETY: called first thing in `run`, before any other thread exists.
    unsafe { std::env::remove_var(SESSION_ENV) };
    let Ok(session) = job::ProcessJob::assign_current_process() else {
        return;
    };
    // The handle must live exactly as long as the process: it is never
    // dropped, and the system closes it when the process ends.
    std::mem::forget(session);
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(SESSION_ACK).and_then(|()| stdout.flush());
}

#[cfg(windows)]
pub(crate) mod job {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectExtendedLimitInformation, SetInformationJobObject,
    };

    /// A private job object whose processes die when its last handle closes.
    pub(crate) struct ProcessJob(HANDLE);

    impl ProcessJob {
        /// Create a kill-on-close job and assign `child` to it. The child should
        /// still be suspended so nothing it starts can escape the job.
        pub(crate) fn assign(child: &std::process::Child) -> Result<Self, String> {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::System::JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW,
            };

            // SAFETY: null attributes/name create a private job owned by Self.
            let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if handle.is_null() {
                return Err(format!(
                    "failed to create job object: {}",
                    std::io::Error::last_os_error()
                ));
            }
            let job = Self(handle);
            job.set_limit_flags(JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE)
                .map_err(|error| format!("failed to configure job object: {error}"))?;
            // SAFETY: Child owns a live process handle for the spawned process.
            let assigned = unsafe { AssignProcessToJobObject(job.0, child.as_raw_handle()) };
            if assigned == 0 {
                return Err(format!(
                    "failed to assign process {} to its job: {}",
                    child.id(),
                    std::io::Error::last_os_error()
                ));
            }
            Ok(job)
        }

        /// Create a kill-on-close job and put THIS process into it. Every
        /// process it starts from now on is a member (a per-call job of
        /// [`ProcessJob::assign`] nests inside), and all of them end when the
        /// last handle closes — which is when this process ends, since the
        /// handle is private and not inheritable.
        pub(crate) fn assign_current_process() -> Result<Self, String> {
            use windows_sys::Win32::System::JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW,
            };
            use windows_sys::Win32::System::Threading::GetCurrentProcess;

            // SAFETY: null attributes/name create a private job owned by Self.
            let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if handle.is_null() {
                return Err(format!(
                    "failed to create job object: {}",
                    std::io::Error::last_os_error()
                ));
            }
            let job = Self(handle);
            job.set_limit_flags(JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE)
                .map_err(|error| format!("failed to configure job object: {error}"))?;
            // SAFETY: the pseudo-handle of the current process is always valid.
            let assigned = unsafe { AssignProcessToJobObject(job.0, GetCurrentProcess()) };
            if assigned == 0 {
                return Err(format!(
                    "failed to assign this process to its session job: {}",
                    std::io::Error::last_os_error()
                ));
            }
            Ok(job)
        }

        /// End every process in the job.
        pub(crate) fn terminate(&self) {
            // SAFETY: self.0 is a live job handle owned by this instance.
            unsafe {
                windows_sys::Win32::System::JobObjects::TerminateJobObject(self.0, 1);
            }
        }

        /// Stop killing the job's processes when the handle closes.
        pub(crate) fn release(&self) {
            if let Err(error) = self.set_limit_flags(0) {
                tracing::warn!("failed to release job object: {error}");
            }
        }

        fn set_limit_flags(&self, flags: u32) -> std::io::Result<()> {
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = flags;
            // SAFETY: limits has the exact structure and size this info class requires.
            let configured = unsafe {
                SetInformationJobObject(
                    self.0,
                    JobObjectExtendedLimitInformation,
                    std::ptr::from_ref(&limits).cast(),
                    std::mem::size_of_val(&limits) as u32,
                )
            };
            if configured == 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        }
    }

    impl Drop for ProcessJob {
        fn drop(&mut self) {
            // SAFETY: this instance exclusively owns the CreateJobObjectW handle.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    /// Resume every thread of a process spawned with `CREATE_SUSPENDED`.
    pub(crate) fn resume(process_id: u32) -> Result<(), String> {
        use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
        use windows_sys::Win32::System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
        };
        use windows_sys::Win32::System::Threading::{
            OpenThread, ResumeThread, THREAD_SUSPEND_RESUME,
        };

        // SAFETY: snapshot handle is closed on every return path below.
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(format!(
                "failed to inspect suspended process {process_id} threads: {}",
                std::io::Error::last_os_error()
            ));
        }
        let mut entry = THREADENTRY32 {
            dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
            ..THREADENTRY32::default()
        };
        // SAFETY: snapshot and entry are valid for ToolHelp thread enumeration.
        let mut has_entry = unsafe { Thread32First(snapshot, &raw mut entry) } != 0;
        let mut resumed = false;
        let result = loop {
            if !has_entry {
                break if resumed {
                    Ok(())
                } else {
                    Err(format!(
                        "suspended process {process_id} exposed no resumable thread"
                    ))
                };
            }
            if entry.th32OwnerProcessID == process_id {
                // SAFETY: entry names a thread owned by the suspended child process.
                let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
                if thread.is_null() {
                    break Err(format!(
                        "failed to open suspended process {process_id} thread: {}",
                        std::io::Error::last_os_error()
                    ));
                }
                // SAFETY: thread is a live handle with THREAD_SUSPEND_RESUME access.
                let resume_result = unsafe { ResumeThread(thread) };
                // SAFETY: this scope owns the OpenThread handle.
                unsafe { CloseHandle(thread) };
                if resume_result == u32::MAX {
                    break Err(format!(
                        "failed to resume process {process_id} thread: {}",
                        std::io::Error::last_os_error()
                    ));
                }
                resumed = true;
            }
            // SAFETY: snapshot and entry remain valid until CloseHandle below.
            has_entry = unsafe { Thread32Next(snapshot, &raw mut entry) } != 0;
        };
        // SAFETY: this scope owns the ToolHelp snapshot handle.
        unsafe { CloseHandle(snapshot) };
        result
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::ProcessTree;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    fn alive(pid: i32) -> bool {
        // SAFETY: signal 0 only probes whether the pid exists.
        unsafe { libc::kill(pid, 0) == 0 }
    }

    #[test]
    fn kill_ends_grandchildren_not_just_the_shell() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "sleep 30 & echo $!; wait"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .process_group(0);
        ProcessTree::prepare(&mut cmd);
        let mut child = cmd.spawn().expect("spawn sh");
        let tree = ProcessTree::start(&mut child).expect("start tree");

        let mut line = String::new();
        std::io::BufRead::read_line(
            &mut std::io::BufReader::new(child.stdout.take().expect("stdout")),
            &mut line,
        )
        .expect("read grandchild pid");
        let grandchild: i32 = line.trim().parse().expect("grandchild pid");
        assert!(
            alive(grandchild),
            "grandchild must be running before the kill"
        );

        tree.kill(&mut child);
        let _ = child.wait();
        let deadline = Instant::now() + Duration::from_secs(5);
        while alive(grandchild) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!alive(grandchild), "kill must reach the shell's grandchild");
    }

    #[test]
    fn release_after_normal_exit_is_a_no_op() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "exit 3"]).process_group(0);
        ProcessTree::prepare(&mut cmd);
        let mut child = cmd.spawn().expect("spawn sh");
        let tree = ProcessTree::start(&mut child).expect("start tree");
        let status = child.wait().expect("wait");
        tree.release();
        assert_eq!(status.code(), Some(3));
    }
}
