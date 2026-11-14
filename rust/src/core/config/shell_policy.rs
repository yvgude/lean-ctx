// SPDX-License-Identifier: Apache-2.0
//! Where shell-security decisions come from.
//!
//! The shell gate (`check_shell_allowlist`, run by the hooks, inside
//! `lean-ctx -c` / `-t` and by `ctx_shell`) decides what an agent's shell may
//! run. A command line from that shell can set environment variables for the
//! lean-ctx it starts (`export`, `env -S`, `set -a`, `declare -n`, `printf -v`,
//! names built by expansion, …) and can write files under any directory it
//! names in `HOME`, `XDG_CONFIG_HOME` or `LEAN_CTX_CONFIG_DIR`. Blocking each
//! of those spellings is a race the gate cannot win. So in release builds the
//! policy that can loosen the gate is read only from places a command line
//! cannot move:
//!
//! - the compiled defaults;
//! - the global `config.toml` of the user this process runs as, at that
//!   user's home as the operating system records it (the passwd database on
//!   Unix, the profile folder from `SHGetKnownFolderPath` on Windows) — never
//!   `$HOME`, `%USERPROFILE%`, `XDG_CONFIG_HOME`, `LEAN_CTX_CONFIG_DIR` or
//!   `LEAN_CTX_DATA_DIR`;
//! - a project's `.lean-ctx.toml`, for a workspace trusted in the trust store
//!   next to that config (the trust environment variables are ignored).
//!
//! No `LEAN_CTX_SHELL_*` (or other gate-loosening) environment variable is
//! read for these decisions.
//!
//! Trade-off: someone who relocates lean-ctx's config with
//! `LEAN_CTX_CONFIG_DIR` (or a non-default `XDG_CONFIG_HOME`) keeps every
//! other setting there, but the shell-security settings (`shell_security`,
//! `shell_allowlist`, `shell_allowlist_extra`, `shell_strict_mode`,
//! `shell_allow_inline_scripts`, `shell_allow_writes`, `shell_approval`,
//! `shell_hook_mode`, `shell_allowlist_subcommand_scoping`) are read from the
//! default location in the real home. Without a file there the secure
//! defaults apply.
//!
//! Tests and debug builds (`cfg(test)` / `debug_assertions`) keep the ambient
//! resolution (`Config::load` plus the environment variables) so the test
//! suites stay hermetic through `isolated_data_dir` and the env helpers. A
//! unit test pins the release resolution with `pin_for_test`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use super::{Config, ShellApproval};

/// The source the gate reads its policy from.
enum Source {
    /// `Config::load()` and the environment: test and debug builds only.
    #[cfg_attr(not(any(test, debug_assertions)), allow(dead_code))]
    Ambient,
    /// The real user's home (`None`: unresolvable, so built-in defaults).
    /// Debug builds outside tests never construct it.
    #[cfg_attr(all(debug_assertions, not(test)), allow(dead_code))]
    Pinned(Option<PathBuf>),
}

/// A test's stand-in for the release resolution.
#[cfg(any(test, feature = "test-support"))]
#[derive(Clone)]
enum TestPin {
    /// The account database names this home.
    Home(PathBuf),
    /// The account database has no home for this user (#2236).
    Unresolvable,
}

#[cfg(any(test, feature = "test-support"))]
static TEST_PIN: Mutex<Option<TestPin>> = Mutex::new(None);

fn source() -> Source {
    #[cfg(any(test, feature = "test-support"))]
    {
        let pin = TEST_PIN
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        match pin {
            Some(TestPin::Home(home)) => return Source::Pinned(Some(home)),
            Some(TestPin::Unresolvable) => return Source::Pinned(None),
            None => {}
        }
    }
    #[cfg(any(test, debug_assertions))]
    {
        Source::Ambient
    }
    #[cfg(not(any(test, debug_assertions)))]
    {
        Source::Pinned(real_user_home())
    }
}

/// Test-only: resolve the policy as a release build does, with `home`
/// standing in for the passwd/profile home. Hold the test env lock while the
/// guard lives; dropping it restores the ambient resolution.
#[cfg(any(test, feature = "test-support"))]
#[must_use]
pub fn pin_for_test(home: &Path) -> PinGuard {
    assert!(
        crate::core::data_dir::holds_test_env_lock(),
        "pin_for_test requires test_env_lock()"
    );
    *TEST_PIN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) =
        Some(TestPin::Home(home.to_path_buf()));
    PinGuard(())
}

