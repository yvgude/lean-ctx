// SPDX-License-Identifier: Apache-2.0
//! Linked git worktrees, read from git's own files without running git.

use std::path::{Path, PathBuf};

/// The main checkout of the linked git worktree at `root`, read from git's
/// own files without running git: `<root>/.git` is a file naming the
/// worktree's git dir, whose `gitdir` points back at that file and whose
/// `commondir` names the repository's `.git` directory of the main checkout.
/// `None` for a main checkout, a bare repository, or files that do not
/// agree with each other.
pub fn main_worktree_root(root: &Path) -> Option<PathBuf> {
    let dot_git = root.join(".git");
    if !std::fs::symlink_metadata(&dot_git).ok()?.is_file() {
        return None;
    }
    let text = std::fs::read_to_string(&dot_git).ok()?;
    let gitdir = text.lines().next()?.strip_prefix("gitdir:")?.trim();
    let gitdir = root.join(gitdir).canonicalize().ok()?;
    let back = std::fs::read_to_string(gitdir.join("gitdir")).ok()?;
    if gitdir.join(back.trim()).canonicalize().ok()? != dot_git.canonicalize().ok()? {
        return None;
    }
    let common = std::fs::read_to_string(gitdir.join("commondir")).ok()?;
    let common = gitdir.join(common.trim()).canonicalize().ok()?;
    if common.file_name()? != ".git" {
        return None;
    }
    let main = common.parent()?;
    let main_dot_git = std::fs::symlink_metadata(main.join(".git")).ok()?;
    (main_dot_git.is_dir() && main != root.canonicalize().ok()?).then(|| main.to_path_buf())
}
