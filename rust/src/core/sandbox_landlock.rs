use std::path::Path;
use std::process::Command;

/// Describes which filesystem paths the Landlock sandbox should allow.
/// Deny-all by default; explicitly listed paths get read or read+write access.
pub struct LandlockRuleset {
    pub read_paths: Vec<String>,
    pub read_write_paths: Vec<String>,
    pub interpreter: String,
}

impl LandlockRuleset {
    #[must_use]
    pub fn new(allowed_read_paths: &[&Path], interpreter_path: &str) -> Self {
        let mut read_paths = vec![
            "/usr".to_string(),
            "/lib".to_string(),
            "/lib64".to_string(),
            "/etc".to_string(),
            "/dev/null".to_string(),
            "/dev/urandom".to_string(),
            "/proc/self".to_string(),
            interpreter_path.to_string(),
        ];

        for p in allowed_read_paths {
            read_paths.push(p.display().to_string());
        }

        let sandbox_tmp = std::env::temp_dir().join("lean-ctx-sandbox");
        let read_write_paths = vec!["/tmp".to_string(), sandbox_tmp.display().to_string()];

        Self {
            read_paths,
            read_write_paths,
            interpreter: interpreter_path.to_string(),
        }
    }

    #[must_use]
    pub fn contains_read_path(&self, path: &str) -> bool {
        self.read_paths.iter().any(|p| p == path)
    }

    #[must_use]
    pub fn contains_rw_path(&self, path: &str) -> bool {
        self.read_write_paths.iter().any(|p| p == path)
    }
}

// ---------------------------------------------------------------------------
// Landlock enforcement via raw syscalls (Linux 5.13+)
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
mod landlock_sys {
    //! Minimal Landlock ABI wrappers using raw syscalls via libc.
    //! Avoids an external crate dependency while supporting ABI v1+.

    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    const LANDLOCK_CREATE_RULESET: libc::c_long = 444;
    const LANDLOCK_ADD_RULE: libc::c_long = 445;
    const LANDLOCK_RESTRICT_SELF: libc::c_long = 446;

    const LANDLOCK_RULE_PATH_BENEATH: u32 = 1;

    // ABI v1 access flags (filesystem)
    const LANDLOCK_ACCESS_FS_EXECUTE: u64 = 1 << 0;
    const LANDLOCK_ACCESS_FS_WRITE_FILE: u64 = 1 << 1;
    const LANDLOCK_ACCESS_FS_READ_FILE: u64 = 1 << 2;
    const LANDLOCK_ACCESS_FS_READ_DIR: u64 = 1 << 3;
    const LANDLOCK_ACCESS_FS_REMOVE_DIR: u64 = 1 << 4;
    const LANDLOCK_ACCESS_FS_REMOVE_FILE: u64 = 1 << 5;
    const LANDLOCK_ACCESS_FS_MAKE_CHAR: u64 = 1 << 6;
    const LANDLOCK_ACCESS_FS_MAKE_DIR: u64 = 1 << 7;
    const LANDLOCK_ACCESS_FS_MAKE_REG: u64 = 1 << 8;
    const LANDLOCK_ACCESS_FS_MAKE_SOCK: u64 = 1 << 9;
    const LANDLOCK_ACCESS_FS_MAKE_FIFO: u64 = 1 << 10;
    const LANDLOCK_ACCESS_FS_MAKE_BLOCK: u64 = 1 << 11;
    const LANDLOCK_ACCESS_FS_MAKE_SYM: u64 = 1 << 12;

    pub(super) const FS_READ: u64 = LANDLOCK_ACCESS_FS_READ_FILE | LANDLOCK_ACCESS_FS_READ_DIR;

    pub(super) const FS_ALL: u64 = LANDLOCK_ACCESS_FS_EXECUTE
        | LANDLOCK_ACCESS_FS_WRITE_FILE
        | LANDLOCK_ACCESS_FS_READ_FILE
        | LANDLOCK_ACCESS_FS_READ_DIR
        | LANDLOCK_ACCESS_FS_REMOVE_DIR
        | LANDLOCK_ACCESS_FS_REMOVE_FILE
        | LANDLOCK_ACCESS_FS_MAKE_CHAR
        | LANDLOCK_ACCESS_FS_MAKE_DIR
        | LANDLOCK_ACCESS_FS_MAKE_REG
        | LANDLOCK_ACCESS_FS_MAKE_SOCK
        | LANDLOCK_ACCESS_FS_MAKE_FIFO
        | LANDLOCK_ACCESS_FS_MAKE_BLOCK
        | LANDLOCK_ACCESS_FS_MAKE_SYM;

    #[repr(C)]
    struct LandlockRulesetAttr {
        handled_access_fs: u64,
    }

    #[repr(C)]
    struct LandlockPathBeneathAttr {
        allowed_access: u64,
        parent_fd: i32,
    }

