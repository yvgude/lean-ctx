// SPDX-License-Identifier: Apache-2.0
//! Project-root derivation from a working directory: the rule an MCP session
//! and the dashboard use to bind a folder to one project. Moved down from
//! `server/mod.rs`.

use std::path::Path;

use super::has_project_marker;

fn git_toplevel_from(dir: &Path) -> Option<String> {
    std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout)
                    .ok()
                    .map(|s| s.trim().to_string())
            } else {
                None
            }
        })
}

pub fn derive_project_root_from_cwd() -> Option<String> {
    derive_project_root_from(&std::env::current_dir().ok()?)
}

/// The project an MCP session started in `cwd` binds to. The dashboard binds
/// a folder by the same rule, so both name one project the same way.
pub fn derive_project_root_from(cwd: &Path) -> Option<String> {
    let canonical = crate::core::pathutil::safe_canonicalize_or_self(cwd);

    if crate::core::pathutil::is_broad_or_unsafe_root(&canonical) {
        return git_toplevel_from(&canonical);
    }

    if has_project_marker(&canonical) {
        return Some(canonical.to_string_lossy().to_string());
    }

    if let Some(git_root) = git_toplevel_from(&canonical) {
        return Some(git_root);
    }

    if let Some(root) = detect_multi_root_workspace(&canonical) {
        return Some(root);
    }

    // Fallback: use CWD as project root if it's a specific, safe directory.
    // This ensures bare directories (no .git, no markers) still work.
    // Guard: reject home dir, filesystem root, and agent sandbox dirs.
    if !crate::core::pathutil::is_broad_or_unsafe_root(&canonical) {
        tracing::info!(
            "No project markers found — using CWD as project root: {}",
            canonical.display()
        );
        return Some(canonical.to_string_lossy().to_string());
    }

    None
}

/// Detect a multi-root workspace: a directory that has no project markers
/// itself, but contains child directories that do. In this case, use the
/// parent as jail root and auto-allow all child projects via LEAN_CTX_ALLOW_PATH.
fn detect_multi_root_workspace(dir: &Path) -> Option<String> {
    // Never enumerate broad roots or macOS TCC-protected dirs: read_dir there
    // can scan an entire user profile or trigger a macOS privacy prompt.
    if crate::core::pathutil::is_broad_or_unsafe_root(dir)
        || crate::core::pathutil::is_tcc_sensitive_home_dir(dir)
    {
        return None;
    }
    let entries = std::fs::read_dir(dir).ok()?;
    let mut child_projects: Vec<String> = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && has_project_marker(&path) {
            let canonical = crate::core::pathutil::safe_canonicalize_or_self(&path);
            child_projects.push(canonical.to_string_lossy().to_string());
        }
    }

    if child_projects.len() >= 2 {
        crate::core::runtime_flags::add_allow_paths(
            child_projects
                .iter()
                .map(std::path::PathBuf::from)
                .collect(),
        );
        tracing::info!(
            "Multi-root workspace detected at {}: auto-allowing {} child projects",
            dir.display(),
            child_projects.len()
        );
        return Some(dir.to_string_lossy().to_string());
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_markers_detected() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("myproject");
        std::fs::create_dir_all(&root).unwrap();
        assert!(!has_project_marker(&root));

        std::fs::create_dir(root.join(".git")).unwrap();
        assert!(has_project_marker(&root));
    }

    #[test]
    fn detect_multi_root_workspace_with_child_projects() {
        // detect_multi_root_workspace adds the child projects to the
        // process-wide runtime allow-list (`runtime_flags::add_allow_paths`),
        // which is never cleared and makes every later jail test in the same
        // process see a relaxation (#695). Run it in its own process.
        if crate::test_env::run_in_own_process(
            "core::mcp_roots::derive::tests::detect_multi_root_workspace_with_child_projects",
            &[],
        ) {
            return;
        }
        let _guard = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();

        let proj_a = workspace.join("project-a");
        let proj_b = workspace.join("project-b");
        std::fs::create_dir_all(proj_a.join(".git")).unwrap();
        std::fs::create_dir_all(&proj_b).unwrap();
        std::fs::write(proj_b.join("package.json"), "{}").unwrap();

        let result = detect_multi_root_workspace(&workspace);
        assert!(
            result.is_some(),
            "should detect workspace with 2 child projects"
        );

        crate::test_env::remove_var("LEAN_CTX_ALLOW_PATH");
    }

    #[test]
    fn detect_multi_root_workspace_returns_none_for_single_project() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();

        let proj_a = workspace.join("project-a");
        std::fs::create_dir_all(proj_a.join(".git")).unwrap();

        let result = detect_multi_root_workspace(&workspace);
        assert!(
            result.is_none(),
            "should not detect workspace with only 1 child project"
        );
    }

    #[test]
    fn derive_project_root_falls_back_to_bare_cwd() {
        let tmp = tempfile::tempdir().unwrap();
        let bare = tmp.path().join("bare-dir");
        std::fs::create_dir_all(&bare).unwrap();

        let result = {
            let _cwd = crate::test_env::CurrentDirGuard::enter(&bare);
            derive_project_root_from_cwd()
        };

        assert!(result.is_some(), "bare dir should produce a project root");
        let root = result.unwrap();
        assert!(
            root.contains("bare-dir"),
            "fallback should use the bare dir path"
        );
    }
}
