use std::path::Path;

use ignore::WalkBuilder;

use crate::core::protocol;
use crate::core::tokens::count_tokens;

/// Maximum number of files to return from a glob search.
const MAX_RESULTS: usize = 500;

/// Finds files matching a glob pattern with compressed output.
///
/// Unlike `ctx_search` which matches file *content*, this matches file *paths*.
/// Uses the `ignore` crate for gitignore-aware walking, with glob matching via
/// the standard `glob` crate patterns.
pub fn handle(
    pattern: &str,
    dir: &str,
    respect_gitignore: bool,
    allow_secret_paths: bool,
    max_results: usize,
) -> (String, usize) {
    let root = Path::new(dir);
    if !root.exists() {
        return (format!("ERROR: {dir} does not exist"), 0);
    }
    if !root.is_dir() {
        return (format!("ERROR: {dir} is not a directory"), 0);
    }

    let max = max_results.min(MAX_RESULTS);

    // Build the glob matcher — support both simple patterns (*.rs) and
    // recursive patterns (**/*.ts). The `glob` crate handles both.
    let glob_matcher = match glob::Pattern::new(pattern) {
        Ok(m) => m,
        Err(e) => return (format!("ERROR: invalid glob pattern '{pattern}': {e}"), 0),
    };

    let mut matches = Vec::new();
    let mut files_walked = 0u32;

    let walker = WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(respect_gitignore)
        .git_global(respect_gitignore)
        .git_exclude(respect_gitignore)
        .filter_entry(crate::core::cloud_files::keep_entry)
        .build();

    for entry in walker.filter_map(std::result::Result::ok) {
        if matches.len() >= max {
            break;
        }

        if entry.file_type().is_none_or(|ft| ft.is_dir()) {
            continue;
        }

        if entry.file_type().is_some_and(|ft| ft.is_symlink()) {
            continue;
        }

        let path = entry.path();
        files_walked += 1;

        // Skip secret-like paths unless allowed
        if !allow_secret_paths && crate::core::io_boundary::is_secret_like(path).is_some() {
            continue;
        }

        // Get the path relative to the root for glob matching
        let rel_path = path.strip_prefix(root).unwrap_or(path);
        let rel_str = rel_path.to_string_lossy();

        // Match against the glob pattern
        if glob_matcher.matches(&rel_str) {
            let short_path =
                protocol::shorten_path_relative(&path.to_string_lossy(), &root.to_string_lossy());
            matches.push(short_path);
        }
    }

    if matches.is_empty() {
        return (
            format!("0 files matched '{pattern}' in {files_walked} files walked"),
            0,
        );
    }

    // Sort for deterministic output
    matches.sort();

    let output = matches.join("\n");
    let raw_tokens = count_tokens(&output);

    let footer = format!(
        "\n\n{} files matched (walked {files_walked})",
        matches.len()
    );
    let full_output = format!("{output}{footer}");

    let saved = raw_tokens; // No compression overhead for simple file list
    (full_output, saved)
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
    fn glob_skips_directories() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("subdir")).unwrap();
        std::fs::write(dir.path().join("file.txt"), "content").unwrap();

        let (out, _) = handle("**/*.txt", &dir.path().to_string_lossy(), true, true, 100);

        assert!(out.contains("file.txt"));
        assert!(!out.contains("subdir"));
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
}
