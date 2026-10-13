// SPDX-License-Identifier: Apache-2.0
//! "Did you mean …?" for a path that does not exist.
//!
//! A missing file is the most frequent tool failure in the field, and the
//! usual causes are mechanical: a typo in the file name, a wrong extension, or
//! a path written relative to the wrong directory (`src/x.rs` for
//! `rust/src/x.rs`). Naming the existing file turns the agent's next call into
//! the right one instead of a `ctx_tree` round trip.
//!
//! Bounded on purpose: one directory listing for the name check, and a capped,
//! gitignore-respecting walk below the nearest existing ancestor only when that
//! ancestor is a safe scan root (never `~`, `/` or a TCC-protected folder).

use std::path::{Path, PathBuf};

/// Entries read from the parent directory for the name check.
const MAX_SIBLINGS: usize = 2_000;
/// Directory entries visited while looking for the same file elsewhere.
const MAX_WALK_ENTRIES: usize = 20_000;
const MAX_WALK_DEPTH: usize = 12;
const MAX_SUGGESTIONS: usize = 3;

/// Existing paths the agent most likely meant by the missing `path`, best
/// first. Empty when nothing is close enough to suggest with confidence.
pub(crate) fn suggestions(path: &Path) -> Vec<PathBuf> {
    if path.exists() {
        return Vec::new();
    }
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return Vec::new();
    };
    if let Some(parent) = path.parent().filter(|p| p.is_dir()) {
        return similar_sibling(parent, name).into_iter().collect();
    }
    nearest_existing_ancestor(path)
        .map(|base| same_tail_below(&base, path))
        .unwrap_or_default()
}

/// The one file `path` unambiguously names, for a read that may follow it
/// without asking: the same name differing only in case in the same
/// directory, or the whole relative path found under exactly one other
/// directory (`src/core/x.rs` for `rust/src/core/x.rs`). Edit-distance and
/// other-extension guesses (`main.ts` → `main.rs`) are a different file and
/// stay suggestions only.
pub(crate) fn unambiguous_match(path: &Path) -> Option<PathBuf> {
    if path.exists() {
        return None;
    }
    let name = path.file_name()?.to_str()?;
    if let Some(parent) = path.parent().filter(|p| p.is_dir()) {
        let lowered = name.to_lowercase();
        let mut same_name = std::fs::read_dir(parent)
            .ok()?
            .filter_map(Result::ok)
            .take(MAX_SIBLINGS)
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| n.to_lowercase() == lowered);
        let only = same_name.next()?;
        return same_name.next().is_none().then(|| parent.join(only));
    }
    let base = nearest_existing_ancestor(path)?;
    let (ranked, tail_len) = ranked_tail_matches(&base, path);
    match ranked.as_slice() {
        [(shared, only)] if *shared == tail_len => Some(only.clone()),
        [(shared, best), (next, _), ..] if *shared == tail_len && next < shared => {
            Some(best.clone())
        }
        _ => None,
    }
}

