// SPDX-License-Identifier: Apache-2.0
//! Where and how the gate's rerun runs: the gate's directories
//! ([`GateDirs`]), the run's environment and command, and the locked
//! files of the run tree (read-only during the run, compared with
//! `spec_commit` after every command).
//!
//! - **Directories.** Private copies, the gate's `CARGO_HOME`, the run's
//!   `CARGO_TARGET_DIR` and `TMPDIR` live under the Instance's data dir
//!   (`work`, 0700). The run tree, where cargo runs, lives **outside
//!   `$HOME`**, because cargo reads `.cargo/config.toml` in the working
//!   directory and every parent, and `~/.cargo/config.toml` is common. A
//!   run under any parent cargo config is refused. Each run gets its own
//!   directory created **directly** in `runs`: `/run/user/<getuid()>`
//!   (which the user cannot rename: its parent is root's; never the
//!   caller's `XDG_RUNTIME_DIR`, N7), else `/tmp` (sticky, root's). No shared directory of the
//!   user's sits between that per-run directory and `/`.
//! - **Locked files.** Before the run they are made read-only (files 0444,
//!   their directories 0555) and then pinned: content, device, inode, mode
//!   and change time of every file and of every directory on its way; the
//!   tree root's device and inode; the per-run directory that holds only
//!   the tree, made 0555 too (device, inode, mode, change time); and every
//!   directory above it up to `/` (device and inode, and a watch for a
//!   rename of any of them, [`MoveWatch`]: inotify on Linux, kqueue on
//!   macOS; elsewhere the change time of each one this user could rename).
//!   After every
//!   command each is compared, added files included. Any write, chmod or
//!   rename during the run moves the change time or the inode, which a
//!   process of the same user cannot set back, so a locked test rewritten
//!   and restored within one command, or the tree, its per-run directory
//!   or any directory above them swapped for a copy and back, still makes
//!   the run a `locked_paths_changed` fail (R4-1, R5-1). Where no watch
//!   exists, a `runs` given by hand inside a directory other processes
//!   write to makes runs fail closed, never pass.
//! - **Toolchain.** `HOME`, `~/.cargo/bin` and `~/.rustup` come from the
//!   user's passwd entry, never from the caller's environment (the CLI
//!   submit runs in the agent's process): a `CARGO_HOME` or `RUSTUP_HOME`
//!   set to anything else refuses the run (R5-4).
//! - **Windows.** The same run with the platform's own facts, never the
//!   caller's environment either: the home is the profile known folder
//!   (not `USERPROFILE`), the system directories come from
//!   `GetSystemWindowsDirectoryW` and the Program Files and ProgramData
//!   known folders (not `SystemRoot`/`PATH`), and the commands run in Git for Windows' `sh`
//!   from there, so the POSIX command lines of a repository mean the same.
//!   Windows has no change time a process cannot set back and no
//!   [`MoveWatch`]: locked files are made read-only (attribute) and pinned
//!   by content, attributes and write time, which a process of the same
//!   user can restore. The rename-and-restore protection of the tree and
//!   of the directories above it (R4-1, R5-1) is Unix-only, and the
//!   default `runs` is the user's local temp directory (the known folder),
//!   so a `~/.cargo/config.toml` refuses the run there.
//!
//! **Known gap:** the code of the judged commit (its library, proc-macros,
//! path and registry dependencies with their build scripts) runs at build
//! time as the same Unix user, and the code under test runs inside the test
//! binary and can end it with exit 0. The harness report in
//! `gate_report.rs` is evidence only against accidental and simple exits
//! (a binary that ends before its summary, a locked test that never ran);
//! it is no evidence against code under test that prints a complete,
//! consistent forged report to the real stdout and exits 0 (N6). Build
//! scripts and test-target keys changed against `spec_commit` are refused
//! before the run, but code that runs anyway can act on the run in ways no
//! file check sees. Only a sandboxed run with a read-only bind mount of
//! the locked paths and no write access outside the run (on pounce, under
//! the gate, §5) closes that. The same holds for a proc-macro of unlocked
//! code that shadows the plain `#[test]` of a locked file by a route the
//! parser does not see (a glob import or a prelude of unlocked code, N7):
//! `#[macro_use] extern crate` on the mount chain and `use .. as test` are
//! refused, other shadowing is not proven absent.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use lean_ctx_protocol::hive::gate::{LockedChange, under_locked};
use sha2::{Digest as _, Sha256};

use super::GateError;

/// Cargo config files cargo reads from a directory and every parent.
pub(super) const CARGO_CONFIGS: [&str; 2] = [".cargo/config", ".cargo/config.toml"];

/// `name` as an executable file name of this platform (`cargo.exe` on
/// Windows).
#[cfg(test)]
pub(super) fn exe(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

/// The system directories git and the gate's helpers run with, nothing of
/// the caller's `PATH`: on Unix `/usr/local/bin`, `/opt/homebrew/bin`,
/// `/usr/bin`, `/bin`.
#[cfg(not(windows))]
pub(super) fn system_dirs() -> Vec<PathBuf> {
    ["/usr/local/bin", "/opt/homebrew/bin", "/usr/bin", "/bin"]
        .iter()
        .map(PathBuf::from)
        .collect()
}

/// Windows: the Windows directory's `System32`, itself, `Wbem` and
/// PowerShell, and Git for Windows' `cmd` (its `git.exe`) under Program
/// Files, from the system's own answers ([`win::windows_dir`],
/// [`win::program_files`]), never from `SystemRoot` or `PATH`.
#[cfg(windows)]
pub(super) fn system_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(windows) = win::windows_dir() {
        let system32 = windows.join("System32");
        dirs.extend([
            system32.clone(),
            windows,
            system32.join("Wbem"),
            system32.join("WindowsPowerShell").join("v1.0"),
        ]);
    }
    if let Some(pf) = win::program_files() {
        dirs.push(pf.join("Git").join("cmd"));
    }
    dirs
}

/// [`system_dirs`] as a `PATH` value.
pub(super) fn system_path() -> std::ffi::OsString {
    std::env::join_paths(system_dirs()).unwrap_or_default()
}

