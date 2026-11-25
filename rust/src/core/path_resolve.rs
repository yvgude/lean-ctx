//! Shared path-resolution for tool handlers.
//!
//! Previously two near-identical `resolve_path_sync` implementations lived in
//! `tools/registered/mod.rs` (SessionState-based) and `server/tool_trait.rs`
//! (ToolContext-based), plus several copies of the project-marker test. This
//! module is the single source of truth: [`resolve_tool_path`] for jailed path
//! resolution and a re-export of [`has_project_marker`] for marker detection.

use std::path::{Path, PathBuf};

/// Single canonical project-marker test (`.git`, `Cargo.toml`, …).
///
/// Re-exported from [`crate::core::pathutil`] so callers that think in terms of
/// path resolution have a local, discoverable handle.
pub use crate::core::pathutil::has_project_marker;

/// Resolve a (possibly relative) tool path to a normalized, jail-checked,
/// secret-screened absolute path.
///
/// Resolution of the input:
/// 1. absolute path → used as-is
/// 2. relative path with a project root → `<project_root>/<path>`, whether or
///    not it exists (a write target), exactly as the tool schemas promise
/// 3. relative path without a project root → `<shell_cwd>/<path>` (the
///    directory the client started the session in), else `<jail_root>/<path>`
///
/// The base of a relative path never depends on earlier tool calls (#2262):
/// `ctx_shell` does not move `shell_cwd`, and with a project root it is not
/// consulted at all. A worktree or other checkout is addressed by absolute
/// path.
///
/// Relative inputs are NEVER resolved against the process CWD: the daemon's
/// CWD is not the project, so a CWD `exists()` probe made resolution
/// nondeterministic across MCP/daemon/CLI contexts (and could pick a
/// same-named file outside the project).
///
/// `jail_root` is `project_root`, else `shell_cwd`, else `"."`. The result is
/// confined to the jail root via [`crate::core::pathjail::jail_path`] and
/// screened by the secret-path I/O boundary.
///
/// Performs blocking filesystem `exists()` checks; callers on async runtimes
/// must wrap this in `tokio::task::block_in_place`.
pub fn resolve_tool_path(
    project_root: Option<&str>,
    shell_cwd: Option<&str>,
    raw: &str,
) -> Result<String, String> {
    resolve_tool_path_with_roots(project_root, shell_cwd, raw, &[])
}

/// Like [`resolve_tool_path`], but also permits paths under any of
/// `extra_roots` (session-scoped trusted roots from `session.extra_roots`).
///
/// An empty `extra_roots` is identical to [`resolve_tool_path`]; this is how
/// sync tool handlers honor MCP `roots/list` / config `extra_roots` for an
/// explicit path without widening the global jail (#403).
pub fn resolve_tool_path_with_roots(
    project_root: Option<&str>,
    shell_cwd: Option<&str>,
    raw: &str,
    extra_roots: &[String],
) -> Result<String, String> {
    resolve_tool_path_refusing(project_root, shell_cwd, raw, extra_roots)
        .map_err(|refusal| refusal.message)
}

/// A path that did not resolve: the message the caller returns, and the
/// boundary class when the I/O boundary refused it (`None` for a path that is
/// merely missing or malformed). The class is what the audit trail records.
#[derive(Debug)]
pub struct PathRefusal {
    pub message: String,
    pub class: Option<crate::core::security_events::RefusalClass>,
}

impl From<String> for PathRefusal {
    fn from(message: String) -> Self {
        Self {
            message,
            class: None,
        }
    }
}

impl PathRefusal {
    /// The refusal of the secret-path I/O boundary (enforce mode).
    pub fn secret_path(message: String) -> Self {
        Self {
            message,
            class: Some(crate::core::security_events::RefusalClass::SecretPath),
        }
    }

    /// The refusal of the path jail, classified by
    /// [`crate::core::security_events::RefusalClass::of_jail_error`].
    pub fn jail(error: &crate::core::error::PathJailError) -> Self {
        Self {
            message: error.to_string(),
            class: crate::core::security_events::RefusalClass::of_jail_error(error),
        }
    }
}

