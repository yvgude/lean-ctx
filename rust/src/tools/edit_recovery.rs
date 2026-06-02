//! Recovery context for failed `ctx_edit` calls (issue #331).
//!
//! When an edit cannot be applied, these helpers turn the failure into
//! actionable guidance so the agent does not have to spend a turn re-reading
//! or guessing which file it meant:
//!
//! - [`moved_or_deleted_hint`]: the target file does not exist — was it moved,
//!   or is the name/path wrong? (#331 point 3)
//! - [`cross_file_hint`]: `old_string` is not in the target file — does a
//!   matching line live in a *different* file the agent should have targeted?
//!   (#331 point 2)
//!
//! Both run only on the (rare) edit-failure path and replace a file read the
//! agent would otherwise do anyway, so a bounded directory walk is an
//! acceptable cost. Walks respect `.gitignore` and are capped on depth, files
//! scanned, and hits reported.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use ignore::WalkBuilder;

/// Depth cap for the recovery walk — deep enough for real trees, bounded
/// enough to stay cheap on the error path.
const MAX_WALK_DEPTH: usize = 12;
/// Hard ceiling on files visited so a giant monorepo cannot stall the walk.
const MAX_FILES_SCANNED: usize = 5_000;
/// Stop after this many candidate files — more than enough to disambiguate.
const MAX_HITS: usize = 5;
/// Skip files larger than this when scanning content (matches `ctx_search`).
const MAX_FILE_SIZE: u64 = 512_000;
/// Minimum length for a line to be a distinctive cross-file search needle,
/// so trivial lines like `}` or `{` never trigger noisy matches.
const MIN_NEEDLE_LEN: usize = 12;

/// #331 point 3: the target file does not exist. Search the enclosing repo for
/// a file with the same name so the agent learns whether it moved or the
/// path/name is simply wrong. Returns a leading-newline hint, or empty if the
/// path has no usable file name.
pub(crate) fn moved_or_deleted_hint(target: &Path) -> String {
    let Some(name) = target.file_name().and_then(|n| n.to_str()) else {
        return String::new();
    };
    let root = search_root(target);

    let mut hits: Vec<PathBuf> = Vec::new();
    let mut scanned = 0usize;
    for entry in walk(&root) {
        if entry.file_type().is_none_or(|ft| ft.is_dir()) {
            continue;
        }
        scanned += 1;
        if scanned > MAX_FILES_SCANNED {
            break;
        }
        if entry.file_name().to_str() == Some(name) {
            hits.push(entry.into_path());
            if hits.len() >= MAX_HITS {
                break;
            }
        }
    }

    if hits.is_empty() {
        format!(
            "\nNo file named `{name}` exists under {}. It may have been deleted, \
             or the path/name is wrong (use create=true to create it).",
            root.display()
        )
    } else {
        let mut out = String::from(
            "\nThe path does not exist, but a same-named file was found — did you mean:",
        );
        for p in &hits {
            let _ = write!(out, "\n  - {}", p.display());
        }
        out
    }
}

/// #331 point 2: `old_string` is not in the target file. Search sibling files
/// for the most distinctive line of `old_string` so the agent can re-target
/// the right file instead of assuming it picked the correct one. Returns a
/// leading-newline hint, or empty if nothing distinctive matches elsewhere.
pub(crate) fn cross_file_hint(target: &Path, old_str: &str) -> String {
    let Some(needle) = distinctive_line(old_str) else {
        return String::new();
    };
    let root = search_root(target);
    let target_canon = std::fs::canonicalize(target).ok();

    let mut hits: Vec<PathBuf> = Vec::new();
    let mut scanned = 0usize;
    for entry in walk(&root) {
        if entry.file_type().is_none_or(|ft| ft.is_dir()) {
            continue;
        }
        let path = entry.path();
        // Never point the agent back at the file it already tried. Compare
        // names first (cheap) and only canonicalize on a name collision.
        if target_canon.is_some()
            && target.file_name() == Some(entry.file_name())
            && std::fs::canonicalize(path).ok() == target_canon
        {
            continue;
        }
        scanned += 1;
        if scanned > MAX_FILES_SCANNED {
            break;
        }
        let too_big = std::fs::metadata(path).is_ok_and(|m| m.len() > MAX_FILE_SIZE);
        if too_big {
            continue;
        }
        if let Ok(content) = std::fs::read_to_string(path) {
            if content.contains(needle) {
                hits.push(path.to_path_buf());
                if hits.len() >= MAX_HITS {
                    break;
                }
            }
        }
    }

    if hits.is_empty() {
        return String::new();
    }
    let mut out = String::from("\nold_string was not found here, but a matching line exists in:");
    for p in &hits {
        let _ = write!(out, "\n  - {}", p.display());
    }
    out.push_str("\nIf you meant one of these, retry the edit against that file.");
    out
}

