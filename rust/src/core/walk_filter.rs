//! Central `filter_entry` predicate shared by every directory walker
//! (graph/index builders, `ctx_search`, `ctx_tree`, `ctx_glob`, CLI scans).
//!
//! Combines three protections:
//! 1. Cloud-placeholder pruning (`cloud_files::keep_entry`) so walks never
//!    hydrate OneDrive/iCloud stubs.
//! 2. A conservative vendor-directory skip list (#400). Package-manager
//!    output like `node_modules` is never useful in scans and explodes the
//!    overview/index when no `.gitignore` applies — e.g. a project without a
//!    `.git` directory, where the `ignore` crate skips `.gitignore` files
//!    entirely unless `require_git(false)` is set.
//! 3. Stale agent worktree copies (`.claude/worktrees`, `.codex-worktrees`, …)
//!    that crowd out canonical files in repo-wide scans (#1480).
//!
//! Explicitly requested roots stay reachable: the guard only prunes entries
//! at `depth > 0`, so `ctx_tree path=node_modules/react` still works.

/// Directory names that are unambiguously package-manager/dependency output.
/// Deliberately conservative — anything project-specific (`dist`, `build`,
/// `target`) is left to `.gitignore` because those names are also used for
/// real source directories.
const VENDOR_DIR_NAMES: &[&str] = &["node_modules", "__pycache__", "bower_components"];

/// Virtualenv directory names; only skipped when they actually contain a
/// `pyvenv.cfg`, so a source folder that happens to be called `venv` survives.
const VENV_DIR_NAMES: &[&str] = &[".venv", "venv"];

/// Basenames of directories that hold stale agent worktree copies (#1480).
/// Kept in sync with the agent-worktree subset of
/// [`crate::core::auto_findings::NOISE_PATH_SEGMENTS`].
const AGENT_COPY_DIR_NAMES: &[&str] = &[".worktrees", ".codex-worktrees", ".claude", ".cursor"];

/// Resolve an explicitly requested Windows directory junction before handing it
/// to `ignore::WalkBuilder`. The walker can treat a reparse-point root as a
/// leaf, while normal file reads transparently follow it (#1003). Canonicalizing
/// only the already-jailed root preserves the rule that nested links are never
/// followed and strips Windows' verbatim prefix for `ignore` compatibility.
pub(crate) fn explicit_walk_root(root: &std::path::Path) -> std::path::PathBuf {
    #[cfg(windows)]
    {
        return crate::core::pathutil::canonicalize_raw(root)
            .unwrap_or_else(|_| root.to_path_buf());
    }
    #[cfg(not(windows))]
    {
        root.to_path_buf()
    }
}

/// Returns `true` when `entry` is a vendor/dependency directory that should
/// never be descended into during a scan.
pub(crate) fn is_vendor_dir(entry: &ignore::DirEntry) -> bool {
    if entry.depth() == 0 || !entry.file_type().is_some_and(|ft| ft.is_dir()) {
        return false;
    }
    let Some(name) = entry.file_name().to_str() else {
        return false;
    };
    if VENDOR_DIR_NAMES.contains(&name) {
        return true;
    }
    VENV_DIR_NAMES.contains(&name) && entry.path().join("pyvenv.cfg").is_file()
}

/// Returns `true` when `entry` is a stale agent worktree directory that
/// should not be descended into during repo-wide scans (#1480).
pub(crate) fn is_agent_worktree_dir(entry: &ignore::DirEntry) -> bool {
    if entry.depth() == 0 || !entry.file_type().is_some_and(|ft| ft.is_dir()) {
        return false;
    }
    let Some(name) = entry.file_name().to_str() else {
        return false;
    };
    if AGENT_COPY_DIR_NAMES.contains(&name) {
        return true;
    }
    // `.claude/worktrees`, `.cursor/worktrees`, … — segment is `worktrees`
    // (not `.worktrees`), so match by name + dot-prefixed parent.
    name == "worktrees"
        && entry.path().parent().is_some_and(|parent| {
            parent
                .file_name()
                .is_some_and(|pn| pn.to_string_lossy().starts_with('.'))
        })
}

/// Whether a *content* walk should skip dot-prefixed entries.
///
/// Always `false` (#1792). What belongs to a project is decided by git and
/// `.gitignore`; a leading dot is a display convention, not a relevance
/// signal. The distinction matters more than it used to: `.github/`,
/// `.agents/`, `.config/` and friends hold source-owned automation and agent
/// instructions that a repository genuinely owns and tracks.
///
/// Before this, walkers disagreed — `ctx_glob` and `ctx_search` found tracked
/// dotfiles while `lean-ctx find` and the BM25/graph corpus builders did not,
/// with no option to change it. The corpus case was the damaging one: an
/// incomplete index is indistinguishable from an empty result, so `ctx_compose`
/// reported "no match" for code that was right there and tracked.
///
/// This constant is deliberately not a config key. A corpus that silently
/// omits tracked files is a correctness bug, not a preference.
///
/// It governs *content* walks only. A walker whose job is to render a listing
/// for a person may still hide dotfiles by default — `ctx_tree` does, behind
/// `--all` — because there the leading dot is exactly the display convention
/// it was meant to be.
///
/// Note that [`keep_entry`] still prunes `.claude` / `.cursor` and the other
/// agent-copy directories regardless of this setting: those hold scratch and
/// worktree copies, and excluding them is a separate, deliberate rule.
pub(crate) const SKIP_HIDDEN_IN_CONTENT_WALK: bool = false;

