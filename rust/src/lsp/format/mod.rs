//! Formatter routing for `ctx_refactor action=reformat`: pick a formatter by
//! file extension, using built-in routing per extension.

/// The formatter selected for a file: either the IDE HTTP backend or an external
/// shell command (template with a `{file}` placeholder).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Formatter {
    Jetbrains,
    Command(String),
}

/// Pick the formatter for `abs_path` using built-in defaults per extension.
/// Extension match is case-insensitive; no extension or an unknown extension → `Jetbrains`.
pub fn resolve_formatter(abs_path: &str) -> Formatter {
    let ext = std::path::Path::new(abs_path)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    builtin_default(&ext)
}

/// Built-in routing when the config has no entry for this extension.
fn builtin_default(ext: &str) -> Formatter {
    match ext {
        "rs" => Formatter::Command("rustfmt {file}".to_string()),
        _ => Formatter::Jetbrains,
    }
}

/// The binary name of a command template, for the `via <name>` output label.
pub fn command_label(template: &str) -> &str {
    template.split_whitespace().next().unwrap_or("formatter")
}

/// Split a command template into argv, substituting the `{file}` placeholder with
/// `abs_path`. `{file}` may be a standalone token or embedded in a token. If no
/// placeholder is present, `abs_path` is appended as the final argument. The path
/// is always a single argv element (spaces in the path are preserved).
pub fn build_argv(template: &str, abs_path: &str) -> Vec<String> {
    let mut argv: Vec<String> = Vec::new();
    let mut saw_placeholder = false;
    for tok in template.split_whitespace() {
        if tok == "{file}" {
            argv.push(abs_path.to_string());
            saw_placeholder = true;
        } else if tok.contains("{file}") {
            argv.push(tok.replace("{file}", abs_path));
            saw_placeholder = true;
        } else {
            argv.push(tok.to_string());
        }
    }
    if !saw_placeholder {
        argv.push(abs_path.to_string());
    }
    argv
}

/// Run an external formatter command on `abs_path` with cwd `project_root` (so
/// tool config like `rustfmt.toml` is discovered). Returns `Err` with a clear
/// message if the binary is missing or the command exits non-zero.
///
/// Formatter commands are trusted local executables. LeanCTX terminates the
/// formatter and ordinary descendants through a Unix process group or Windows
/// Job Object; a deliberately detached Unix process (`setsid`) is outside this
/// cooperative cleanup boundary.
pub fn run_command_formatter(
    template: &str,
    abs_path: &str,
    project_root: &str,
) -> Result<(), String> {
    run_command_formatter_with_timeout(
        template,
        abs_path,
        project_root,
        std::time::Duration::from_secs(30),
    )
}

fn run_command_formatter_with_timeout(
    template: &str,
    abs_path: &str,
    project_root: &str,
    timeout: std::time::Duration,
) -> Result<(), String> {
    let formatter = spawn_command_formatter(template, abs_path, project_root)?;
    wait_for_command_formatter(formatter, timeout)
}

struct CapturedFormatter {
    child: std::process::Child,
    stdout: std::fs::File,
    stderr: std::fs::File,
    bin: String,
    cleanup: FormatterCleanup,
}

#[derive(Default)]
struct FormatterCleanup {
    #[cfg(windows)]
    job: Option<FormatterJob>,
}

