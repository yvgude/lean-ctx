use std::path::{Path, PathBuf};

/// Error returned when a write/edit tool targets a path that resolved *only*
/// via a read-only root. Centralized so the message is identical everywhere it
/// is enforced (dispatch pre-resolution + in-handler write resolvers) and so
/// tests can assert against a single source of truth.
pub const READ_ONLY_ROOT_WRITE_ERR: &str =
    "path is under a read-only root; reads are allowed, writes are not";

const IDE_CONFIG_DIRS: &[&str] = &[
    ".lean-ctx",
    ".cursor",
    ".claude",
    ".codex",
    ".codeium",
    ".gemini",
    ".qwen",
    ".trae",
    ".kiro",
    ".verdent",
    ".pi",
    ".amp",
    ".aider",
    ".continue",
    ".codebuddy",
];

/// Expands `~`, `$VAR` and `${VAR}` in a config-supplied path entry.
///
/// `allow_paths` / `extra_roots` come from `config.toml`, where no shell ever
/// runs — users writing `"$HOME/code"` or `"~/code"` got a literal,
/// never-matching prefix and concluded the whole option was broken (GH #392).
/// Unset variables are left verbatim (and warned about) so the entry fails
/// loudly in `lean-ctx doctor` instead of silently matching something else.
pub fn expand_user_path(raw: &str) -> PathBuf {
    let mut s = raw.to_string();

    if (s == "~" || s.starts_with("~/"))
        && let Some(home) = dirs::home_dir()
    {
        s = format!("{}{}", home.to_string_lossy(), &s[1..]);
    }

    while let Some(start) = s.find('$') {
        let rest = &s[start + 1..];
        let (name, token_len) = if let Some(stripped) = rest.strip_prefix('{') {
            match stripped.find('}') {
                Some(end) => (stripped[..end].to_string(), end + 3),
                None => break,
            }
        } else {
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(rest.len());
            (rest[..end].to_string(), end + 1)
        };
        if name.is_empty() {
            break;
        }
        if let Ok(val) = std::env::var(&name) {
            s.replace_range(start..start + token_len, &val);
        } else {
            tracing::warn!(
                "allow_paths/extra_roots entry '{raw}' references unset variable ${name} — entry will never match"
            );
            break;
        }
    }

    PathBuf::from(s)
}

pub fn allow_paths_from_env_and_config() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let cfg = crate::core::config::Config::load();

    // The allow-list defines the jail boundary, so it must be canonicalized the
    // same (security, symlink-resolving) way as the candidate it is compared
    // against — otherwise a guarded (lexical) root vs a resolved candidate would
    // break `is_under_prefix`. These entries are data_dir / IDE-config dirs /
    // user-configured paths, virtually never under ~/Documents.
    if let Ok(data_dir) = crate::core::data_dir::lean_ctx_data_dir() {
        out.push(canonicalize_secure(&data_dir));
    }

    if let Some(home) = dirs::home_dir() {
        let ide_dirs_allowed = cfg.allow_ide_config_dirs
            || std::env::var("LEAN_CTX_ALLOW_IDE_DIRS").is_ok_and(|v| v == "1");
        out.extend(home_allow_dirs(&home, ide_dirs_allowed));
    }

    for p in &cfg.allow_paths {
        out.push(canonicalize_secure(&expand_user_path(p)));
    }
    for p in &cfg.extra_roots {
        out.push(canonicalize_secure(&expand_user_path(p)));
    }

    // Env entries are expanded too: MCP host configs pass env blocks verbatim
    // (no shell), so "$HOME/code" arrives literally there as well.
    let v = std::env::var("LCTX_ALLOW_PATH")
        .or_else(|_| std::env::var("LEAN_CTX_ALLOW_PATH"))
        .unwrap_or_default();
    if !v.trim().is_empty() {
        for p in std::env::split_paths(&v) {
            out.push(canonicalize_secure(&expand_user_path(&p.to_string_lossy())));
        }
    }

    let extra = std::env::var("LEAN_CTX_EXTRA_ROOTS").unwrap_or_default();
    if !extra.trim().is_empty() {
        for p in std::env::split_paths(&extra) {
            out.push(canonicalize_secure(&expand_user_path(&p.to_string_lossy())));
        }
    }

    out
}

