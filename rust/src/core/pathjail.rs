use std::path::{Path, PathBuf};

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

    // Read-only roots are *readable* (the whole point is read access to sibling
    // repos); writes into them are denied separately by `enforce_writable`
    // (#475). Add them to the read allow-list so reads resolve, exactly like
    // `extra_roots`, without granting write access.
    out.extend(canonicalized_roots(
        &cfg.read_only_roots,
        "LEAN_CTX_READ_ONLY_ROOTS",
    ));

    out
}

/// Canonicalize a set of config-supplied root entries plus an env override
/// (path-list separated), expanding `~`/`$VAR` first. Shared by the read
/// allow-list and the read-only-roots collector so both tiers parse roots
/// identically.
fn canonicalized_roots(config_entries: &[String], env_var: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for p in config_entries {
        out.push(canonicalize_secure(&expand_user_path(p)));
    }
    let v = std::env::var(env_var).unwrap_or_default();
    if !v.trim().is_empty() {
        for p in std::env::split_paths(&v) {
            out.push(canonicalize_secure(&expand_user_path(&p.to_string_lossy())));
        }
    }
    out
}

/// A read-only root is a sibling subtree the agent may **read** but never
/// **write** — e.g. a reference repo mounted next to the project. Empty by
/// default, so [`is_read_only_path`]/[`enforce_writable`] are zero-cost no-ops
/// for everyone who hasn't opted in (#475).
pub fn read_only_roots_from_env_and_config() -> Vec<PathBuf> {
    let cfg = crate::core::config::Config::load();
    canonicalized_roots(&cfg.read_only_roots, "LEAN_CTX_READ_ONLY_ROOTS")
}

/// A single active relaxation of the path jail. Each one widens or disables what
/// tools can reach beyond the project root, so it is surfaced loudly (GH security
/// audit, finding 3): the MCP/HTTP server inherits its process env from the
/// IDE/launchd, so a globally-set `LEAN_CTX_ALLOW_PATH` / `LEAN_CTX_EXTRA_ROOTS`
/// / `LEAN_CTX_ALLOW_IDE_DIRS` (or `path_jail = false`) silently loosens the
/// boundary with no in-band signal otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JailRelaxation {
    /// The knob that activated it (env var name, config key, or build feature).
    pub source: &'static str,
    /// Human-readable effect of the relaxation.
    pub detail: &'static str,
}

fn env_is_set(var: &str) -> bool {
    std::env::var(var).is_ok_and(|v| !v.trim().is_empty())
}

/// Collect every currently-active path-jail relaxation. An empty result means
/// the jail is fully in force. This is the single source of truth shared by the
/// startup warning ([`warn_if_relaxed`]) and `lean-ctx doctor`.
#[must_use]
pub fn active_relaxations() -> Vec<JailRelaxation> {
    let mut out = Vec::new();

    if cfg!(feature = "no-jail") {
        out.push(JailRelaxation {
            source: "no-jail (build feature)",
            detail: "path jail compiled out — every tool path is allowed",
        });
    }

    if crate::core::config::Config::load().path_jail == Some(false) {
        out.push(JailRelaxation {
            source: "path_jail = false (config.toml)",
            detail: "path jail disabled — every tool path is allowed",
        });
    }

    if env_is_set("LEAN_CTX_ALLOW_PATH") || env_is_set("LCTX_ALLOW_PATH") {
        out.push(JailRelaxation {
            source: "LEAN_CTX_ALLOW_PATH",
            detail: "widens the read/write allow-list beyond the project root",
        });
    }

    if env_is_set("LEAN_CTX_EXTRA_ROOTS") {
        out.push(JailRelaxation {
            source: "LEAN_CTX_EXTRA_ROOTS",
            detail: "adds extra accessible roots beyond the project root",
        });
    }

    let ide_env = std::env::var("LEAN_CTX_ALLOW_IDE_DIRS").is_ok_and(|v| v == "1");
    if ide_env || crate::core::config::Config::load().allow_ide_config_dirs {
        out.push(JailRelaxation {
            source: if ide_env {
                "LEAN_CTX_ALLOW_IDE_DIRS=1"
            } else {
                "allow_ide_config_dirs = true (config.toml)"
            },
            detail: "exposes ~/.cursor, ~/.claude, … (other agents' sessions/credentials) to tools",
        });
    }

    out
}