/// The shell the gate's commands run in: `/bin/sh` on Unix, Git for
/// Windows' `sh.exe` under Program Files on Windows.
pub(super) fn shell() -> Result<PathBuf, GateError> {
    #[cfg(windows)]
    {
        let sh = win::program_files()
            .map(|pf| pf.join("Git").join("bin").join("sh.exe"))
            .filter(|sh| sh.is_file())
            .ok_or_else(|| {
                GateError::Check(
                    "the gate runs its commands in Git for Windows' sh (Program Files\\Git\\bin\\sh.exe), which is missing".into(),
                )
            })?;
        Ok(sh)
    }
    #[cfg(not(windows))]
    {
        Ok(PathBuf::from("/bin/sh"))
    }
}

/// The platform calls behind the Windows run: the system's own directories
/// and a job object that holds the run's whole process tree.
#[cfg(windows)]
mod win {
    use std::ffi::{OsString, c_void};
    use std::os::windows::ffi::OsStringExt as _;
    use std::path::PathBuf;

    #[repr(C)]
    struct Guid {
        data1: u32,
        data2: u16,
        data3: u16,
        data4: [u8; 8],
    }

    /// `FOLDERID_ProgramFiles` {905E63B6-C1BF-494E-B29C-65B732D3D21A}.
    const FOLDERID_PROGRAM_FILES: Guid = Guid {
        data1: 0x905E_63B6,
        data2: 0xC1BF,
        data3: 0x494E,
        data4: [0xB2, 0x9C, 0x65, 0xB7, 0x32, 0xD3, 0xD2, 0x1A],
    };

    /// `FOLDERID_ProgramData` {62AB5D82-FDC1-4DC3-A9DD-070D1D495D97}.
    const FOLDERID_PROGRAM_DATA: Guid = Guid {
        data1: 0x62AB_5D82,
        data2: 0xFDC1,
        data3: 0x4DC3,
        data4: [0xA9, 0xDD, 0x07, 0x0D, 0x1D, 0x49, 0x5D, 0x97],
    };

    #[repr(C)]
    #[derive(Default)]
    struct BasicLimits {
        per_process_user_time_limit: i64,
        per_job_user_time_limit: i64,
        limit_flags: u32,
        minimum_working_set_size: usize,
        maximum_working_set_size: usize,
        active_process_limit: u32,
        affinity: usize,
        priority_class: u32,
        scheduling_class: u32,
    }

    #[repr(C)]
    #[derive(Default)]
    struct IoCounters {
        counts: [u64; 6],
    }

    /// `JOBOBJECT_EXTENDED_LIMIT_INFORMATION`.
    #[repr(C)]
    #[derive(Default)]
    struct ExtendedLimits {
        basic: BasicLimits,
        io: IoCounters,
        process_memory_limit: usize,
        job_memory_limit: usize,
        peak_process_memory_used: usize,
        peak_job_memory_used: usize,
    }

