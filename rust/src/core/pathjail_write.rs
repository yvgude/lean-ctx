// SPDX-License-Identifier: Apache-2.0
//! Write destinations below a trusted root that refuse symlinked components.
//!
//! Project data (`<project>/.lean-ctx/...`) lives inside a working tree that
//! other people, checkouts and tools can populate. A symlinked `.lean-ctx`,
//! `proofs` or target file there must not redirect a LeanCTX write outside the
//! project, so these writes rebuild the path component by component below the
//! trusted root and fail closed on any link. The mechanics (directory handles,
//! `O_NOFOLLOW`, `renameat`) live in [`lean_ctx_fs::confined_write`].

use std::path::{Path, PathBuf};

const PROJECT_DATA_DIR: &str = ".lean-ctx";

/// Rebuild `root/rel` component by component, refusing every symlinked
/// component below `root` (a check only; nothing is created).
pub fn rebuild_below(root: &Path, rel: &Path) -> Result<PathBuf, String> {
    lean_ctx_fs::confined_write::rebuild_below(root, rel).map_err(|e| e.to_string())
}

/// Write `content` to `root/rel` (file `mode` on Unix), creating missing
/// directories, without following a symlink anywhere below `root`.
///
/// Only the symlink boundary is enforced here; whether `root` may be written
/// at all (read-only roots, jail scope) stays the caller's policy.
pub fn write_below(root: &Path, rel: &Path, content: &[u8], mode: u32) -> Result<PathBuf, String> {
    lean_ctx_fs::confined_write::write_below(root, rel, content, mode).map_err(|e| e.to_string())
}

/// The relative path below the project root for `rel` inside the project's
/// data directory, after the data-dir collision check.
fn project_data_rel(project_root: &Path, rel: &Path) -> Result<PathBuf, String> {
    crate::core::pathutil::safe_project_data_dir(project_root)?;
    Ok(Path::new(PROJECT_DATA_DIR).join(rel))
}

/// Check a destination `<project>/.lean-ctx/<rel>` without writing it.
pub fn project_data_destination(project_root: &Path, rel: &Path) -> Result<PathBuf, String> {
    rebuild_below(project_root, &project_data_rel(project_root, rel)?)
}

/// Write `<project>/.lean-ctx/<rel>`: the project root is trusted, `.lean-ctx`
/// and everything below it must not be a symlink.
pub fn write_project_data(
    project_root: &Path,
    rel: &Path,
    content: &[u8],
) -> Result<PathBuf, String> {
    write_below(
        project_root,
        &project_data_rel(project_root, rel)?,
        content,
        0o600,
    )
}

/// `path` as plain components below `root`: lexically first (so a symlinked
/// component stays visible and is refused), then against the canonical root
/// for a caller that already resolved it.
pub fn relative_below(root: &Path, path: &Path) -> Result<PathBuf, String> {
    let plain = |rel: &Path| {
        rel.components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
            && rel.components().next().is_some()
    };
    if let Ok(rel) = path.strip_prefix(root)
        && plain(rel)
    {
        return Ok(rel.to_path_buf());
    }
    let canonical_root = crate::core::pathutil::canonicalize_secure_or_self(root);
    match path.strip_prefix(&canonical_root) {
        Ok(rel) if plain(rel) => Ok(rel.to_path_buf()),
        _ => Err(format!(
            "{} is not below {}",
            path.display(),
            root.display()
        )),
    }
}

/// Write a caller-chosen destination below `root`: the lexical `requested`
/// path when it names plain components below `root` (a symlinked component
/// then stays visible and is refused), else the jail-resolved `jailed` path.
///
/// A `jailed` path outside `root` (an extra jail root the user allowed, or a
/// disabled jail) keeps working: its parent directory, which the jail already
/// resolved, is the trusted root and only the file itself must not be a link.
pub fn write_requested_below(
    root: &Path,
    requested: &Path,
    jailed: &Path,
    content: &[u8],
) -> Result<PathBuf, String> {
    if let Ok(rel) = relative_below(root, requested).or_else(|_| relative_below(root, jailed)) {
        return write_below(root, &rel, content, 0o600);
    }
    let (Some(parent), Some(name)) = (jailed.parent(), jailed.file_name()) else {
        return Err(format!("invalid destination: {}", jailed.display()));
    };
    std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    write_below(parent, Path::new(name), content, 0o600)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn project_data_writes_land_in_the_project() {
        let project = tempfile::tempdir().unwrap();
        let path = write_project_data(project.path(), Path::new("proofs/a.json"), b"{}").unwrap();
        assert_eq!(path, project.path().join(".lean-ctx/proofs/a.json"));
        assert_eq!(std::fs::read(&path).unwrap(), b"{}");
    }

    #[test]
    fn a_symlinked_data_dir_pointing_outside_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("project");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        symlink(&outside, project.join(".lean-ctx")).unwrap();
        let err = write_project_data(&project, Path::new("proofs/a.json"), b"x").unwrap_err();
        assert!(err.contains("symlink"), "{err}");
        assert!(!outside.join("proofs").exists());
    }

    #[test]
    fn relative_below_keeps_links_visible() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("r");
        std::fs::create_dir_all(&root).unwrap();
        assert_eq!(
            relative_below(&root, &root.join("a/b")).unwrap(),
            PathBuf::from("a/b")
        );
        assert!(relative_below(&root, &tmp.path().join("x")).is_err());
    }
}
