// SPDX-License-Identifier: Apache-2.0
use std::path::Path;

use ignore::WalkBuilder;

use lean_ctx::core::protocol;
use lean_ctx::core::tokens::count_tokens;

/// Hard ceiling on the number of files returned from a single glob search,
/// independent of the caller-supplied `max_results`.
const MAX_RESULTS: usize = 500;

/// Default ceiling on directory entries one glob walk may visit (#2250). A
/// walk that reaches it stops and says `complete=false` instead of crawling
/// millions of unrelated descendants.
const WALK_ENTRY_BUDGET: usize = 200_000;

/// `*` and `?` stay inside one path component (#2182); `**` still recurses.
const STRICT_SEPARATORS: glob::MatchOptions = glob::MatchOptions {
    case_sensitive: true,
    require_literal_separator: true,
    require_literal_leading_dot: false,
};

/// Finds files matching a glob `pattern` under `dir` with compressed output.
///
/// Unlike `ctx_search` which matches file *content*, this matches file *paths*.
/// Uses the `ignore` crate for gitignore-aware, hidden-aware walking and matches
/// against the standard `glob` crate's pattern syntax (`*.rs`, `**/*.ts`, …).
/// In a pattern with a `/`, `*` and `?` never cross a directory boundary
/// (`src/*.rs` lists only `src`'s own files) and `**` matches any depth
/// (`src/**/*.rs`). A pattern without `/` (`*.rs`) matches file names at any
/// depth.
///
/// The walk is ordered by path (`sort_by_file_path`) so that — even when the
/// result set is truncated to `max_results` — the *set* of returned files, not
/// just their printed order, is deterministic across runs.
///
/// The walk follows the pattern (#2250): a pattern with a `/` and no `**`
/// (`src/*.rs`, `./prefix*`) only descends through its literal leading
/// directories and never deeper than its component count. Recursive patterns
/// (`**`, or a slash-free name matched at any depth) stop after
/// `WALK_ENTRY_BUDGET` visited entries and report `complete=false`.
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
    let walk = walk(
        pattern,
        dir,
        respect_gitignore,
        allow_secret_paths,
        max_results,
        WALK_ENTRY_BUDGET,
    );
    (walk.output, walk.original_tokens)
}

/// One glob walk's result; `visited` counts every entry the walker yielded.
pub(crate) struct GlobWalk {
    pub output: String,
    pub original_tokens: usize,
    /// Read by the #2250 regression tests to bound the walk without a clock.
    #[cfg_attr(not(test), allow(dead_code))]
    pub visited: usize,
}

impl GlobWalk {
    fn error(output: String) -> Self {
        Self {
            output,
            original_tokens: 0,
            visited: 0,
        }
    }
}

/// How far a walk for a pattern must reach: the literal leading directories
/// to follow and, unless the pattern recurses, the deepest level to visit.
#[derive(Debug, PartialEq, Eq)]
struct WalkPlan {
    prefix: Vec<String>,
    max_depth: Option<usize>,
}

/// Derives the walk extent from an (already repaired, `./`-stripped)
/// pattern. `anchored` is false for slash-free patterns, which match a file
/// name at any depth and therefore recurse.
fn walk_plan(pattern: &str, anchored: bool) -> WalkPlan {
    if !anchored {
        return WalkPlan {
            prefix: Vec::new(),
            max_depth: None,
        };
    }
    let components: Vec<&str> = pattern.split('/').collect();
    let (dirs, _file) = components.split_at(components.len() - 1);
    // Only plain names become a prefix: `.`, `..`, empty (absolute or `//`)
    // and wildcard components end it, so confinement stays with the walk.
    let prefix: Vec<String> = dirs
        .iter()
        .take_while(|c| {
            !c.is_empty() && **c != "." && **c != ".." && !c.contains(['*', '?', '[', ']'])
        })
        .map(|c| (*c).to_string())
        .collect();
    let max_depth = if components.contains(&"**") {
        None
    } else {
        Some(components.len())
    };
    WalkPlan { prefix, max_depth }
}

/// Why a walk ended before visiting everything the pattern could reach.
enum Incomplete {
    WalkBudget(usize),
    ResultCap(usize),
}