    const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION: u32 = 9;
    const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x2000;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetSystemWindowsDirectoryW(buffer: *mut u16, size: u32) -> u32;
        fn CreateJobObjectW(attributes: *mut c_void, name: *const u16) -> *mut c_void;
        fn SetInformationJobObject(
            job: *mut c_void,
            class: u32,
            info: *const c_void,
            length: u32,
        ) -> i32;
        fn AssignProcessToJobObject(job: *mut c_void, process: *mut c_void) -> i32;
        fn TerminateJobObject(job: *mut c_void, exit_code: u32) -> i32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }

    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtResumeProcess(process: *mut c_void) -> i32;
    }

    /// `CREATE_SUSPENDED`: the child's first thread waits until it is
    /// resumed, so it starts nothing before it sits in the job.
    pub(super) const CREATE_SUSPENDED: u32 = 0x0000_0004;

    #[link(name = "shell32")]
    unsafe extern "system" {
        fn SHGetKnownFolderPath(
            id: *const Guid,
            flags: u32,
            token: *mut c_void,
            path: *mut *mut u16,
        ) -> i32;
    }

    #[link(name = "ole32")]
    unsafe extern "system" {
        fn CoTaskMemFree(memory: *mut c_void);
    }

    /// The system's Windows directory (`C:\Windows`).
    pub(super) fn windows_dir() -> Option<PathBuf> {
        let mut buf = vec![0u16; 1024];
        let len = u32::try_from(buf.len()).ok()?;
        // SAFETY: writes at most `len` UTF-16 units into `buf`.
        let n = unsafe { GetSystemWindowsDirectoryW(buf.as_mut_ptr(), len) };
        let n = usize::try_from(n).ok()?;
        (n > 0 && n < buf.len()).then(|| PathBuf::from(OsString::from_wide(&buf[..n])))
    }

    /// The Program Files known folder (`C:\Program Files`).
    pub(super) fn program_files() -> Option<PathBuf> {
        known_folder(&FOLDERID_PROGRAM_FILES)
    }

    /// The ProgramData known folder (`C:\ProgramData`), where the Visual
    /// Studio setup lookup rustc uses to find the MSVC linker keeps its
    /// instances.
    pub(super) fn program_data() -> Option<PathBuf> {
        known_folder(&FOLDERID_PROGRAM_DATA)
    }

    fn known_folder(id: &Guid) -> Option<PathBuf> {
        let mut raw: *mut u16 = std::ptr::null_mut();
        // SAFETY: the GUID lives across the call; on success `raw` is a
        // NUL-terminated string the caller frees with CoTaskMemFree.
        let hr = unsafe {
            SHGetKnownFolderPath(
                std::ptr::from_ref(id),
                0,
                std::ptr::null_mut(),
                &raw mut raw,
            )
        };
        let path = (hr == 0 && !raw.is_null()).then(|| {
            let mut len = 0usize;
            // SAFETY: `raw` is NUL-terminated (above); read up to the NUL.
            unsafe {
                while *raw.add(len) != 0 {
                    len += 1;
                }
                PathBuf::from(OsString::from_wide(std::slice::from_raw_parts(raw, len)))
            }
        });
        // SAFETY: frees what SHGetKnownFolderPath allocated (null is fine).
        unsafe { CoTaskMemFree(raw.cast()) };
        path
    }

    /// A job object that kills every process in it when it is closed or
    /// terminated: the run's process tree (Git's `sh` launcher, the shell,
    /// cargo and the test binaries), as the process group on Unix.
    pub(super) struct Job(*mut c_void);

    impl Job {
        pub(super) fn new() -> Option<Self> {
            // SAFETY: an anonymous job object, owned by the returned value.
            let handle = unsafe { CreateJobObjectW(std::ptr::null_mut(), std::ptr::null()) };
            if handle.is_null() {
                return None;
            }
            let job = Self(handle);
            let mut limits = ExtendedLimits::default();
            limits.basic.limit_flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let size = u32::try_from(std::mem::size_of::<ExtendedLimits>()).ok()?;
            // SAFETY: `limits` is the structure the class names, `size` its size.
            let ok = unsafe {
                SetInformationJobObject(
                    job.0,
                    JOB_OBJECT_EXTENDED_LIMIT_INFORMATION,
                    (&raw const limits).cast(),
                    size,
                )
            };
            (ok != 0).then_some(job)
        }

        /// Put the suspended `child` (and so everything it will start) in the
        /// job, then let it run. False when either step failed; the caller
        /// then kills the child, which has not run a single instruction.
        pub(super) fn assign_and_resume(&self, child: &std::process::Child) -> bool {
            use std::os::windows::io::AsRawHandle as _;
            let process = child.as_raw_handle().cast();
            // SAFETY: both handles are valid while `self` and `child` live.
            if unsafe { AssignProcessToJobObject(self.0, process) } == 0 {
                return false;
            }
            // SAFETY: the child's process handle (full access), valid while
            // `child` lives; NTSTATUS 0 is success.
            unsafe { NtResumeProcess(process) == 0 }
        }

        /// Kill every process in the job.
        pub(super) fn terminate(&self) {
            // SAFETY: the job handle is valid while `self` lives.
            unsafe {
                TerminateJobObject(self.0, 1);
            }
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            // SAFETY: closes the handle this value owns; KILL_ON_JOB_CLOSE
            // ends whatever of the run is still alive.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

/// The gate's directories: `work` under the Instance's data dir (private
/// copies, the gate's `CARGO_HOME`, targets), `runs` outside `$HOME`, the
/// directory each run's own directory is created in directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateDirs {
    pub work: PathBuf,
    pub runs: PathBuf,
}

impl GateDirs {
    /// The directories of the Instance whose data dir is `data_dir`.
    #[must_use]
    pub fn for_instance(data_dir: &Path) -> Self {
        let work = data_dir.join("g6");
        Self {
            work,
            runs: default_runs(),
        }
    }
}

/// `/run/user/<getuid()>` when it is this user's directory (Linux: 0700,
/// its parent root's), else `/tmp` as the system resolves it. Never the
/// caller's `XDG_RUNTIME_DIR` (N7: it could name a mount of the user's own
/// that raises no inotify event), and not the per-user temp dir on macOS:
/// its parent is the user's, and other processes write to it all the time.
/// On macOS `/tmp` is root's symlink to `/private/tmp`, and the gate refuses
/// to run in a symlink, so the resolved directory is the base.
fn default_runs() -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        // SAFETY: getuid takes no arguments and cannot fail.
        let uid = unsafe { libc::getuid() };
        let run = PathBuf::from(format!("/run/user/{uid}"));
        if std::fs::symlink_metadata(&run).is_ok_and(|m| m.is_dir() && m.uid() == uid) {
            return run;
        }
    }
    #[cfg(windows)]
    {
        // No root-owned shared directory: the user's local temp directory
        // from the known folder (never `TMP`/`TEMP`), see the module docs.
        dirs::data_local_dir().map_or_else(std::env::temp_dir, |local| local.join("Temp"))
    }
    #[cfg(not(windows))]
    {
        std::fs::canonicalize("/tmp").unwrap_or_else(|_| PathBuf::from("/tmp"))
    }
}

/// `path` resolved, without the Windows `\\?\` prefix: the form cargo,
/// rustc's dep-info, git and Git's `sh` use, so paths compare as written.
pub(super) fn resolved(path: &Path) -> std::io::Result<PathBuf> {
    std::fs::canonicalize(path).map(lean_ctx::core::pathutil::strip_verbatim)
}

#[cfg(unix)]
fn private_mode(dir: &Path) -> Result<(), GateError> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    let meta = std::fs::symlink_metadata(dir)
        .map_err(|e| GateError::Check(format!("{}: {e}", dir.display())))?;
    // SAFETY: getuid takes no arguments and cannot fail.
    let uid = unsafe { libc::getuid() };
    if meta.file_type().is_symlink() || !meta.is_dir() || meta.uid() != uid {
        return Err(GateError::Check(format!(
            "{} is no directory of this user; the gate refuses to work in it",
            dir.display()
        )));
    }
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| GateError::Check(format!("{}: {e}", dir.display())))
}

#[cfg(not(unix))]
fn private_mode(_dir: &Path) -> Result<(), GateError> {
    Ok(())
}

/// `dir` created if needed, this user's, mode 0700.
pub(super) fn make_private_dir(dir: &Path) -> Result<(), GateError> {
    std::fs::create_dir_all(dir)
        .map_err(|e| GateError::Check(format!("the gate's directory {}: {e}", dir.display())))?;
    private_mode(dir)
}

/// A fresh private directory under `work` (private copies, the recheck,
/// targets). Nothing runs cargo here, so parent configs do not matter.
pub(super) fn private_dir(work: &Path) -> Result<tempfile::TempDir, GateError> {
    make_private_dir(work)?;
    tempfile::Builder::new()
        .prefix("g6-")
        .tempdir_in(work)
        .map_err(|e| GateError::Check(format!("a fresh directory: {e}")))
}