/// Predicate for `ignore::WalkBuilder::filter_entry`: prunes vendor
/// directories, stale agent worktree copies, and cloud placeholders.
pub(crate) fn keep_entry(entry: &ignore::DirEntry) -> bool {
    !is_vendor_dir(entry)
        && !is_agent_worktree_dir(entry)
        && crate::core::cloud_files::keep_entry(entry)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(root: &std::path::Path) -> Vec<ignore::DirEntry> {
        ignore::WalkBuilder::new(root)
            .hidden(false)
            .build()
            .filter_map(std::result::Result::ok)
            .collect()
    }

    fn entry_named(root: &std::path::Path, name: &str) -> ignore::DirEntry {
        entries(root)
            .into_iter()
            .find(|e| e.file_name().to_str() == Some(name))
            .unwrap_or_else(|| panic!("entry {name} not found"))
    }

    #[test]
    fn node_modules_is_vendor() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join("node_modules/lodash")).expect("mkdir");
        let e = entry_named(tmp.path(), "node_modules");
        assert!(is_vendor_dir(&e));
        assert!(!keep_entry(&e));
    }

    #[test]
    fn pycache_is_vendor() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join("__pycache__")).expect("mkdir");
        assert!(is_vendor_dir(&entry_named(tmp.path(), "__pycache__")));
    }

    #[test]
    fn venv_with_pyvenv_cfg_is_vendor() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join(".venv")).expect("mkdir");
        std::fs::write(tmp.path().join(".venv/pyvenv.cfg"), "home = /usr").expect("write");
        assert!(is_vendor_dir(&entry_named(tmp.path(), ".venv")));
    }

    #[test]
    fn venv_named_source_dir_without_cfg_survives() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join("venv")).expect("mkdir");
        std::fs::write(tmp.path().join("venv/mod.rs"), "pub fn x() {}").expect("write");
        let e = entry_named(tmp.path(), "venv");
        assert!(!is_vendor_dir(&e));
        assert!(keep_entry(&e));
    }

    #[test]
    fn regular_dirs_and_files_are_kept() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join("src")).expect("mkdir");
        std::fs::write(tmp.path().join("src/main.rs"), "fn main() {}").expect("write");
        assert!(keep_entry(&entry_named(tmp.path(), "src")));
        assert!(keep_entry(&entry_named(tmp.path(), "main.rs")));
    }

    #[test]
    fn explicit_root_named_node_modules_is_not_pruned() {
        // depth == 0 must never be filtered, otherwise an explicitly
        // requested path like `ctx_tree node_modules/react` returns nothing.
        let tmp = tempfile::tempdir().expect("tempdir");
        let nm = tmp.path().join("node_modules");
        std::fs::create_dir_all(&nm).expect("mkdir");
        let root_entry = entries(&nm)
            .into_iter()
            .find(|e| e.depth() == 0)
            .expect("root entry");
        assert!(!is_vendor_dir(&root_entry));
        assert!(keep_entry(&root_entry));
    }

    #[test]
    fn walker_with_filter_skips_node_modules_contents() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join("node_modules/react")).expect("mkdir");
        std::fs::write(tmp.path().join("node_modules/react/index.js"), "x").expect("write");
        std::fs::create_dir_all(tmp.path().join("src")).expect("mkdir");
        std::fs::write(tmp.path().join("src/app.js"), "y").expect("write");

        let seen: Vec<String> = ignore::WalkBuilder::new(tmp.path())
            .filter_entry(keep_entry)
            .build()
            .filter_map(std::result::Result::ok)
            .map(|e| e.path().to_string_lossy().to_string())
            .collect();

        assert!(seen.iter().any(|p| p.ends_with("app.js")));
        assert!(!seen.iter().any(|p| p.contains("node_modules")));
    }

    #[test]
    fn claude_worktrees_are_skipped_but_canonical_src_survives() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join(".claude/worktrees/wt/src")).expect("mkdir");
        std::fs::write(
            tmp.path().join(".claude/worktrees/wt/src/stale.rs"),
            "fn stale_canary() {}",
        )
        .expect("write");
        std::fs::create_dir_all(tmp.path().join("src")).expect("mkdir");
        std::fs::write(tmp.path().join("src/main.rs"), "fn main() {}").expect("write");

        let seen: Vec<String> = ignore::WalkBuilder::new(tmp.path())
            .hidden(false)
            .filter_entry(keep_entry)
            .build()
            .filter_map(std::result::Result::ok)
            .map(|e| e.path().to_string_lossy().to_string())
            .collect();

        assert!(seen.iter().any(|p| p.ends_with("main.rs")));
        assert!(!seen.iter().any(|p| p.contains(".claude/worktrees")));
    }

    #[test]
    fn codex_worktrees_are_skipped() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join(".codex-worktrees/wt")).expect("mkdir");
        std::fs::write(tmp.path().join(".codex-worktrees/wt/copy.rs"), "x").expect("write");
        std::fs::write(tmp.path().join("real.rs"), "y").expect("write");

        let seen: Vec<String> = ignore::WalkBuilder::new(tmp.path())
            .hidden(false)
            .filter_entry(keep_entry)
            .build()
            .filter_map(std::result::Result::ok)
            .map(|e| e.path().to_string_lossy().to_string())
            .collect();

        assert!(seen.iter().any(|p| p.ends_with("real.rs")));
        assert!(!seen.iter().any(|p| p.contains(".codex-worktrees")));
    }

    /// #1792: the reporter's fixture — a tracked dotfile and a tracked file
    /// under a hidden directory must both be reachable by a content walk.
    /// `.gitignore` decides membership; the leading dot must not.
    #[test]
    fn content_walk_reaches_tracked_hidden_paths() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        std::fs::create_dir_all(root.join(".hidden")).expect("mkdir");
        std::fs::write(root.join(".editorconfig"), "root\n").expect("write");
        std::fs::write(root.join(".hidden/tool"), "nested\n").expect("write");
        std::fs::write(root.join("visible.txt"), "visible\n").expect("write");

        let seen: Vec<String> = ignore::WalkBuilder::new(root)
            .hidden(SKIP_HIDDEN_IN_CONTENT_WALK)
            .filter_entry(keep_entry)
            .build()
            .filter_map(std::result::Result::ok)
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();

        for name in [".editorconfig", "tool", "visible.txt"] {
            assert!(
                seen.iter().any(|s| s == name),
                "{name} must be discoverable"
            );
        }
    }

    /// The policy does not override `.gitignore` — an ignored dotfile stays out.
    /// Otherwise "include hidden" would quietly become "include everything".
    #[test]
    fn content_walk_still_honours_gitignore_for_hidden_paths() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        std::fs::write(root.join(".gitignore"), ".secret\n").expect("write");
        std::fs::write(root.join(".secret"), "nope\n").expect("write");
        std::fs::write(root.join(".editorconfig"), "root\n").expect("write");

        let seen: Vec<String> = ignore::WalkBuilder::new(root)
            .hidden(SKIP_HIDDEN_IN_CONTENT_WALK)
            .git_ignore(true)
            .require_git(false)
            .filter_entry(keep_entry)
            .build()
            .filter_map(std::result::Result::ok)
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();

        assert!(seen.iter().any(|s| s == ".editorconfig"));
        assert!(
            !seen.iter().any(|s| s == ".secret"),
            "an ignored dotfile must stay excluded"
        );
    }

    /// Including hidden paths must not resurrect the agent-copy directories
    /// #1480 prunes — those are a separate rule and stay pruned.
    #[test]
    fn content_walk_does_not_resurrect_agent_copy_dirs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        std::fs::create_dir_all(root.join(".codex-worktrees/wt")).expect("mkdir");
        std::fs::write(root.join(".codex-worktrees/wt/copy.rs"), "x").expect("write");
        std::fs::write(root.join(".editorconfig"), "root\n").expect("write");

        let seen: Vec<String> = ignore::WalkBuilder::new(root)
            .hidden(SKIP_HIDDEN_IN_CONTENT_WALK)
            .filter_entry(keep_entry)
            .build()
            .filter_map(std::result::Result::ok)
            .map(|e| e.path().to_string_lossy().to_string())
            .collect();

        assert!(seen.iter().any(|p| p.ends_with(".editorconfig")));
        assert!(!seen.iter().any(|p| p.contains(".codex-worktrees")));
    }

    #[test]
    fn explicit_root_named_claude_worktrees_is_not_pruned() {
        // `ctx_search path=.claude/worktrees/wt` still works.
        let tmp = tempfile::tempdir().expect("tempdir");
        let wt = tmp.path().join(".claude/worktrees/wt");
        std::fs::create_dir_all(&wt).expect("mkdir");
        std::fs::write(wt.join("inside.rs"), "fn inside() {}").expect("write");
        let root_entry = ignore::WalkBuilder::new(&wt)
            .hidden(false)
            .build()
            .filter_map(std::result::Result::ok)
            .find(|e| e.depth() == 0)
            .expect("root entry");
        assert!(!is_agent_worktree_dir(&root_entry));
        assert!(keep_entry(&root_entry));
    }
}