/// Test-only: resolve the policy as a release build does for an account the
/// operating system cannot resolve to a home (#2236). Hold the test env lock
/// while the guard lives.
#[cfg(any(test, feature = "test-support"))]
#[must_use]
pub fn pin_unresolvable_for_test() -> PinGuard {
    assert!(
        crate::core::data_dir::holds_test_env_lock(),
        "pin_unresolvable_for_test requires test_env_lock()"
    );
    *TEST_PIN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(TestPin::Unresolvable);
    PinGuard(())
}

/// Restores the ambient policy source when dropped.
#[cfg(any(test, feature = "test-support"))]
pub struct PinGuard(());

#[cfg(any(test, feature = "test-support"))]
impl Drop for PinGuard {
    fn drop(&mut self) {
        *TEST_PIN
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }
}

/// True when the gate reads the pinned (release) policy.
#[must_use]
pub fn pinned() -> bool {
    matches!(source(), Source::Pinned(_))
}

/// The home directory of the user this process runs as, from the operating
/// system's account database — never from `$HOME`. Resolved once per process:
/// the effective uid does not change during a run, and the Linux fallback
/// below starts a process.
#[must_use]
pub fn real_user_home() -> Option<PathBuf> {
    static HOME: OnceLock<Option<PathBuf>> = OnceLock::new();
    HOME.get_or_init(lookup_real_user_home).clone()
}

#[cfg(unix)]
fn lookup_real_user_home() -> Option<PathBuf> {
    // SAFETY: geteuid has no arguments or failure state.
    let uid = unsafe { libc::geteuid() };
    // #2236: the static musl build reads only `/etc/passwd` and has no NSS,
    // so an account served by SSSD, LDAP, NIS or winbind is not found there.
    // `getent` is the system's dynamically linked NSS client and sees it.
    // `$HOME` is never a fallback: that is the bypass the pin exists to close.
    #[cfg(target_os = "linux")]
    {
        passwd_home(uid).or_else(|| getent_home(uid))
    }
    #[cfg(not(target_os = "linux"))]
    {
        passwd_home(uid)
    }
}

/// `getpwuid_r(uid)`'s home directory, when it is absolute.
#[cfg(unix)]
fn passwd_home(uid: libc::uid_t) -> Option<PathBuf> {
    use std::ffi::{CStr, OsStr};
    use std::os::unix::ffi::OsStrExt;

    let mut buf = vec![0_u8; 4096];
    loop {
        // SAFETY: `passwd` is a plain C struct; all-zero is a valid value
        // that getpwuid_r overwrites.
        let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        // SAFETY: every pointer refers to live, writable memory of the
        // advertised size for the duration of the call.
        let rc = unsafe {
            libc::getpwuid_r(
                uid,
                &raw mut pwd,
                buf.as_mut_ptr().cast(),
                buf.len(),
                &raw mut result,
            )
        };
        if rc == libc::ERANGE && buf.len() < (1 << 20) {
            buf.resize(buf.len() * 2, 0);
            continue;
        }
        if rc != 0 || result.is_null() || pwd.pw_dir.is_null() {
            return None;
        }
        // SAFETY: on success pw_dir points to a NUL-terminated string inside
        // `buf`, which outlives this borrow.
        let dir = unsafe { CStr::from_ptr(pwd.pw_dir) };
        let path = PathBuf::from(OsStr::from_bytes(dir.to_bytes()));
        return (path.is_absolute()).then_some(path);
    }
}