/// Read-only extra roots, sourced exactly like the read-write allow-list but
/// kept in a separate tier: a candidate under one of these is *readable* yet
/// must be *refused to write/edit tools*. Sourced from config `read_only_roots`
/// and the additive `LEAN_CTX_READ_ONLY_ROOTS` env var (mirrors `extra_roots`).
///
/// Canonicalized the same (security, symlink-resolving) way as the candidate so
/// `is_under_prefix` compares like with like — identical to the read-write tier.
pub fn read_only_roots_from_env_and_config() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let cfg = crate::core::config::Config::load();

    for p in &cfg.read_only_roots {
        out.push(canonicalize_secure(&expand_user_path(p)));
    }

    let env = std::env::var("LEAN_CTX_READ_ONLY_ROOTS").unwrap_or_default();
    if !env.trim().is_empty() {
        for p in std::env::split_paths(&env) {
            out.push(canonicalize_secure(&expand_user_path(&p.to_string_lossy())));
        }
    }

    out
}

/// Home-level allow-dirs for the jail. `~/.lean-ctx` (own state) is always
/// allowed; the *other* IDE config dirs (~/.cursor, ~/.claude, …) expose
/// foreign projects' sessions, MCP configs and credentials to any agent, so
/// they are opt-in only (config `allow_ide_config_dirs = true` or
/// `LEAN_CTX_ALLOW_IDE_DIRS=1`).
fn home_allow_dirs(home: &Path, ide_dirs_allowed: bool) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for dir in IDE_CONFIG_DIRS {
        if *dir != ".lean-ctx" && !ide_dirs_allowed {
            continue;
        }
        let p = home.join(dir);
        if p.exists() {
            out.push(canonicalize_secure(&p));
        }
    }
    out
}

fn is_under_prefix(path: &Path, prefix: &Path) -> bool {
    path.starts_with(prefix)
}

/// Heuristic canonicalize — honours the #356 TCC guard. Used by the
/// jail-disabled bypass and by external callers (session/startup/server roots)
/// that must not pop a privacy prompt on their own initiative.
pub fn canonicalize_or_self(path: &Path) -> PathBuf {
    super::pathutil::safe_canonicalize_bounded(path, 2000)
}

/// SECURITY canonicalize for the jail boundary itself (roots + candidate +
/// escape re-check). Deliberately bypasses the #356 TCC guard: the jail must
/// keep resolving symlinks to detect escapes, and it only ever runs on a path
/// the client explicitly asked to access, where a one-time prompt is legitimate.
fn canonicalize_secure(path: &Path) -> PathBuf {
    super::pathutil::canonicalize_secure_bounded(path, 2000)
}

fn canonicalize_existing_ancestor(path: &Path) -> Option<(PathBuf, Vec<std::ffi::OsString>)> {
    let mut cur = path.to_path_buf();
    let mut remainder: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if cur.exists() {
            return Some((canonicalize_secure(&cur), remainder));
        }
        let name = cur.file_name()?.to_os_string();
        remainder.push(name);
        if !cur.pop() {
            return None;
        }
    }
}

pub fn jail_path(candidate: &Path, jail_root: &Path) -> Result<PathBuf, String> {
    jail_path_with_roots(candidate, jail_root, &[])
}

/// Outcome of a jailed path resolution that also tracks the read-only tier.
///
/// `read_only` is true when the candidate was permitted *only* because it lives
/// under a [`read_only_roots_from_env_and_config`] root (or a session-supplied
/// `read_only_roots` entry) — i.e. it is NOT under the jail root, the
/// read-write allow-list, or `extra_roots`. Read tools may use `path`; write/
/// edit tools MUST refuse it. A path reachable via a read-write root is never
/// flagged (read-write wins), so the flag is strictly the "writes forbidden" bit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JailedPath {
    pub path: PathBuf,
    pub read_only: bool,
}