#[cfg(windows)]
struct FormatterJob(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl Drop for FormatterJob {
    fn drop(&mut self) {
        // SAFETY: this instance exclusively owns the CreateJobObjectW handle.
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

fn spawn_command_formatter(
    template: &str,
    abs_path: &str,
    project_root: &str,
) -> Result<CapturedFormatter, String> {
    use std::process::{Command, Stdio};

    let argv = build_argv(template, abs_path);
    let (bin, rest) = argv
        .split_first()
        .ok_or_else(|| "INVALID_TARGET: empty formatter template".to_string())?;
    let stdout = tempfile::tempfile()
        .map_err(|error| format!("failed to create formatter '{bin}' stdout capture: {error}"))?;
    let stderr = tempfile::tempfile()
        .map_err(|error| format!("failed to create formatter '{bin}' stderr capture: {error}"))?;
    let mut command = Command::new(bin);
    command
        .args(rest)
        .current_dir(project_root)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout.try_clone().map_err(|error| {
            format!("failed to clone formatter '{bin}' stdout capture: {error}")
        })?))
        .stderr(Stdio::from(stderr.try_clone().map_err(|error| {
            format!("failed to clone formatter '{bin}' stderr capture: {error}")
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
            format!("formatter '{bin}' not found in PATH")
        } else {
            format!("failed to run '{bin}': {e}")
        }
    })?;

    let cleanup = match formatter_cleanup(&child, bin) {
        Ok(cleanup) => cleanup,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
    };

    Ok(CapturedFormatter {
        child,
        stdout,
        stderr,
        bin: bin.clone(),
        cleanup,
    })
}

#[cfg(windows)]
fn formatter_cleanup(child: &std::process::Child, bin: &str) -> Result<FormatterCleanup, String> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };

    // SAFETY: null attributes/name create a private job owned by FormatterJob.
    let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if handle.is_null() {
        return Err(format!(
            "failed to create formatter job for '{bin}': {}",
            std::io::Error::last_os_error()
        ));
    }
    let job = FormatterJob(handle);
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
            "failed to configure formatter job for '{bin}': {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: Child owns a live process handle for the formatter process.
    let assigned = unsafe { AssignProcessToJobObject(job.0, child.as_raw_handle() as _) };
    if assigned == 0 {
        return Err(format!(
            "failed to assign formatter '{bin}' to its job: {}",
            std::io::Error::last_os_error()
        ));
    }
    resume_formatter_threads(child.id(), bin)?;
    Ok(FormatterCleanup { job: Some(job) })
}

