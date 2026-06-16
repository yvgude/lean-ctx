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
    let Some(root) = search_root(target) else {
        // Outside any git repo: report the path as missing without scanning a
        // foreign tree (system temp, `$HOME`, `/`) we have no business walking.
        return format!(
            "\nNo file at `{}` — it may have been deleted, or the path/name is wrong \
             (use create=true to create it).",
            target.display()
        );
    };

    let mut hits: Vec<PathBuf> = Vec::new();
    let mut scanned = 0usize;
    for entry in walk(&root) {
        // Only ever consider regular files — never a dir, symlink, FIFO, socket
        // or device. Suggesting a special file is wrong, and scanning one risks
        // a blocking open (see `cross_file_hint`).
        if entry.file_type().is_none_or(|ft| !ft.is_file()) {
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
    let Some(root) = search_root(target) else {
        return String::new();
    };
    let target_canon = std::fs::canonicalize(target).ok();

    let mut hits: Vec<PathBuf> = Vec::new();
    let mut scanned = 0usize;
    for entry in walk(&root) {
        // Only ever read regular files. A FIFO/socket/device would make the
        // `read_to_string` below block forever (the #331 CI hang); a symlink
        // could redirect the read outside the repo. Skip anything non-regular.
        if entry.file_type().is_none_or(|ft| !ft.is_file()) {
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
        if let Ok(content) = std::fs::read_to_string(path)
            && content.contains(needle)
        {
            hits.push(path.to_path_buf());
            if hits.len() >= MAX_HITS {
                break;
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
fn walk(root: &Path) -> impl Iterator<Item = ignore::DirEntry> + use<> {
    WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .require_git(false)
        .max_depth(Some(MAX_WALK_DEPTH))
        .filter_entry(crate::core::walk_filter::keep_entry)
        .build()
        .filter_map(Result::ok)
}

/// Resolves the enclosing git repository root that scopes the recovery search.
///
/// Returns `None` when `target` is not inside a git repo. Outside a repo a
/// "where did it move?" walk is meaningless and would scan an unrelated tree
/// (the system temp dir, `$HOME`, or `/`) — potentially reading foreign or
/// blocking special files — so callers skip the hint entirely instead. This
/// also keeps the scan bounded to the project the agent is actually editing.
fn search_root(target: &Path) -> Option<PathBuf> {
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
        base = base.parent()?.to_path_buf();
    }
    if base.is_file() {
        base = base.parent()?.to_path_buf();
    }

    // Only ever search inside an enclosing git repo (bounded climb).
    let mut probe: &Path = base.as_path();
    for _ in 0..40 {
        if probe.join(".git").exists() {
            return Some(probe.to_path_buf());
        }
        probe = probe.parent()?;
    }
    None
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

    /// A bare tempfile lives in the shared system temp dir with no enclosing
    /// `.git`. Recovery must NOT walk that foreign tree (it can hold unrelated
    /// or blocking files): the cross-file hint is empty and the moved hint
    /// stays generic without scanning.
    #[test]
    fn outside_a_repo_does_not_scan() {
        let f = tempfile::NamedTempFile::new().unwrap();
        assert!(
            cross_file_hint(f.path(), "fn a_distinctive_needle_line() {}").is_empty(),
            "no repo => no cross-file scan"
        );

        let missing = f.path().with_file_name("definitely_missing_zzz_q9.rs");
        let hint = moved_or_deleted_hint(&missing);
        assert!(hint.contains("No file at"), "got: {hint}");
    }

    /// Regression for the #331 CI hang: a FIFO (or any non-regular file) in the
    /// repo must be skipped, never `read_to_string`-d, or the walk blocks
    /// forever. Runs the search on a worker thread with a hard timeout so a
    /// regression fails fast instead of re-introducing a 90-minute hang.
    #[cfg(unix)]
    #[test]
    fn cross_file_hint_skips_blocking_fifo() {
        use std::ffi::CString;
        use std::sync::mpsc;
        use std::time::Duration;

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        repo(&root);
        let target = root.join("a.rs");
        fs::write(&target, "fn unrelated_a() {}\n").unwrap();
        fs::write(root.join("b.rs"), "pub fn the_real_target_symbol() {}\n").unwrap();
        // A FIFO with no writer: `read_to_string` on it blocks until a writer
        // appears — which never happens — unless the walker skips it.
        let fifo = root.join("blocking.pipe");
        let c = CString::new(fifo.to_str().unwrap()).unwrap();
        assert_eq!(
            // SAFETY: `c` is a live CString providing a valid NUL-terminated
            // path pointer for the duration of the call.
            unsafe { libc::mkfifo(c.as_ptr(), 0o644) },
            0,
            "mkfifo failed"
        );

        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(cross_file_hint(
                &target,
                "pub fn the_real_target_symbol() {}",
            ));
        });
        let hint = rx.recv_timeout(Duration::from_secs(10)).expect(
            "cross_file_hint hung on a FIFO — non-regular files must be skipped (#331 regression)",
        );
        assert!(hint.contains("b.rs"), "got: {hint}");
        assert!(
            !hint.contains("blocking.pipe"),
            "must skip the FIFO: {hint}"
        );
    }
}
