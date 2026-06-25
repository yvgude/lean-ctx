use std::path::{Path, PathBuf};

/// Canonicalize a path and strip the Windows verbatim/extended-length prefix (`\\?\`)
/// that `std::fs::canonicalize` adds on Windows. This prefix breaks many tools and
/// string-based path comparisons.
///
/// On non-Windows platforms this is equivalent to `std::fs::canonicalize`.
pub fn safe_canonicalize(path: &Path) -> std::io::Result<PathBuf> {
    // TCC choke-point (#356): a launchd-standalone process (daemon/proxy/auto-
    // updater, ppid 1) must never realpath a path under ~/Documents, ~/Desktop
    // or ~/Downloads — the `stat` trips the macOS privacy prompt in lean-ctx's
    // own name, and every release re-invalidates the grant (new cdhash), so it
    // re-prompts forever. Heuristic call sites (project-root detection, session
    // matching, path normalization, scan-root checks) all funnel through here,
    // so guarding the sink protects them centrally instead of one opt-in check
    // per call site. Return the path unchanged (lexical) rather than touching
    // the filesystem. Security boundaries (PathJail) deliberately bypass this
    // guard via `canonicalize_secure` — they only resolve paths the client
    // explicitly asked to access, where a prompt is legitimate, and must keep
    // resolving symlinks to detect jail escapes.
    if !may_probe_path(path) {
        return Ok(path.to_path_buf());
    }
    canonicalize_raw(path)
}

/// Raw realpath + Windows-verbatim strip, with **no** TCC guard. Internal sink
/// shared by [`safe_canonicalize`] (which gates it behind `may_probe_path`) and
/// [`canonicalize_secure`] (which never gates it).
fn canonicalize_raw(path: &Path) -> std::io::Result<PathBuf> {
    let canon = std::fs::canonicalize(path)?;
    Ok(strip_verbatim(canon))
}