/// Builds the bounded, `.gitignore`-aware walker shared by both helpers.
fn walk(root: &Path) -> impl Iterator<Item = ignore::DirEntry> {
    WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .max_depth(Some(MAX_WALK_DEPTH))
        .build()
        .filter_map(Result::ok)
}

/// Resolves the directory to search from: the enclosing git repository root
/// when one exists, otherwise the nearest existing ancestor of `target`
/// (which itself may not exist). Never climbs above the repo root, so a
/// non-git directory stays scoped to itself rather than the whole filesystem.
fn search_root(target: &Path) -> PathBuf {
    let abs = if target.is_absolute() {
        target.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(target)
    };

    // The target may not exist; climb to the nearest existing ancestor.
    let mut base = abs;
    while !base.exists() {
        match base.parent() {
            Some(parent) => base = parent.to_path_buf(),
            None => break,
        }
    }
    if base.is_file() {
        if let Some(parent) = base.parent() {
            base = parent.to_path_buf();
        }
    }

    // Prefer the enclosing git repo root (bounded climb).
    let mut probe: &Path = base.as_path();
    for _ in 0..40 {
        if probe.join(".git").exists() {
            return probe.to_path_buf();
        }
        match probe.parent() {
            Some(parent) => probe = parent,
            None => break,
        }
    }
    base
}

/// Returns the longest trimmed line in `s` that is distinctive enough
/// (`>= MIN_NEEDLE_LEN` chars) to search for across files, or `None` if every
/// line is too trivial to yield a meaningful match.
fn distinctive_line(s: &str) -> Option<&str> {
    s.lines()
        .map(str::trim)
        .filter(|l| l.len() >= MIN_NEEDLE_LEN)
        .max_by_key(|l| l.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Anchors a tempdir as a repo root so walks never escape into the real FS.
    fn repo(dir: &Path) {
        fs::create_dir_all(dir.join(".git")).unwrap();
    }

    #[test]
    fn moved_hint_points_to_relocated_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        repo(root);
        fs::create_dir_all(root.join("src/new")).unwrap();
        fs::write(root.join("src/new/widget.rs"), "fn widget() {}\n").unwrap();

        // Agent targets the old (non-existent) location.
        let hint = moved_or_deleted_hint(&root.join("src/old/widget.rs"));
        assert!(hint.contains("same-named file was found"), "got: {hint}");
        assert!(hint.contains("widget.rs"), "got: {hint}");
    }

    #[test]
    fn moved_hint_reports_truly_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        repo(root);
        fs::write(root.join("present.rs"), "fn present() {}\n").unwrap();

        let hint = moved_or_deleted_hint(&root.join("totally_unique_zzz.rs"));
        assert!(hint.contains("No file named"), "got: {hint}");
        assert!(hint.contains("totally_unique_zzz.rs"), "got: {hint}");
    }

    #[test]
    fn cross_file_hint_finds_symbol_in_other_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        repo(root);
        let target = root.join("a.rs");
        fs::write(&target, "fn unrelated_in_a() {}\n").unwrap();
        fs::write(root.join("b.rs"), "pub fn the_distinctive_function() {}\n").unwrap();

        let hint = cross_file_hint(&target, "pub fn the_distinctive_function() {}");
        assert!(hint.contains("matching line exists in"), "got: {hint}");
        assert!(hint.contains("b.rs"), "got: {hint}");
        assert!(
            !hint.contains("a.rs"),
            "must not point back at target: {hint}"
        );
    }

    #[test]
    fn cross_file_hint_empty_when_nowhere() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        repo(root);
        let target = root.join("a.rs");
        fs::write(&target, "fn only_here() {}\n").unwrap();

        let hint = cross_file_hint(&target, "fn nonexistent_symbol_xyzzy() {}");
        assert!(hint.is_empty(), "got: {hint}");
    }

    #[test]
    fn distinctive_line_skips_trivial_lines() {
        assert_eq!(distinctive_line("}\n{\n  )"), None);
        assert_eq!(
            distinctive_line("}\nfn meaningful_name() {\n}"),
            Some("fn meaningful_name() {")
        );
    }
}
