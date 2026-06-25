use std::path::Path;

use ignore::WalkBuilder;

use crate::core::protocol;
use crate::core::tokens::count_tokens;

/// Generates a compact directory tree listing with file counts.
/// When `respect_gitignore` is true, entries matching .gitignore patterns are excluded.
#[must_use]
pub fn handle(
    path: &str,
    depth: usize,
    show_hidden: bool,
    respect_gitignore: bool,
) -> (String, usize) {
    let root = Path::new(path);
    if root.is_file() {
        let parent = root
            .parent()
            .map_or(path.to_string(), |p| p.display().to_string());
        return (
            format!(
                "ERROR: '{path}' is a file, not a directory. Use path=\"{parent}\" for the containing directory."
            ),
            0,
        );
    }
    if !root.is_dir() {
        return (
            format!("ERROR: {path} does not exist or is not a directory"),
            0,
        );
    }
    // Broad-root guard (#356 class): with cwd == $HOME a defaulted `path`
    // would walk the whole home dir and trip macOS TCC privacy prompts.
    if let Some(err) = crate::tools::walk_guard::deny_unsafe_walk_root(path) {
        return (err, 0);
    }

    let raw_output = generate_raw_tree(root, depth, show_hidden, respect_gitignore);
    let compact_output = generate_compact_tree(root, depth, show_hidden, respect_gitignore);

    if compact_output.trim().is_empty() {
        return (format!("{path}/ (empty directory, depth={depth})"), 0);
    }

    let _mode_guard = crate::core::savings_footer::ModeGuard::new("tree");
    let raw_tokens = count_tokens(&raw_output);
    let compact_tokens = count_tokens(&compact_output);
    let savings = protocol::format_savings(raw_tokens, compact_tokens);

    (format!("{compact_output}\n{savings}"), raw_tokens)
}

fn generate_compact_tree(
    root: &Path,
    max_depth: usize,
    show_hidden: bool,
    respect_gitignore: bool,
) -> String {
    let mut lines = Vec::new();

    struct Entry {
        depth: usize,
        name: String,
        is_dir: bool,
        path: std::path::PathBuf,
    }
    let mut entries: Vec<Entry> = Vec::new();

    // Vendor dirs (node_modules, …) follow the gitignore toggle: explicitly
    // disabling gitignore is the escape hatch to look inside them (#400).
    let walker = WalkBuilder::new(root)
        .hidden(!show_hidden)
        .git_ignore(respect_gitignore)
        .git_global(respect_gitignore)
        .git_exclude(respect_gitignore)
        .require_git(false)
        .max_depth(Some(max_depth))
        .sort_by_file_name(std::cmp::Ord::cmp)
        .filter_entry(move |e| {
            if respect_gitignore {
                crate::core::walk_filter::keep_entry(e)
            } else {
                crate::core::cloud_files::keep_entry(e)
            }
        })
        .build();

    for entry in walker.filter_map(std::result::Result::ok) {
        if entry.depth() == 0 {
            continue;
        }
        entries.push(Entry {
            depth: entry.depth(),
            name: entry.file_name().to_string_lossy().to_string(),
            is_dir: entry.file_type().is_some_and(|ft| ft.is_dir()),
            path: entry.path().to_path_buf(),
        });
    }

    let mut dir_file_counts: std::collections::HashMap<&std::path::Path, usize> =
        std::collections::HashMap::new();
    for e in &entries {
        if !e.is_dir
            && let Some(parent) = e.path.parent()
        {
            *dir_file_counts.entry(parent).or_default() += 1;
        }
    }

    for e in &entries {
        let indent = "  ".repeat(e.depth.saturating_sub(1));
        if e.is_dir {
            let count = dir_file_counts.get(e.path.as_path()).copied().unwrap_or(0);
            lines.push(format!("{indent}{}/ ({count})", e.name));
        } else {
            lines.push(format!("{indent}{}", e.name));
        }
    }

    lines.join("\n")
}

