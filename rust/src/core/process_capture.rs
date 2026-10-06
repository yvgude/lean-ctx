// SPDX-License-Identifier: Apache-2.0

//! Captured execution with bounded child waiting and process-tree cleanup.
//!
//! Uses private temporary files instead of pipes, so inherited output handles
//! cannot keep reader threads alive. Unix cleanup covers the dedicated process
//! group; deliberately detached descendants are outside that cleanup boundary.
//! Windows children are assigned to a kill-on-close Job Object before resuming.

#[cfg(windows)]
mod secret_windows;
#[cfg(windows)]
pub(crate) use secret_windows::read_secret_stdout;

pub(crate) struct CapturedOutput {
    pub(crate) output: std::process::Output,
    pub(crate) timed_out: bool,
    pub(crate) cancelled: bool,
}

#[cfg(test)]
#[cfg_attr(not(unix), allow(dead_code))] // its only consumer is a cfg(unix) test
pub(crate) fn run_with_timeout(
    command: &mut std::process::Command,
    timeout: std::time::Duration,
) -> Result<CapturedOutput, String> {
    wait_for_capture(spawn_capture(command)?, timeout)
}

/// Bound captured bytes without allowing inherited handles to extend waiting.
/// Length checks stop oversized writers; temporary-file growth between polls
/// is not an operating-system disk quota. `None` preserves unlimited waiting.
pub(crate) fn run_with_output_limits(
    command: &mut std::process::Command,
    timeout: Option<std::time::Duration>,
    stdout_limit: usize,
    stderr_limit: usize,
) -> Result<CapturedOutput, String> {
    wait_for_capture_with_limits(
        spawn_capture(command)?,
        timeout,
        Some((stdout_limit as u64, stderr_limit as u64)),
        &mut || false,
    )
}

/// Use the same bounded capture and process-tree cleanup for cancellable work.
pub(crate) fn run_with_output_limits_cancellable(
    command: &mut std::process::Command,
    timeout: std::time::Duration,
    stdout_limit: usize,
    stderr_limit: usize,
    mut cancellation_requested: impl FnMut() -> bool,
) -> Result<CapturedOutput, String> {
    wait_for_capture_with_limits(
        spawn_capture(command)?,
        Some(timeout),
        Some((stdout_limit as u64, stderr_limit as u64)),
        &mut cancellation_requested,
    )
}

pub(crate) struct CapturedChild {
    child: std::process::Child,
    stdout: std::fs::File,
    stderr: std::fs::File,
    bin: String,
    cleanup: ProcessCleanup,
}

impl CapturedChild {
    pub(crate) fn program(&self) -> &str {
        &self.bin
    }
}

#[derive(Default)]
struct ProcessCleanup {
    #[cfg(windows)]
    job: Option<ProcessJob>,
}

#[cfg(windows)]
struct ProcessJob(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl Drop for ProcessJob {
    fn drop(&mut self) {
        // SAFETY: this instance exclusively owns the CreateJobObjectW handle.
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

pub(crate) fn spawn_capture(command: &mut std::process::Command) -> Result<CapturedChild, String> {
    use std::process::Stdio;

    let program = command.get_program().to_string_lossy().into_owned();
    let bin = program.as_str();
    let stdout = tempfile::tempfile()
        .map_err(|error| format!("failed to create command '{bin}' stdout capture: {error}"))?;
    let stderr = tempfile::tempfile()
        .map_err(|error| format!("failed to create command '{bin}' stderr capture: {error}"))?;
    command
        .stdout(Stdio::from(stdout.try_clone().map_err(|error| {
            format!("failed to clone command '{bin}' stdout capture: {error}")
        })?))
        .stderr(Stdio::from(stderr.try_clone().map_err(|error| {
            format!("failed to clone command '{bin}' stderr capture: {error}")
        })?));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(windows_sys::Win32::System::Threading::CREATE_SUSPENDED);
    }
    let mut child = command.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            format!("command '{bin}' not found in PATH")
        } else {
            format!("failed to run '{bin}': {e}")
        }
    })?;

    let cleanup = match process_cleanup(&child, bin) {
        Ok(cleanup) => cleanup,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
    };

    Ok(CapturedChild {
        child,
        stdout,
        stderr,
        bin: bin.to_owned(),
        cleanup,
    })
}

