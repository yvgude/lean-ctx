// SPDX-License-Identifier: Apache-2.0
//! The project a command-line invocation acts for, and the read boundary a
//! command-line read enforces against it.
//!
//! The root comes from `--root`/`--project-root`, then `LEAN_CTX_PROJECT_ROOT`
//! (reconciled with the working directory, AP04-G10), then the git root above
//! the working directory. Shared by the CLI commands, the daemon client and the
//! shell-hook rewriters, so every entry point names the same project.

use std::path::Path;

/// The project this process acts for, from `args` and the working directory.
pub(crate) fn detect_project_root(args: &[String]) -> String {
    detect_project_root_in(args, std::env::current_dir().ok().as_deref())
}

/// The project a command that writes project-scoped state acts for; `Err`
/// instead of `~`, `/` or an agent directory (AP04-G10).
pub(crate) fn require_project_root() -> Result<String, String> {
    require_project_root_in(std::env::current_dir().ok().as_deref())
}

fn require_project_root_in(cwd: Option<&Path>) -> Result<String, String> {
    let root = detect_project_root_in(&[], cwd);
    if crate::core::pathutil::is_broad_or_unsafe_root(Path::new(&root)) {
        return Err(format!(
            "{root} is not a project — run inside a project or set LEAN_CTX_PROJECT_ROOT"
        ));
    }
    Ok(root)
}

fn detect_project_root_in(args: &[String], cwd: Option<&Path>) -> String {
    let mut it = args.iter().peekable();
    while let Some(a) = it.next() {
        if let Some(v) = a.strip_prefix("--root=")
            && !v.trim().is_empty()
        {
            return normalize_explicit_root(v);
        }
        if let Some(v) = a.strip_prefix("--project-root=")
            && !v.trim().is_empty()
        {
            return normalize_explicit_root(v);
        }
        if (a == "--root" || a == "--project-root")
            && let Some(v) = it.peek()
            && !v.starts_with("--")
            && !v.trim().is_empty()
        {
            return normalize_explicit_root(v);
        }
    }
    let cwd_root = cwd.map(|cwd| promote_to_git_root(&cwd.to_string_lossy()));
    if let Ok(root) = std::env::var("LEAN_CTX_PROJECT_ROOT")
        && !root.trim().is_empty()
    {
        return crate::server::roots::reconcile_pinned_root(
            normalize_explicit_root(&root),
            cwd_root,
        );
    }
    cwd_root.unwrap_or_else(|| ".".to_string())
}

fn normalize_explicit_root(path: &str) -> String {
    let expanded = expand_home(path.trim());
    crate::core::index_paths::normalize_project_root(&expanded)
}

fn expand_home(path: &str) -> String {
    if path == "~" {
        return dirs::home_dir().map_or_else(
            || path.to_string(),
            |home| home.to_string_lossy().to_string(),
        );
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return dirs::home_dir().map_or_else(
            || path.to_string(),
            |home| home.join(rest).to_string_lossy().to_string(),
        );
    }
    path.to_string()
}

pub(crate) fn promote_to_git_root(path: &str) -> String {
    let mut p = Path::new(path);
    loop {
        // A dotfiles repository at `~` is not the project of everything below.
        if p.join(".git").exists() && !crate::core::pathutil::is_broad_or_unsafe_root(p) {
            return p.to_string_lossy().to_string();
        }
        match p.parent() {
            Some(parent) => p = parent,
            None => return path.to_string(),
        }
    }
}

/// #1903: `lean-ctx read` enforces the same boundary as MCP `ctx_read` — the
/// PathJail (project root + `allow_paths`/`extra_roots`/`read_only_roots` + the
/// lean-ctx state dir) and the secret-path policy — with the same error text.
/// A relative path is resolved against the process CWD (a shell user's
/// intent), never re-anchored at the project root. A broad root (home, `/`,
/// agent config dir) is refused like `lean-ctx call` does, unless the jail is
/// disabled (`path_jail = false`), because jailing to `~` would admit every
/// file the user owns.
pub(crate) fn jail_cli_read_path(raw: &str, project_root: &str) -> Result<String, String> {
    let candidate = if Path::new(raw).is_relative() {
        std::env::current_dir().map_or_else(
            |_| raw.to_string(),
            |cwd| cwd.join(raw).to_string_lossy().into_owned(),
        )
    } else {
        raw.to_string()
    };
    let jail_disabled = crate::core::config::Config::load().path_jail == Some(false);
    if !jail_disabled && crate::core::pathutil::is_broad_or_unsafe_root(Path::new(project_root)) {
        return Err(format!(
            "path escapes project root: {candidate} (root: {project_root}). Access denied: \
             no project detected — run inside a project, or pass --root <dir> \
             (or set LEAN_CTX_PROJECT_ROOT)"
        ));
    }
    crate::core::path_resolve::resolve_tool_path(Some(project_root), None, &candidate)
}

/// Whether `lean-ctx read <raw>` from this process would be refused (#1903).
/// Shell-hook rewriters keep such reads on the native command instead of
/// turning a working `cat` into an access-denied error.
pub(crate) fn cli_read_is_refused(raw: &str) -> bool {
    jail_cli_read_path(raw, &detect_project_root(&[])).is_err()
}

#[cfg(test)]
#[path = "command_root_tests.rs"]
mod tests;