/// Like [`jail_path`], but also accepts paths under any of `extra_roots`.
///
/// `extra_roots` are session-scoped trusted roots (MCP `roots/list` and config
/// `extra_roots`, surfaced via `session.extra_roots`) — e.g. sibling git
/// worktrees the agent legitimately spans. They widen the allow-list for *this
/// call only*, so an explicit `path` under a worktree resolves instead of
/// failing with "path escapes project root", without loosening the global jail
/// (#403). `path_jail = false` still bypasses entirely and an empty slice is
/// byte-for-byte identical to the old single-root behaviour.
pub fn jail_path_with_roots(
    candidate: &Path,
    jail_root: &Path,
    extra_roots: &[String],
) -> Result<PathBuf, String> {
    jail_path_with_roots_ro(candidate, jail_root, extra_roots, &[]).map(|j| j.path)
}

/// Like [`jail_path_with_roots`], but also accepts paths under any
/// `read_only_roots` (session-supplied) plus the config/env read-only tier, and
/// reports — via [`JailedPath::read_only`] — whether the candidate was permitted
/// *only* because it lives under a read-only root.
///
/// Read tools call this and use the path regardless of the flag; write/edit
/// tools call this and MUST refuse when `read_only` is true. A read-only root is
/// therefore readable but never writable. Every other PathJail invariant
/// (canonicalize-secure, symlink/TOCTOU re-validation, post-canonicalize
/// recheck, null-byte rejection, `path_jail=false`/`no-jail` bypass) is
/// preserved unchanged; an empty `read_only_roots` plus an empty config/env
/// read-only tier makes this byte-for-byte identical to
/// [`jail_path_with_roots`].
pub fn jail_path_with_roots_ro(
    candidate: &Path,
    jail_root: &Path,
    extra_roots: &[String],
    read_only_roots: &[String],
) -> Result<JailedPath, String> {
    if candidate.to_string_lossy().as_bytes().contains(&0) {
        return Err("path contains null byte".to_string());
    }

    #[cfg(feature = "no-jail")]
    {
        let _ = (jail_root, extra_roots, read_only_roots);
        return Ok(JailedPath {
            path: canonicalize_or_self(candidate),
            read_only: false,
        });
    }

    #[allow(unreachable_code)]
    {
        let cfg = crate::core::config::Config::load();
        if cfg.path_jail == Some(false) {
            return Ok(JailedPath {
                path: canonicalize_or_self(candidate),
                read_only: false,
            });
        }

        let root = canonicalize_secure(jail_root);

        // Resolve relative candidates against the (absolute) jail root — never the process
        // CWD. The daemon's CWD is not the project, so CWD-relative resolution made
        // graph-relative paths (e.g. auto-preload candidates like `rust/src/core/foo.rs`)
        // spuriously fail with "no existing ancestor". Absolute candidates are unchanged.
        let resolved: PathBuf;
        let candidate: &Path = if candidate.is_absolute() {
            candidate
        } else {
            resolved = root.join(candidate);
            resolved.as_path()
        };

        // Read-write allow-list: jail root ∪ config/env allow-list ∪ session
        // `extra_roots`. A candidate under any of these is fully read-write.
        let mut allow = allow_paths_from_env_and_config();
        // Session-scoped roots widen the allow-list for this call only.
        allow.extend(
            extra_roots
                .iter()
                .filter(|r| !r.is_empty())
                .map(|r| canonicalize_secure(Path::new(r))),
        );

        // Read-only tier (config/env + session): permits reads, forbids writes.
        // Kept separate from `allow` so we can tag the resolution accordingly.
        let mut ro_allow = read_only_roots_from_env_and_config();
        ro_allow.extend(
            read_only_roots
                .iter()
                .filter(|r| !r.is_empty())
                .map(|r| canonicalize_secure(Path::new(r))),
        );

        let (base, remainder) = canonicalize_existing_ancestor(candidate).ok_or_else(|| {
            format!(
                "path does not exist and has no existing ancestor: {}",
                candidate.display()
            )
        })?;

        let under_rw =
            is_under_prefix(&base, &root) || allow.iter().any(|p| is_under_prefix(&base, p));
        #[cfg(windows)]
        let under_rw = under_rw || is_under_prefix_windows(&base, &root);

        let under_ro = ro_allow.iter().any(|p| is_under_prefix(&base, p));

        if !under_rw && !under_ro {
            let base_msg = format!(
                "path escapes project root: {} (root: {})",
                candidate.display(),
                root.display(),
            );
            let hint = if crate::core::protocol::meta_visible() {
                format!(
                    ". Hint: set LEAN_CTX_ALLOW_PATH={} or add it to allow_paths in ~/.lean-ctx/config.toml",
                    candidate.parent().unwrap_or(candidate).display()
                )
            } else {
                String::new()
            };
            return Err(format!("{base_msg}{hint}"));
        }

        #[cfg(windows)]
        reject_symlink_on_windows(candidate)?;

        let mut out = base;
        for part in remainder.iter().rev() {
            out.push(part);
        }

        // Re-validate after reconstruction: if the final path exists, canonicalize
        // and re-check to close TOCTOU window (symlink created between check and use).
        // The recheck spans BOTH tiers so a symlink escaping into a read-only root
        // is still validated; the read-only flag is recomputed from the canonical
        // target so a symlink from a read-write area into a read-only root (or vice
        // versa) is classified by where it actually lands.
        let mut read_only = under_ro && !under_rw;
        if out.exists() {
            let final_canon = canonicalize_secure(&out);
            let final_rw = is_under_prefix(&final_canon, &root)
                || allow.iter().any(|p| is_under_prefix(&final_canon, p));
            #[cfg(windows)]
            let final_rw = final_rw || is_under_prefix_windows(&final_canon, &root);
            let final_ro = ro_allow.iter().any(|p| is_under_prefix(&final_canon, p));
            if !final_rw && !final_ro {
                return Err(format!(
                    "post-canonicalize jail escape detected: {} resolves to {}",
                    candidate.display(),
                    final_canon.display()
                ));
            }
            read_only = final_ro && !final_rw;
        }

        Ok(JailedPath {
            path: out,
            read_only,
        })
    }
}