/// Emit a loud `tracing::warn!` for every active path-jail relaxation. Called
/// once at MCP/HTTP server startup so a trusted-but-loosening env/config leaves
/// an in-band audit signal instead of silently defeating the jail (finding 3).
pub fn warn_if_relaxed() {
    for relaxation in active_relaxations() {
        tracing::warn!(
            "[SECURITY] path jail relaxed via {}: {} — intended for trusted local use only",
            relaxation.source,
            relaxation.detail
        );
    }
}

/// True when `candidate` resolves to a location inside a configured read-only
/// root. The candidate's nearest existing ancestor is canonicalized (so a
/// not-yet-existing file inherits the read-only status of the directory it
/// would be created in — closing the "create a new file in a read-only repo"
/// hole) and matched against the (symlink-resolved) read-only roots.
///
/// A `false` return is only authoritative when the roots list is empty or the
/// path provably sits outside every root; an unresolvable candidate (no
/// existing ancestor) is treated as *not* read-only here and is rejected later
/// by the ordinary write/jail error, never silently written.
pub fn is_read_only_path(candidate: &Path) -> bool {
    let roots = read_only_roots_from_env_and_config();
    if roots.is_empty() {
        return false;
    }

    // Compare the canonicalized nearest-existing-ancestor (resolves symlinks so
    // a symlink *into* a read-only root can't launder a write past the prefix
    // check), reconstructing the full path for the comparison.
    let base = match canonicalize_existing_ancestor(candidate) {
        Some((base, remainder)) => {
            let mut p = base;
            for part in remainder.iter().rev() {
                p.push(part);
            }
            p
        }
        None => canonicalize_or_self(candidate),
    };

    roots.iter().any(|r| is_under_prefix(&base, r))
}

