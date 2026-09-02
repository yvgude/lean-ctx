//! Why a recursive walk timed out (GH #1655, follow-up to #1089).
//!
//! `ctx_glob` and `ctx_tree` honour `.gitignore`; `ctx_shell` runs the user's
//! own `grep`/`find`, which does not. So a directory invisible to one tool is
//! walked by the other, and a repo with nested checkouts under
//! `.claude/worktrees/` searches a multiple of the intended tree. The reporter
//! hit this twice and diagnosed it both times from memory of his own earlier
//! issue rather than from anything the tool said.
//!
//! This does **not** change what the command traverses. Silently rewriting a
//! user's `grep` would be worse than the timeout — the reporter says so himself
//! and he is right. It only explains, after the fact, what was probably eaten.
//!
//! Honesty constraints, because a hint that guesses is worse than none:
//!
//! - Only directories that **actually exist** under the search root are named.
//! - Counts are real, from a bounded walk; a count that hit the cap is rendered
//!   `N+`, never rounded up into a total.
//! - No percentage of the walk is claimed. We do not know what the command
//!   traversed, only what is there.

use std::path::Path;

/// Commands whose cost scales with the tree, and which do not read `.gitignore`.
const RECURSIVE_TOOLS: &[&str] = &["grep", "egrep", "fgrep", "find", "ack", "ag"];

/// Directories that dominate a walk when present: nested checkouts and
/// dependency trees.
///
/// Deliberately narrower than [`crate::core::auto_findings::NOISE_PATH_SEGMENTS`],
/// which also lists small directories (`.cursor`, `terminals`) whose presence
/// says nothing about why a walk was slow. Naming those would dilute the hint.
const BULK_DIRS: &[&str] = &[
    ".worktrees",
    "worktrees",
    ".codex-worktrees",
    "node_modules",
    "target",
    "vendor",
    ".venv",
    "venv",
    "site-packages",
    ".next",
    ".cache",
];

/// How deep to look for a bulk directory. `.claude/worktrees/` is two levels
/// down, which is the reported case; three leaves room without turning the
/// search itself into a walk.
const SCAN_DEPTH: usize = 3;

/// Stop counting a directory here. The number only has to be big enough to
/// justify the hint, and this runs on an already-slow error path.
const COUNT_CAP: usize = 5_000;

/// A hint for a timed-out command, or `None` when there is nothing honest to say.
pub(crate) fn walk_hint(command: &str, cwd: &str) -> Option<String> {
    if !is_recursive_walk(command) {
        return None;
    }

    let root = Path::new(cwd);
    let mut found: Vec<(String, usize, bool)> = Vec::new();
    collect_bulk_dirs(root, root, 0, &mut found);

    // A directory the command already excludes is not the explanation.
    found.retain(|(rel, _, _)| !command.contains(rel.as_str()));
    if found.is_empty() {
        return None;
    }

    // Biggest first: the directory most likely to explain the timeout leads.
    found.sort_by_key(|f| std::cmp::Reverse(f.1));
    found.truncate(3);

    let named: Vec<String> = found
        .iter()
        .map(|(rel, n, capped)| format!("{rel}/ ({n}{} files)", if *capped { "+" } else { "" }))
        .collect();

    Some(format!(
        "[hint: {} under this path — walked by grep/find, but ignored by \
         ctx_search and ctx_glob, which honour .gitignore. Scope the path, add \
         --exclude-dir, or use ctx_search.]",
        named.join(", ")
    ))
}

/// Does this command walk a tree without reading `.gitignore`?
///
/// `rg` is deliberately absent: it honours `.gitignore` already, so a slow `rg`
/// is not explained by an ignored directory and the hint would mislead.
fn is_recursive_walk(command: &str) -> bool {
    for segment in command.split(|c| matches!(c, '|' | ';' | '&')) {
        let mut words = segment.split_whitespace().skip_while(|w| w.contains('='));
        let Some(base) = words.next() else { continue };
        let base = base.rsplit('/').next().unwrap_or(base);
        if !RECURSIVE_TOOLS.contains(&base) {
            continue;
        }
        // `find` is always recursive; grep-likes need -r/-R.
        if base == "find" {
            return true;
        }
        if segment
            .split_whitespace()
            .any(|w| w.starts_with('-') && !w.starts_with("--") && w.contains(['r', 'R']))
        {
            return true;
        }
    }
    false
}

