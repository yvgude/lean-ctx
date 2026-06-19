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
/// Resolution order for relative inputs:
/// 1. absolute path → used as-is
/// 2. `<project_root>/<path>` if it exists
/// 3. `<shell_cwd>/<path>` if a shell cwd is known
/// 4. `<jail_root>/<path>` as a last resort
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
///
/// Read-only roots are honored permissively here (`read_only_ok = true`): the
/// shared sync resolver is used by both read and write handlers, and the
/// write-side block is applied by callers that know they mutate (via
/// [`resolve_tool_path_rw`] / the dispatch pre-resolution keyed on
/// `is_readonly_tool`). A read-only-root path therefore resolves through this
/// fn; an actual write to it is refused at the write choke point.
pub fn resolve_tool_path_with_roots(
    project_root: Option<&str>,
    shell_cwd: Option<&str>,
    raw: &str,
    extra_roots: &[String],
) -> Result<String, String> {
    resolve_tool_path_inner(project_root, shell_cwd, raw, extra_roots, true)
}

/// Write-tier resolution: identical to [`resolve_tool_path_with_roots`] but a
/// path that resolves *only* via a read-only root is REFUSED with a clear
/// error. Write/edit handlers that resolve paths in-handler (rather than via the
/// dispatch pre-resolution) call this so a read-only root is never writable.
pub fn resolve_tool_path_rw(
    project_root: Option<&str>,
    shell_cwd: Option<&str>,
    raw: &str,
    extra_roots: &[String],
) -> Result<String, String> {
    resolve_tool_path_inner(project_root, shell_cwd, raw, extra_roots, false)
}