pub(crate) fn walk(
    pattern: &str,
    dir: &str,
    respect_gitignore: bool,
    allow_secret_paths: bool,
    max_results: usize,
    entry_budget: usize,
) -> GlobWalk {
    let requested_root = Path::new(dir);
    let walk_root = lean_ctx::core::walk_filter::explicit_walk_root(requested_root);
    let root = walk_root.as_path();
    if !root.exists() {
        let hint = lean_ctx::core::path_suggest::hint(root);
        return GlobWalk::error(format!("ERROR: {dir} does not exist{hint}"));
    }
    if !root.is_dir() {
        return GlobWalk::error(format!("ERROR: {dir} is not a directory"));
    }
    // Broad-root guard (#356 class): with cwd == $HOME a defaulted `path`
    // would walk the whole home dir and trip macOS TCC privacy prompts.
    if let Some(err) = crate::tools::walk_guard::deny_unsafe_walk_root(dir) {
        return GlobWalk::error(err);
    }

    let max = max_results.min(MAX_RESULTS);

    // Support both simple (`*.rs`) and recursive (`**/*.ts`) patterns.
    // `**.rs` / `src**/x` mean "recursively"; the glob crate only accepts `**`
    // as a whole path component, so repair that one shape before compiling.
    // A leading `./` anchors the pattern at `dir` (`./prefix*` = only `dir`'s
    // own entries) and is not part of the relative paths matched below.
    let repaired = repair_recursive_wildcards(pattern);
    let anchored = repaired.contains('/');
    let mut repaired = repaired.as_str();
    while let Some(rest) = repaired.strip_prefix("./") {
        repaired = rest;
    }
    let glob_matcher = match glob::Pattern::new(repaired) {
        Ok(m) => m,
        Err(e) => {
            return GlobWalk::error(format!("ERROR: invalid glob pattern '{pattern}': {e}"));
        }
    };
    // #2182: `*` and `?` never cross `/`; only a whole `**` component spans
    // directories. A pattern with no `/` (`*.rs`, `Cargo.toml`) names a file
    // at any depth, so it is matched against the file name alone.
    let match_file_name = !anchored;
    let plan = walk_plan(repaired, anchored);

    let mut matches = Vec::new();
    let mut files_walked = 0u32;
    let mut visited = 0usize;
    let mut incomplete = None;
    let protected = lean_ctx::core::policy::runtime::active().is_some();

    // Vendor dirs (node_modules, …) follow the gitignore toggle: explicitly
    // disabling gitignore is the escape hatch to look inside them (#400).
    // GH #1431: hidden(false) — dot-directories like .agents/, .github/,
    // .cursor/ must be walkable; the user's glob pattern is the intent
    // filter, gitignore handles exclusion, is_secret_like guards secrets.
    // #2250: the walk keeps its root (so ignore rules apply exactly as
    // before) but only descends through the pattern's literal prefix and,
    // for non-recursive patterns, no deeper than the pattern reaches.
    let prefix = plan.prefix;
    let walker = WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(respect_gitignore)
        .git_global(respect_gitignore)
        .git_exclude(respect_gitignore)
        .require_git(false)
        .max_depth(plan.max_depth)
        .filter_entry(move |e| {
            let depth = e.depth();
            if depth >= 1
                && depth <= prefix.len()
                && e.file_name().to_str() != Some(prefix[depth - 1].as_str())
            {
                return false;
            }
            if respect_gitignore {
                lean_ctx::core::walk_filter::keep_entry(e)
            } else {
                lean_ctx::core::cloud_files::keep_entry(e)
            }
        })
        .sort_by_file_path(std::path::Path::cmp)
        .build();

    for entry in walker.filter_map(std::result::Result::ok) {
        if protected && visited >= 20_000 {
            return GlobWalk::error("ERROR: protected directory walk limit exceeded".into());
        }
        if matches.len() >= max {
            incomplete = Some(Incomplete::ResultCap(max));
            break;
        }
        if visited >= entry_budget {
            incomplete = Some(Incomplete::WalkBudget(entry_budget));
            break;
        }
        visited += 1;

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
        if !allow_secret_paths && lean_ctx::core::io_boundary::is_secret_like(path).is_some() {
            continue;
        }

        let admitted = if protected {
            match crate::server::policy_guard::protect_result("ctx_glob", &path.to_string_lossy()) {
                Ok(path) => path,
                Err(_) => continue,
            }
        } else {
            path.to_string_lossy().into_owned()
        };
        let rel_path = Path::new(&admitted)
            .strip_prefix(root)
            .unwrap_or(Path::new(&admitted));
        let candidate = if match_file_name {
            rel_path.file_name().map_or(rel_path, Path::new)
        } else {
            rel_path
        };

        if glob_matcher.matches_path_with(candidate, STRICT_SEPARATORS) {
            let short_path = protocol::shorten_path_relative(&admitted, &root.to_string_lossy());
            matches.push((short_path, admitted));
        }
    }

    let incomplete_note = match incomplete {
        Some(Incomplete::WalkBudget(n)) => {
            format!(" complete=false (walk budget {n} entries reached)")
        }
        Some(Incomplete::ResultCap(n)) => format!(" complete=false (result cap {n} reached)"),
        None => String::new(),
    };

    if matches.is_empty() {
        return GlobWalk {
            output: format!(
                "0 files matched '{pattern}' in {files_walked} files walked{incomplete_note}"
            ),
            original_tokens: 0,
            visited,
        };
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

    let footer =
        format!("\n\n{match_count} files matched (walked {files_walked}){incomplete_note}");
    GlobWalk {
        output: format!("{output}{footer}"),
        original_tokens: raw_tokens,
        visited,
    }
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

/// Splits a `**` glued to other characters into its own path component:
/// `**.rs` → `**/*.rs`, `src**` → `src*/**`, `a/**b/c` → `a/**/*b/c`. A
/// pattern whose `**` already stands alone is returned unchanged.
fn repair_recursive_wildcards(pattern: &str) -> String {
    if !pattern.contains("**") {
        return pattern.to_string();
    }
    pattern
        .split('/')
        .map(|part| {
            if part == "**" || !part.contains("**") {
                return part.to_string();
            }
            let collapsed = part.replace("***", "**");
            let (before, after) = collapsed.split_once("**").unwrap_or((&collapsed, ""));
            let mut pieces = Vec::new();
            if !before.is_empty() {
                pieces.push(format!("{before}*"));
            }
            pieces.push("**".to_string());
            if !after.is_empty() {
                pieces.push(format!("*{after}"));
            }
            pieces.join("/")
        })
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    #[test]
    fn glued_recursive_wildcards_are_repaired() {
        use super::repair_recursive_wildcards as r;
        assert_eq!(r("**.rs"), "**/*.rs");
        assert_eq!(r("src**"), "src*/**");
        assert_eq!(r("a/**b/c.ts"), "a/**/*b/c.ts");
        assert_eq!(r("**/*.ts"), "**/*.ts");
        assert_eq!(r("*.rs"), "*.rs");
    }

    #[test]
    fn glued_recursive_pattern_finds_nested_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("a/b")).unwrap();
        std::fs::write(dir.path().join("a/b/x.rs"), "").unwrap();
        let (out, _) = super::handle("**.rs", &dir.path().to_string_lossy(), true, true, 100);
        assert!(out.contains("x.rs"), "{out}");
    }

    use super::*;

    /// Fixture for #2182: `src/a.rs`, `src/sub/b.rs` and `top.rs`.
    fn separator_fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/sub")).unwrap();
        std::fs::write(dir.path().join("src/a.rs"), "").unwrap();
        std::fs::write(dir.path().join("src/sub/b.rs"), "").unwrap();
        std::fs::write(dir.path().join("top.rs"), "").unwrap();
        dir
    }

    #[test]
    fn single_star_with_directory_stays_in_that_directory() {
        let dir = separator_fixture();
        let (out, _) = handle("src/*.rs", &dir.path().to_string_lossy(), true, true, 100);
        assert!(out.contains("a.rs"), "{out}");
        assert!(!out.contains("b.rs"), "`*` must not cross `/`: {out}");
        assert!(!out.contains("top.rs"), "{out}");
        assert!(out.contains("1 files matched"), "{out}");
    }

    #[test]
    fn slash_free_pattern_matches_file_names_at_any_depth() {
        let dir = separator_fixture();
        let (out, _) = handle("*.rs", &dir.path().to_string_lossy(), true, true, 100);
        assert!(out.contains("top.rs"), "{out}");
        assert!(out.contains("a.rs"), "{out}");
        assert!(out.contains("b.rs"), "{out}");
        assert!(out.contains("3 files matched"), "{out}");
    }

    #[test]
    fn double_star_with_directory_recurses() {
        let dir = separator_fixture();
        let (out, _) = handle(
            "src/**/*.rs",
            &dir.path().to_string_lossy(),
            true,
            true,
            100,
        );
        assert!(out.contains("a.rs"), "{out}");
        assert!(out.contains("b.rs"), "{out}");
        assert!(!out.contains("top.rs"), "{out}");
        assert!(out.contains("2 files matched"), "{out}");
    }

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
        // HOME is read here and again inside the check: another test may move
        // it in between (macOS release prebuild, 3.12.1).
        let _env = lean_ctx::core::data_dir::test_env_lock();
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

    /// #2250 fixture: three immediate `sess-*` matches above a few hundred
    /// irrelevant nested entries (including deeper `sess-*` look-alikes).
    fn wide_tree_fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let tmp = dir.path().join(".tmp");
        for name in ["sess-a.log", "sess-b.log", "sess-c.log", "other.log"] {
            std::fs::create_dir_all(&tmp).unwrap();
            std::fs::write(tmp.join(name), "x").unwrap();
        }
        for d in 0..20 {
            let nested = tmp.join(format!("run-{d:02}/inner"));
            std::fs::create_dir_all(&nested).unwrap();
            for f in 0..15 {
                std::fs::write(nested.join(format!("artifact-{f:02}.bin")), "x").unwrap();
            }
            std::fs::write(nested.join("sess-deep.log"), "x").unwrap();
        }
        for d in 0..10 {
            let sibling = dir.path().join(format!("sibling-{d}/deep"));
            std::fs::create_dir_all(&sibling).unwrap();
            for f in 0..10 {
                std::fs::write(sibling.join(format!("f{f}.txt")), "x").unwrap();
            }
        }
        dir
    }

    #[test]
    fn anchored_pattern_walks_only_immediate_entries() {
        let dir = wide_tree_fixture();
        let tmp = dir.path().join(".tmp");
        let walk = walk(
            "./sess-*",
            &tmp.to_string_lossy(),
            true,
            true,
            200,
            1_000_000,
        );
        assert!(walk.output.contains("3 files matched"), "{}", walk.output);
        assert!(!walk.output.contains("sess-deep"), "{}", walk.output);
        assert!(!walk.output.contains("complete=false"), "{}", walk.output);
        // root + 4 files + 20 run dirs; never the 340 nested entries.
        assert!(walk.visited <= 25, "visited {} entries", walk.visited);
    }

    #[test]
    fn static_prefix_prunes_sibling_trees() {
        let dir = wide_tree_fixture();
        let walk = walk(
            ".tmp/sess-*",
            &dir.path().to_string_lossy(),
            true,
            true,
            200,
            1_000_000,
        );
        assert!(walk.output.contains("3 files matched"), "{}", walk.output);
        assert!(!walk.output.contains("sess-deep"), "{}", walk.output);
        // root + .tmp + its 24 immediate entries; no sibling or nested walk.
        assert!(walk.visited <= 30, "visited {} entries", walk.visited);
    }

    #[test]
    fn bounded_depth_pattern_still_matches_its_level() {
        let dir = wide_tree_fixture();
        let walk = walk(
            ".tmp/*/inner/sess-*.log",
            &dir.path().to_string_lossy(),
            true,
            true,
            200,
            1_000_000,
        );
        assert!(walk.output.contains("20 files matched"), "{}", walk.output);
        assert!(!walk.output.contains("complete=false"), "{}", walk.output);
    }

    #[test]
    fn recursive_walk_reports_budget_exhaustion() {
        let dir = wide_tree_fixture();
        let path = dir.path().to_string_lossy();
        for pattern in ["**/sess-*.log", "sess-*.log"] {
            let walk = walk(pattern, &path, true, true, 200, 50);
            assert!(
                walk.output
                    .contains("complete=false (walk budget 50 entries reached)"),
                "{pattern}: {}",
                walk.output
            );
            assert!(walk.visited <= 50, "visited {} entries", walk.visited);
        }
        let full = walk("**/sess-*.log", &path, true, true, 200, 1_000_000);
        assert!(full.output.contains("23 files matched"), "{}", full.output);
        assert!(!full.output.contains("complete=false"), "{}", full.output);
    }

    #[test]
    fn result_cap_does_not_imply_completeness() {
        let dir = wide_tree_fixture();
        let walk = walk(
            "**/sess-*.log",
            &dir.path().to_string_lossy(),
            true,
            true,
            2,
            1_000_000,
        );
        assert!(
            walk.output
                .contains("complete=false (result cap 2 reached)"),
            "{}",
            walk.output
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
