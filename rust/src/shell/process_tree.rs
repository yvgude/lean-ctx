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
            let assigned = unsafe { AssignProcessToJobObject(job.0, child.as_raw_handle() as _) };
            if assigned == 0 {
                return Err(format!(
                    "failed to assign process {} to its job: {}",
                    child.id(),
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
        let mut has_entry = unsafe { Thread32First(snapshot, &mut entry) } != 0;
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
            has_entry = unsafe { Thread32Next(snapshot, &mut entry) } != 0;
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