fn generate_raw_tree(
    root: &Path,
    depth: usize,
    show_hidden: bool,
    respect_gitignore: bool,
) -> String {
    let mut lines = Vec::new();

    let walker = WalkBuilder::new(root)
        .hidden(!show_hidden)
        .git_ignore(respect_gitignore)
        .git_global(respect_gitignore)
        .git_exclude(respect_gitignore)
        .max_depth(Some(depth))
        .sort_by_file_name(std::cmp::Ord::cmp)
        .build();

    for entry in walker.filter_map(std::result::Result::ok) {
        if entry.depth() == 0 {
            continue;
        }
        let rel = entry
            .path()
            .strip_prefix(root)
            .unwrap_or(entry.path())
            .to_string_lossy();
        lines.push(rel.to_string());
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a deterministic source-tree fixture so the assertions do not
    /// depend on the live repository size or platform path separators (the live
    /// repo coupling previously made this test tip over its token threshold on
    /// Windows as the codebase grew).
    fn make_fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let files = [
            "Cargo.toml",
            "README.md",
            "src/main.rs",
            "src/lib.rs",
            "src/core/mod.rs",
            "src/core/engine.rs",
            "src/core/util.rs",
            "src/tools/mod.rs",
            "src/tools/reader.rs",
            "tests/integration.rs",
            "tests/smoke.rs",
        ];
        for rel in files {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, "// fixture\n").unwrap();
        }
        dir
    }

    #[test]
    fn tree_savings_are_reasonable() {
        let dir = make_fixture();
        let (output, original) = handle(&dir.path().to_string_lossy(), 3, false, true);
        let compact_tokens = count_tokens(&output);

        eprintln!("=== ctx_tree savings test ===");
        eprintln!("  original (raw) tokens: {original}");
        eprintln!("  compact tokens:        {compact_tokens}");
        eprintln!(
            "  savings:               {}",
            original.saturating_sub(compact_tokens)
        );

        assert!(original > 0, "raw tree should have some tokens");
        assert!(
            original < 2000,
            "raw tree for the fixture should be small, got {original}"
        );
        if original > compact_tokens {
            let ratio = (original - compact_tokens) as f64 / original as f64;
            eprintln!("  savings ratio:         {:.1}%", ratio * 100.0);
            assert!(
                ratio < 0.90,
                "savings ratio should be < 90% for same-depth comparison, got {:.1}%",
                ratio * 100.0
            );
        }
    }

    #[test]
    fn tree_refuses_home_directory_root() {
        // #356 class: never walk the whole home dir (macOS TCC prompts).
        let home = dirs::home_dir().expect("home dir in test env");
        let (output, tokens) = handle(home.to_string_lossy().as_ref(), 2, false, true);
        assert!(
            output.starts_with("ERROR:") && output.contains("refusing to scan"),
            "home root must be refused: {output}"
        );
        assert_eq!(tokens, 0);
    }

    #[test]
    fn tree_hides_node_modules_by_default_even_without_git() {
        // #400: vendor dirs are pruned by default; respect_gitignore=false is
        // the explicit escape hatch to look inside them.
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join("node_modules/react")).expect("mkdir");
        std::fs::write(tmp.path().join("node_modules/react/index.js"), "x").expect("write");
        std::fs::create_dir_all(tmp.path().join("src")).expect("mkdir");
        std::fs::write(tmp.path().join("src/app.js"), "y").expect("write");
        let root = tmp.path().to_string_lossy().to_string();

        let (default_out, _) = handle(&root, 4, false, true);
        assert!(default_out.contains("src"), "src visible: {default_out}");
        assert!(
            !default_out.contains("node_modules"),
            "node_modules must be hidden by default: {default_out}"
        );

        let (opt_out, _) = handle(&root, 4, false, false);
        assert!(
            opt_out.contains("node_modules"),
            "respect_gitignore=false must reveal vendor dirs: {opt_out}"
        );
    }
}