/// A fresh private directory for the run tree, created directly in `runs`
/// (which is created private when missing, and left as it is otherwise:
/// `/tmp` is root's), refused when `runs` or any parent holds a cargo
/// config (cargo would read it in the run).
pub(super) fn cargo_run_dir(runs: &Path) -> Result<tempfile::TempDir, GateError> {
    match std::fs::symlink_metadata(runs) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => {
            return Err(GateError::Check(format!(
                "{} is no directory; the gate refuses to run in it",
                runs.display()
            )));
        }
        Err(_) => make_private_dir(runs)?,
    }
    let canonical =
        resolved(runs).map_err(|e| GateError::Check(format!("the gate's run directory: {e}")))?;
    for dir in canonical.ancestors() {
        for name in CARGO_CONFIGS {
            let file = dir.join(Path::new(name).components().collect::<PathBuf>());
            if std::fs::symlink_metadata(&file).is_ok() {
                return Err(GateError::Check(format!(
                    "{} lies above the gate's run directory, and cargo would read it in the rerun; the gate refuses to run under it",
                    file.display()
                )));
            }
        }
    }
    // tempfile creates it 0700.
    tempfile::Builder::new()
        .prefix("lean-ctx-g6-run-")
        .tempdir_in(&canonical)
        .map_err(|e| GateError::Check(format!("a fresh directory: {e}")))
}

/// This user's home from the passwd entry, never `$HOME` (R5-4).
#[cfg(unix)]
pub(super) fn user_home() -> Result<PathBuf, GateError> {
    use std::ffi::{CStr, OsStr};
    use std::os::unix::ffi::OsStrExt as _;
    let mut buf = vec![0 as libc::c_char; 16 * 1024];
    // SAFETY: a zeroed passwd is a valid out-parameter for getpwuid_r.
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut found: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: getuid cannot fail; getpwuid_r writes only into `pwd` and
    // `buf` (whose length it is given) and sets `found`.
    let rc = unsafe {
        libc::getpwuid_r(
            libc::getuid(),
            &raw mut pwd,
            buf.as_mut_ptr(),
            buf.len(),
            &raw mut found,
        )
    };
    if rc != 0 || found.is_null() || pwd.pw_dir.is_null() {
        return Err(GateError::Check(
            "this user has no passwd entry, so the gate cannot tell its home".into(),
        ));
    }
    // SAFETY: pw_dir points into `buf`, NUL-terminated, while `buf` lives.
    let dir = unsafe { CStr::from_ptr(pwd.pw_dir) };
    let home = PathBuf::from(OsStr::from_bytes(dir.to_bytes()));
    if !home.is_absolute() {
        return Err(GateError::Check(
            "this user's passwd home is not absolute".into(),
        ));
    }
    Ok(home)
}

/// Windows: this user's profile from the known folder (`dirs` asks
/// `SHGetKnownFolderPath(FOLDERID_Profile)`), never `USERPROFILE`/`HOME`.
#[cfg(not(unix))]
pub(super) fn user_home() -> Result<PathBuf, GateError> {
    dirs::home_dir()
        .filter(|home| home.is_absolute())
        .ok_or_else(|| GateError::Check("this user has no profile directory".into()))
}

/// rustup's directories of `home`: `~/.cargo` (whose `bin` holds cargo) and
/// `~/.rustup`. Refused when the caller's `CARGO_HOME` or `RUSTUP_HOME`
/// names anything else: the CLI submit runs in the agent's process, so
/// those would let the agent pick the cargo that judges it (R5-4).
pub(super) fn toolchain_dirs(
    home: &Path,
    cargo_home: Option<&std::ffi::OsStr>,
    rustup_home: Option<&std::ffi::OsStr>,
) -> Result<(PathBuf, PathBuf), GateError> {
    let fixed = (home.join(".cargo"), home.join(".rustup"));
    let same = |given: &Path, fixed: &Path| {
        given == fixed
            || std::fs::canonicalize(given)
                .is_ok_and(|g| std::fs::canonicalize(fixed).ok().as_ref() == Some(&g))
    };
    for (name, given, fixed) in [
        ("CARGO_HOME", cargo_home, &fixed.0),
        ("RUSTUP_HOME", rustup_home, &fixed.1),
    ] {
        if let Some(given) = given.filter(|g| !g.is_empty())
            && !same(Path::new(given), fixed)
        {
            return Err(GateError::Check(format!(
                "{name} is {}, not {}; the gate runs only rustup's toolchain of this user's home, so it refuses the run",
                Path::new(given).display(),
                fixed.display()
            )));
        }
    }
    Ok(fixed)
}

/// rustup's directories the run uses: [`toolchain_dirs`] of the passwd
/// home, checked against the caller's `CARGO_HOME`/`RUSTUP_HOME`.
#[cfg(not(test))]
fn run_toolchain(home: &Path) -> Result<(PathBuf, PathBuf), GateError> {
    toolchain_dirs(
        home,
        std::env::var_os("CARGO_HOME").as_deref(),
        std::env::var_os("RUSTUP_HOME").as_deref(),
    )
}