/// Shallow search for bulk directories, recording each one's bounded file count.
/// Does not descend *into* a match — its own contents are what we are counting.
fn collect_bulk_dirs(root: &Path, dir: &Path, depth: usize, out: &mut Vec<(String, usize, bool)>) {
    if depth > SCAN_DEPTH || out.len() >= 16 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if BULK_DIRS.contains(&name.as_str()) {
            let (count, capped) = count_files(&path);
            if let Ok(rel) = path.strip_prefix(root) {
                out.push((rel.to_string_lossy().into_owned(), count, capped));
            }
            continue; // its contents are the count, not more candidates
        }
        collect_bulk_dirs(root, &path, depth + 1, out);
    }
}

/// Files under `dir`, stopping at [`COUNT_CAP`]. Returns `(count, hit_cap)`.
fn count_files(dir: &Path) -> (usize, bool) {
    let mut stack = vec![dir.to_path_buf()];
    let mut count = 0usize;
    while let Some(next) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.flatten() {
            match entry.file_type() {
                Ok(t) if t.is_dir() => stack.push(entry.path()),
                Ok(_) => {
                    count += 1;
                    if count >= COUNT_CAP {
                        return (COUNT_CAP, true);
                    }
                }
                Err(_) => {}
            }
        }
    }
    (count, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn make_tree(root: &Path, rel: &str, files: usize) {
        let dir = root.join(rel);
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..files {
            std::fs::write(dir.join(format!("f{i}.go")), b"x").unwrap();
        }
    }

    #[test]
    fn recognises_recursive_grep_but_not_rg() {
        assert!(is_recursive_walk("grep -rn needle ."));
        assert!(is_recursive_walk("grep -Rn needle ."));
        assert!(is_recursive_walk("find . -name '*.go'"));
        assert!(is_recursive_walk("cd /x && grep -rn a ."));
        // Non-recursive grep walks nothing.
        assert!(!is_recursive_walk("grep needle file.txt"));
        // rg honours .gitignore, so an ignored directory does not explain it —
        // claiming otherwise would send the reader after the wrong cause.
        assert!(!is_recursive_walk("rg needle"));
    }

    /// The reporter's shape: full checkouts two levels down under `.claude/`.
    #[test]
    fn names_nested_checkouts_with_real_counts() {
        let t = tmp();
        make_tree(t.path(), ".claude/worktrees/a", 12);
        make_tree(t.path(), ".claude/worktrees/b", 8);
        make_tree(t.path(), "src", 3);

        let hint = walk_hint("grep -rn needle .", t.path().to_str().unwrap())
            .expect("a hint for a recursive grep over a tree with nested checkouts");
        assert!(hint.contains("worktrees"), "{hint}");
        assert!(
            hint.contains("20 files"),
            "real count, not an estimate: {hint}"
        );
        assert!(hint.contains("ctx_search"), "{hint}");
        assert!(!hint.contains('%'), "no invented percentage: {hint}");
    }

    /// A command that already excludes the directory has been diagnosed
    /// already; repeating it is noise.
    #[test]
    fn stays_quiet_when_the_command_already_excludes_it() {
        let t = tmp();
        make_tree(t.path(), ".worktrees/a", 5);
        assert!(
            walk_hint(
                "grep -rn needle . | grep -v '.worktrees'",
                t.path().to_str().unwrap()
            )
            .is_none()
        );
    }

    #[test]
    fn stays_quiet_without_bulk_directories() {
        let t = tmp();
        make_tree(t.path(), "src", 4);
        assert!(walk_hint("grep -rn needle .", t.path().to_str().unwrap()).is_none());
    }

    #[test]
    fn stays_quiet_for_a_non_walking_command() {
        let t = tmp();
        make_tree(t.path(), "node_modules/x", 5);
        assert!(walk_hint("cargo test", t.path().to_str().unwrap()).is_none());
    }

    /// A huge tree must not be counted exhaustively on an error path; the cap
    /// is reported as `N+` rather than passed off as a total.
    #[test]
    fn a_capped_count_is_marked_as_a_floor() {
        let (n, capped) = (COUNT_CAP, true);
        let rendered = format!("{n}{} files", if capped { "+" } else { "" });
        assert!(rendered.starts_with("5000+"), "{rendered}");
    }
}