/// [`resolve_tool_path_with_roots`], saying which boundary refused the path.
pub fn resolve_tool_path_refusing(
    project_root: Option<&str>,
    shell_cwd: Option<&str>,
    raw: &str,
    extra_roots: &[String],
) -> Result<String, PathRefusal> {
    let normalized = crate::core::pathutil::normalize_tool_path(raw);
    if normalized.is_empty() || normalized == "." {
        return Ok(crate::core::workspace_authority::project_dot(
            &normalized,
            project_root,
        )?);
    }

    let p = Path::new(&normalized);
    let jail_root = project_root.or(shell_cwd).unwrap_or(".").to_string();

    let resolved: PathBuf = if p.is_absolute() {
        PathBuf::from(&normalized)
    } else if let Some(root) = project_root {
        Path::new(root).join(&normalized)
    } else if let Some(cwd) = shell_cwd {
        Path::new(cwd).join(&normalized)
    } else {
        Path::new(&jail_root).join(&normalized)
    };

    if project_root.is_none()
        && let Some(error) = unbound_relative_path(raw, &resolved, shell_cwd.unwrap_or(&jail_root))
    {
        return Err(error.into());
    }

    let jail_root_path = Path::new(&jail_root);
    let jailed =
        crate::core::pathjail::jail_path_auto_admitting(&resolved, jail_root_path, extra_roots)
            .map_err(|e| PathRefusal::jail(&e))?;
    crate::core::io_boundary::check_secret_path_for_tool("resolve_path", &jailed)
        .map_err(PathRefusal::secret_path)?;

    Ok(crate::core::pathutil::normalize_tool_path(
        &jailed.to_string_lossy().replace('\\', "/"),
    ))
}