/// The account database as NSS sees it, through `getent passwd <uid>` (#2236).
///
/// Nothing here comes from the caller's environment: `getent` is taken only
/// from fixed absolute paths, must be a root-owned file no group or other
/// user can write, and runs with an empty environment, so `PATH`,
/// `LD_PRELOAD` and friends cannot pick the program or the answer. The answer
/// counts only when it names this uid and an absolute home.
#[cfg(target_os = "linux")]
fn getent_home(uid: libc::uid_t) -> Option<PathBuf> {
    use std::io::Read;
    use std::os::unix::fs::MetadataExt;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    const CANDIDATES: [&str; 2] = ["/usr/bin/getent", "/bin/getent"];
    const TIMEOUT: Duration = Duration::from_secs(3);

    let getent = CANDIDATES.iter().map(Path::new).find(|path| {
        std::fs::metadata(path)
            .is_ok_and(|meta| meta.is_file() && meta.uid() == 0 && meta.mode() & 0o022 == 0)
    })?;
    let mut child = Command::new(getent)
        .arg("passwd")
        .arg(uid.to_string())
        .env_clear()
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // A directory service that hangs must not hang the gate: give up and
    // fall back to the built-in defaults, which enforce.
    let deadline = Instant::now() + TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    if !status.success() {
        return None;
    }
    let mut out = Vec::new();
    child
        .stdout
        .take()?
        .take(64 * 1024)
        .read_to_end(&mut out)
        .ok()?;
    parse_passwd_home(&out, uid)
}

/// The home field of a `passwd` line (`name:pw:uid:gid:gecos:dir:shell`) when
/// the line names `uid` and an absolute directory. Only the first line counts.
#[cfg(any(all(test, unix), target_os = "linux"))]
fn parse_passwd_home(out: &[u8], uid: u32) -> Option<PathBuf> {
    let line = std::str::from_utf8(out).ok()?.lines().next()?;
    let fields: Vec<&str> = line.split(':').collect();
    if fields.len() < 7 || fields[2].parse::<u32>().ok()? != uid {
        return None;
    }
    let dir = PathBuf::from(fields[5]);
    dir.is_absolute().then_some(dir)
}

/// The home directory of the user this process runs as. `dirs` asks
/// `SHGetKnownFolderPath(FOLDERID_Profile)` on Windows and never reads
/// `%USERPROFILE%` or `%HOME%`.
#[cfg(not(unix))]
fn lookup_real_user_home() -> Option<PathBuf> {
    dirs::home_dir().filter(|p| p.is_absolute())
}

/// The config location for messages: the pinned or ambient `config.toml`,
/// or — for an account the OS cannot resolve (#2236) — a statement that no
/// location exists, instead of a path the gate never reads.
#[must_use]
pub fn config_path_display() -> String {
    match config_path() {
        Some(path) => path.display().to_string(),
        None if pinned() => "none — this user has no account-database entry with a home \
                             directory, so no config.toml location can be derived; the \
                             built-in defaults apply"
            .to_string(),
        None => "~/.lean-ctx/config.toml".to_string(),
    }
}

/// lean-ctx's config directory inside `home`, laid out as an install without
/// any directory override resolves it (legacy `~/.lean-ctx` or a pre-split
/// `~/.config/lean-ctx` still count; otherwise `~/.config/lean-ctx`).
fn config_dir_in(home: &Path) -> PathBuf {
    let xdg_config = home.join(".config");
    let xdg_data = home.join(".local").join("share");
    crate::core::paths::single_dir_override_fs(home, &xdg_config, Some(&xdg_data))
        .unwrap_or_else(|| xdg_config.join("lean-ctx"))
}

/// The global `config.toml` the gate reads its policy from, for messages.
#[must_use]
pub fn config_path() -> Option<PathBuf> {
    match source() {
        Source::Ambient => Config::path(),
        Source::Pinned(home) => home.map(|h| config_dir_in(&h).join("config.toml")),
    }
}

/// The home whose `.claude/settings.json` the gate may mirror.
#[must_use]
pub fn settings_home() -> Option<PathBuf> {
    match source() {
        Source::Ambient => crate::core::home::resolve_home_dir(),
        Source::Pinned(home) => home,
    }
}

type PinnedCache = Option<(String, Arc<Config>)>;

/// The configuration shell-security decisions are made from.
#[must_use]
pub fn config() -> Arc<Config> {
    match source() {
        Source::Ambient => Config::load_arc(),
        Source::Pinned(None) => Arc::new(Config::default()),
        Source::Pinned(Some(home)) => pinned_config(&home),
    }
}