    fn landlock_create_ruleset(handled_access_fs: u64) -> Result<i32, String> {
        let attr = LandlockRulesetAttr { handled_access_fs };
        // SAFETY: `attr` is a live, fully-initialised struct; the kernel reads
        // exactly `size_of::<LandlockRulesetAttr>()` bytes through the const
        // pointer and returns a file descriptor or a negative errno.
        let fd = unsafe {
            libc::syscall(
                LANDLOCK_CREATE_RULESET,
                &raw const attr,
                std::mem::size_of::<LandlockRulesetAttr>(),
                0u32,
            )
        };
        if fd < 0 {
            return Err(format!(
                "landlock_create_ruleset failed (errno {}); kernel may not support Landlock",
                std::io::Error::last_os_error()
            ));
        }
        Ok(fd as i32)
    }

    fn landlock_add_path_rule(ruleset_fd: i32, path: &Path, access: u64) -> Result<(), String> {
        let c_path =
            CString::new(path.as_os_str().as_bytes()).map_err(|e| format!("invalid path: {e}"))?;

        // SAFETY: `c_path` is a live CString; `open` only reads its NUL-
        // terminated bytes and returns a descriptor or -1.
        let parent_fd = unsafe { libc::open(c_path.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
        if parent_fd < 0 {
            return Err(format!(
                "open O_PATH '{}': {}",
                path.display(),
                std::io::Error::last_os_error()
            ));
        }

        let attr = LandlockPathBeneathAttr {
            allowed_access: access,
            parent_fd,
        };

        // SAFETY: `ruleset_fd` is a valid open ruleset descriptor and `attr` is
        // a live, initialised `LandlockPathBeneathAttr` read by const pointer.
        let ret = unsafe {
            libc::syscall(
                LANDLOCK_ADD_RULE,
                ruleset_fd,
                LANDLOCK_RULE_PATH_BENEATH,
                &raw const attr,
                0u32,
            )
        };

        // SAFETY: `parent_fd` is a valid descriptor opened above and is not used
        // again after being closed here.
        unsafe { libc::close(parent_fd) };

        if ret < 0 {
            return Err(format!(
                "landlock_add_rule '{}': {}",
                path.display(),
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }

    fn landlock_restrict_self(ruleset_fd: i32) -> Result<(), String> {
        // SAFETY: `ruleset_fd` is a valid Landlock ruleset descriptor; the
        // syscall takes integer arguments only.
        let ret = unsafe { libc::syscall(LANDLOCK_RESTRICT_SELF, ruleset_fd, 0u32) };
        if ret < 0 {
            return Err(format!(
                "landlock_restrict_self: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }

    /// Apply the Landlock ruleset to the current process.
    /// Returns `Ok(true)` if enforced, `Ok(false)` if Landlock is unsupported.
    pub(super) fn apply(ruleset: &super::LandlockRuleset) -> Result<bool, String> {
        // no_new_privs is required for unprivileged Landlock
        // SAFETY: `prctl(PR_SET_NO_NEW_PRIVS, …)` takes fixed integer arguments
        // and dereferences no pointers.
        let ret = unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
        if ret < 0 {
            return Err(format!(
                "prctl(NO_NEW_PRIVS): {}",
                std::io::Error::last_os_error()
            ));
        }

        let ruleset_fd = match landlock_create_ruleset(FS_ALL) {
            Ok(fd) => fd,
            Err(e) => {
                eprintln!("[lean-ctx] landlock not supported: {e}");
                return Ok(false);
            }
        };

        for path_str in &ruleset.read_paths {
            let path = Path::new(path_str);
            if path.exists()
                && let Err(e) = landlock_add_path_rule(ruleset_fd, path, FS_READ)
            {
                eprintln!("[lean-ctx] landlock: skipping read rule for {path_str}: {e}");
            }
        }

        for path_str in &ruleset.read_write_paths {
            let path = Path::new(path_str);
            if std::fs::create_dir_all(path).is_err() {
                eprintln!("[lean-ctx] landlock: cannot ensure dir {path_str}");
            }
            if path.exists()
                && let Err(e) = landlock_add_path_rule(ruleset_fd, path, FS_ALL)
            {
                eprintln!("[lean-ctx] landlock: skipping rw rule for {path_str}: {e}");
            }
        }

        landlock_restrict_self(ruleset_fd)?;
        // SAFETY: `ruleset_fd` is a valid descriptor created above and is not
        // used again after being closed here.
        unsafe { libc::close(ruleset_fd) };

        Ok(true)
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

pub fn execute_sandboxed(
    interpreter: &str,
    args: &[&str],
    allowed_read_paths: &[&Path],
    env: &[(String, String)],
    timeout_secs: u64,
) -> Result<(String, String, i32), String> {
    let ruleset = LandlockRuleset::new(allowed_read_paths, interpreter);
    execute_with_landlock(&ruleset, interpreter, args, env, timeout_secs)
}

#[cfg(target_os = "linux")]
fn execute_with_landlock(
    ruleset: &LandlockRuleset,
    interpreter: &str,
    args: &[&str],
    env: &[(String, String)],
    timeout_secs: u64,
) -> Result<(String, String, i32), String> {
    use std::os::unix::process::CommandExt;

    let mut cmd = Command::new(interpreter);
    cmd.args(args);

    cmd.env_clear();
    cmd.env("PATH", "/usr/bin:/bin:/usr/local/bin");
    cmd.env("HOME", std::env::var("HOME").unwrap_or_default());
    cmd.env("LEAN_CTX_SANDBOX", "1");
    for (k, v) in env {
        cmd.env(k, v);
    }

    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let read_paths = ruleset.read_paths.clone();
    let rw_paths = ruleset.read_write_paths.clone();
    let interp = ruleset.interpreter.clone();

    // SAFETY: `pre_exec` runs the closure in the forked child before `exec`.
    // The closure operates only on data captured by value and applies Landlock
    // via direct libc syscalls. The sandboxed command is spawned from a
    // controlled, effectively single-threaded path, so the heap clones it makes
    // cannot deadlock on an allocator lock held across the fork.
    unsafe {
        cmd.pre_exec(move || {
            let rs = LandlockRuleset {
                read_paths: read_paths.clone(),
                read_write_paths: rw_paths.clone(),
                interpreter: interp.clone(),
            };
            match landlock_sys::apply(&rs) {
                Ok(true) => Ok(()),
                Ok(false) => {
                    eprintln!("[lean-ctx] landlock: not enforced, continuing unsandboxed");
                    Ok(())
                }
                Err(e) => Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, e)),
            }
        });
    }

    let child = cmd
        .spawn()
        .map_err(|e| format!("landlock spawn failed: {e}"))?;

    let output = wait_with_timeout(child, timeout_secs)?;

    Ok((
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
        output.status.code().unwrap_or(1),
    ))
}

#[cfg(not(target_os = "linux"))]
fn execute_with_landlock(
    _ruleset: &LandlockRuleset,
    _interpreter: &str,
    _args: &[&str],
    _env: &[(String, String)],
    _timeout_secs: u64,
) -> Result<(String, String, i32), String> {
    unreachable!("sandbox_landlock module should only be called on Linux")
}

fn wait_with_timeout(
    mut child: std::process::Child,
    timeout_secs: u64,
) -> Result<std::process::Output, String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().map_err(|e| e.to_string()),
            Ok(None) => {
                if std::time::Instant::now() > deadline {
                    let _ = child.kill();
                    return Err(format!("Execution timed out after {timeout_secs}s"));
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn ruleset_denies_all_by_default() {
        let rs = LandlockRuleset::new(&[], "/usr/bin/python3");
        assert!(!rs.contains_read_path("/home/user/secret"));
        assert!(!rs.contains_rw_path("/home/user/secret"));
    }

    #[test]
    fn ruleset_includes_system_dirs() {
        let rs = LandlockRuleset::new(&[], "/usr/bin/python3");
        assert!(rs.contains_read_path("/usr"));
        assert!(rs.contains_read_path("/lib"));
        assert!(rs.contains_read_path("/lib64"));
        assert!(rs.contains_read_path("/etc"));
    }

    #[test]
    fn ruleset_includes_interpreter() {
        let rs = LandlockRuleset::new(&[], "/usr/bin/python3");
        assert!(rs.contains_read_path("/usr/bin/python3"));
    }

    #[test]
    fn ruleset_includes_allowed_paths() {
        let p = PathBuf::from("/home/user/project");
        let rs = LandlockRuleset::new(&[p.as_path()], "/usr/bin/python3");
        assert!(rs.contains_read_path("/home/user/project"));
    }

    #[test]
    fn ruleset_allows_tmp_rw() {
        let rs = LandlockRuleset::new(&[], "/usr/bin/python3");
        assert!(rs.contains_rw_path("/tmp"));
        let sandbox_tmp = std::env::temp_dir().join("lean-ctx-sandbox");
        assert!(rs.contains_rw_path(&sandbox_tmp.display().to_string()));
    }

    #[test]
    fn ruleset_includes_dev_null() {
        let rs = LandlockRuleset::new(&[], "/bin/echo");
        assert!(rs.contains_read_path("/dev/null"));
        assert!(rs.contains_read_path("/dev/urandom"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires Linux 5.13+ with Landlock; run manually"]
    fn landlock_exec_echo() {
        let result = execute_sandboxed("/bin/echo", &["hello"], &[], &[], 5);
        assert!(result.is_ok());
        let (stdout, _, code) = result.unwrap();
        assert_eq!(code, 0);
        assert!(stdout.contains("hello"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires Linux 5.13+ with Landlock; run manually"]
    fn landlock_denies_read_outside_allowed() {
        let result = execute_sandboxed("/bin/cat", &["/root/.bashrc"], &[], &[], 5);
        if let Ok((_, _, code)) = result {
            assert_ne!(code, 0, "cat should fail reading outside allowed paths");
        }
    }
}