/// Unit tests only (never compiled into a binary): as in production when
/// the passwd home has rustup's cargo. A home without it (CI's disposable
/// test user, whose toolchain is the image's, with a job-local
/// `CARGO_HOME`) runs the cargo fixtures with the toolchain of the test
/// process instead: the first `cargo` on its `PATH` and its `RUSTUP_HOME`.
/// The refusal itself is covered by `toolchain_dirs_refuse_a_differing_env`.
#[cfg(test)]
fn run_toolchain(home: &Path) -> Result<(PathBuf, PathBuf), GateError> {
    if home.join(".cargo").join("bin").join(exe("cargo")).is_file() {
        return toolchain_dirs(
            home,
            std::env::var_os("CARGO_HOME").as_deref(),
            std::env::var_os("RUSTUP_HOME").as_deref(),
        );
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    let cargo_dir = std::env::split_paths(&path)
        .find(|dir| dir.is_absolute() && dir.join(exe("cargo")).is_file())
        .and_then(|bin| bin.parent().map(Path::to_path_buf))
        .ok_or_else(|| GateError::Check("no cargo for the gate's test runs".into()))?;
    let rustup = std::env::var_os("RUSTUP_HOME")
        .filter(|v| !v.is_empty())
        .map_or_else(|| home.join(".rustup"), PathBuf::from);
    Ok((cargo_dir, rustup))
}

/// The environment of the gate's run: nothing inherited. A fixed `PATH`
/// (the host's cargo gate in `~/.local/bin`, rustup's `~/.cargo/bin`, the
/// system), `HOME` and rustup's home from the passwd entry
/// ([`toolchain_dirs`]), the gate's own `CARGO_HOME` under `work`, and
/// `CARGO_TARGET_DIR` and `TMPDIR` in `scratch`.
///
/// `~/.local/bin` and `~/.cargo/bin` are writable by the same Unix user
/// (the known gap); they go once the run moves to pounce's gate.
pub(super) fn run_env(
    scratch: &Path,
    work: &Path,
) -> Result<Vec<(String, std::ffi::OsString)>, GateError> {
    let home = user_home()?;
    let (rustup_cargo, rustup) = run_toolchain(&home)?;
    let cargo_home = work.join("cargo-home");
    make_private_dir(&cargo_home)?;
    for name in ["config", "config.toml", "credentials", "credentials.toml"] {
        let file = cargo_home.join(name);
        if std::fs::symlink_metadata(&file).is_ok() {
            return Err(GateError::Check(format!(
                "{} is in the gate's CARGO_HOME, which the gate never writes; refused",
                file.display()
            )));
        }
    }
    let tmp = scratch.join("tmp");
    make_private_dir(&tmp)?;
    let mut dirs = vec![home.join(".local").join("bin"), rustup_cargo.join("bin")];
    dirs.extend(system_dirs());
    let path = std::env::join_paths(&dirs)
        .map_err(|e| GateError::Check(format!("the run's PATH: {e}")))?;
    let mut env: Vec<(String, std::ffi::OsString)> = vec![
        ("PATH".into(), path),
        ("HOME".into(), home.clone().into()),
        ("LANG".into(), "C.UTF-8".into()),
        ("CARGO_HOME".into(), cargo_home.into()),
        ("CARGO_TARGET_DIR".into(), scratch.join("target").into()),
        ("CARGO_TERM_COLOR".into(), "never".into()),
        ("TMPDIR".into(), tmp.clone().into()),
    ];
    // Windows programs read their own names for the same places: the
    // profile (rustup's and cargo's home lookup), the temp directory, and
    // the Windows directory (process start-up, COM for the MSVC linker's
    // lookup) and ProgramData (where that lookup finds the Visual Studio
    // instances; without it rustc falls back to the `link.exe` on `PATH`,
    // which in Git's `sh` is coreutils' `link`), all from the gate's facts
    // above, known folders rather than the caller's variables.
    #[cfg(windows)]
    {
        env.push(("USERPROFILE".into(), home.clone().into()));
        env.push(("TEMP".into(), tmp.clone().into()));
        env.push(("TMP".into(), tmp.into()));
        if let Some(windows) = win::windows_dir() {
            env.push(("SystemRoot".into(), windows.into()));
        }
        if let Some(data) = win::program_data() {
            env.push(("ProgramData".into(), data.into()));
        }
    }
    if rustup.is_dir() {
        env.push(("RUSTUP_HOME".into(), rustup.into()));
    }
    Ok(env)
}

/// Run `command` in `dir` until `deadline`, its output appended to `log`;
/// the exit code (`-1` when it was killed or ended by a signal). The
/// command leads its own process group, which is killed on the deadline.
pub(super) fn run_command(
    dir: &Path,
    command: &str,
    env: &[(String, std::ffi::OsString)],
    log: &mut std::fs::File,
    deadline: Instant,
) -> Result<i32, GateError> {
    let out = log
        .try_clone()
        .map_err(|e| GateError::Check(format!("the run log: {e}")))?;
    let err = log
        .try_clone()
        .map_err(|e| GateError::Check(format!("the run log: {e}")))?;
    let mut cmd = Command::new(shell()?);
    // Windows: the command goes to `sh` as a script file in the run's temp
    // directory, so no Windows command-line quoting stands between the
    // command and the shell.
    #[cfg(windows)]
    let _script = {
        let tmp = env
            .iter()
            .find(|(k, _)| k == "TMPDIR")
            .map(|(_, v)| PathBuf::from(v))
            .ok_or_else(|| GateError::Check("the run has no temp directory".into()))?;
        let mut file = tempfile::Builder::new()
            .prefix("g6-command-")
            .suffix(".sh")
            .tempfile_in(&tmp)
            .map_err(|e| GateError::Check(format!("the command file: {e}")))?;
        file.write_all(command.as_bytes())
            .map_err(|e| GateError::Check(format!("the command file: {e}")))?;
        let path = file.into_temp_path();
        cmd.arg(&*path);
        path
    };
    #[cfg(not(windows))]
    cmd.arg("-c").arg(command);
    cmd.env_clear()
        .envs(env.iter().map(|(k, v)| (k, v)))
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        cmd.process_group(0);
    }
    // Windows: the job holds what the shell starts, as the process group on
    // Unix. The child starts suspended and runs only once it is in the job,
    // so nothing it starts can escape; without a job there is no run.
    // Dropping the job at the end kills whatever of the run is left.
    #[cfg(windows)]
    let job = {
        use std::os::windows::process::CommandExt as _;
        cmd.creation_flags(win::CREATE_SUSPENDED);
        win::Job::new().ok_or_else(|| GateError::Check("no job object for the run".into()))?
    };
    let mut child = cmd
        .spawn()
        .map_err(|e| GateError::Check(format!("the command did not start: {e}")))?;
    #[cfg(windows)]
    if !job.assign_and_resume(&child) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(GateError::Check(
            "the run could not be held in its job object".into(),
        ));
    }
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status.code().unwrap_or(-1)),
            Ok(None) if Instant::now() >= deadline => {
                #[cfg(unix)]
                if let Ok(group) = i32::try_from(child.id()) {
                    // SAFETY: signals the process group the child leads.
                    unsafe {
                        libc::kill(-group, libc::SIGKILL);
                    }
                }
                #[cfg(windows)]
                job.terminate();
                let _ = child.kill();
                let _ = child.wait();
                let _ = writeln!(log, "\n[G6: the run passed its time limit and was stopped]");
                return Ok(-1);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(GateError::Check(format!("waiting for the command: {e}"))),
        }
    }
}

/// The inode facts of an entry that a process of the same user cannot put
/// back: device and inode (a rename over it gives another inode), mode, and
/// the change time (any write, chmod, link or rename sets it to now, and no
/// call sets it back).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Stamp {
    dev: u64,
    ino: u64,
    mode: u32,
    ctime: i64,
    ctime_ns: i64,
}