fn pinned_config(home: &Path) -> Arc<Config> {
    static CACHE: Mutex<PinnedCache> = Mutex::new(None);
    let dir = config_dir_in(home);
    let global = std::fs::read_to_string(dir.join("config.toml")).ok();
    // The project is the working directory's checkout: neither
    // `LEAN_CTX_PROJECT_ROOT` nor a session's root decides it here.
    let root = project_root_from_cwd();
    let local = root.as_deref().and_then(|r| {
        let path = Config::local_path(r);
        crate::core::pathutil::may_probe_path(&path)
            .then(|| std::fs::read_to_string(path).ok())
            .flatten()
    });
    let local_hash = local.as_deref().map(crate::core::hasher::hash_str);
    let trusted = match (root.as_deref(), local_hash.as_deref()) {
        (Some(r), Some(hash)) => crate::core::workspace_trust::is_pinned_in_store(
            &dir.join(crate::core::workspace_trust::FILE_NAME),
            Path::new(r),
            hash,
        ),
        _ => false,
    };
    let key = format!(
        "{}\0{}\0{}\0{}\0{trusted}",
        dir.display(),
        global
            .as_deref()
            .map(crate::core::hasher::hash_str)
            .unwrap_or_default(),
        root.as_deref().unwrap_or_default(),
        local_hash.as_deref().unwrap_or_default(),
    );
    let mut guard = CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((cached_key, cfg)) = guard.as_ref()
        && *cached_key == key
    {
        return Arc::clone(cfg);
    }
    // An unparsable file falls back to the defaults, which enforce.
    let mut cfg = global
        .as_deref()
        .and_then(|raw| super::loader::parse_config_with_profile(raw, None).ok())
        .unwrap_or_default();
    if let Some(local) = local.as_deref() {
        super::loader::without_env_profile(|| cfg.merge_local(local, trusted));
    }
    let cfg = Arc::new(cfg);
    *guard = Some((key, Arc::clone(&cfg)));
    cfg
}

/// The nearest directory at or above the working directory that holds a
/// `.git` entry, else the working directory itself.
fn project_root_from_cwd() -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    let root = cwd
        .ancestors()
        .find(|dir| dir.join(".git").exists())
        .unwrap_or(&cwd);
    Some(root.to_string_lossy().into_owned())
}

/// A gate-loosening environment variable, honoured only by the ambient
/// (test/debug) source. Release builds never read it.
#[must_use]
pub fn env(name: &str) -> Option<String> {
    match source() {
        Source::Ambient => std::env::var(name).ok(),
        Source::Pinned(_) => None,
    }
}

