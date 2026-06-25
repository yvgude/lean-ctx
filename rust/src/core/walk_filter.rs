//! Central `filter_entry` predicate shared by every directory walker
//! (graph/index builders, `ctx_search`, `ctx_tree`, `ctx_glob`, CLI scans).
//!
//! Combines two protections:
//! 1. Cloud-placeholder pruning (`cloud_files::keep_entry`) so walks never
//!    hydrate OneDrive/iCloud stubs.
//! 2. A conservative vendor-directory skip list (#400). Package-manager
//!    output like `node_modules` is never useful in scans and explodes the
//!    overview/index when no `.gitignore` applies — e.g. a project without a
//!    `.git` directory, where the `ignore` crate skips `.gitignore` files
//!    entirely unless `require_git(false)` is set.
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

/// Returns `true` when `entry` is a vendor/dependency directory that should
/// never be descended into during a scan.
#[must_use]
pub fn is_vendor_dir(entry: &ignore::DirEntry) -> bool {
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

/// Predicate for `ignore::WalkBuilder::filter_entry`: prunes vendor
/// directories and cloud placeholders.
#[must_use]
pub fn keep_entry(entry: &ignore::DirEntry) -> bool {
    !is_vendor_dir(entry) && crate::core::cloud_files::keep_entry(entry)
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
}
