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

    // #1662: `cd repo && grep -rn … .` walks a tree the call's own cwd never
    // named. Scanning the call directory would then look at the wrong place and
    // report nothing, which is indistinguishable from "this walk is fine".
    let moved = crate::core::command_cwd::final_cwd(command, Some(Path::new(cwd)));
    let base: &Path = match moved.as_deref() {
        Some(dir) => dir,
        // A `cd` this cannot resolve means the directory walked is unknown;
        // naming directories from somewhere else would be a guess.
        None if command.split_whitespace().any(|w| w == "cd") => return None,
        None => Path::new(cwd),
    };

    // Scan what the command actually walks. Now that this runs for every
    // recursive walk rather than only on timeout (#1662), scanning the starting
    // directory regardless of scope would announce `node_modules/` for a
    // `grep -r pattern src/` that never goes near it — a confident hint about a
    // directory the command never entered.
    let mut found: Vec<(String, usize, bool)> = Vec::new();
    for root in walk_roots(command, base) {
        collect_bulk_dirs(&root, &root, 0, &mut found);
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found.dedup_by(|a, b| a.0 == b.0);

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

/// The directories a recursive command was pointed at.
///
/// For a grep-like the first non-flag word is the pattern and the rest are
/// paths; `find` takes its paths first, before any predicate. When no path is
/// given, the walk starts where the command runs.
fn walk_roots(command: &str, base: &Path) -> Vec<std::path::PathBuf> {
    let mut roots = Vec::new();
    for segment in command.split(|c| matches!(c, '|' | ';' | '&')) {
        let mut words = segment.split_whitespace().skip_while(|w| w.contains('='));
        let Some(tool) = words.next() else { continue };
        let tool = tool.rsplit('/').next().unwrap_or(tool);
        if !RECURSIVE_TOOLS.contains(&tool) {
            continue;
        }
        let is_find = tool == "find";
        let mut operands: Vec<&str> = Vec::new();
        for word in words {
            if word.starts_with('-') {
                // A `find` predicate takes arguments (`-name '*.go'`), and its
                // paths are already behind us, so stop at the first one.
                if is_find {
                    break;
                }
                continue;
            }
            operands.push(word);
        }
        let paths = if is_find || operands.is_empty() {
            operands.as_slice()
        } else {
            &operands[1..]
        };
        for path in paths {
            let trimmed = path.trim_matches(['"', '\'']);
            // A path the shell would have to expand is not one to scan.
            if trimmed.is_empty() || trimmed.contains('$') {
                continue;
            }
            let candidate = Path::new(trimmed);
            roots.push(if candidate.is_absolute() || trimmed.starts_with('/') {
                candidate.to_path_buf()
            } else {
                crate::core::command_cwd::join_in(base, trimmed)
            });
        }
        if paths.is_empty() {
            roots.push(base.to_path_buf());
        }
    }
    if roots.is_empty() {
        roots.push(base.to_path_buf());
    }
    roots
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

#[cfg(test)]
mod gh1662 {
    use super::*;

    /// A repo with the two directories that dominate a `grep -r`.
    fn repo_with_bulk() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        for bulk in ["node_modules", ".git"] {
            let sub = dir.path().join(bulk);
            std::fs::create_dir_all(&sub).expect("mkdir");
            for i in 0..40 {
                std::fs::write(sub.join(format!("f{i}.js")), "x").expect("write");
            }
        }
        std::fs::write(dir.path().join("main.go"), "package main").expect("write");
        dir
    }

    /// The reported shape: a recursive grep piped into `head`. `head` closes
    /// early, so the visible output arrives fast while the walk runs on.
    #[test]
    fn a_piped_recursive_grep_is_still_a_recursive_walk() {
        assert!(is_recursive_walk(
            "grep -rn \"spine-go/mocks\" --include=*.go . | head"
        ));
    }

    #[test]
    fn a_piped_recursive_grep_gets_the_hint() {
        let repo = repo_with_bulk();
        let hint = walk_hint(
            "grep -rn \"mocks\" --include=*.go . | head",
            &repo.path().to_string_lossy(),
        );
        let hint = hint.expect("bulk directories under the walked path must be named");
        assert!(hint.contains("node_modules/"), "{hint}");
        assert!(hint.contains("ctx_search"), "{hint}");
    }

    /// The walked tree is the one the command `cd`s into, not the call's own.
    #[test]
    fn the_hint_follows_a_leading_cd() {
        let repo = repo_with_bulk();
        let elsewhere = tempfile::tempdir().expect("tempdir");

        let command = format!(
            "cd {} && grep -rn \"mocks\" --include=*.go . | head",
            repo.path().display()
        );
        let hint = walk_hint(&command, &elsewhere.path().to_string_lossy());
        assert!(
            hint.is_some_and(|h| h.contains("node_modules/")),
            "the scan must follow the cd, not the call directory"
        );
    }

    /// A `cd` into a directory this cannot resolve leaves the walked tree
    /// unknown — better silent than naming directories from somewhere else.
    #[test]
    fn an_unresolvable_cd_stays_silent() {
        let repo = repo_with_bulk();
        let hint = walk_hint(
            "cd \"$REPO\" && grep -rn x . | head",
            &repo.path().to_string_lossy(),
        );
        assert!(hint.is_none(), "must not guess: {hint:?}");
    }
}

#[cfg(test)]
mod gh1662_scope {
    use super::*;

    fn repo_with_bulk() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let nm = dir.path().join("node_modules");
        std::fs::create_dir_all(&nm).expect("mkdir");
        for i in 0..40 {
            std::fs::write(nm.join(format!("f{i}.js")), "x").expect("write");
        }
        std::fs::create_dir_all(dir.path().join("src")).expect("mkdir");
        std::fs::write(dir.path().join("src/main.go"), "package main").expect("write");
        dir
    }

    /// Now that the hint fires for every recursive walk and not only on
    /// timeout, it must not announce a directory the command never enters.
    #[test]
    fn a_scoped_walk_is_not_blamed_on_a_directory_outside_its_scope() {
        let repo = repo_with_bulk();
        let cwd = repo.path().to_string_lossy();

        assert!(
            walk_hint("grep -rn pattern src/", &cwd).is_none(),
            "src/ holds no bulk directory"
        );
        assert!(
            walk_hint("grep -rn pattern .", &cwd).is_some(),
            "a walk rooted at the repo does hit node_modules"
        );
    }

    #[test]
    fn find_takes_its_paths_before_the_predicates() {
        let repo = repo_with_bulk();
        let cwd = repo.path().to_string_lossy();

        assert!(walk_hint("find src -name '*.go'", &cwd).is_none());
        assert!(walk_hint("find . -name '*.go'", &cwd).is_some());
    }

    /// An absolute path argument is scanned as given, not joined onto the cwd.
    #[test]
    fn an_absolute_path_argument_is_used_as_is() {
        let repo = repo_with_bulk();
        let elsewhere = tempfile::tempdir().expect("tempdir");
        let command = format!("grep -rn pattern {}", repo.path().display());

        assert!(walk_hint(&command, &elsewhere.path().to_string_lossy()).is_some());
    }

    /// A path the shell would expand is not something to scan; the command's
    /// own directory remains the honest fallback.
    #[test]
    fn an_unexpandable_path_falls_back_to_the_command_directory() {
        let repo = repo_with_bulk();
        let cwd = repo.path().to_string_lossy();
        assert!(walk_hint("grep -rn pattern \"$DIR\"", &cwd).is_some());
    }
}