impl Stamp {
    #[cfg(unix)]
    fn of(meta: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt as _;
        Self {
            dev: meta.dev(),
            ino: meta.ino(),
            mode: meta.mode(),
            ctime: meta.ctime(),
            ctime_ns: meta.ctime_nsec(),
        }
    }

    /// Windows has no change time a process cannot set back and no inode
    /// in stable std: the attributes (read-only among them) and the last
    /// write time, which a process of the same user can restore (module
    /// docs).
    #[cfg(windows)]
    fn of(meta: &std::fs::Metadata) -> Self {
        use std::os::windows::fs::MetadataExt as _;
        let written = meta.last_write_time();
        Self {
            mode: meta.file_attributes(),
            ctime: i64::try_from(written / 10_000_000).unwrap_or(i64::MAX),
            ctime_ns: i64::try_from(written % 10_000_000).unwrap_or_default() * 100,
            ..Self::default()
        }
    }

    #[cfg(not(any(unix, windows)))]
    fn of(_meta: &std::fs::Metadata) -> Self {
        Self::default()
    }
}

/// What a locked entry of the run tree is: a file or a symlink, by the
/// sha256 of its content or link text, with its [`Stamp`].
#[derive(Debug, Clone, PartialEq, Eq)]
enum Held {
    File(String, Stamp),
    Link(String, Stamp),
}

/// Every file and symlink under a locked path in the run tree, keyed by its
/// repository path, and every directory on the way to one (below the tree's
/// root) with its [`Stamp`]. `root` is the tree root (compared by device and
/// inode only, since cargo writing `Cargo.lock` moves its change time);
/// `chain` is the private per-run directory that holds only the tree
/// (compared whole, so renaming the tree in or out of it is seen) and every
/// directory above it up to `/` ([`chain`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct LockedFiles {
    held: BTreeMap<String, Held>,
    dirs: BTreeMap<String, Option<Stamp>>,
    root: Option<(u64, u64)>,
    chain: Vec<(PathBuf, Option<Stamp>)>,
}

/// Whether this user could rename `dir`: its parent is this user's (who
/// can chmod it writable) or writable by them now (`/tmp` is, sticky or
/// not: they may rename their own entries there).
#[cfg(unix)]
fn movable(dir: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::MetadataExt as _;
    let Some(parent) = dir.parent() else {
        return false;
    };
    // SAFETY: getuid takes no arguments and cannot fail.
    let uid = unsafe { libc::getuid() };
    if std::fs::metadata(parent).is_ok_and(|m| m.uid() == uid) {
        return true;
    }
    let Ok(c) = std::ffi::CString::new(parent.as_os_str().as_bytes()) else {
        return true;
    };
    // SAFETY: `c` is a NUL-terminated path that lives across the call.
    unsafe { libc::access(c.as_ptr(), libc::W_OK) == 0 }
}

#[cfg(not(unix))]
fn movable(_dir: &Path) -> bool {
    true
}

/// The per-run directory `cradle` and every directory above it, each with
/// its [`Stamp`]: whole for the per-run directory; device and inode for
/// the rest, whose renames the [`MoveWatch`] sees (Linux, macOS). Where no
/// watch sees them, also whole for every directory this user could rename
/// ([`movable`]): swapping one for a copy and back renames it, which moves
/// its change time (R5-1); entries other processes make there move it too,
/// so such a `runs` fails closed. Root's
/// directories (`/tmp`, `/`) keep device and inode only. On Windows only
/// the per-run directory is compared (its attributes and write time; the
/// directories above have no stamp a rename moves, module docs).
fn chain(cradle: &Path) -> Vec<(PathBuf, Option<Stamp>)> {
    cradle
        .ancestors()
        .enumerate()
        .map(|(i, dir)| {
            let stamp = std::fs::symlink_metadata(dir)
                .ok()
                .filter(std::fs::Metadata::is_dir)
                .map(|m| {
                    let s = Stamp::of(&m);
                    if i == 0 || (cfg!(unix) && !MoveWatch::SEES_RENAMES && movable(dir)) {
                        s
                    } else {
                        Stamp {
                            dev: s.dev,
                            ino: s.ino,
                            ..Stamp::default()
                        }
                    }
                });
            (dir.to_path_buf(), stamp)
        })
        .collect()
}

/// Linux: an inotify watch for `IN_MOVE_SELF`/`IN_DELETE_SELF` on the
/// per-run directory and every directory above it, set before the run. A
/// rename of any of them during the run, even one put back, is an event
/// (R5-1); entries other processes make there are not.
#[cfg(target_os = "linux")]
pub(super) struct MoveWatch(std::os::fd::OwnedFd);