#[cfg(windows)]
fn resume_formatter_threads(process_id: u32, bin: &str) -> Result<(), String> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};

    // SAFETY: snapshot handle is closed on every return path below.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(format!(
            "failed to inspect suspended formatter '{bin}' threads: {}",
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
                    "suspended formatter '{bin}' exposed no resumable thread"
                ))
            };
        }
        if entry.th32OwnerProcessID == process_id {
            // SAFETY: entry names a thread owned by the suspended child process.
            let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
            if thread.is_null() {
                break Err(format!(
                    "failed to open suspended formatter '{bin}' thread: {}",
                    std::io::Error::last_os_error()
                ));
            }
            // SAFETY: thread is a live handle with THREAD_SUSPEND_RESUME access.
            let resume_result = unsafe { ResumeThread(thread) };
            // SAFETY: this scope owns the OpenThread handle.
            unsafe { CloseHandle(thread) };
            if resume_result == u32::MAX {
                break Err(format!(
                    "failed to resume formatter '{bin}' thread: {}",
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

#[cfg(not(windows))]
fn formatter_cleanup(_: &std::process::Child, _: &str) -> Result<FormatterCleanup, String> {
    Ok(FormatterCleanup::default())
}

fn wait_for_command_formatter(
    CapturedFormatter {
        mut child,
        mut stdout,
        mut stderr,
        bin,
        mut cleanup,
    }: CapturedFormatter,
    timeout: std::time::Duration,
) -> Result<(), String> {
    const CLEANUP_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

    let deadline = std::time::Instant::now() + timeout;
    let (status, timed_out, cleanup_error) = loop {
        match child.try_wait() {
            Err(wait_error) => {
                let cleanup_error = terminate_timed_out_formatter(&mut child, &bin, &mut cleanup)
                    .err()
                    .map(|error| format!("cleanup failed: {error}"));
                let reap_error = reap_formatter(&mut child, &bin, CLEANUP_GRACE)
                    .err()
                    .map(|error| format!("reap failed: {error}"));
                let details = [cleanup_error, reap_error]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join("; ");
                return Err(if details.is_empty() {
                    format!("failed to wait for '{bin}': {wait_error}")
                } else {
                    format!("failed to wait for '{bin}': {wait_error}; {details}")
                });
            }
            Ok(Some(status)) => {
                break (
                    status,
                    false,
                    stop_remaining_formatter_group(&child, &bin, &mut cleanup).err(),
                );
            }
            Ok(None) if std::time::Instant::now() >= deadline => {
                let cleanup_error =
                    terminate_timed_out_formatter(&mut child, &bin, &mut cleanup).err();
                break (
                    reap_formatter(&mut child, &bin, CLEANUP_GRACE)?,
                    true,
                    cleanup_error,
                );
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(10)),
        }
    };
    let _stdout = read_capture(&mut stdout, &bin, "stdout")?;
    let stderr = read_capture(&mut stderr, &bin, "stderr")?;

    if let Some(error) = cleanup_error {
        return Err(error);
    }

    if timed_out {
        return Err(format!(
            "formatter '{bin}' timed out after {}s",
            timeout.as_secs()
        ));
    }
    if !status.success() {
        let code = status
            .code()
            .map_or_else(|| "signal".to_string(), |c| c.to_string());
        let stderr = String::from_utf8_lossy(&stderr);
        return Err(format!("{bin} exited {code}: {}", stderr.trim()));
    }
    Ok(())
}

fn read_capture(capture: &mut std::fs::File, bin: &str, stream: &str) -> Result<Vec<u8>, String> {
    use std::io::{Read, Seek};

    capture
        .rewind()
        .map_err(|error| format!("failed to rewind formatter '{bin}' {stream}: {error}"))?;
    let mut bytes = Vec::new();
    capture
        .read_to_end(&mut bytes)
        .map_err(|error| format!("failed to read formatter '{bin}' {stream}: {error}"))?;
    Ok(bytes)
}

fn reap_formatter(
    child: &mut std::process::Child,
    bin: &str,
    timeout: std::time::Duration,
) -> Result<std::process::ExitStatus, String> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child
            .try_wait()
            .map_err(|error| format!("failed to reap formatter '{bin}': {error}"))?
        {
            Some(status) => return Ok(status),
            None if std::time::Instant::now() >= deadline => {
                return Err(format!("formatter '{bin}' did not exit after termination"));
            }
            None => std::thread::sleep(std::time::Duration::from_millis(10)),
        }
    }
}

#[cfg(unix)]
fn stop_remaining_formatter_group(
    child: &std::process::Child,
    bin: &str,
    _: &mut FormatterCleanup,
) -> Result<(), String> {
    let pgid = child.id() as libc::pid_t;
    if pgid <= 0 {
        return Err(format!(
            "failed to stop formatter process group '{bin}': invalid process group id {pgid}"
        ));
    }
    // SAFETY: the formatter was spawned as leader of its dedicated process group.
    if unsafe { libc::killpg(pgid, libc::SIGKILL) } == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(format!(
            "failed to stop formatter process group '{bin}': {error}"
        ))
    }
}

#[cfg(windows)]
fn stop_remaining_formatter_group(
    _: &std::process::Child,
    _: &str,
    cleanup: &mut FormatterCleanup,
) -> Result<(), String> {
    drop(cleanup.job.take());
    Ok(())
}

fn terminate_timed_out_formatter(
    child: &mut std::process::Child,
    bin: &str,
    cleanup: &mut FormatterCleanup,
) -> Result<(), String> {
    #[cfg(unix)]
    {
        match stop_remaining_formatter_group(child, bin, cleanup) {
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
        stop_remaining_formatter_group(child, bin, cleanup)?;
        return Ok(());
    }

    #[allow(unreachable_code)]
    child
        .kill()
        .map_err(|e| format!("failed to stop timed-out formatter '{bin}': {e}"))
}

/// Hex BLAKE3 of the file content, for honest before/after change detection.
pub fn blake3_of(abs_path: &str) -> Result<String, String> {
    let bytes = std::fs::read(abs_path).map_err(|e| format!("FILE_NOT_FOUND: {abs_path}: {e}"))?;
    Ok(crate::core::hasher::hash_hex(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait_for_path(path: &std::path::Path, timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        while !path.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        path.exists()
    }

    fn assert_descendant_stopped(release: &std::path::Path, survived: &std::path::Path) {
        std::fs::write(release, "release").unwrap();
        assert!(
            !wait_for_path(survived, std::time::Duration::from_secs(2)),
            "formatter descendant performed work after cleanup"
        );
    }

    #[test]
    fn rs_defaults_to_rustfmt() {
        let f = resolve_formatter("/x/a.rs");
        assert!(matches!(f, Formatter::Command(ref t) if t == "rustfmt {file}"));
    }

    #[test]
    fn md_and_unknown_and_no_ext_default_to_jetbrains() {
        assert!(matches!(resolve_formatter("/x/a.md"), Formatter::Jetbrains));
        assert!(matches!(
            resolve_formatter("/x/a.txt"),
            Formatter::Jetbrains
        ));
        assert!(matches!(
            resolve_formatter("/x/README"),
            Formatter::Jetbrains
        ));
    }

    #[test]
    fn extension_is_case_insensitive() {
        assert!(matches!(
            resolve_formatter("/x/A.RS"),
            Formatter::Command(_)
        ));
    }

    #[test]
    fn command_label_is_first_token() {
        assert_eq!(command_label("rustfmt {file}"), "rustfmt");
        assert_eq!(command_label("ruff format {file}"), "ruff");
        assert_eq!(command_label(""), "formatter");
    }

    #[test]
    fn argv_substitutes_placeholder() {
        assert_eq!(
            build_argv("rustfmt {file}", "/x/a.rs"),
            vec!["rustfmt".to_string(), "/x/a.rs".to_string()]
        );
        assert_eq!(
            build_argv("ruff format {file}", "/x/a.py"),
            vec![
                "ruff".to_string(),
                "format".to_string(),
                "/x/a.py".to_string()
            ]
        );
    }

    #[test]
    fn argv_appends_path_when_no_placeholder() {
        assert_eq!(
            build_argv("gofmt -w", "/x/a.go"),
            vec!["gofmt".to_string(), "-w".to_string(), "/x/a.go".to_string()]
        );
    }

    #[test]
    fn argv_path_with_spaces_stays_one_arg() {
        let argv = build_argv("rustfmt {file}", "/x/my dir/a.rs");
        assert_eq!(
            argv,
            vec!["rustfmt".to_string(), "/x/my dir/a.rs".to_string()]
        );
    }

    #[test]
    fn blake3_detects_change() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.txt");
        std::fs::write(&f, "one").unwrap();
        let p = f.to_str().unwrap();
        let h1 = blake3_of(p).unwrap();
        let h2 = blake3_of(p).unwrap();
        assert_eq!(h1, h2, "same content → same hash");
        std::fs::write(&f, "two").unwrap();
        assert_ne!(
            h1,
            blake3_of(p).unwrap(),
            "changed content → different hash"
        );
    }

    #[test]
    fn blake3_missing_file_errors() {
        assert!(blake3_of("/no/such/file.xyz").is_err());
    }

    #[test]
    fn run_command_missing_binary_errors() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.rs");
        std::fs::write(&f, "fn x(){}\n").unwrap();
        let err = run_command_formatter(
            "definitely-not-a-formatter-binary {file}",
            f.to_str().unwrap(),
            dir.path().to_str().unwrap(),
        )
        .unwrap_err();
        assert!(err.contains("not found"), "got: {err}");
    }

    #[test]
    fn run_command_nonzero_exit_errors() {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        let (template, formatter) = {
            let formatter = dir.path().join("fail-formatter");
            std::fs::write(&formatter, "#!/bin/sh\nexit 7\n").unwrap();
            ("sh {file}", formatter)
        };
        #[cfg(windows)]
        let (template, formatter) = {
            let formatter = dir.path().join("fail-formatter.cmd");
            std::fs::write(&formatter, "@echo off\r\nexit /B 7\r\n").unwrap();
            ("cmd.exe /D /S /C {file}", formatter)
        };
        let err = run_command_formatter(
            template,
            formatter.to_str().unwrap(),
            dir.path().to_str().unwrap(),
        )
        .unwrap_err();
        assert!(err.contains("exited"), "got: {err}");
    }

    #[cfg(windows)]
    #[test]
    fn command_formatter_timeout_closes_descendant_pipes() {
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        let formatter = dir.path().join("stall-formatter.ps1");
        let descendant = dir.path().join("descendant.ps1");
        let ready = dir.path().join("descendant.ready");
        let release = dir.path().join("descendant.release");
        let survived = dir.path().join("descendant.survived");
        let ps_quote = |path: &std::path::Path| path.display().to_string().replace('\'', "''");
        std::fs::write(
            &descendant,
            format!(
                "$ErrorActionPreference = 'Stop'\r\nSet-Content -LiteralPath '{}' -Value ready\r\nwhile (-not (Test-Path -LiteralPath '{}')) {{ Start-Sleep -Milliseconds 10 }}\r\nSet-Content -LiteralPath '{}' -Value survived\r\n",
                ps_quote(&ready),
                ps_quote(&release),
                ps_quote(&survived)
            ),
        )
        .unwrap();
        std::fs::write(
            &formatter,
            format!(
                "$ErrorActionPreference = 'Stop'\r\nStart-Process powershell.exe -NoNewWindow -ArgumentList @('-NoProfile','-ExecutionPolicy','Bypass','-File','{}')\r\nwhile ($true) {{ Start-Sleep -Seconds 1 }}\r\n",
                ps_quote(&descendant)
            ),
        )
        .unwrap();
        let process = spawn_command_formatter(
            "powershell.exe -NoProfile -ExecutionPolicy Bypass -File {file}",
            formatter.to_str().unwrap(),
            dir.path().to_str().unwrap(),
        )
        .expect("formatter process must spawn");
        // #1536: PowerShell cold-start on a loaded Windows runner can exceed
        // 5 s. The poll is bounded either way, so the generous ceiling only
        // costs wall-clock time in the case where the test is about to fail.
        assert!(
            wait_for_path(&ready, Duration::from_secs(30)),
            "formatter descendant must start"
        );
        let error = wait_for_command_formatter(process, Duration::from_millis(250))
            .expect_err("stalled formatter tree must fail closed");

        assert!(error.contains("timed out"), "got: {error}");
        assert_descendant_stopped(&release, &survived);
    }

    #[cfg(unix)]
    #[test]
    fn command_formatter_times_out_and_reaps_a_stalled_child() {
        use std::os::unix::fs::PermissionsExt;
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        let formatter = dir.path().join("stall-formatter");
        std::fs::write(
            &formatter,
            "#!/bin/sh\n(printf ready > \"${0}.ready\"; while [ ! -e \"${0}.release\" ]; do sleep 0.01; done; printf survived > \"${0}.survived\") &\nwait\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&formatter).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&formatter, permissions).unwrap();
        let source = dir.path().join("a.rs");
        std::fs::write(&source, "fn x() {}\n").unwrap();
        let ready = formatter.with_file_name("stall-formatter.ready");
        let release = formatter.with_file_name("stall-formatter.release");
        let survived = formatter.with_file_name("stall-formatter.survived");
        let template = format!("{} {{file}}", formatter.display());
        let source_path = source.to_str().unwrap().to_owned();
        let project_root = dir.path().to_str().unwrap().to_owned();
        let process = spawn_command_formatter(&template, &source_path, &project_root)
            .expect("formatter process must spawn");

        assert!(
            wait_for_path(&ready, Duration::from_secs(2)),
            "formatter descendant must start"
        );
        let error = wait_for_command_formatter(process, Duration::from_millis(250))
            .expect_err("stalled formatter must fail closed");

        assert!(error.contains("timed out"), "unexpected error: {error}");
        assert_descendant_stopped(&release, &survived);
    }

    #[cfg(unix)]
    #[test]
    fn command_formatter_cleans_descendant_after_parent_exits() {
        use std::os::unix::fs::PermissionsExt;
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        let formatter = dir.path().join("background-formatter");
        std::fs::write(
            &formatter,
            "#!/bin/sh\n(printf ready > \"${0}.ready\"; while [ ! -e \"${0}.release\" ]; do sleep 0.01; done; printf survived > \"${0}.survived\") &\nexit 0\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&formatter).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&formatter, permissions).unwrap();
        let source = dir.path().join("a.rs");
        std::fs::write(&source, "fn x() {}\n").unwrap();

        let process = spawn_command_formatter(
            &format!("{} {{file}}", formatter.display()),
            source.to_str().unwrap(),
            dir.path().to_str().unwrap(),
        )
        .expect("formatter process must spawn");
        let ready = formatter.with_file_name("background-formatter.ready");
        let release = formatter.with_file_name("background-formatter.release");
        let survived = formatter.with_file_name("background-formatter.survived");
        assert!(
            wait_for_path(&ready, Duration::from_secs(2)),
            "formatter descendant must start"
        );

        wait_for_command_formatter(process, Duration::from_secs(1))
            .expect("successful formatter must clean up its background process");
        assert_descendant_stopped(&release, &survived);
    }

    #[test]
    fn run_rustfmt_formats_and_reports_change() {
        // Gated: only runs when rustfmt is installed.
        if std::process::Command::new("rustfmt")
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("SKIP: rustfmt not in PATH");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.rs");
        std::fs::write(&f, "fn   x( ){let y=1;}\n").unwrap(); // deliberate drift
        let p = f.to_str().unwrap();
        let before = blake3_of(p).unwrap();
        run_command_formatter("rustfmt {file}", p, dir.path().to_str().unwrap()).unwrap();
        let after = blake3_of(p).unwrap();
        assert_ne!(
            before, after,
            "rustfmt should have changed the drifted file"
        );

        // A second run is a no-op (already conformant).
        let before2 = blake3_of(p).unwrap();
        run_command_formatter("rustfmt {file}", p, dir.path().to_str().unwrap()).unwrap();
        assert_eq!(
            before2,
            blake3_of(p).unwrap(),
            "second run should be unchanged"
        );
    }
}