#[cfg(windows)]
fn process_cleanup(child: &std::process::Child, bin: &str) -> Result<ProcessCleanup, String> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };

    // SAFETY: null attributes/name create a private job owned by ProcessJob.
    let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if handle.is_null() {
        return Err(format!(
            "failed to create child job for '{bin}': {}",
            std::io::Error::last_os_error()
        ));
    }
    let job = ProcessJob(handle);
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    // SAFETY: limits has the exact structure and size required by this info class.
    let configured = unsafe {
        SetInformationJobObject(
            job.0,
            JobObjectExtendedLimitInformation,
            std::ptr::from_ref(&limits).cast(),
            std::mem::size_of_val(&limits) as u32,
        )
    };
    if configured == 0 {
        return Err(format!(
            "failed to configure child job for '{bin}': {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: Child owns a live process handle for the child process.
    let assigned = unsafe { AssignProcessToJobObject(job.0, child.as_raw_handle().cast()) };
    if assigned == 0 {
        return Err(format!(
            "failed to assign command '{bin}' to its job: {}",
            std::io::Error::last_os_error()
        ));
    }
    resume_process_threads(child.id(), bin)?;
    Ok(ProcessCleanup { job: Some(job) })
}

#[cfg(windows)]
fn resume_process_threads(process_id: u32, bin: &str) -> Result<(), String> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};

    // SAFETY: snapshot handle is closed on every return path below.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(format!(
            "failed to inspect suspended command '{bin}' threads: {}",
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
                    "suspended command '{bin}' exposed no resumable thread"
                ))
            };
        }
        if entry.th32OwnerProcessID == process_id {
            // SAFETY: entry names a thread owned by the suspended child process.
            let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
            if thread.is_null() {
                break Err(format!(
                    "failed to open suspended command '{bin}' thread: {}",
                    std::io::Error::last_os_error()
                ));
            }
            // SAFETY: thread is a live handle with THREAD_SUSPEND_RESUME access.
            let resume_result = unsafe { ResumeThread(thread) };
            // SAFETY: this scope owns the OpenThread handle.
            unsafe { CloseHandle(thread) };
            if resume_result == u32::MAX {
                break Err(format!(
                    "failed to resume command '{bin}' thread: {}",
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

#[cfg(not(windows))]
fn process_cleanup(_: &std::process::Child, _: &str) -> Result<ProcessCleanup, String> {
    Ok(ProcessCleanup::default())
}

pub(crate) fn wait_for_capture(
    child: CapturedChild,
    timeout: std::time::Duration,
) -> Result<CapturedOutput, String> {
    wait_for_capture_with_limits(child, Some(timeout), None, &mut || false)
}

fn wait_for_capture_with_limits(
    CapturedChild {
        mut child,
        mut stdout,
        mut stderr,
        bin,
        mut cleanup,
    }: CapturedChild,
    timeout: Option<std::time::Duration>,
    limits: Option<(u64, u64)>,
    cancellation_requested: &mut dyn FnMut() -> bool,
) -> Result<CapturedOutput, String> {
    const CLEANUP_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

    let started = std::time::Instant::now();
    let (status, timed_out, cancelled, cleanup_error) = loop {
        let state = child
            .try_wait()
            .map_err(|error| format!("failed to wait for '{bin}': {error}"))
            .and_then(|state| {
                if let Some((stdout_limit, stderr_limit)) = limits {
                    capture_length(&stdout, &bin, "stdout", Some(stdout_limit))?;
                    capture_length(&stderr, &bin, "stderr", Some(stderr_limit))?;
                }
                Ok(state)
            });
        match state {
            Err(wait_error) => {
                let cleanup_error = terminate_process_tree(&mut child, &bin, &mut cleanup)
                    .err()
                    .map(|error| format!("cleanup failed: {error}"));
                let reap_error = reap_child(&mut child, &bin, CLEANUP_GRACE)
                    .err()
                    .map(|error| format!("reap failed: {error}"));
                let details = [cleanup_error, reap_error]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join("; ");
                return Err(if details.is_empty() {
                    wait_error
                } else {
                    format!("{wait_error}; {details}")
                });
            }
            Ok(Some(status)) => {
                break (
                    status,
                    false,
                    false,
                    stop_remaining_process_group(&child, &bin, &mut cleanup).err(),
                );
            }
            Ok(None) if cancellation_requested() => {
                let cleanup_error = terminate_process_tree(&mut child, &bin, &mut cleanup).err();
                break (
                    reap_child(&mut child, &bin, CLEANUP_GRACE)?,
                    false,
                    true,
                    cleanup_error,
                );
            }
            Ok(None) if timeout.is_some_and(|timeout| started.elapsed() >= timeout) => {
                let cleanup_error = terminate_process_tree(&mut child, &bin, &mut cleanup).err();
                break (
                    reap_child(&mut child, &bin, CLEANUP_GRACE)?,
                    true,
                    false,
                    cleanup_error,
                );
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(10)),
        }
    };
    let stdout = read_capture(&mut stdout, &bin, "stdout", limits.map(|limits| limits.0))?;
    let stderr = read_capture(&mut stderr, &bin, "stderr", limits.map(|limits| limits.1))?;

    if let Some(error) = cleanup_error {
        return Err(error);
    }

    Ok(CapturedOutput {
        output: std::process::Output {
            status,
            stdout,
            stderr,
        },
        timed_out,
        cancelled,
    })
}

fn read_capture(
    capture: &mut std::fs::File,
    bin: &str,
    stream: &str,
    limit: Option<u64>,
) -> Result<Vec<u8>, String> {
    use std::io::{Read, Seek};

    capture
        .rewind()
        .map_err(|error| format!("failed to rewind command '{bin}' {stream}: {error}"))?;
    // Snapshot the length: a detached descendant cannot extend this read forever.
    let length = capture_length(capture, bin, stream, limit)?;
    let mut bytes = Vec::new();
    capture
        .take(length)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("failed to read command '{bin}' {stream}: {error}"))?;
    Ok(bytes)
}

fn capture_length(
    capture: &std::fs::File,
    bin: &str,
    stream: &str,
    limit: Option<u64>,
) -> Result<u64, String> {
    let length = capture
        .metadata()
        .map_err(|error| format!("failed to inspect command '{bin}' {stream}: {error}"))?
        .len();
    if let Some(limit) = limit
        && length > limit
    {
        return Err(format!(
            "command '{bin}' {stream} output exceeded the capture bound ({limit} bytes)"
        ));
    }
    Ok(length)
}

fn reap_child(
    child: &mut std::process::Child,
    bin: &str,
    timeout: std::time::Duration,
) -> Result<std::process::ExitStatus, String> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child
            .try_wait()
            .map_err(|error| format!("failed to reap command '{bin}': {error}"))?
        {
            Some(status) => return Ok(status),
            None if std::time::Instant::now() >= deadline => {
                return Err(format!("command '{bin}' did not exit after termination"));
            }
            None => std::thread::sleep(std::time::Duration::from_millis(10)),
        }
    }
}

#[cfg(unix)]
fn stop_remaining_process_group(
    child: &std::process::Child,
    bin: &str,
    _: &mut ProcessCleanup,
) -> Result<(), String> {
    let pgid = child.id() as libc::pid_t;
    if pgid <= 0 {
        return Err(format!(
            "failed to stop child process group '{bin}': invalid process group id {pgid}"
        ));
    }
    // SAFETY: the child was spawned as leader of its dedicated process group.
    if unsafe { libc::killpg(pgid, libc::SIGKILL) } == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(format!(
            "failed to stop child process group '{bin}': {error}"
        ))
    }
}

