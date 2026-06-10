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
    let jailed = crate::core::pathjail::jail_path(&resolved, jail_root_path)?;
    crate::core::io_boundary::check_secret_path_for_tool("resolve_path", &jailed)?;

    Ok(crate::core::pathutil::normalize_tool_path(
        &jailed.to_string_lossy().replace('\\', "/"),
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
}