#[cfg(test)]
mod gh1680 {
    use super::*;

    /// The reporter's case: `grep -rn PATTERN /abs/path/outside/file.go` run
    /// from a project root that contains `node_modules/`. The hint named three
    /// directories under the cwd and asserted they were "walked by grep/find" —
    /// grep read exactly one file, and none of them are under it.
    #[test]
    fn an_explicit_path_outside_the_root_is_not_blamed_on_the_cwd() {
        let repo = tempfile::tempdir().expect("tempdir");
        let bulk = repo.path().join("node_modules");
        std::fs::create_dir_all(&bulk).expect("mkdir");
        for i in 0..40 {
            std::fs::write(bulk.join(format!("f{i}.js")), "x").expect("write");
        }

        let elsewhere = tempfile::tempdir().expect("tempdir");
        let target = elsewhere.path().join("types.go");
        std::fs::write(&target, "package main").expect("write");

        let command = format!("grep -rn \"EvConnected\" {}", target.display());
        let hint = walk_hint(&command, &repo.path().to_string_lossy());

        assert!(
            hint.is_none(),
            "grep read one file outside the root; nothing under the cwd was walked: {hint:?}"
        );
    }

    /// The same command rooted at the cwd still gets the hint — the fix must
    /// not silence the case the hint exists for.
    #[test]
    fn the_intended_case_still_fires() {
        let repo = tempfile::tempdir().expect("tempdir");
        let bulk = repo.path().join("node_modules");
        std::fs::create_dir_all(&bulk).expect("mkdir");
        for i in 0..40 {
            std::fs::write(bulk.join(format!("f{i}.js")), "x").expect("write");
        }
        std::fs::write(repo.path().join("main.go"), "package main").expect("write");

        let hint = walk_hint(
            "grep -rn \"EvConnected\" --include=*.go .",
            &repo.path().to_string_lossy(),
        );
        assert!(
            hint.is_some_and(|h| h.contains("node_modules/")),
            "a walk rooted at the repo does traverse node_modules"
        );
    }

    /// A single explicit file is not a tree, so even under the cwd the advice
    /// ("scope the path", "add --exclude-dir") would have nothing to act on.
    #[test]
    fn a_single_file_operand_never_produces_a_hint() {
        let repo = tempfile::tempdir().expect("tempdir");
        let bulk = repo.path().join("node_modules");
        std::fs::create_dir_all(&bulk).expect("mkdir");
        for i in 0..40 {
            std::fs::write(bulk.join(format!("f{i}.js")), "x").expect("write");
        }
        let file = repo.path().join("main.go");
        std::fs::write(&file, "package main").expect("write");

        let hint = walk_hint(
            &format!("grep -rn x {}", file.display()),
            &repo.path().to_string_lossy(),
        );
        assert!(hint.is_none(), "{hint:?}");
    }
}