/// The suggestions as a one-line suffix for an error message, or "".
pub(crate) fn hint(path: &Path) -> String {
    let found = suggestions(path);
    match found.as_slice() {
        [] => String::new(),
        [one] => format!(" — did you mean {}?", one.display()),
        many => format!(
            " — did you mean one of: {}?",
            many.iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// A sibling whose name is within a small edit distance of `name`, or the same
/// stem with another extension (`main.ts` for `main.js`).
fn similar_sibling(parent: &Path, name: &str) -> Option<PathBuf> {
    let names: Vec<String> = std::fs::read_dir(parent)
        .ok()?
        .filter_map(Result::ok)
        .take(MAX_SIBLINGS)
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    let lowered = name.to_lowercase();
    if let Some(exact_case) = names.iter().find(|n| n.to_lowercase() == lowered) {
        return Some(parent.join(exact_case));
    }
    if let Some(close) = crate::core::levenshtein::closest(name, names.iter().map(String::as_str)) {
        return Some(parent.join(close));
    }
    let stem = Path::new(name).file_stem()?.to_str()?;
    names
        .iter()
        .find(|n| Path::new(n).file_stem().and_then(|s| s.to_str()) == Some(stem))
        .map(|n| parent.join(n))
}

fn nearest_existing_ancestor(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .skip(1)
        .take(32)
        .find(|a| a.is_dir())
        .map(Path::to_path_buf)
}

/// Files below `base` whose path ends with the missing part of `path` — the
/// same file reached from a different directory. Falls back to the file name
/// alone when no longer tail matches.
fn same_tail_below(base: &Path, path: &Path) -> Vec<PathBuf> {
    ranked_tail_matches(base, path)
        .0
        .into_iter()
        .take(MAX_SUGGESTIONS)
        .map(|(_, p)| p)
        .collect()
}

/// Every file below `base` named like `path`, best first, with how many
/// trailing components it shares with the missing part of `path`; plus that
/// missing part's length (a full match shares all of it).
fn ranked_tail_matches(base: &Path, path: &Path) -> (Vec<(usize, PathBuf)>, usize) {
    if !crate::core::graph_index::is_safe_scan_root_public(&base.to_string_lossy()) {
        return (Vec::new(), 0);
    }
    let Ok(missing) = path.strip_prefix(base) else {
        return (Vec::new(), 0);
    };
    let tail: Vec<_> = missing.components().collect();
    let Some(name) = tail.last().map(|c| c.as_os_str().to_owned()) else {
        return (Vec::new(), 0);
    };

    // Matches ranked by how many trailing components they share with `path`.
    let mut ranked: Vec<(usize, PathBuf)> = Vec::new();
    let walker = ignore::WalkBuilder::new(base)
        .max_depth(Some(MAX_WALK_DEPTH))
        .hidden(true)
        .build();
    for entry in walker.filter_map(Result::ok).take(MAX_WALK_ENTRIES) {
        let candidate = entry.path();
        if candidate.file_name() != Some(name.as_os_str()) {
            continue;
        }
        let shared = candidate
            .components()
            .rev()
            .zip(tail.iter().rev())
            .take_while(|(a, b)| a == *b)
            .count();
        ranked.push((shared, candidate.to_path_buf()));
    }
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    (ranked, tail.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("rust/src/core")).unwrap();
        std::fs::write(root.join("rust/src/core/config.rs"), "").unwrap();
        std::fs::write(root.join("rust/src/main.rs"), "").unwrap();
        dir
    }

    #[test]
    fn typo_in_file_name_suggests_the_sibling() {
        let dir = project();
        let wanted = dir.path().join("rust/src/core/confg.rs");
        assert_eq!(
            suggestions(&wanted),
            vec![dir.path().join("rust/src/core/config.rs")]
        );
    }

    #[test]
    fn wrong_extension_suggests_the_same_stem() {
        let dir = project();
        let wanted = dir.path().join("rust/src/main.ts");
        assert_eq!(
            suggestions(&wanted),
            vec![dir.path().join("rust/src/main.rs")]
        );
    }

    #[test]
    fn wrong_directory_finds_the_file_by_its_tail() {
        let dir = project();
        let wanted = dir.path().join("src/core/config.rs");
        let found = suggestions(&wanted);
        assert_eq!(
            found.first(),
            Some(&dir.path().join("rust/src/core/config.rs"))
        );
        assert!(hint(&wanted).contains("did you mean"), "{}", hint(&wanted));
    }

    #[test]
    fn unambiguous_match_follows_case_and_wrong_prefix_only() {
        let dir = project();
        let root = dir.path();
        // Same name, other case, same directory — only observable on a
        // case-sensitive file system (APFS/NTFS defaults open it directly).
        let other_case = root.join("rust/src/core/Config.rs");
        if !other_case.exists() {
            assert_eq!(
                unambiguous_match(&other_case),
                Some(root.join("rust/src/core/config.rs"))
            );
        }
        // Whole relative path found under exactly one other directory.
        assert_eq!(
            unambiguous_match(&root.join("src/core/config.rs")),
            Some(root.join("rust/src/core/config.rs"))
        );
        // A typo or another extension names a different file: no substitute.
        assert_eq!(
            unambiguous_match(&root.join("rust/src/core/confg.rs")),
            None
        );
        assert_eq!(unambiguous_match(&root.join("rust/src/main.ts")), None);
        // Existing paths are never substituted.
        assert_eq!(unambiguous_match(&root.join("rust/src/main.rs")), None);
    }

    #[test]
    fn unambiguous_match_refuses_two_equally_good_candidates() {
        let dir = project();
        let root = dir.path();
        std::fs::create_dir_all(root.join("other/src/core")).unwrap();
        std::fs::write(root.join("other/src/core/config.rs"), "").unwrap();
        assert_eq!(unambiguous_match(&root.join("src/core/config.rs")), None);
        assert_eq!(suggestions(&root.join("src/core/config.rs")).len(), 2);
    }

    #[test]
    fn nothing_close_suggests_nothing() {
        let dir = project();
        assert!(suggestions(&dir.path().join("rust/src/zzzzzzzz.py")).is_empty());
        assert_eq!(hint(&dir.path().join("nope/void.md")), "");
    }
}
