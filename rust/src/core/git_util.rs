//! Minimal, dependency-free git shell-outs shared by the context-artifact and
//! analysis tools (`ctx_impact`, `ctx_architecture`, `context_artifacts`).
//!
//! These intentionally stay tiny and best-effort: a missing or failing git
//! never aborts a tool, it just yields `false`/`None` so callers degrade
//! gracefully.

use std::path::Path;
use std::process::{Command, Stdio};

/// Returns `true` when the working tree at `project_root` has uncommitted
/// changes. Any git failure (not a repo, git absent) reports `false`.
pub(crate) fn git_dirty(project_root: &Path) -> bool {
    let out = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(project_root)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();
    match out {
        Ok(o) if o.status.success() => !o.stdout.is_empty(),
        _ => false,
    }
}

/// Runs `git <args>` in `project_root` and returns trimmed stdout, or `None`
/// on non-zero exit, non-UTF-8 output, empty output, or spawn failure.
pub(crate) fn git_out(project_root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(project_root)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?;
    let s = s.trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}