/// Like `safe_canonicalize` but returns the original path on failure.
#[must_use]
pub fn safe_canonicalize_or_self(path: &Path) -> PathBuf {
    safe_canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// SECURITY canonicalize: always resolves symlinks, even under ~/Documents in a
/// launchd-standalone process. `PathJail` relies on this to detect symlink jail
/// escapes (#356 must never weaken the security boundary). A standalone process
/// only reaches here for a path the client *explicitly* asked to access, where a
/// one-time TCC prompt is legitimate — unlike the self-initiated heuristic
/// probes that [`safe_canonicalize`] suppresses.
pub fn canonicalize_secure(path: &Path) -> std::io::Result<PathBuf> {
    canonicalize_raw(path)
}

/// Like `canonicalize_secure` but returns the original path on failure.
#[must_use]
pub fn canonicalize_secure_or_self(path: &Path) -> PathBuf {
    canonicalize_secure(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Canonicalize with a timeout guard. Protects against hangs on WSL2 `DrvFS`,
/// Windows reparse points, NFS, FUSE, sshfs, and other slow filesystems.
/// Falls back to the original path if canonicalize doesn't complete within the timeout.
/// Self-healing: after a timeout, subsequent calls to slow mounts skip the thread entirely.
///
/// Heuristic variant — honours the #356 TCC guard (see [`safe_canonicalize`]).
pub fn safe_canonicalize_bounded(path: &Path, timeout_ms: u64) -> PathBuf {
    canonicalize_bounded_with(path, timeout_ms, safe_canonicalize_or_self)
}

/// SECURITY variant of [`safe_canonicalize_bounded`] — bypasses the #356 TCC
/// guard so `PathJail` keeps resolving symlinks to detect jail escapes. See
/// [`canonicalize_secure`] for why a prompt here (explicit request) is legitimate.
pub fn canonicalize_secure_bounded(path: &Path, timeout_ms: u64) -> PathBuf {
    canonicalize_bounded_with(path, timeout_ms, canonicalize_secure_or_self)
}

/// Shared timeout machinery for the bounded canonicalizers. `resolve` selects
/// the guarded (`safe_canonicalize_or_self`) or security (`canonicalize_secure_or_self`)
/// sink so both variants get identical slow-mount/self-healing behaviour.
fn canonicalize_bounded_with(
    path: &Path,
    timeout_ms: u64,
    resolve: fn(&Path) -> PathBuf,
) -> PathBuf {
    use super::io_health;

    let path_str = path.to_string_lossy();
    if io_health::is_slow_mount(&path_str) && io_health::recent_freeze_count() > 0 {
        return resolve(path);
    }

    let effective_timeout =
        io_health::adaptive_timeout(std::time::Duration::from_millis(timeout_ms));

    let path_owned = path.to_path_buf();
    let (tx, rx) = std::sync::mpsc::channel();
    let _ = std::thread::Builder::new()
        .name("canonicalize-bounded".into())
        .spawn(move || {
            let _ = tx.send(resolve(&path_owned));
        });
    if let Ok(canonical) = rx.recv_timeout(effective_timeout) {
        canonical
    } else {
        io_health::record_freeze();
        tracing::warn!(
            "[SECURITY] canonicalize timed out ({}ms) for {}; PathJail checks on \
             uncanonicalized paths may be less reliable",
            effective_timeout.as_millis(),
            path.display()
        );
        path.to_path_buf()
    }
}

/// Remove the `\\?\` / `//?/` verbatim prefix from a `PathBuf`.
/// Handles both regular verbatim (`\\?\C:\...`) and UNC verbatim (`\\?\UNC\...`).
#[must_use]
pub fn strip_verbatim(path: PathBuf) -> PathBuf {
    let s = path.to_string_lossy();
    if let Some(stripped) = strip_verbatim_str(&s) {
        PathBuf::from(stripped)
    } else {
        path
    }
}

/// Remove the `\\?\` / `//?/` verbatim prefix from a path string.
/// Returns `Some(cleaned)` if a prefix was found, `None` otherwise.
pub fn strip_verbatim_str(path: &str) -> Option<String> {
    let normalized = path.replace('\\', "/");

    if let Some(rest) = normalized.strip_prefix("//?/UNC/") {
        Some(format!("//{rest}"))
    } else {
        normalized
            .strip_prefix("//?/")
            .map(std::string::ToString::to_string)
    }
}

/// MSYS2/Git Bash drive mapping: `/c/Users/...` -> `C:/Users/...`.
///
/// Returns `None` when the path does not carry a single-letter drive prefix.
/// Callers must apply this **only on Windows hosts**: clients running under
/// MSYS2/Git Bash hand POSIX-style drive paths to a native Windows lean-ctx.
/// On Linux/macOS `/c/...` is a literal directory and must pass through
/// untouched (GH #397 — the unconditional rewrite broke every `ctx_*` tool
/// for Linux projects rooted under `/c/...` and similar paths).
fn translate_msys_drive_prefix(p: &str) -> Option<String> {
    if p.len() >= 3
        && p.starts_with('/')
        && p.as_bytes()[1].is_ascii_alphabetic()
        && p.as_bytes()[2] == b'/'
    {
        let drive = p.as_bytes()[1].to_ascii_uppercase() as char;
        Some(format!("{drive}:{}", &p[2..]))
    } else {
        None
    }
}

/// Lexical (string-only) part of [`normalize_tool_path`]: MSYS2 drive prefix
/// (Windows hosts only), separators, double slashes, trailing slash. Performs
/// **no** filesystem access, so it is safe on persisted paths in
/// TCC-standalone processes (launchd daemon, #356) and as a dedupe key where
/// symlink resolution is not worth a `realpath` per entry.
#[must_use]
pub fn normalize_tool_path_lexical(path: &str) -> String {
    let mut p = match strip_verbatim_str(path) {
        Some(stripped) => stripped,
        None => path.to_string(),
    };

    if cfg!(windows)
        && let Some(translated) = translate_msys_drive_prefix(&p)
    {
        p = translated;
    }

    p = p.replace('\\', "/");

    // Collapse double slashes (preserve UNC paths starting with //)
    while p.contains("//") && !p.starts_with("//") {
        p = p.replace("//", "/");
    }

    // Remove trailing slash (unless root like "/" or "C:/")
    if p.len() > 1 && p.ends_with('/') && !p.ends_with(":/") {
        p.pop();
    }

    p
}

/// Normalize paths from any client format to a consistent OS-native form.
/// Handles MSYS2/Git Bash drive prefixes on Windows hosts
/// (`/c/Users/...` -> `C:/Users/...`), mixed separators, double slashes, and
/// trailing slashes. Uses forward slashes for consistency. On non-Windows
/// hosts `/c/...` is a literal directory and passes through unchanged (#397).
#[must_use]
pub fn normalize_tool_path(path: &str) -> String {
    let mut p = normalize_tool_path_lexical(path);

    // Resolve symlinks for absolute paths to ensure cache key consistency.
    // Skip relative paths (preserve "." / "../" as-is), root-only paths (/ or C:/),
    // slow mounts (WSL DrvFS /mnt/) where canonicalize can hang, and paths a
    // TCC-standalone process must not stat (launchd daemon + ~/Documents, #356).
    // Uses safe_canonicalize to strip Windows \\?\ prefix.
    let is_absolute = p.starts_with('/') || (p.len() >= 3 && p.as_bytes()[1] == b':');
    let is_root_only = p == "/" || (p.len() <= 3 && p.ends_with('/') && is_absolute);
    if is_absolute
        && !is_root_only
        && !crate::core::io_health::is_slow_mount(&p)
        && may_probe_path(Path::new(&*p))
        && let Ok(canonical) = safe_canonicalize(Path::new(&*p))
    {
        let canonical_str = canonical.to_string_lossy().replace('\\', "/");
        if !canonical_str.is_empty() {
            p = canonical_str;
        }
    }

    p
}

/// Returns `true` if the directory is too broad to be a valid project root.
/// Rejects home directory, filesystem root, `.` (bare CWD), and agent sandbox
/// directories (`.claude`, `.codex`). Used to prevent writing project-scoped
/// data (overlays, policies) into the global `~/.lean-ctx/` data directory.
#[must_use]
pub fn is_broad_or_unsafe_root(dir: &Path) -> bool {
    if let Some(home) = dirs::home_dir()
        && dir == home
    {
        return true;
    }
    let s = dir.to_string_lossy();
    if s == "/" || s == "\\" || s == "." {
        return true;
    }
    s.ends_with("/.claude")
        || s.ends_with("/.codex")
        || s.ends_with("/.codebuddy")
        || s.contains("/.claude/")
        || s.contains("/.codex/")
        || s.contains("/.codebuddy/")
}

/// Well-known project markers used to identify project roots.
pub const PROJECT_MARKERS: &[&str] = &[
    ".git",
    "Cargo.toml",
    "package.json",
    "go.mod",
    "pyproject.toml",
    "setup.py",
    "pom.xml",
    "build.gradle",
    "Makefile",
    "project.godot",
    ".lean-ctx.toml",
    ".planning",
];

/// Returns `true` if `dir` contains at least one known project marker.
///
/// TCC guard (#356): a launchd-owned process (daemon/proxy/auto-updater) must
/// not stat marker files under `~/Documents` & co. — the probe itself pops the
/// macOS privacy prompt. For those processes this conservatively reports
/// "no marker" without touching the filesystem.
#[must_use]
pub fn has_project_marker(dir: &Path) -> bool {
    if !may_probe_path(dir) {
        return false;
    }
    PROJECT_MARKERS.iter().any(|m| dir.join(m).exists())
}

/// Returns `true` if the (lstat) metadata describes a symlink — or, on
/// Windows, *any* reparse point (junctions, mount points, app-exec links).
///
/// Security boundaries must use this instead of `FileType::is_symlink`:
/// Rust's `is_symlink()` reports `false` for NTFS junctions, which redirect
/// exactly like directory symlinks and would otherwise bypass jail/TOCTOU
/// checks on Windows (GL#442).
#[must_use]
pub fn is_symlink_or_reparse(meta: &std::fs::Metadata) -> bool {
    if meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        return meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0;
    }
    #[cfg(not(windows))]
    false
}

/// Returns `true` if `dir` is the home directory or one of the macOS "magic"
/// home subdirectories (`Documents`, `Desktop`, `Downloads`).
///
/// macOS guards these with TCC: the first time a process *enumerates or stats
/// inside* one, the OS pops a privacy prompt ("lean-ctx would like to access
/// files in your Documents folder", #356). They are also never valid project
/// roots or multi-repo workspace parents, so scan heuristics should treat them
/// as off-limits *without* calling `read_dir` (which is what trips the prompt).
#[must_use]
pub fn is_tcc_sensitive_home_dir(dir: &Path) -> bool {
    let Some(home) = dirs::home_dir() else {
        return false;
    };
    if dir == home {
        return true;
    }
    if dir.parent() != Some(home.as_path()) {
        return false;
    }
    matches!(
        dir.file_name().and_then(|n| n.to_str()),
        Some("Documents" | "Desktop" | "Downloads")
    )
}

/// Returns `true` if `path` lies inside (or is) one of the macOS TCC-protected
/// home folders (`~/Documents`, `~/Desktop`, `~/Downloads`). Pure string/path
/// comparison — performs **no** filesystem access itself.
///
/// Unlike [`is_tcc_sensitive_home_dir`] (which only matches the magic dirs
/// themselves), this also matches nested paths like `~/Documents/proj/src`,
/// because *any* `stat` below the magic dir trips the TCC prompt (#356).
#[must_use]
pub fn is_under_tcc_protected_dir(path: &Path) -> bool {
    if !cfg!(target_os = "macos") {
        return false;
    }
    let Some(home) = dirs::home_dir() else {
        return false;
    };
    ["Documents", "Desktop", "Downloads"]
        .iter()
        .any(|magic| path.starts_with(home.join(magic)))
}

/// Returns `true` when this process is its own TCC identity on macOS — i.e.
/// it was started (or re-parented) by `launchd` rather than by a
/// TCC-granted host like a terminal or an editor.
///
/// Context (#356): TCC permissions attach to the *responsible process*. The
/// lean-ctx daemon/proxy `LaunchAgents` and the scheduled auto-updater run
/// directly under `launchd` (ppid 1), so any `stat`/`read_dir` they perform
/// under `~/Documents` pops the privacy prompt **in lean-ctx's own name** —
/// and because every release replaces the ad-hoc-signed binary (new cdhash),
/// a previously granted permission is invalidated on each update, re-prompting
/// forever. Such processes must never probe TCC-protected paths on their own
/// initiative. Child processes of a terminal or editor (MCP server, CLI)
/// inherit their host's TCC grant and keep full functionality.
#[must_use]
pub fn process_is_tcc_standalone() -> bool {
    #[cfg(target_os = "macos")]
    {
        // Deliberately uncached: getppid is a cheap syscall, the env override
        // must stay testable within one process, and a daemonizing fork could
        // change the answer after startup.
        if let Ok(v) = std::env::var("LEAN_CTX_TCC_STANDALONE") {
            match v.trim() {
                "1" | "true" => return true,
                "0" | "false" => return false,
                _ => {}
            }
        }
        // A process carrying the deny-~/Documents seatbelt sentinel is, by
        // construction, a launchd-standalone descendant: the sentinel is set
        // only by the LaunchAgent plist env and the self re-exec, and child
        // processes inherit it. This catches a daemon the long-lived standalone
        // proxy spawned via `start_daemon` (ppid = proxy, not 1), whose code-side
        // path guards would otherwise stay off because `getppid()` is no longer
        // 1. (#356)
        if std::env::var_os(crate::core::tcc_guard_sandbox::SEATBELT_SENTINEL).is_some() {
            return true;
        }
        // SAFETY: `getppid` takes no arguments and cannot fail.
        (unsafe { libc::getppid() }) == 1
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// Returns `true` when this process may `stat`/`read_dir`/`canonicalize`
/// `path` without risking a macOS TCC privacy prompt in lean-ctx's name.
///
/// Heuristic call sites (project-marker probes, session/root matching) must
/// consult this before touching paths from persisted state; security
/// boundaries (`PathJail`) are exempt — they only ever canonicalize paths the
/// client explicitly asked to access, in which case a prompt is legitimate.
#[must_use]
pub fn may_probe_path(path: &Path) -> bool {
    !(process_is_tcc_standalone() && is_under_tcc_protected_dir(path))
}

/// Returns `true` if `dir` is a multi-repo workspace parent — i.e. it has at
/// least 2 immediate child directories that each contain a project marker.
pub fn has_multi_repo_children(dir: &Path) -> bool {
    // Never enumerate the home dir or macOS TCC-protected dirs: read_dir there
    // pops a macOS privacy prompt (#356) and they are never workspace parents.
    // `is_tcc_sensitive_home_dir` only matches the magic dirs themselves;
    // `!may_probe_path` additionally refuses *nested* paths like
    // `~/Documents/proj` when this process is launchd-standalone.
    if is_tcc_sensitive_home_dir(dir) || !may_probe_path(dir) {
        return false;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    let count = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|ft| ft.is_dir()))
        .filter(|e| has_project_marker(&e.path()))
        .take(2)
        .count();
    count >= 2
}

/// Returns `true` if `project_root` collides with the lean-ctx data directory.
/// This prevents project-scoped files (overlays.json, policies.json) from being
/// written into `~/.lean-ctx/` or `~/.config/lean-ctx/`.
#[must_use]
pub fn is_data_dir_collision(project_root: &Path) -> bool {
    if is_broad_or_unsafe_root(project_root) {
        return true;
    }
    if let Ok(data_dir) = crate::core::data_dir::lean_ctx_data_dir() {
        let project_lean_ctx = project_root.join(".lean-ctx");
        if project_lean_ctx == data_dir || data_dir.starts_with(&project_lean_ctx) {
            return true;
        }
    }
    false
}

/// Returns the project-scoped `.lean-ctx/` directory if the project root is safe.
/// Returns `Err` if the project root collides with the global data directory.
pub fn safe_project_data_dir(project_root: &Path) -> Result<PathBuf, String> {
    if is_data_dir_collision(project_root) {
        return Err(format!(
            "project root {} collides with global data directory; \
             skipping project-scoped write",
            project_root.display()
        ));
    }
    Ok(project_root.join(".lean-ctx"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_regular_verbatim() {
        let p = PathBuf::from(r"\\?\C:\Users\dev\project");
        let result = strip_verbatim(p);
        assert_eq!(result, PathBuf::from("C:/Users/dev/project"));
    }

    #[test]
    fn tcc_sensitive_home_dir_matches_home_and_magic_dirs() {
        let Some(home) = dirs::home_dir() else {
            return;
        };
        // Home itself and the macOS magic dirs are off-limits (#356).
        assert!(is_tcc_sensitive_home_dir(&home));
        assert!(is_tcc_sensitive_home_dir(&home.join("Documents")));
        assert!(is_tcc_sensitive_home_dir(&home.join("Desktop")));
        assert!(is_tcc_sensitive_home_dir(&home.join("Downloads")));
    }

    #[test]
    fn tcc_sensitive_home_dir_allows_real_projects() {
        let Some(home) = dirs::home_dir() else {
            return;
        };
        // A real project (even nested under Documents) and non-magic home children
        // are scannable — only the bare magic dirs / home are blocked.
        assert!(!is_tcc_sensitive_home_dir(
            &home.join("Documents").join("my-project")
        ));
        assert!(!is_tcc_sensitive_home_dir(&home.join("code")));
        assert!(!is_tcc_sensitive_home_dir(&home.join("Projects")));
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn under_tcc_protected_dir_matches_nested_paths() {
        let Some(home) = dirs::home_dir() else {
            return;
        };
        // The magic dirs themselves and anything nested below them (#356).
        assert!(is_under_tcc_protected_dir(&home.join("Documents")));
        assert!(is_under_tcc_protected_dir(
            &home.join("Documents/deep/nested/project")
        ));
        assert!(is_under_tcc_protected_dir(&home.join("Desktop/scratch")));
        assert!(is_under_tcc_protected_dir(&home.join("Downloads/x.zip")));
        // Home itself, siblings, and non-home paths are fine.
        assert!(!is_under_tcc_protected_dir(&home));
        assert!(!is_under_tcc_protected_dir(&home.join("code/project")));
        assert!(!is_under_tcc_protected_dir(Path::new("/tmp/Documents")));
    }

    #[test]
    #[cfg(target_os = "macos")]
    #[serial_test::serial]
    fn tcc_standalone_blocks_probes_under_protected_dirs() {
        let Some(home) = dirs::home_dir() else {
            return;
        };
        let doc_proj = home.join("Documents/some-project");

        crate::test_env::set_var("LEAN_CTX_TCC_STANDALONE", "1");
        assert!(process_is_tcc_standalone());
        assert!(!may_probe_path(&doc_proj));
        // Non-protected paths stay probeable even for standalone processes.
        assert!(may_probe_path(Path::new("/tmp/some-project")));
        // has_project_marker must refuse without touching the filesystem.
        assert!(!has_project_marker(&doc_proj));

        crate::test_env::set_var("LEAN_CTX_TCC_STANDALONE", "0");
        assert!(!process_is_tcc_standalone());
        assert!(may_probe_path(&doc_proj));
        crate::test_env::remove_var("LEAN_CTX_TCC_STANDALONE");
    }

    #[test]
    #[cfg(target_os = "macos")]
    #[serial_test::serial]
    fn tcc_standalone_detected_via_seatbelt_sentinel() {
        let Some(home) = dirs::home_dir() else {
            return;
        };
        let doc_proj = home.join("Documents/some-project");

        // No explicit override: a process carrying the deny-~/Documents seatbelt
        // sentinel (inherited from its sandboxed launchd parent) counts as
        // standalone even when ppid != 1, so its heuristic probes stay
        // suppressed — this is the proxy→daemon chain the ppid check missed. (#356)
        crate::test_env::remove_var("LEAN_CTX_TCC_STANDALONE");
        crate::test_env::set_var(crate::core::tcc_guard_sandbox::SEATBELT_SENTINEL, "1");
        assert!(process_is_tcc_standalone());
        assert!(!may_probe_path(&doc_proj));
        crate::test_env::remove_var(crate::core::tcc_guard_sandbox::SEATBELT_SENTINEL);

        // With neither override nor sentinel a normal test process (ppid != 1)
        // is not standalone, so the sentinel is what flipped the result above.
        assert!(!process_is_tcc_standalone());
    }

    #[test]
    #[cfg(target_os = "macos")]
    #[serial_test::serial]
    fn tcc_standalone_skips_canonicalize_under_protected_dirs() {
        let Some(home) = dirs::home_dir() else {
            return;
        };
        // A path that does NOT exist under ~/Documents. With the TCC choke-point
        // guard active, `safe_canonicalize` returns Ok(input) *without* calling
        // `std::fs::canonicalize` (which would Err on a missing path) — proving
        // the filesystem is never touched (#356). This is the structural fix:
        // every heuristic canonicalize funnels through here.
        let missing = home.join("Documents/lean-ctx-tcc-test-does-not-exist-xyzzy");

        crate::test_env::set_var("LEAN_CTX_TCC_STANDALONE", "1");
        let guarded = safe_canonicalize(&missing);
        assert!(
            guarded.is_ok(),
            "standalone safe_canonicalize must short-circuit (no stat) under ~/Documents"
        );
        assert_eq!(guarded.unwrap(), missing);
        assert_eq!(safe_canonicalize_or_self(&missing), missing);

        // Outside the protected dirs the guard never engages, even when standalone.
        let tmp_missing = Path::new("/tmp/lean-ctx-tcc-test-does-not-exist-xyzzy");
        assert!(safe_canonicalize(tmp_missing).is_err());

        // Without standalone the guard is inactive: a missing ~/Documents path
        // Errs from the real `std::fs::canonicalize` as before.
        crate::test_env::set_var("LEAN_CTX_TCC_STANDALONE", "0");
        assert!(safe_canonicalize(&missing).is_err());

        crate::test_env::remove_var("LEAN_CTX_TCC_STANDALONE");
    }

    #[test]
    #[cfg(target_os = "macos")]
    #[serial_test::serial]
    fn canonicalize_secure_bypasses_tcc_guard_for_pathjail() {
        // SECURITY counterpart to the test above (#356): PathJail must keep
        // resolving symlinks even when standalone under ~/Documents, so the jail
        // can detect escapes. `canonicalize_secure` therefore must NOT honour the
        // guard — it always touches the filesystem. We prove that by feeding a
        // missing ~/Documents path while standalone: the guarded path returns
        // Ok(lexical) (no stat), while the secure path Errs (it did stat).
        let Some(home) = dirs::home_dir() else {
            return;
        };
        let missing = home.join("Documents/lean-ctx-secure-canon-does-not-exist-xyzzy");

        crate::test_env::set_var("LEAN_CTX_TCC_STANDALONE", "1");
        // Guarded sink short-circuits (no fs access).
        assert_eq!(safe_canonicalize(&missing).unwrap(), missing);
        // Security sink ignores the guard and actually stats -> Err on a missing
        // path. If this ever returns Ok(lexical), the jail's symlink-escape
        // detection has silently regressed under ~/Documents.
        assert!(
            canonicalize_secure(&missing).is_err(),
            "canonicalize_secure must bypass the TCC guard and touch the filesystem"
        );
        assert_eq!(canonicalize_secure_or_self(&missing), missing);
        crate::test_env::remove_var("LEAN_CTX_TCC_STANDALONE");
    }

    #[test]
    fn strip_unc_verbatim() {
        let p = PathBuf::from(r"\\?\UNC\server\share\dir");
        let result = strip_verbatim(p);
        assert_eq!(result, PathBuf::from("//server/share/dir"));
    }

    #[test]
    fn no_prefix_unchanged() {
        let p = PathBuf::from("/home/user/project");
        let result = strip_verbatim(p.clone());
        assert_eq!(result, p);
    }

    #[test]
    fn windows_drive_unchanged() {
        let p = PathBuf::from("C:/Users/dev");
        let result = strip_verbatim(p.clone());
        assert_eq!(result, p);
    }

    #[test]
    fn strip_str_regular() {
        assert_eq!(
            strip_verbatim_str(r"\\?\E:\code\lean-ctx"),
            Some("E:/code/lean-ctx".to_string())
        );
    }

    #[test]
    fn strip_str_unc() {
        assert_eq!(
            strip_verbatim_str(r"\\?\UNC\myserver\data"),
            Some("//myserver/data".to_string())
        );
    }

    #[test]
    fn strip_str_forward_slash_variant() {
        assert_eq!(
            strip_verbatim_str("//?/C:/Users/dev"),
            Some("C:/Users/dev".to_string())
        );
    }

    #[test]
    fn strip_str_no_prefix() {
        assert_eq!(strip_verbatim_str("/home/user"), None);
    }

    #[test]
    fn safe_canonicalize_or_self_nonexistent() {
        let p = Path::new("/this/path/should/not/exist/xyzzy");
        let result = safe_canonicalize_or_self(p);
        assert_eq!(result, p.to_path_buf());
    }

    // The drive translation itself is platform-independent and testable
    // everywhere; only its *application* is gated on Windows hosts (#397).
    #[test]
    fn msys_drive_prefix_translation() {
        assert_eq!(
            translate_msys_drive_prefix("/c/Users/ABC").as_deref(),
            Some("C:/Users/ABC")
        );
        assert_eq!(
            translate_msys_drive_prefix("/D/Program Files").as_deref(),
            Some("D:/Program Files")
        );
        assert_eq!(translate_msys_drive_prefix("/usr/local/bin"), None);
        assert_eq!(translate_msys_drive_prefix("/c"), None);
        assert_eq!(translate_msys_drive_prefix("c/Users"), None);
    }

    #[cfg(windows)]
    #[test]
    fn normalize_msys_path_to_native() {
        assert_eq!(
            normalize_tool_path("/c/Users/ABC/AppData/lean-ctx"),
            "C:/Users/ABC/AppData/lean-ctx"
        );
        assert_eq!(
            normalize_tool_path("/D/Program Files/lean-ctx.exe"),
            "D:/Program Files/lean-ctx.exe"
        );
    }

    // GH #397: on Linux/macOS, /c/… is a literal directory — a Linux project
    // rooted there must not be rewritten to a Windows drive path.
    #[cfg(not(windows))]
    #[test]
    fn normalize_single_letter_unix_path_untouched() {
        assert_eq!(
            normalize_tool_path_lexical("/c/Users/me/proj"),
            "/c/Users/me/proj"
        );
        assert_eq!(
            normalize_tool_path_lexical("/x/projects/app/src"),
            "/x/projects/app/src"
        );
    }

    #[test]
    fn normalize_native_windows_path_unchanged() {
        assert_eq!(
            normalize_tool_path("C:/Users/ABC/lean-ctx.exe"),
            "C:/Users/ABC/lean-ctx.exe"
        );
    }

    #[test]
    fn normalize_backslash_windows_path() {
        assert_eq!(
            normalize_tool_path(r"C:\Users\ABC\lean-ctx.exe"),
            "C:/Users/ABC/lean-ctx.exe"
        );
    }

    #[test]
    fn normalize_unix_path_unchanged() {
        assert_eq!(
            normalize_tool_path("/usr/local/bin/lean-ctx"),
            "/usr/local/bin/lean-ctx"
        );
    }

    #[test]
    fn normalize_windows_path_with_spaces_and_backslashes() {
        // The exact "paths with spaces" scenario reported on Windows (#324):
        // backslashes are converted to forward slashes (so client render layers
        // never escape-mangle them) while spaces in directory names survive.
        assert_eq!(
            normalize_tool_path(r"C:\Users\My Name\My Project\src\main.rs"),
            "C:/Users/My Name/My Project/src/main.rs"
        );
        assert_eq!(
            normalize_tool_path(r"C:\Program Files\app\config.toml"),
            "C:/Program Files/app/config.toml"
        );
    }

    #[test]
    fn normalize_double_slashes() {
        assert_eq!(
            normalize_tool_path("C:/Users//ABC//lean-ctx"),
            "C:/Users/ABC/lean-ctx"
        );
    }

    #[test]
    fn normalize_trailing_slash_removed() {
        assert_eq!(normalize_tool_path("C:/Users/ABC/"), "C:/Users/ABC");
        assert_eq!(
            normalize_tool_path_lexical("/tmp/nonexistent-dir-xyzzy/"),
            "/tmp/nonexistent-dir-xyzzy"
        );
    }

    #[test]
    fn normalize_root_slash_preserved() {
        assert_eq!(normalize_tool_path("/"), "/");
    }

    #[test]
    fn normalize_drive_root_preserved() {
        assert_eq!(normalize_tool_path("C:/"), "C:/");
    }

    #[test]
    fn normalize_verbatim_with_msys() {
        assert_eq!(normalize_tool_path(r"\\?\C:\Users\dev"), "C:/Users/dev");
    }

    #[test]
    fn broad_root_rejects_home() {
        if let Some(home) = dirs::home_dir() {
            assert!(is_broad_or_unsafe_root(&home));
        }
    }

    #[test]
    fn broad_root_rejects_filesystem_root() {
        assert!(is_broad_or_unsafe_root(Path::new("/")));
    }

    #[test]
    fn broad_root_rejects_dot() {
        assert!(is_broad_or_unsafe_root(Path::new(".")));
    }

    #[test]
    fn broad_root_rejects_agent_dirs() {
        assert!(is_broad_or_unsafe_root(Path::new("/home/user/.claude")));
        assert!(is_broad_or_unsafe_root(Path::new("/home/user/.codex")));
    }

    #[test]
    fn broad_root_allows_project_subdir() {
        let tmp = tempfile::tempdir().unwrap();
        let subdir = tmp.path().join("my-project");
        std::fs::create_dir_all(&subdir).unwrap();
        assert!(!is_broad_or_unsafe_root(&subdir));
    }

    #[test]
    fn broad_root_allows_home_subdirs() {
        if let Some(home) = dirs::home_dir() {
            let subdir = home.join("projects").join("my-app");
            assert!(!is_broad_or_unsafe_root(&subdir));
        }
    }

    #[test]
    fn data_dir_collision_rejects_home() {
        if let Some(home) = dirs::home_dir() {
            assert!(is_data_dir_collision(&home));
        }
    }

    #[test]
    fn data_dir_collision_allows_normal_project() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("my-project");
        std::fs::create_dir_all(&project).unwrap();
        assert!(!is_data_dir_collision(&project));
    }

    #[test]
    fn has_project_marker_detects_git() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();
        assert!(!has_project_marker(&root));
        std::fs::create_dir(root.join(".git")).unwrap();
        assert!(has_project_marker(&root));
    }

    #[test]
    fn has_project_marker_detects_cargo_toml() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("rust-project");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("Cargo.toml"), "[package]").unwrap();
        assert!(has_project_marker(&root));
    }

    #[test]
    fn has_project_marker_detects_godot_project() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("godot-game");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("project.godot"), "config_version=5\n").unwrap();
        assert!(has_project_marker(&root));
    }

    #[test]
    fn multi_repo_children_needs_two() {
        let tmp = tempfile::tempdir().unwrap();
        let parent = tmp.path().join("code");
        std::fs::create_dir_all(&parent).unwrap();

        // 0 repos → false
        assert!(!has_multi_repo_children(&parent));

        // 1 repo → false
        let repo1 = parent.join("repo1");
        std::fs::create_dir_all(repo1.join(".git")).unwrap();
        assert!(!has_multi_repo_children(&parent));

        // 2 repos → true
        let repo2 = parent.join("repo2");
        std::fs::create_dir_all(repo2.join(".git")).unwrap();
        assert!(has_multi_repo_children(&parent));
    }

    #[test]
    fn multi_repo_children_ignores_files() {
        let tmp = tempfile::tempdir().unwrap();
        let parent = tmp.path().join("mixed");
        std::fs::create_dir_all(&parent).unwrap();

        // One repo dir + one plain file with .git name (not a dir)
        let repo1 = parent.join("repo1");
        std::fs::create_dir_all(repo1.join(".git")).unwrap();
        std::fs::write(parent.join("not-a-repo"), "file").unwrap();
        assert!(!has_multi_repo_children(&parent));

        // Add second actual repo
        let repo2 = parent.join("repo2");
        std::fs::create_dir_all(&repo2).unwrap();
        std::fs::write(repo2.join("package.json"), "{}").unwrap();
        assert!(has_multi_repo_children(&parent));
    }

    #[test]
    fn multi_repo_children_nonexistent_dir() {
        assert!(!has_multi_repo_children(Path::new("/nonexistent/path/xyz")));
    }

    #[test]
    fn regular_file_is_not_symlink_or_reparse() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("plain.txt");
        std::fs::write(&file, "x").unwrap();
        let meta = std::fs::symlink_metadata(&file).unwrap();
        assert!(!is_symlink_or_reparse(&meta));
    }

    #[cfg(unix)]
    #[test]
    fn unix_symlink_is_detected() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("target.txt");
        std::fs::write(&target, "x").unwrap();
        let link = tmp.path().join("link.txt");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let meta = std::fs::symlink_metadata(&link).unwrap();
        assert!(is_symlink_or_reparse(&meta));
    }

    /// Runs in the windows-latest CI lane (GL#442). Symlink creation needs
    /// either admin or Developer Mode — skip gracefully when unavailable.
    #[cfg(windows)]
    #[test]
    fn windows_symlink_is_detected() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("target.txt");
        std::fs::write(&target, "x").unwrap();
        let link = tmp.path().join("link.txt");
        if std::os::windows::fs::symlink_file(&target, &link).is_err() {
            eprintln!("skipping: symlink creation not permitted on this runner");
            return;
        }
        let meta = std::fs::symlink_metadata(&link).unwrap();
        assert!(is_symlink_or_reparse(&meta));
    }
}