#[cfg(windows)]
fn stop_remaining_process_group(
    _: &std::process::Child,
    _: &str,
    cleanup: &mut ProcessCleanup,
) -> Result<(), String> {
    drop(cleanup.job.take());
    Ok(())
}

fn terminate_process_tree(
    child: &mut std::process::Child,
    bin: &str,
    cleanup: &mut ProcessCleanup,
) -> Result<(), String> {
    #[cfg(unix)]
    {
        match stop_remaining_process_group(child, bin, cleanup) {
            Ok(()) => return Ok(()),
            Err(group_error) => {
                child.kill().map_err(|error| {
                    format!("{group_error}; failed to stop direct formatter child '{bin}': {error}")
                })?;
                return Err(group_error);
            }
        }
    }
    #[cfg(windows)]
    {
        stop_remaining_process_group(child, bin, cleanup)?;
        return Ok(());
    }

    #[allow(unreachable_code)]
    child
        .kill()
        .map_err(|e| format!("failed to stop timed-out command '{bin}': {e}"))
}

#[cfg(test)]
mod bounded_capture_tests {
    use std::io::Write;

    #[test]
    fn capture_checks_length_before_reading_and_keeps_exact_boundary() {
        let mut file = tempfile::tempfile().expect("capture file");
        file.write_all(b"four").expect("capture bytes");
        let error = super::read_capture(&mut file, "test", "stdout", Some(3))
            .expect_err("oversized capture must be rejected");
        assert!(error.contains("output exceeded"));
        assert_eq!(
            super::read_capture(&mut file, "test", "stdout", Some(4)).unwrap(),
            b"four"
        );
        assert_eq!(
            super::read_capture(&mut file, "test", "stdout", None).unwrap(),
            b"four"
        );
    }

    #[test]
    fn capture_read_errors_are_not_silently_accepted_as_eof() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let mut file =
            std::fs::File::create(directory.path().join("write-only")).expect("write-only capture");
        file.write_all(b"four").expect("capture bytes");
        for limit in [None, Some(4)] {
            let error = super::read_capture(&mut file, "test", "stderr", limit)
                .expect_err("write-only file cannot supply readable output");
            assert!(error.contains("failed to read"), "{error}");
        }
    }
}