#[cfg(windows)]
fn is_under_prefix_windows(path: &Path, prefix: &Path) -> bool {
    let path_str = normalize_windows_path(&path.to_string_lossy());
    let prefix_str = normalize_windows_path(&prefix.to_string_lossy());
    path_str.starts_with(&prefix_str)
}

#[cfg(windows)]
fn normalize_windows_path(s: &str) -> String {
    let stripped = super::pathutil::strip_verbatim_str(s).unwrap_or_else(|| s.to_string());
    stripped.to_lowercase().replace('/', "\\")
}

#[cfg(windows)]
fn reject_symlink_on_windows(path: &Path) -> Result<(), String> {
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        // Junctions and other reparse points redirect like symlinks but are
        // invisible to `is_symlink()` — reject them too (GL#442).
        if super::pathutil::is_symlink_or_reparse(&meta) {
            return Err(format!(
                "symlink not allowed in jailed path: {}",
                path.display()
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(feature = "no-jail"))]
    #[test]
    fn rejects_path_outside_root() {
        // Hermetic config (empty data dir => jail on) so a parallel test that
        // flips `path_jail` cannot leak into this enforcement check. Also hold the
        // allow-path env lock: a parallel test setting `LEAN_CTX_ALLOW_PATH` (e.g.
        // "/") would otherwise turn this escape into an accepted path.
        let _iso = crate::core::data_dir::isolated_data_dir();
        let _alp = ALLOW_PATH_ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        let other = tmp.path().join("other");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(root.join("a.txt"), "ok").unwrap();
        std::fs::write(other.join("b.txt"), "no").unwrap();

        let ok = jail_path(&root.join("a.txt"), &root);
        assert!(ok.is_ok());

        let bad = jail_path(&other.join("b.txt"), &root);
        assert!(bad.is_err());
    }

    /// #406 regression: a long-lived process (the MCP server) must honor
    /// `path_jail = false` written to config after startup. The config cache is
    /// now keyed on content, so even an edit that preserves the file mtime takes
    /// effect — a path outside the jail root is accepted once the flag flips.
    /// (With the former mtime-only cache the stale `None` kept the jail on.)
    #[cfg(not(feature = "no-jail"))]
    #[test]
    fn honors_path_jail_false_after_mtime_preserving_edit() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let cfg_path = crate::core::config::Config::path().unwrap();
        if let Some(parent) = cfg_path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let secret = outside.join("secret.txt");
        std::fs::write(&secret, "x").unwrap();

        // Warm the config cache with the jail on (no path_jail key).
        std::fs::write(&cfg_path, "# jail on\n").unwrap();
        let mtime0 = std::fs::metadata(&cfg_path).unwrap().modified().unwrap();
        assert_eq!(crate::core::config::Config::load().path_jail, None);

        // Flip path_jail=false but restore the original mtime, so any mtime-only
        // cache would keep serving the stale jail-on value.
        std::fs::write(&cfg_path, "path_jail = false\n").unwrap();
        filetime::set_file_mtime(&cfg_path, filetime::FileTime::from_system_time(mtime0)).unwrap();

        assert!(
            jail_path(&secret, &root).is_ok(),
            "path_jail=false must take effect without a fresh process (#406)"
        );
    }

    #[test]
    fn allows_nonexistent_child_under_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.txt"), "ok").unwrap();

        let p = root.join("new").join("file.txt");
        let ok = jail_path(&p, &root).unwrap();
        assert!(ok.to_string_lossy().contains("file.txt"));
    }

    #[cfg(not(feature = "no-jail"))]
    #[test]
    fn relative_candidate_resolves_against_root_not_cwd() {
        // Regression: in the daemon (CWD != project) a relative graph path like
        // `sub/file.rs` must resolve under the jail root, not the process CWD.
        let _iso = crate::core::data_dir::isolated_data_dir();
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub").join("file.rs"), "ok").unwrap();

        let jailed = jail_path(Path::new("sub/file.rs"), &root)
            .expect("relative candidate should resolve under the jail root");
        assert!(jailed.ends_with("sub/file.rs"));
        assert!(
            is_under_prefix(&canonicalize_or_self(&jailed), &canonicalize_or_self(&root)),
            "resolved path must live under the jail root: {jailed:?}"
        );
    }

    #[test]
    fn ide_config_dirs_list_is_not_empty() {
        assert!(IDE_CONFIG_DIRS.len() >= 10);
        assert!(IDE_CONFIG_DIRS.contains(&".codex"));
        assert!(IDE_CONFIG_DIRS.contains(&".cursor"));
        assert!(IDE_CONFIG_DIRS.contains(&".claude"));
        assert!(IDE_CONFIG_DIRS.contains(&".gemini"));
    }

    // P0-10 (#422): home-level IDE config dirs are opt-in; only ~/.lean-ctx
    // is allowed unconditionally.
    #[test]
    fn ide_config_dirs_are_excluded_by_default() {
        let home = tempfile::tempdir().unwrap();
        for d in [".lean-ctx", ".cursor", ".claude", ".codex"] {
            std::fs::create_dir_all(home.path().join(d)).unwrap();
        }

        let denied = home_allow_dirs(home.path(), false);
        assert_eq!(
            denied.len(),
            1,
            "only ~/.lean-ctx may be allowed: {denied:?}"
        );
        assert!(denied[0].ends_with(".lean-ctx"));

        let allowed = home_allow_dirs(home.path(), true);
        assert_eq!(allowed.len(), 4, "opt-in must allow all existing IDE dirs");
    }

    #[test]
    fn canonicalize_or_self_strips_verbatim() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("project");
        std::fs::create_dir_all(&dir).unwrap();

        let result = canonicalize_or_self(&dir);
        let s = result.to_string_lossy();
        assert!(
            !s.starts_with(r"\\?\"),
            "canonicalize_or_self should strip verbatim prefix, got: {s}"
        );
    }

    #[test]
    fn jail_path_accepts_same_dir_different_format() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("file.rs"), "ok").unwrap();

        let result = jail_path(&root.join("file.rs"), &root);
        assert!(result.is_ok(), "same dir should be accepted: {result:?}");
    }

    #[cfg(not(feature = "no-jail"))]
    #[test]
    fn error_message_contains_escape_info() {
        // Hold the allow-path env lock: a parallel test setting
        // `LEAN_CTX_ALLOW_PATH="/"` would otherwise make this escape resolve to Ok.
        let _iso = crate::core::data_dir::isolated_data_dir();
        let _alp = ALLOW_PATH_ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        let other = tmp.path().join("other");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("b.txt"), "no").unwrap();

        let err = jail_path(&other.join("b.txt"), &root).unwrap_err();
        assert!(
            err.contains("path escapes project root"),
            "error should mention escape: {err}"
        );
    }

    // GH #392: config entries like "$HOME/code" or "~/code" were taken
    // literally and never matched.
    #[test]
    fn expand_user_path_expands_tilde_and_vars() {
        let home = dirs::home_dir().expect("home dir");
        let home_s = home.to_string_lossy().to_string();

        assert_eq!(expand_user_path("~"), home);
        assert_eq!(expand_user_path("~/code"), home.join("code"));
        assert_eq!(expand_user_path("$HOME/code"), home.join("code"));
        assert_eq!(expand_user_path("${HOME}/code"), home.join("code"));
        // Multiple variables in one entry.
        crate::test_env::set_var("LEAN_CTX_TEST_SUB", "sub");
        assert_eq!(
            expand_user_path("$HOME/$LEAN_CTX_TEST_SUB/x"),
            PathBuf::from(format!("{home_s}/sub/x"))
        );
        crate::test_env::remove_var("LEAN_CTX_TEST_SUB");
        // Absolute paths pass through untouched.
        assert_eq!(expand_user_path("/etc"), PathBuf::from("/etc"));
    }

    #[test]
    fn expand_user_path_leaves_unset_vars_verbatim() {
        crate::test_env::remove_var("LEAN_CTX_TEST_UNSET_VAR");
        let p = expand_user_path("$LEAN_CTX_TEST_UNSET_VAR/code");
        assert_eq!(p, PathBuf::from("$LEAN_CTX_TEST_UNSET_VAR/code"));
    }

    /// Serializes tests that mutate `LEAN_CTX_ALLOW_PATH` — cargo runs tests in
    /// parallel threads and `set_var`/`remove_var` are process-global.
    static ALLOW_PATH_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    // GH #392: `allow_paths = ["/"]` (via the same env-var channel) must grant
    // access to any absolute path — "/" is a prefix of everything.
    #[cfg(unix)]
    #[test]
    fn allow_path_root_slash_permits_everything() {
        let _guard = ALLOW_PATH_ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        let other = tmp.path().join("other");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("b.txt"), "allowed").unwrap();

        crate::test_env::set_var("LEAN_CTX_ALLOW_PATH", "/");
        let result = jail_path(&other.join("b.txt"), &root);
        crate::test_env::remove_var("LEAN_CTX_ALLOW_PATH");

        assert!(result.is_ok(), "allow path '/' must permit all: {result:?}");
    }

    #[test]
    fn allow_path_env_permits_outside_root() {
        let _guard = ALLOW_PATH_ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        let other = tmp.path().join("other");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("b.txt"), "allowed").unwrap();

        let canon = canonicalize_or_self(&other);
        crate::test_env::set_var("LEAN_CTX_ALLOW_PATH", canon.to_string_lossy().as_ref());
        let result = jail_path(&other.join("b.txt"), &root);
        crate::test_env::remove_var("LEAN_CTX_ALLOW_PATH");

        assert!(
            result.is_ok(),
            "LEAN_CTX_ALLOW_PATH should permit access: {result:?}"
        );
    }

    #[cfg(all(unix, not(feature = "no-jail")))]
    #[test]
    fn rejects_symlink_escape_on_unix() {
        use std::os::unix::fs::symlink;

        // Hold the allow-path env lock: a parallel test setting
        // `LEAN_CTX_ALLOW_PATH="/"` would otherwise let the symlink escape resolve.
        let _iso = crate::core::data_dir::isolated_data_dir();
        let _alp = ALLOW_PATH_ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        let other = tmp.path().join("other");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("secret.txt"), "no").unwrap();

        let link = root.join("link.txt");
        symlink(other.join("secret.txt"), &link).unwrap();

        let bad = jail_path(&link, &root);
        assert!(bad.is_err(), "symlink escape must be rejected: {bad:?}");
    }

    #[test]
    fn rejects_null_byte_in_path() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        std::fs::create_dir_all(&root).unwrap();

        let bad_path = PathBuf::from("file\0.txt");
        let result = jail_path(&bad_path, &root);
        assert!(result.is_err(), "null byte in path must be rejected");
        assert!(
            result.unwrap_err().contains("null byte"),
            "error must mention null byte"
        );
    }

    /// #403 Bug 1: an explicit path under a session-scoped `extra_root` (e.g. a
    /// sibling git worktree from MCP `roots/list`) must resolve, while the same
    /// path is rejected without it — and a path under *no* root is rejected even
    /// when extra roots are present. Holds both env locks so neither a parallel
    /// `path_jail` flip nor a `LEAN_CTX_ALLOW_PATH` mutation can leak in.
    #[cfg(not(feature = "no-jail"))]
    #[test]
    fn extra_roots_permit_paths_outside_jail() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let _alp = ALLOW_PATH_ENV_LOCK.lock().unwrap();

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        let worktree = tmp.path().join("worktree");
        let elsewhere = tmp.path().join("elsewhere");
        for d in [&root, &worktree, &elsewhere] {
            std::fs::create_dir_all(d).unwrap();
        }
        let in_worktree = worktree.join("a.txt");
        std::fs::write(&in_worktree, "x").unwrap();
        let outside = elsewhere.join("b.txt");
        std::fs::write(&outside, "y").unwrap();

        // Parity: with no extra roots, the worktree path escapes the jail.
        assert!(jail_path(&in_worktree, &root).is_err());
        assert!(jail_path_with_roots(&in_worktree, &root, &[]).is_err());

        // The session-scoped extra root permits it — via the slice alone, with
        // nothing in env/config.
        let extra = vec![worktree.to_string_lossy().to_string()];
        assert!(
            jail_path_with_roots(&in_worktree, &root, &extra).is_ok(),
            "path under a session extra_root must resolve (#403)"
        );

        // A path under neither the jail nor any extra root is still rejected.
        assert!(
            jail_path_with_roots(&outside, &root, &extra).is_err(),
            "paths outside ALL roots must still be rejected"
        );

        // Empty entries are ignored (no accidental allow-all).
        assert!(jail_path_with_roots(&outside, &root, &[String::new()]).is_err());
    }

    /// read_only_roots (config/env tier): a path under a read-only root is
    /// READABLE and TAGGED `read_only`; a normal `extra_root` stays read-write
    /// (`read_only=false`); a path outside ALL roots still escapes. Holds the
    /// read-only env lock so a parallel test mutating `LEAN_CTX_READ_ONLY_ROOTS`
    /// cannot leak in, plus the allow-path lock and an isolated data dir.
    #[cfg(not(feature = "no-jail"))]
    #[test]
    fn read_only_root_is_readable_but_tagged_read_only() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let _alp = ALLOW_PATH_ENV_LOCK.lock().unwrap();
        let _rol = READ_ONLY_ROOTS_ENV_LOCK.lock().unwrap();
        crate::test_env::remove_var("LEAN_CTX_READ_ONLY_ROOTS");

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        let ro = tmp.path().join("sibling");
        let rw = tmp.path().join("worktree");
        let elsewhere = tmp.path().join("elsewhere");
        for d in [&root, &ro, &rw, &elsewhere] {
            std::fs::create_dir_all(d).unwrap();
        }
        let in_ro = ro.join("a.txt");
        std::fs::write(&in_ro, "x").unwrap();
        let in_rw = rw.join("b.txt");
        std::fs::write(&in_rw, "y").unwrap();
        let outside = elsewhere.join("c.txt");
        std::fs::write(&outside, "z").unwrap();

        let ro_roots = vec![ro.to_string_lossy().to_string()];
        let rw_roots = vec![rw.to_string_lossy().to_string()];

        // Read-only root: permitted, flagged read_only.
        let j = jail_path_with_roots_ro(&in_ro, &root, &[], &ro_roots)
            .expect("path under read_only root must resolve");
        assert!(j.read_only, "path under a read_only root must be flagged");
        assert!(j.path.ends_with("a.txt"));

        // Read-write extra_root: permitted, NOT flagged (writes allowed).
        let j2 = jail_path_with_roots_ro(&in_rw, &root, &rw_roots, &[])
            .expect("path under extra_root must resolve");
        assert!(!j2.read_only, "a read-write extra_root must not be flagged");

        // A read_only root must NOT grant write semantics via the bare RW helper's
        // contract: the flag is the only signal, and here it is set.
        // Path outside every root still escapes, even with a read_only root present.
        assert!(
            jail_path_with_roots_ro(&outside, &root, &[], &ro_roots).is_err(),
            "a path outside all roots must still escape"
        );

        // Empty read_only entries are ignored (no accidental allow-all).
        assert!(jail_path_with_roots_ro(&outside, &root, &[], &[String::new()]).is_err());
    }

    /// A symlink inside the jail root that escapes into a read-only root is still
    /// caught by the post-canonicalize recheck — and, because it lands in the
    /// read-only root, is flagged read_only (so writes through it are refused too).
    #[cfg(all(unix, not(feature = "no-jail")))]
    #[test]
    fn symlink_into_read_only_root_is_revalidated_and_flagged() {
        use std::os::unix::fs::symlink;
        let _iso = crate::core::data_dir::isolated_data_dir();
        let _alp = ALLOW_PATH_ENV_LOCK.lock().unwrap();
        let _rol = READ_ONLY_ROOTS_ENV_LOCK.lock().unwrap();
        crate::test_env::remove_var("LEAN_CTX_READ_ONLY_ROOTS");

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        let ro = tmp.path().join("sibling");
        let outside = tmp.path().join("outside");
        for d in [&root, &ro, &outside] {
            std::fs::create_dir_all(d).unwrap();
        }
        let target = ro.join("secret.txt");
        std::fs::write(&target, "ro").unwrap();
        let escaped = outside.join("secret.txt");
        std::fs::write(&escaped, "no").unwrap();

        let ro_roots = vec![ro.to_string_lossy().to_string()];

        // A symlink in the jail root pointing INTO the read-only root: the post-
        // canonicalize recheck spans the read-only tier, so it resolves — and the
        // flag reflects where it actually lands (read-only).
        let link_ro = root.join("link_ro.txt");
        symlink(&target, &link_ro).unwrap();
        let j = jail_path_with_roots_ro(&link_ro, &root, &[], &ro_roots)
            .expect("symlink into a read_only root resolves (read allowed)");
        assert!(
            j.read_only,
            "a symlink resolving into a read_only root must be flagged read_only"
        );

        // A symlink escaping to a path under NEITHER tier is still rejected.
        let link_bad = root.join("link_bad.txt");
        symlink(&escaped, &link_bad).unwrap();
        assert!(
            jail_path_with_roots_ro(&link_bad, &root, &[], &ro_roots).is_err(),
            "a symlink escaping all roots must still be rejected"
        );
    }

    /// `LEAN_CTX_READ_ONLY_ROOTS` is parsed (path-list separator) and feeds the
    /// read-only tier: a path under an env-listed root resolves and is flagged.
    #[cfg(not(feature = "no-jail"))]
    #[test]
    fn read_only_roots_env_var_is_parsed() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let _alp = ALLOW_PATH_ENV_LOCK.lock().unwrap();
        let _rol = READ_ONLY_ROOTS_ENV_LOCK.lock().unwrap();

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        let ro = tmp.path().join("sibling");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&ro).unwrap();
        let in_ro = ro.join("a.txt");
        std::fs::write(&in_ro, "x").unwrap();

        // Canonicalize: the env tier is compared against the canonical candidate.
        let ro_canon = canonicalize_or_self(&ro);
        crate::test_env::set_var(
            "LEAN_CTX_READ_ONLY_ROOTS",
            ro_canon.to_string_lossy().as_ref(),
        );
        let parsed = read_only_roots_from_env_and_config();
        let j = jail_path_with_roots_ro(&in_ro, &root, &[], &[]);
        crate::test_env::remove_var("LEAN_CTX_READ_ONLY_ROOTS");

        assert!(
            parsed.iter().any(|p| in_ro.starts_with(p)),
            "env read_only root must be parsed into the tier: {parsed:?}"
        );
        let j = j.expect("env read_only root must permit the read");
        assert!(
            j.read_only,
            "env-sourced read_only root must flag read_only"
        );
    }

    /// Serializes tests mutating `LEAN_CTX_READ_ONLY_ROOTS` (process-global env).
    /// Gated like its only users so `--features no-jail` (which filters those
    /// tests out) does not see an unused static.
    #[cfg(not(feature = "no-jail"))]
    static READ_ONLY_ROOTS_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
}
