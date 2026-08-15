use std::path::Path;

use ignore::WalkBuilder;

use crate::core::protocol;
use crate::core::tokens::count_tokens;

/// Hard ceiling on the number of files returned from a single glob search,
/// independent of the caller-supplied `max_results`.
const MAX_RESULTS: usize = 500;

/// Finds files matching a glob `pattern` under `dir` with compressed output.
///
/// Unlike `ctx_search` which matches file *content*, this matches file *paths*.
/// Uses the `ignore` crate for gitignore-aware, hidden-aware walking and matches
/// against the standard `glob` crate's pattern syntax (`*.rs`, `**/*.ts`, …).
///
/// The walk is ordered by path (`sort_by_file_path`) so that — even when the
/// result set is truncated to `max_results` — the *set* of returned files, not
/// just their printed order, is deterministic across runs.
///
/// Returns `(output, original_tokens)`. On error the output starts with
/// `"ERROR:"` and `original_tokens` is `0`.
pub fn handle(
    pattern: &str,
    dir: &str,
    respect_gitignore: bool,
    allow_secret_paths: bool,
    max_results: usize,
) -> (String, usize) {
    let requested_root = Path::new(dir);
    let walk_root = crate::core::walk_filter::explicit_walk_root(requested_root);
    let root = walk_root.as_path();
    if !root.exists() {
        return (format!("ERROR: {dir} does not exist"), 0);
    }
    if !root.is_dir() {
        return (format!("ERROR: {dir} is not a directory"), 0);
    }
    // Broad-root guard (#356 class): with cwd == $HOME a defaulted `path`
    // would walk the whole home dir and trip macOS TCC privacy prompts.
    if let Some(err) = crate::tools::walk_guard::deny_unsafe_walk_root(dir) {
        return (err, 0);
    }

    let max = max_results.min(MAX_RESULTS);

    // Support both simple (`*.rs`) and recursive (`**/*.ts`) patterns.
    let glob_matcher = match glob::Pattern::new(pattern) {
        Ok(m) => m,
        Err(e) => return (format!("ERROR: invalid glob pattern '{pattern}': {e}"), 0),
    };

    let mut matches = Vec::new();
    let mut files_walked = 0u32;

    // Vendor dirs (node_modules, …) follow the gitignore toggle: explicitly
    // disabling gitignore is the escape hatch to look inside them (#400).
    // GH #1431: hidden(false) — dot-directories like .agents/, .github/,
    // .cursor/ must be walkable; the user's glob pattern is the intent
    // filter, gitignore handles exclusion, is_secret_like guards secrets.
    let walker = WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(respect_gitignore)
        .git_global(respect_gitignore)
        .git_exclude(respect_gitignore)
        .require_git(false)
        .filter_entry(move |e| {
            if respect_gitignore {
                crate::core::walk_filter::keep_entry(e)
            } else {
                crate::core::cloud_files::keep_entry(e)
            }
        })
        .sort_by_file_path(std::path::Path::cmp)
        .build();

    for entry in walker.filter_map(std::result::Result::ok) {
        if matches.len() >= max {
            break;
        }

        // Skip directories; only files are matchable results.
        if entry.file_type().is_none_or(|ft| ft.is_dir()) {
            continue;
        }
        // Skip symlinks — never follow them out of the search root.
        if entry.file_type().is_some_and(|ft| ft.is_symlink()) {
            continue;
        }

        let path = entry.path();
        files_walked += 1;

        // Never surface secret-like paths (.env, keys, …) unless the active role
        // explicitly allows it.
        if !allow_secret_paths && crate::core::io_boundary::is_secret_like(path).is_some() {
            continue;
        }

        let rel_path = path.strip_prefix(root).unwrap_or(path);
        let rel_str = rel_path.to_string_lossy();

        if glob_matcher.matches(&rel_str) {
            let short_path =
                protocol::shorten_path_relative(&path.to_string_lossy(), &root.to_string_lossy());
            matches.push((short_path, path.to_string_lossy().to_string()));
        }
    }

    if matches.is_empty() {
        return (
            format!("0 files matched '{pattern}' in {files_walked} files walked"),
            0,
        );
    }

    matches.sort_by(|a, b| a.0.cmp(&b.0));

    let raw_listing = matches
        .iter()
        .map(|(_, abs)| abs.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let raw_tokens = count_tokens(&raw_listing);

    let output = group_paths_by_directory(&matches);
    let match_count = matches.len();

    let footer = format!("\n\n{match_count} files matched (walked {files_walked})");
    let full_output = format!("{output}{footer}");

    (full_output, raw_tokens)
}

fn group_paths_by_directory(matches: &[(String, String)]) -> String {
    use std::collections::BTreeMap;
    let mut groups: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (short, _) in matches {
        if let Some(pos) = short.rfind('/') {
            let dir = &short[..=pos];
            let file = &short[pos + 1..];
            groups.entry(dir).or_default().push(file);
        } else {
            groups.entry("").or_default().push(short);
        }
    }
    let mut out = String::new();
    for (dir, files) in &groups {
        if dir.is_empty() {
            for f in files {
                out.push_str(f);
                out.push('\n');
            }
        } else if files.len() == 1 {
            out.push_str(&format!("{}{}", dir, files[0]));
            out.push('\n');
        } else {
            out.push_str(&format!("{} {}", dir, files.join(", ")));
            out.push('\n');
        }
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_results_are_deterministically_ordered() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("b.txt"), "content").unwrap();
        std::fs::write(dir.path().join("a.txt"), "content").unwrap();
        std::fs::write(dir.path().join("c.rs"), "content").unwrap();

        let (out, _) = handle("*.txt", &dir.path().to_string_lossy(), true, true, 100);

        let lines: Vec<&str> = out
            .lines()
            .filter(|l| {
                std::path::Path::new(l)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("txt"))
            })
            .collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0] < lines[1], "results must be sorted: {lines:?}");
    }

    #[test]
    fn glob_refuses_home_directory_root() {
        // #356 class: never walk the whole home dir (macOS TCC prompts).
        let home = dirs::home_dir().expect("home dir in test env");
        let (out, tokens) = handle("*.txt", home.to_string_lossy().as_ref(), true, true, 10);
        assert!(
            out.starts_with("ERROR:") && out.contains("refusing to scan"),
            "home root must be refused: {out}"
        );
        assert_eq!(tokens, 0);
    }

    #[test]
    fn glob_skips_directories() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("subdir")).unwrap();
        std::fs::write(dir.path().join("file.txt"), "content").unwrap();

        let (out, _) = handle("**/*.txt", &dir.path().to_string_lossy(), true, true, 100);

        assert!(out.contains("file.txt"));
        assert!(!out.contains("subdir"));
    }

    #[test]
    fn glob_recursive_pattern_descends_subdirs() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("nested")).unwrap();
        std::fs::write(dir.path().join("nested").join("deep.rs"), "fn x() {}").unwrap();
        std::fs::write(dir.path().join("top.rs"), "fn y() {}").unwrap();

        let (out, _) = handle("**/*.rs", &dir.path().to_string_lossy(), true, true, 100);

        assert!(
            out.contains("deep.rs"),
            "recursive glob must descend: {out}"
        );
        assert!(out.contains("top.rs"));
    }

    #[test]
    fn glob_respects_gitignore() {
        let dir = tempfile::tempdir().unwrap();
        // The `ignore` crate only honours .gitignore inside a git repo (its
        // `require_git` default); mark the tempdir as a repo root so the test
        // exercises real-world behaviour without shelling out to `git`.
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join(".gitignore"), "ignored.rs\n").unwrap();
        std::fs::write(dir.path().join("ignored.rs"), "fn a() {}").unwrap();
        std::fs::write(dir.path().join("kept.rs"), "fn b() {}").unwrap();

        let (respected, _) = handle("**/*.rs", &dir.path().to_string_lossy(), true, true, 100);
        assert!(respected.contains("kept.rs"));
        assert!(
            !respected.contains("ignored.rs"),
            "gitignored file must be skipped: {respected}"
        );

        // With gitignore disabled, the ignored file reappears.
        let (unrespected, _) = handle("**/*.rs", &dir.path().to_string_lossy(), false, true, 100);
        assert!(unrespected.contains("ignored.rs"));
    }

    #[test]
    fn glob_invalid_pattern_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let (out, _) = handle("[invalid", &dir.path().to_string_lossy(), true, true, 100);

        assert!(out.starts_with("ERROR:"));
        assert!(out.contains("invalid glob pattern"));
    }

    #[test]
    fn glob_nonexistent_dir_returns_error() {
        let (out, _) = handle("*.txt", "/nonexistent/path", true, true, 100);

        assert!(out.starts_with("ERROR:"));
        assert!(out.contains("does not exist"));
    }

    #[test]
    fn glob_respects_max_results() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..10 {
            std::fs::write(dir.path().join(format!("file{i}.txt")), "content").unwrap();
        }

        let (out, _) = handle("*.txt", &dir.path().to_string_lossy(), true, true, 5);

        let file_lines: Vec<&str> = out
            .lines()
            .filter(|l| {
                std::path::Path::new(l)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("txt"))
            })
            .collect();
        assert!(file_lines.len() <= 5, "should respect max_results");
    }

    #[cfg(windows)]
    #[test]
    fn glob_walks_explicit_directory_reparse_root() {
        use std::os::windows::fs::symlink_dir;

        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("target");
        let link = tmp.path().join("junction");
        std::fs::create_dir_all(&target).expect("target");
        std::fs::write(target.join("visible.rs"), "fn visible() {}\n").expect("fixture");
        if symlink_dir(&target, &link).is_err() {
            return;
        }
        let (out, _) = handle("**/*.rs", &link.to_string_lossy(), true, true, 20);
        assert!(
            out.contains("visible.rs"),
            "junction root must be traversed: {out}"
        );
    }

    /// GH #1431: dot-directories are walkable (hidden(false)).
    #[test]
    fn glob_finds_files_in_dot_directories() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        // .gitignore only ignores a specific file, not the whole dir.
        std::fs::write(dir.path().join(".gitignore"), ".agents/finding\n").unwrap();
        std::fs::create_dir_all(dir.path().join(".agents/plans")).unwrap();
        std::fs::write(dir.path().join(".agents/plans/todo-1.md"), "task 1").unwrap();
        std::fs::write(dir.path().join(".agents/plans/todo-2.md"), "task 2").unwrap();
        std::fs::write(dir.path().join(".agents/finding"), "secret").unwrap();

        let (out, _) = handle(
            ".agents/plans/todo-*.md",
            &dir.path().to_string_lossy(),
            true,
            true,
            100,
        );
        assert!(
            out.contains("todo-1.md"),
            "dot-dir files must be walkable: {out}"
        );
        assert!(out.contains("todo-2.md"), "must find all matches: {out}");
        assert!(
            !out.contains("finding"),
            "gitignored file must still be excluded: {out}"
        );
    }
}