fn env_flag(name: &str) -> Option<bool> {
    env(name).map(|raw| {
        matches!(
            raw.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

/// `shell_allow_inline_scripts` for the gate.
#[must_use]
pub fn allow_inline_scripts() -> bool {
    env_flag("LEAN_CTX_SHELL_ALLOW_INLINE_SCRIPTS")
        .unwrap_or_else(|| config().shell_allow_inline_scripts)
}

/// `shell_allow_writes` for `ctx_shell`.
#[must_use]
pub fn allow_writes() -> bool {
    env_flag("LEAN_CTX_SHELL_ALLOW_WRITES").unwrap_or_else(|| config().shell_allow_writes)
}

/// `shell_approval` for the gate.
#[must_use]
pub fn approval() -> ShellApproval {
    match source() {
        Source::Ambient => ShellApproval::effective(&config()),
        Source::Pinned(_) => config().shell_approval,
    }
}

/// `shell_hook_mode` for the hooks (empty when unset).
#[must_use]
pub fn shell_hook_mode() -> String {
    env("LEAN_CTX_SHELL_HOOK_MODE")
        .filter(|v| !v.trim().is_empty())
        .or_else(|| config().shell_hook_mode.clone())
        .unwrap_or_default()
}

/// `LEAN_CTX_ALLOWLIST_WARN_ONLY`: the test/debug opt-out of the CLI gate.
#[must_use]
pub fn cli_warn_only_requested() -> bool {
    env("LEAN_CTX_ALLOWLIST_WARN_ONLY").is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_user_home_ignores_home_env() {
        let _lock = crate::core::data_dir::test_env_lock();
        let before = real_user_home();
        let _home = crate::test_env::VarGuard::set("HOME", "/tmp/forged-home");
        assert_eq!(real_user_home(), before);
        assert_ne!(
            real_user_home().as_deref(),
            Some(Path::new("/tmp/forged-home"))
        );
    }

    #[test]
    fn pinned_source_ignores_env_and_relocated_config() {
        let _lock = crate::core::data_dir::test_env_lock();
        let real = tempfile::tempdir().unwrap();
        let forged = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(forged.path().join(".config/lean-ctx")).unwrap();
        std::fs::write(
            forged.path().join(".config/lean-ctx/config.toml"),
            "shell_security = \"off\"\nshell_allow_inline_scripts = true\n",
        )
        .unwrap();
        let _pin = pin_for_test(real.path());
        let _a = crate::test_env::VarGuard::set("LEAN_CTX_SHELL_SECURITY", "off");
        let _b = crate::test_env::VarGuard::set("LEAN_CTX_SHELL_ALLOW_INLINE_SCRIPTS", "1");
        let _c = crate::test_env::VarGuard::set("HOME", forged.path());
        let _d = crate::test_env::VarGuard::set("LEAN_CTX_CONFIG_DIR", forged.path());
        assert_eq!(env("LEAN_CTX_SHELL_SECURITY"), None);
        assert!(!allow_inline_scripts());
        assert_eq!(config().shell_security, None);
        assert_eq!(
            config_path(),
            Some(real.path().join(".config/lean-ctx/config.toml"))
        );
    }

    /// `passwd` homes are Unix paths; on Windows `/home/jdoe` is not absolute
    /// (and the parser only runs on Linux).
    #[cfg(unix)]
    #[test]
    fn gh2236_passwd_line_yields_the_home_of_this_uid_only() {
        let line = b"jdoe:*:123456:100:J Doe:/home/jdoe:/bin/bash\n";
        assert_eq!(
            parse_passwd_home(line, 123_456),
            Some(PathBuf::from("/home/jdoe"))
        );
        // A line for another uid, a relative home, a short line, a
        // non-numeric uid and empty output are all refused.
        assert_eq!(parse_passwd_home(line, 1000), None);
        assert_eq!(parse_passwd_home(b"jdoe:*:7:7::home/jdoe:/bin/sh", 7), None);
        assert_eq!(parse_passwd_home(b"jdoe:*:7:7::", 7), None);
        assert_eq!(
            parse_passwd_home(b"jdoe:*:x7:7::/home/jdoe:/bin/sh", 7),
            None
        );
        assert_eq!(parse_passwd_home(b"", 7), None);
        // Only the first line counts.
        assert_eq!(
            parse_passwd_home(b"a:*:8:8::/home/a:/bin/sh\nb:*:7:7::/home/b:/bin/sh", 7),
            None
        );
    }

    /// The real `getent` path (fixed program, owner check, spawn, wait)
    /// agrees with the account database wherever both answer.
    #[cfg(target_os = "linux")]
    #[test]
    fn gh2236_getent_agrees_with_the_account_database() {
        // SAFETY: geteuid only reads this process identity.
        let uid = unsafe { libc::geteuid() };
        if let (Some(getent), Some(passwd)) = (getent_home(uid), passwd_home(uid)) {
            assert_eq!(getent, passwd);
        }
    }

    #[test]
    fn gh2236_unresolvable_pin_reads_defaults_and_names_no_config() {
        let _lock = crate::core::data_dir::test_env_lock();
        let _pin = pin_unresolvable_for_test();
        assert!(pinned());
        assert_eq!(config_path(), None);
        assert_eq!(settings_home(), None);
        assert_eq!(config().shell_security, None);
        let shown = config_path_display();
        assert!(shown.contains("no account-database entry"), "{shown}");
        assert!(!shown.contains("~/.lean-ctx"), "{shown}");
    }

    #[test]
    fn pinned_source_reads_the_real_home_config() {
        let _lock = crate::core::data_dir::test_env_lock();
        let real = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(real.path().join(".config/lean-ctx")).unwrap();
        std::fs::write(
            real.path().join(".config/lean-ctx/config.toml"),
            "shell_security = \"warn\"\n",
        )
        .unwrap();
        let _pin = pin_for_test(real.path());
        assert_eq!(config().shell_security.as_deref(), Some("warn"));
    }
}