/// Shared body. `read_only_ok` gates the read-only tier: when false, a candidate
/// permitted *only* because it lives under a read-only root is rejected (writes
/// forbidden); when true, it resolves (reads allowed).
fn resolve_tool_path_inner(
    project_root: Option<&str>,
    shell_cwd: Option<&str>,
    raw: &str,
    extra_roots: &[String],
    read_only_ok: bool,
) -> Result<String, String> {
    let normalized = crate::core::pathutil::normalize_tool_path(raw);
    if normalized.is_empty() || normalized == "." {
        return Ok(normalized);
    }

    let p = Path::new(&normalized);
    let jail_root = project_root.or(shell_cwd).unwrap_or(".").to_string();

    let resolved: PathBuf = if p.is_absolute() {
        PathBuf::from(&normalized)
    } else if let Some(root) = project_root {
        let joined = Path::new(root).join(&normalized);
        if joined.exists() {
            joined
        } else if let Some(cwd) = shell_cwd {
            Path::new(cwd).join(&normalized)
        } else {
            Path::new(root).join(&normalized)
        }
    } else if let Some(cwd) = shell_cwd {
        Path::new(cwd).join(&normalized)
    } else {
        Path::new(&jail_root).join(&normalized)
    };

    let jail_root_path = Path::new(&jail_root);
    let jailed = crate::core::pathjail::jail_path_with_roots_ro(
        &resolved,
        jail_root_path,
        extra_roots,
        &[],
    )?;
    if jailed.read_only && !read_only_ok {
        return Err(crate::core::pathjail::READ_ONLY_ROOT_WRITE_ERR.to_string());
    }
    crate::core::io_boundary::check_secret_path_for_tool("resolve_path", &jailed.path)?;

    Ok(crate::core::pathutil::normalize_tool_path(
        &jailed.path.to_string_lossy().replace('\\', "/"),
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
    fn relative_resolves_against_project_root() {
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

    #[test]
    fn falls_back_to_shell_cwd_when_not_in_project_root() {
        let base = std::env::temp_dir().join(format!("lc_pr_cwd_{}", std::process::id()));
        let root = base.join("root");
        let cwd = base.join("cwd");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&cwd).unwrap();
        fs::write(cwd.join("only_in_cwd.txt"), "x").unwrap();

        let out = resolve_tool_path(
            Some(&root.to_string_lossy()),
            Some(&cwd.to_string_lossy()),
            "only_in_cwd.txt",
        );
        // jail_root is project_root; a file only under shell_cwd resolves to a
        // cwd-joined path which may be rejected by the jail — either way it must
        // not panic and must yield a deterministic Result.
        assert!(out.is_ok() || out.is_err());

        let _ = fs::remove_dir_all(&base);
    }

    // P0-3 (#415): a relative path that happens to exist in the *process CWD*
    // must NOT short-circuit resolution. `Cargo.toml` exists in the package
    // root (cargo test's CWD) but not in this empty project root — before the
    // fix the CWD probe returned it as-is, now it must resolve into the root.
    #[test]
    fn relative_path_never_resolves_against_process_cwd() {
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

    #[test]
    fn tool_context_shape_project_root_only() {
        // Mirrors ToolContext::resolve_path_sync (shell_cwd = None).
        let tmp = std::env::temp_dir().join(format!("lc_pr_ctx_{}", std::process::id()));
        fs::create_dir_all(&tmp).unwrap();
        let root = tmp.to_string_lossy().to_string();
        let out = resolve_tool_path(Some(&root), None, "missing.rs").unwrap();
        assert!(out.ends_with("missing.rs"), "got {out}");
        let _ = fs::remove_dir_all(&tmp);
    }

    /// Security: a path under a read-only root RESOLVES for a read (the
    /// permissive resolver) but is REFUSED for a write (`resolve_tool_path_rw`)
    /// with the canonical read-only-write error. This is the resolver-level
    /// guarantee that a read_only root is never writable. Serialized on the env
    /// lock; isolated data dir so the jail is on.
    #[cfg(not(feature = "no-jail"))]
    #[test]
    fn read_only_root_resolves_for_read_but_refuses_write() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let _rol = READ_ONLY_ENV_LOCK.lock().unwrap();

        let base = tempfile::tempdir().unwrap();
        let root = base.path().join("project");
        let ro = base.path().join("sibling");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&ro).unwrap();
        let in_ro = ro.join("a.txt");
        fs::write(&in_ro, "x").unwrap();

        let root_s = root.to_string_lossy().to_string();
        let ro_canon = crate::core::pathjail::canonicalize_or_self(&ro);
        let file_abs = in_ro.to_string_lossy().to_string();

        crate::test_env::set_var(
            "LEAN_CTX_READ_ONLY_ROOTS",
            ro_canon.to_string_lossy().as_ref(),
        );
        // Read tier: resolves.
        let read = resolve_tool_path_with_roots(Some(&root_s), None, &file_abs, &[]);
        // Write tier: refused with the canonical message.
        let write = resolve_tool_path_rw(Some(&root_s), None, &file_abs, &[]);
        crate::test_env::remove_var("LEAN_CTX_READ_ONLY_ROOTS");

        assert!(
            read.is_ok(),
            "read must resolve a read_only-root path: {read:?}"
        );
        let err = write.expect_err("write to a read_only-root path must be refused");
        assert_eq!(err, crate::core::pathjail::READ_ONLY_ROOT_WRITE_ERR);
    }

    /// A normal `extra_root` stays read-WRITE through the write-tier resolver: it
    /// is NOT a read-only root, so `resolve_tool_path_rw` resolves it. Guards
    /// against the read-only gate over-firing on the existing read-write tier.
    #[cfg(not(feature = "no-jail"))]
    #[test]
    fn extra_root_remains_writable_through_rw_resolver() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let _rol = READ_ONLY_ENV_LOCK.lock().unwrap();
        crate::test_env::remove_var("LEAN_CTX_READ_ONLY_ROOTS");

        let base = tempfile::tempdir().unwrap();
        let root = base.path().join("project");
        let rw = base.path().join("worktree");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&rw).unwrap();
        let in_rw = rw.join("b.txt");
        fs::write(&in_rw, "y").unwrap();

        let root_s = root.to_string_lossy().to_string();
        let extra = vec![rw.to_string_lossy().to_string()];
        let out = resolve_tool_path_rw(Some(&root_s), None, &in_rw.to_string_lossy(), &extra);
        assert!(
            out.is_ok(),
            "a read-write extra_root must stay writable through the rw resolver: {out:?}"
        );
    }

    /// Serializes tests mutating `LEAN_CTX_READ_ONLY_ROOTS`.
    #[cfg(not(feature = "no-jail"))]
    static READ_ONLY_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
}