/// Default-deny write guard for the read-only tier (#475): returns an error if
/// `candidate` is inside a configured read-only root, `Ok(())` otherwise.
///
/// This is the single read-only-aware choke point. Every filesystem write that
/// can target a caller-supplied path routes through it (the atomic writers in
/// `ctx_edit`/`edit_apply`, the handoff/session export bundle writers, the
/// in-place memory-compaction writer, and the refactor IDE pre-write gate), so
/// a "read-only" root cannot be written through any tool. Reads are unaffected.
pub fn enforce_writable(candidate: &Path) -> Result<(), String> {
    if is_read_only_path(candidate) {
        return Err(format!(
            "path is inside a read-only root — writes are denied (read_only_roots): {}",
            candidate.display()
        ));
    }
    Ok(())
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
    if candidate.to_string_lossy().as_bytes().contains(&0) {
        return Err("path contains null byte".to_string());
    }

    #[cfg(feature = "no-jail")]
    {
        let _ = (jail_root, extra_roots);
        return Ok(canonicalize_or_self(candidate));
    }

    #[allow(unreachable_code)]
    {
        let cfg = crate::core::config::Config::load();
        if cfg.path_jail == Some(false) {
            return Ok(canonicalize_or_self(candidate));
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

        let mut allow = allow_paths_from_env_and_config();
        // Session-scoped roots widen the allow-list for this call only.
        allow.extend(
            extra_roots
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

        let allowed =
            is_under_prefix(&base, &root) || allow.iter().any(|p| is_under_prefix(&base, p));

        #[cfg(windows)]
        let allowed = allowed || is_under_prefix_windows(&base, &root);

        if !allowed {
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
        if out.exists() {
            let final_canon = canonicalize_secure(&out);
            let final_ok = is_under_prefix(&final_canon, &root)
                || allow.iter().any(|p| is_under_prefix(&final_canon, p));
            #[cfg(windows)]
            let final_ok = final_ok || is_under_prefix_windows(&final_canon, &root);
            if !final_ok {
                return Err(format!(
                    "post-canonicalize jail escape detected: {} resolves to {}",
                    candidate.display(),
                    final_canon.display()
                ));
            }
        }

        Ok(out)
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

    /// #475: a configured read-only root is readable but never writable. Reads
    /// resolve (the root joins the allow-list like an extra_root), while the
    /// single write choke point `enforce_writable` default-denies every write
    /// inside it — including a not-yet-existing file, which inherits the
    /// directory's read-only status. `isolated_data_dir` holds `test_env_lock`,
    /// serialising the `LEAN_CTX_READ_ONLY_ROOTS` mutation against other tests.
    #[cfg(not(feature = "no-jail"))]
    #[test]
    fn read_only_roots_deny_writes_but_allow_reads() {
        let _iso = crate::core::data_dir::isolated_data_dir();

        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("project");
        let refrepo = tmp.path().join("refrepo");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(refrepo.join("sub")).unwrap();
        std::fs::write(refrepo.join("lib.rs"), "pub fn x() {}\n").unwrap();

        // Canonicalize the configured root the same (symlink-resolving) way the
        // guard does, so macOS /var → /private/var can't defeat the prefix match.
        let ro_canon = canonicalize_secure(&refrepo);
        crate::test_env::set_var(
            "LEAN_CTX_READ_ONLY_ROOTS",
            ro_canon.to_string_lossy().as_ref(),
        );

        let existing = refrepo.join("lib.rs");
        let new_file = refrepo.join("sub").join("new.rs");
        let proj_file = project.join("main.rs");

        // Capture every decision while the env is live (it is cleared below).
        let read_existing = jail_path(&existing, &project);
        let deny_existing = enforce_writable(&existing);
        let deny_new = enforce_writable(&new_file);
        let allow_project = enforce_writable(&proj_file);
        let ro_existing = is_read_only_path(&existing);
        let ro_project = is_read_only_path(&proj_file);

        crate::test_env::remove_var("LEAN_CTX_READ_ONLY_ROOTS");

        assert!(
            deny_existing.is_err(),
            "write to an existing file in a read-only root must be denied"
        );
        assert!(
            deny_new.is_err(),
            "creating a new file in a read-only root must be denied"
        );
        assert!(
            allow_project.is_ok(),
            "writes into the project root must stay allowed: {allow_project:?}"
        );
        assert!(
            read_existing.is_ok(),
            "reads inside a read-only root must resolve (read allow-list): {read_existing:?}"
        );
        assert!(ro_existing, "the file is inside the read-only root");
        assert!(!ro_project, "the project file is not read-only");
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

    // Finding 3 (GH security audit): env-channel jail relaxations must be
    // detectable so startup + doctor can surface them loudly.
    #[test]
    fn active_relaxations_detects_allow_path_env() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let _alp = ALLOW_PATH_ENV_LOCK.lock().unwrap();
        crate::test_env::remove_var("LEAN_CTX_EXTRA_ROOTS");
        crate::test_env::remove_var("LEAN_CTX_ALLOW_IDE_DIRS");
        crate::test_env::set_var("LEAN_CTX_ALLOW_PATH", "/tmp");

        let relaxed = active_relaxations();

        crate::test_env::remove_var("LEAN_CTX_ALLOW_PATH");

        assert!(
            relaxed.iter().any(|r| r.source == "LEAN_CTX_ALLOW_PATH"),
            "LEAN_CTX_ALLOW_PATH must be reported as a jail relaxation: {relaxed:?}"
        );
    }

    #[cfg(not(feature = "no-jail"))]
    #[test]
    fn active_relaxations_empty_when_jail_intact() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let _alp = ALLOW_PATH_ENV_LOCK.lock().unwrap();
        for var in [
            "LEAN_CTX_ALLOW_PATH",
            "LCTX_ALLOW_PATH",
            "LEAN_CTX_EXTRA_ROOTS",
            "LEAN_CTX_ALLOW_IDE_DIRS",
        ] {
            crate::test_env::remove_var(var);
        }

        assert!(
            active_relaxations().is_empty(),
            "an intact jail (clean config, no relaxation env) must report no relaxations: {:?}",
            active_relaxations()
        );
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
}