#[cfg(target_os = "linux")]
impl MoveWatch {
    pub(super) fn new(cradle: &Path) -> Result<Self, GateError> {
        use std::os::fd::{AsRawFd as _, FromRawFd as _};
        use std::os::unix::ffi::OsStrExt as _;
        // SAFETY: plain syscall; the fd is owned right below.
        let raw = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if raw < 0 {
            return Err(GateError::Check(format!(
                "watching the run directory: {}",
                std::io::Error::last_os_error()
            )));
        }
        // SAFETY: `raw` is a fresh fd nothing else owns.
        let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw) };
        for dir in cradle.ancestors() {
            let c = std::ffi::CString::new(dir.as_os_str().as_bytes())
                .map_err(|_| GateError::Check("a run path with a NUL byte".into()))?;
            // SAFETY: `c` is NUL-terminated and lives across the call.
            let wd = unsafe {
                libc::inotify_add_watch(
                    fd.as_raw_fd(),
                    c.as_ptr(),
                    libc::IN_MOVE_SELF | libc::IN_DELETE_SELF | libc::IN_ONLYDIR,
                )
            };
            if wd < 0 {
                return Err(GateError::Check(format!(
                    "watching {}: {}",
                    dir.display(),
                    std::io::Error::last_os_error()
                )));
            }
        }
        Ok(Self(fd))
    }

    /// Whether any watched directory was renamed or removed (or the queue
    /// overflowed, or reading failed: fail closed).
    pub(super) fn moved(&self) -> bool {
        use std::os::fd::AsRawFd as _;
        let mut buf = [0u8; 4096];
        // SAFETY: reads at most `buf.len()` bytes into `buf`.
        let n = unsafe { libc::read(self.0.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
        !(n < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::WouldBlock)
    }
}

#[cfg(target_os = "linux")]
impl MoveWatch {
    /// [`chain`] compares the directories above the per-run one by device
    /// and inode only: this watch sees their renames.
    const SEES_RENAMES: bool = true;
}

/// macOS: a kqueue `EVFILT_VNODE` filter for `NOTE_RENAME`/`NOTE_DELETE`/
/// `NOTE_REVOKE` on the per-run directory and every directory above it,
/// each held open (`O_EVTONLY`) from before the run. A rename of any of
/// them during the run, even one put back, is an event (R5-1); entries
/// other processes make there are not (their `NOTE_WRITE` is not asked for).
#[cfg(target_vendor = "apple")]
pub(super) struct MoveWatch {
    queue: std::os::fd::OwnedFd,
    _dirs: Vec<std::os::fd::OwnedFd>,
}

#[cfg(target_vendor = "apple")]
impl MoveWatch {
    const SEES_RENAMES: bool = true;

    pub(super) fn new(cradle: &Path) -> Result<Self, GateError> {
        use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
        use std::os::unix::ffi::OsStrExt as _;
        let fail = |what: &Path| {
            GateError::Check(format!(
                "watching {}: {}",
                what.display(),
                std::io::Error::last_os_error()
            ))
        };
        // SAFETY: plain syscall; the fd is owned right below.
        let raw = unsafe { libc::kqueue() };
        if raw < 0 {
            return Err(fail(cradle));
        }
        // SAFETY: `raw` is a fresh fd nothing else owns.
        let queue = unsafe { OwnedFd::from_raw_fd(raw) };
        let mut dirs = Vec::new();
        for dir in cradle.ancestors() {
            let c = std::ffi::CString::new(dir.as_os_str().as_bytes())
                .map_err(|_| GateError::Check("a run path with a NUL byte".into()))?;
            // SAFETY: `c` is NUL-terminated and lives across the call.
            let fd = unsafe {
                libc::open(
                    c.as_ptr(),
                    libc::O_EVTONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                return Err(fail(dir));
            }
            // SAFETY: `fd` is a fresh fd nothing else owns.
            let fd = unsafe { OwnedFd::from_raw_fd(fd) };
            let ident = usize::try_from(fd.as_raw_fd()).map_err(|_| fail(dir))?;
            let change = libc::kevent {
                ident,
                filter: libc::EVFILT_VNODE,
                flags: libc::EV_ADD | libc::EV_CLEAR,
                fflags: libc::NOTE_RENAME | libc::NOTE_DELETE | libc::NOTE_REVOKE,
                data: 0,
                udata: std::ptr::null_mut(),
            };
            // SAFETY: one change from a live local, no event buffer.
            let added = unsafe {
                libc::kevent(
                    queue.as_raw_fd(),
                    &raw const change,
                    1,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null(),
                )
            };
            if added < 0 {
                return Err(fail(dir));
            }
            dirs.push(fd);
        }
        Ok(Self { queue, _dirs: dirs })
    }

    /// Whether any watched directory was renamed or removed (or polling
    /// failed: fail closed).
    pub(super) fn moved(&self) -> bool {
        use std::os::fd::AsRawFd as _;
        // SAFETY: kevent is plain old data; all zeroes is a valid value.
        let mut event: libc::kevent = unsafe { std::mem::zeroed() };
        let now = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: no changes; room for one event in a live local; zero wait.
        let n = unsafe {
            libc::kevent(
                self.queue.as_raw_fd(),
                std::ptr::null(),
                0,
                &raw mut event,
                1,
                &raw const now,
            )
        };
        n != 0
    }
}

/// Elsewhere the change times of [`chain`] do this job.
#[cfg(not(any(target_os = "linux", target_vendor = "apple")))]
pub(super) struct MoveWatch;

#[cfg(not(any(target_os = "linux", target_vendor = "apple")))]
impl MoveWatch {
    const SEES_RENAMES: bool = false;

    #[allow(clippy::unnecessary_wraps)]
    pub(super) fn new(_cradle: &Path) -> Result<Self, GateError> {
        Ok(Self)
    }

    pub(super) fn moved(&self) -> bool {
        false
    }
}

impl LockedFiles {
    /// Read the locked entries of `tree` (symlinks are not followed).
    pub(super) fn read(tree: &Path, locked: &[String]) -> Result<Self, GateError> {
        let mut held = BTreeMap::new();
        if !locked.is_empty() {
            walk(tree, tree, &mut |rel, path, meta| {
                if !locked.iter().any(|c| under_locked(rel, c)) {
                    return Ok(());
                }
                let fail = |e: std::io::Error| GateError::Check(format!("reading {rel}: {e}"));
                let stamp = Stamp::of(meta);
                let entry = if meta.file_type().is_symlink() {
                    let target = std::fs::read_link(path).map_err(fail)?;
                    Held::Link(
                        hex::encode(Sha256::digest(target.to_string_lossy().as_bytes())),
                        stamp,
                    )
                } else {
                    Held::File(
                        hex::encode(Sha256::digest(std::fs::read(path).map_err(fail)?)),
                        stamp,
                    )
                };
                held.insert(rel.to_owned(), entry);
                Ok(())
            })?;
        }
        let mut dirs = BTreeMap::new();
        for rel in held.keys() {
            for dir in parents(rel) {
                dirs.entry(dir.clone()).or_insert_with(|| {
                    std::fs::symlink_metadata(tree.join(&dir))
                        .ok()
                        .filter(std::fs::Metadata::is_dir)
                        .map(|m| Stamp::of(&m))
                });
            }
        }
        // The tree root and the directories above it: a swap of the whole
        // tree, or of a directory above it, touches no entry inside, only
        // these (R4-1, R5-1).
        let root = std::fs::symlink_metadata(tree)
            .ok()
            .filter(std::fs::Metadata::is_dir)
            .map(|m| {
                let s = Stamp::of(&m);
                (s.dev, s.ino)
            });
        let chain = tree.parent().map(chain).unwrap_or_default();
        Ok(Self {
            held,
            dirs,
            root,
            chain,
        })
    }

    /// How the tree's locked entries now differ from `self`, the tree as
    /// `spec_commit` holds it once locked. A file whose content, mode, inode
    /// or change time differs is `modified`, whatever it holds now; a
    /// directory on its way that changed does the same to every file below.
    /// Re-reads only the pinned directories, so an entry added elsewhere
    /// under a locked path comes from the walk.
    /// `moved`: the [`MoveWatch`] saw a directory above the tree renamed.
    pub(super) fn changes(
        &self,
        tree: &Path,
        now: &Self,
        moved: bool,
    ) -> BTreeMap<String, LockedChange> {
        let mut changed = BTreeMap::new();
        for (path, was) in &self.held {
            match now.held.get(path) {
                None => {
                    changed.insert(path.clone(), LockedChange::Deleted);
                }
                Some(is) if std::mem::discriminant(is) != std::mem::discriminant(was) => {
                    changed.insert(path.clone(), LockedChange::TypeChanged);
                }
                Some(is) if is != was => {
                    changed.insert(path.clone(), LockedChange::Modified);
                }
                Some(_) => {}
            }
        }
        for (dir, was) in &self.dirs {
            let is = std::fs::symlink_metadata(tree.join(dir))
                .ok()
                .filter(std::fs::Metadata::is_dir)
                .map(|m| Stamp::of(&m));
            if is != *was {
                for path in self
                    .held
                    .keys()
                    .filter(|p| p.starts_with(&format!("{dir}/")))
                {
                    changed
                        .entry(path.clone())
                        .or_insert(LockedChange::Modified);
                }
            }
        }
        for path in now.held.keys() {
            if !self.held.contains_key(path) {
                changed.insert(path.clone(), LockedChange::Added);
            }
        }
        // The tree root swapped, or the per-run directory or one above it
        // touched: every locked file is suspect, whatever its own stamp
        // says now.
        let root_moved = self.root.is_some() && self.root != now.root;
        let chain_moved = moved || self.chain != now.chain;
        if (root_moved || chain_moved) && !self.held.is_empty() {
            for path in self.held.keys() {
                changed
                    .entry(path.clone())
                    .or_insert(LockedChange::Modified);
            }
        }
        changed
    }

    /// Make every locked file read-only (0444) and every directory on its
    /// way below the tree's root 0555, so a plain write of the run fails.
    /// Read the entries again afterwards: the chmod moves their change time.
    #[cfg(unix)]
    pub(super) fn lock(&self, tree: &Path) -> Result<(), GateError> {
        use std::os::unix::fs::PermissionsExt as _;
        let set = |path: &Path, mode: u32| {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
                .map_err(|e| GateError::Check(format!("{}: {e}", path.display())))
        };
        for (rel, held) in &self.held {
            if matches!(held, Held::File(..)) {
                set(&tree.join(rel), 0o444)?;
            }
        }
        for dir in self.dirs.keys() {
            set(&tree.join(dir), 0o555)?;
        }
        // The per-run directory holds only the tree: read-only, so moving
        // the tree out or back must first change its mode and change time
        // (R4-1). [`Unlock`] on that directory opens it again.
        if let Some(cradle) = tree.parent() {
            set(cradle, 0o555)?;
        }
        Ok(())
    }

    /// Windows: every locked file gets the read-only attribute (directories
    /// have none that stops a write).
    #[cfg(not(unix))]
    pub(super) fn lock(&self, tree: &Path) -> Result<(), GateError> {
        for (rel, held) in &self.held {
            if matches!(held, Held::File(..)) {
                let path = tree.join(rel);
                let mut perms = std::fs::symlink_metadata(&path)
                    .map_err(|e| GateError::Check(format!("{}: {e}", path.display())))?
                    .permissions();
                perms.set_readonly(true);
                std::fs::set_permissions(&path, perms)
                    .map_err(|e| GateError::Check(format!("{}: {e}", path.display())))?;
            }
        }
        Ok(())
    }
}

/// The directories on the way to the repository path `rel`, below the root.
fn parents(rel: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut parent = Path::new(rel).parent();
    while let Some(dir) = parent.filter(|d| !d.as_os_str().is_empty()) {
        out.push(dir.to_string_lossy().replace('\\', "/"));
        parent = dir.parent();
    }
    out
}

/// Visit every entry below `dir` (symlinks not followed) with its path
/// relative to `root`; directories are descended into, not visited.
fn walk(
    root: &Path,
    dir: &Path,
    visit: &mut dyn FnMut(&str, &Path, &std::fs::Metadata) -> Result<(), GateError>,
) -> Result<(), GateError> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| GateError::Check(format!("reading {}: {e}", dir.display())))?;
    for entry in entries {
        let entry = entry.map_err(|e| GateError::Check(format!("reading the tree: {e}")))?;
        let path = entry.path();
        let meta = std::fs::symlink_metadata(&path)
            .map_err(|e| GateError::Check(format!("{}: {e}", path.display())))?;
        if meta.is_dir() {
            walk(root, &path, visit)?;
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .map_err(|_| GateError::Check("a path outside the run tree".into()))?
            .to_string_lossy()
            .replace('\\', "/");
        visit(&rel, &path, &meta)?;
    }
    Ok(())
}

/// Makes every directory of the run tree writable again when dropped (on
/// Windows every read-only file), so the tree can be removed whatever the
/// run left read-only.
pub(super) struct Unlock(pub(super) PathBuf);

impl Drop for Unlock {
    fn drop(&mut self) {
        fn open_up(dir: &Path) {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
            }
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    match std::fs::symlink_metadata(&path) {
                        Ok(m) if m.is_dir() => open_up(&path),
                        #[cfg(not(unix))]
                        Ok(m) if m.is_file() && m.permissions().readonly() => {
                            let mut perms = m.permissions();
                            #[expect(
                                clippy::permissions_set_readonly_false,
                                reason = "Windows: clears the read-only attribute, no mode bits"
                            )]
                            perms.set_readonly(false);
                            let _ = std::fs::set_permissions(&path, perms);
                        }
                        _ => {}
                    }
                }
            }
        }
        open_up(&self.0);
    }
}