/// Refusal for a relative path that cannot mean anything yet: the session has
/// no project, the host started lean-ctx in a broad directory (OpenCode
/// launched from `~`), and nothing exists at `resolved`. Without it agents got
/// "file not found: ~/src/main.rs" and searched the wrong tree. `None` when the
/// path is absolute, exists, or `base` is an ordinary working directory.
pub fn unbound_relative_path(raw: &str, resolved: &Path, base: &str) -> Option<String> {
    let normalized = crate::core::pathutil::normalize_tool_path(raw);
    if Path::new(&normalized).is_absolute() || resolved.exists() {
        return None;
    }
    let base = std::path::absolute(base).unwrap_or_else(|_| PathBuf::from(base));
    if !crate::core::pathutil::is_broad_or_unsafe_root(&base) {
        return None;
    }
    Some(format!(
        "relative path '{raw}' has no project to resolve against: lean-ctx has no project for \
         this session yet (the host started it in {}, outside a project). Pass the absolute \
         path instead, e.g. /home/me/code/my-app/{normalized}; that project then stays the \
         session project.",
        base.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn empty_and_dot_pass_through() {
        assert_eq!(resolve_tool_path(None, None, "").unwrap(), "");
        assert_eq!(resolve_tool_path(None, None, ".").unwrap(), ".");
    }

    #[test]
    fn missing_relative_path_without_a_project_says_what_is_missing() {
        let _home = crate::core::data_dir::test_env_lock();
        // A host started outside a project: the home directory, as OpenCode
        // does when launched from `~` (broad on every platform; `/` is not
        // broad on Windows, where it means the current drive's root).
        let home = dirs::home_dir()
            .expect("home dir")
            .to_string_lossy()
            .to_string();
        let error =
            resolve_tool_path(None, Some(&home), "src/no_such_file_lc_7f3a.rs").unwrap_err();
        assert!(
            error.contains("has no project to resolve against"),
            "{error}"
        );
        assert!(error.contains("absolute path"), "{error}");

        // A relative path in an ordinary working directory keeps resolving there.
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().to_string_lossy().to_string();
        let outcome = resolve_tool_path(None, Some(&cwd), "src/no_such_file_lc.rs");
        assert!(
            outcome
                .as_ref()
                .err()
                .is_none_or(|e| !e.contains("no project to resolve against")),
            "{outcome:?}"
        );
    }

    #[test]
    fn relative_resolves_against_project_root() {
        let _environment = crate::core::data_dir::test_env_lock();
        let tmp = std::env::temp_dir().join(format!("lc_pr_{}", std::process::id()));
        let _ = fs::create_dir_all(&tmp);
        let file = tmp.join("a.txt");
        fs::write(&file, "x").unwrap();
        let root = tmp.to_string_lossy().to_string();

        let out = resolve_tool_path(Some(&root), None, "a.txt").unwrap();
        assert!(out.ends_with("a.txt"), "got {out}");
        assert!(out.contains(&root) || Path::new(&out).is_absolute());

        let _ = fs::remove_dir_all(&tmp);
    }

    /// #2262: with a project root, a relative path names the file under that
    /// root — a session cwd elsewhere is never a second base, not even for a
    /// file that exists only there.
    #[test]
    fn shell_cwd_is_no_second_base_when_a_project_root_is_set() {
        let _environment = crate::core::data_dir::test_env_lock();
        let base = tempfile::tempdir().unwrap();
        let root = base.path().join("root");
        let cwd = root.join("sub");
        fs::create_dir_all(&cwd).unwrap();
        fs::write(cwd.join("only_in_cwd.txt"), "x").unwrap();

        let out = resolve_tool_path(
            Some(&root.to_string_lossy()),
            Some(&cwd.to_string_lossy()),
            "only_in_cwd.txt",
        )
        .unwrap();
        let parent = crate::core::pathjail::canonicalize_or_self(Path::new(&out).parent().unwrap());
        assert_eq!(
            parent,
            crate::core::pathjail::canonicalize_or_self(&root),
            "{out}"
        );
    }

    // P0-3 (#415): a relative path that happens to exist in the *process CWD*
    // must NOT short-circuit resolution. `Cargo.toml` exists in the package
    // root (cargo test's CWD) but not in this empty project root — before the
    // fix the CWD probe returned it as-is, now it must resolve into the root.
    #[test]
    fn relative_path_never_resolves_against_process_cwd() {
        let _environment = crate::core::data_dir::test_env_lock();
        let cwd = std::env::current_dir().unwrap();
        assert!(
            cwd.join("Cargo.toml").exists(),
            "test premise: CWD contains Cargo.toml"
        );

        let tmp = std::env::temp_dir().join(format!("lc_pr_nocwd_{}", std::process::id()));
        fs::create_dir_all(&tmp).unwrap();
        let root = tmp.to_string_lossy().to_string();

        let out = resolve_tool_path(Some(&root), None, "Cargo.toml").unwrap();
        // Canonicalize BOTH sides before comparing: on macOS temp_dir() is a
        // symlink (/var → /private/var) and on Windows it may carry 8.3 short
        // names (RUNNER~1), so comparing raw strings is platform-flaky. The
        // resolved file itself does not exist, but its parent does — compare
        // the canonicalized parents.
        let canonical_root = crate::core::pathjail::canonicalize_or_self(&tmp);
        let out_parent = crate::core::pathjail::canonicalize_or_self(
            Path::new(&out)
                .parent()
                .expect("resolved path has a parent"),
        );
        assert_eq!(
            out_parent, canonical_root,
            "resolved {out} must live under the project root, not the process CWD"
        );
        let canonical_cwd = crate::core::pathjail::canonicalize_or_self(&cwd);
        assert_ne!(
            out_parent, canonical_cwd,
            "resolved {out} must not resolve against the process CWD"
        );

        let _ = fs::remove_dir_all(&tmp);
    }

    // #403: session-scoped extra_roots must thread through to the jail so an
    // explicit path under a worktree resolves where the bare resolver rejects
    // it. Asserts only the Ok case (robust against parallel env mutation): with
    // the jail on, success here is only possible because extra_roots were honored.
    #[cfg(not(feature = "no-jail"))]
    #[test]
    fn extra_roots_thread_through_resolve_tool_path() {
        let _environment = crate::core::data_dir::test_env_lock();
        let base = std::env::temp_dir().join(format!("lc_pr_extra_{}", std::process::id()));
        let root = base.join("root");
        let worktree = base.join("worktree");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&worktree).unwrap();
        let file = worktree.join("a.txt");
        fs::write(&file, "x").unwrap();

        let root_s = root.to_string_lossy().to_string();
        let file_abs = file.to_string_lossy().to_string();
        let extra = vec![worktree.to_string_lossy().to_string()];

        let out = resolve_tool_path_with_roots(Some(&root_s), None, &file_abs, &extra);
        assert!(
            out.is_ok(),
            "extra_roots must thread through the resolver: {out:?}"
        );

        let _ = fs::remove_dir_all(&base);
    }

    /// #2262: the Claude Code `EnterWorktree` topology — a full checkout with
    /// its own `.git` FILE nested under `<repo>/.claude/worktrees/<n>/`, and a
    /// session cwd inside it. A project-relative path still names the project
    /// root's file (existing or a write target); the worktree is reached by
    /// absolute path, or by the path relative to the root.
    #[test]
    fn worktree_shell_cwd_does_not_move_the_relative_base() {
        // Path authorization reads the process-wide protected-runtime profile.
        // Serialize with tests that temporarily change that profile.
        let _environment = crate::core::data_dir::test_env_lock();
        let base = tempfile::tempdir().unwrap();
        let repo = base.path().join("repo");
        let wt = repo.join(".claude").join("worktrees").join("fix-x");
        fs::create_dir_all(repo.join("src")).unwrap();
        fs::create_dir_all(repo.join(".git")).unwrap(); // main checkout: .git dir
        fs::create_dir_all(wt.join("src")).unwrap();
        fs::write(wt.join(".git"), "gitdir: ../../.git/worktrees/fix-x\n").unwrap(); // worktree: .git FILE
        fs::write(repo.join("src/scoring.rs"), "root").unwrap();
        fs::write(wt.join("src/scoring.rs"), "worktree").unwrap();
        let (repo_s, wt_s) = (repo.to_string_lossy(), wt.to_string_lossy());

        let out = resolve_tool_path(Some(&repo_s), Some(&wt_s), "src/scoring.rs").unwrap();
        assert_eq!(fs::read_to_string(&out).unwrap(), "root", "{out}");

        let new = resolve_tool_path(Some(&repo_s), Some(&wt_s), "src/new_file.rs").unwrap();
        assert!(
            !new.contains("worktrees"),
            "write target stays in the root: {new}"
        );

        // The #2262 repro path: relative to the root, never doubled.
        let rel = resolve_tool_path(
            Some(&repo_s),
            Some(&wt_s),
            ".claude/worktrees/fix-x/src/scoring.rs",
        )
        .unwrap();
        assert_eq!(fs::read_to_string(&rel).unwrap(), "worktree", "{rel}");
        assert_eq!(rel.matches("worktrees").count(), 1, "{rel}");

        let abs = resolve_tool_path(
            Some(&repo_s),
            Some(&wt_s),
            &wt.join("src/scoring.rs").to_string_lossy(),
        )
        .unwrap();
        assert_eq!(fs::read_to_string(&abs).unwrap(), "worktree", "{abs}");
    }

    #[test]
    fn tool_context_shape_project_root_only() {
        let _environment = crate::core::data_dir::test_env_lock();
        // Mirrors ToolContext::resolve_path_sync (shell_cwd = None).
        let tmp = std::env::temp_dir().join(format!("lc_pr_ctx_{}", std::process::id()));
        fs::create_dir_all(&tmp).unwrap();
        let root = tmp.to_string_lossy().to_string();
        let out = resolve_tool_path(Some(&root), None, "missing.rs").unwrap();
        assert!(out.ends_with("missing.rs"), "got {out}");
        let _ = fs::remove_dir_all(&tmp);
    }

    // GH #397: on Unix an absolute path under a single-letter root (`/c/…`)
    // was rewritten to `C:/…`, which `Path::is_absolute()` rejects on Unix —
    // the path was then re-joined under the (also-translated) project root,
    // producing the doubled `C:/root/C:/root/file` form from the report.
    // `/c` cannot be created in this test environment, so the jail may still
    // reject the path as nonexistent — the regression assertion is that no
    // `C:/` drive form appears anywhere in the outcome (Ok or Err).
    #[cfg(not(windows))]
    #[test]
    fn single_letter_root_is_never_drive_translated_on_unix() {
        let _environment = crate::core::data_dir::test_env_lock();
        for raw in ["/c/Users/me/proj/src/app.ts", "src/app.ts"] {
            let rendered = match resolve_tool_path(Some("/c/Users/me/proj"), None, raw) {
                Ok(p) => p,
                Err(e) => e,
            };
            assert!(
                !rendered.contains("C:/"),
                "drive translation must not run on unix hosts (raw={raw}): {rendered}"
            );
        }
    }

    // What the audit trail records for a refused path: the boundary's class,
    // and none for a path that merely does not resolve.
    #[test]
    fn a_refusal_names_the_boundary_that_refused() {
        use crate::core::security_events::RefusalClass;
        let _environment = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        fs::write(root.join(".env"), "TOKEN=x").unwrap();
        let root = root.to_string_lossy().into_owned();
        let prev = std::env::var("LEAN_CTX_IO_BOUNDARY_MODE").ok();
        crate::test_env::set_var("LEAN_CTX_IO_BOUNDARY_MODE", "enforce");
        let secret = resolve_tool_path_refusing(Some(&root), None, ".env", &[]);
        let outside =
            resolve_tool_path_refusing(Some(&root), None, &format!("{root}/../outside.txt"), &[]);
        match prev {
            Some(v) => crate::test_env::set_var("LEAN_CTX_IO_BOUNDARY_MODE", v),
            None => crate::test_env::remove_var("LEAN_CTX_IO_BOUNDARY_MODE"),
        }
        let secret = secret.expect_err("enforce mode refuses a secret path");
        assert_eq!(secret.class, Some(RefusalClass::SecretPath));
        assert!(
            secret.message.contains("Secret-like path"),
            "{}",
            secret.message
        );
        if !cfg!(feature = "no-jail") {
            let outside = outside.expect_err("the jail refuses a path outside the root");
            assert_eq!(outside.class, Some(RefusalClass::OutsideRoot));
        }
        let unbound = resolve_tool_path_refusing(None, None, "src/no_such_file_lc_9c1.rs", &[]);
        if let Err(refusal) = unbound {
            assert_eq!(refusal.class, None, "{}", refusal.message);
        }
    }
}
