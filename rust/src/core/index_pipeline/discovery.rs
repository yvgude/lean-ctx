//! Mode-aware file discovery for the indexing pipeline.
//!
//! Walks a project root with `ignore::WalkBuilder`, applies CBM-style skip
//! lists gated by [`IndexingMode`], and returns a sorted list of
//! [`DiscoveredFile`] entries. Every entry passes `is_ingestible()` so binaries
//! and unreadable files are excluded before downstream processing.
//!
//! # Architecture
//!
//! ```text
//! discover_files(root, config)
//!   ├── filter_entry: walk_filter::keep_entry + dir-level skip lists
//!   ├── for each file entry:
//!   │   ├── rel_path = strip_prefix(root) → normalize '/'   [skip if empty/fails]
//!   │   ├── should_include(rel_path, mode)                   [suffix/pattern/filename lists]
//!   │   ├── file_size <= max_file_size
//!   │   └── is_ingestible(path)
//!   ├── sort + dedup
//!   └── return Vec<DiscoveredFile>
//! ```

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::Result;

use crate::core::config::IndexingMode;
use crate::core::walk_filter;

// ---------------------------------------------------------------------------
// CBM skip lists — match spec in /tmp/codebase-memory-mcp/discover.c
// ---------------------------------------------------------------------------

/// Directories ALWAYS skipped regardless of indexing mode.
const ALWAYS_SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "__pycache__",
    "target",
    "dist",
    "vendor",
];

/// Directories skipped in MODERATE and FAST modes but NOT in FULL.
const FAST_SKIP_DIRS: &[&str] = &[
    "generated",
    "docs",
    "examples",
    "tests",
    "fixtures",
    "assets",
    "public",
    "build",
    "scripts",
];

/// Filenames skipped in MODERATE and FAST modes but NOT in FULL.
const FAST_SKIP_FILENAMES: &[&str] = &[
    "LICENSE",
    "CHANGELOG",
    "go.sum",
    "Cargo.lock",
    "package-lock.json",
];

/// File patterns (matched against path suffix) skipped in MODERATE and FAST modes.
const FAST_PATTERNS: &[&str] = &[".d.ts", ".pb.go", "mock_", ".test.", ".spec.", ".stories."];

/// Suffixes ALWAYS ignored regardless of mode (binary/unusable outputs).
const ALWAYS_IGNORED_SUFFIXES: &[&str] = &[".pyc", ".o", ".so", ".png", ".wasm", ".exe", ".db"];

/// Suffixes ignored in MODERATE and FAST modes only.
const FAST_IGNORED_SUFFIXES: &[&str] = &[".zip", ".pdf", ".map", ".min.js", ".pem"];

// ---------------------------------------------------------------------------
// Config & output types
// ---------------------------------------------------------------------------

/// Configuration for a file-discovery pass.
#[derive(Debug, Clone)]
pub struct DiscoveryConfig {
    /// Indexing mode — controls which skip lists are applied.
    pub mode: IndexingMode,
    /// Maximum file size in bytes. Larger files are skipped.
    pub max_file_size: u64,
}

/// A single file discovered during the walk, with metadata needed by the
/// downstream indexing pipeline.
#[derive(Debug, Clone)]
pub struct DiscoveredFile {
    /// Absolute path on disk.
    pub path: PathBuf,
    /// Project-relative path (always `/` separators).
    pub rel_path: String,
    /// Lower-case file extension (empty string if none).
    pub ext: String,
    /// File size in bytes.
    pub size: u64,
    /// Last modification time.
    pub mtime: SystemTime,
}

// ---------------------------------------------------------------------------
// Core entry point
// ---------------------------------------------------------------------------

/// Walk `root` and return ingestible files matching `config`.
///
/// * Uses `ignore::WalkBuilder` with `walk_filter::keep_entry` + dir-level
///   skip-list filtering.
/// * Applies suffix/pattern/filename skip lists per `config.mode`.
/// * Filters by `config.max_file_size` and `is_ingestible()`.
/// * Returns entries sorted by relative path (stable order).
pub fn discover_files(root: &Path, config: &DiscoveryConfig) -> Result<Vec<DiscoveredFile>> {
    let mode = config.mode;
    let walker = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .require_git(false)
        .max_depth(Some(20))
        .filter_entry(move |entry| {
            if !walk_filter::keep_entry(entry) {
                return false;
            }
            !is_skipped_dir(entry, mode)
        })
        .build();

    let mut files: Vec<DiscoveredFile> = Vec::new();

    for entry in walker.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        // Compute project-relative path with normalised separators.
        let rel_path = match path.strip_prefix(root) {
            Ok(p) => p.to_string_lossy().replace('\\', "/"),
            Err(_) => continue,
        };
        if rel_path.is_empty() {
            continue;
        }

        // Skip lists (suffix / pattern / filename).
        if !should_include(&rel_path, config.mode) {
            continue;
        }

        // File size gate.
        let Ok(meta) = path.metadata() else { continue };
        if meta.len() > config.max_file_size {
            continue;
        }

        // Broader ingestibility check (binaries, unreadable files, …).
        if !crate::core::ingestion::is_ingestible(path) {
            continue;
        }

        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        let mtime = match meta.modified() {
            Ok(t) => t,
            Err(_) => SystemTime::UNIX_EPOCH,
        };

        files.push(DiscoveredFile {
            path: path.to_path_buf(),
            rel_path,
            ext,
            size: meta.len(),
            mtime,
        });
    }

    // Stable, deterministic order.
    files.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    files.dedup_by(|a, b| a.rel_path == b.rel_path);

    Ok(files)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Dir-level skip check for the `filter_entry` callback.
///
/// Returns `true` when the directory *should* be skipped (pruned from the
/// walk). The root entry (`depth == 0`) is never skipped — explicitly
/// requested roots stay reachable.
fn is_skipped_dir(entry: &ignore::DirEntry, mode: IndexingMode) -> bool {
    if entry.depth() == 0 {
        return false;
    }
    let Some(ft) = entry.file_type() else {
        return false;
    };
    if !ft.is_dir() {
        return false;
    }
    let Some(name) = entry.file_name().to_str() else {
        return false;
    };

    if ALWAYS_SKIP_DIRS.contains(&name) {
        return true;
    }
    if mode != IndexingMode::Full && FAST_SKIP_DIRS.contains(&name) {
        return true;
    }
    false
}

/// Post-walk skip check against suffix, pattern, and filename lists.
///
/// `path_str` is the project-relative path with `/` separators.
fn should_include(path_str: &str, mode: IndexingMode) -> bool {
    // Always-skip suffixes.
    if ALWAYS_IGNORED_SUFFIXES
        .iter()
        .any(|s| path_str.ends_with(s))
    {
        return false;
    }

    if mode != IndexingMode::Full {
        // Mode-specific suffixes.
        if FAST_IGNORED_SUFFIXES.iter().any(|s| path_str.ends_with(s)) {
            return false;
        }
        // Mode-specific path patterns.
        if FAST_PATTERNS.iter().any(|p| path_str.contains(p)) {
            return false;
        }
        // Mode-specific filenames.
        if let Some(filename) = path_str.rsplit('/').next()
            && FAST_SKIP_FILENAMES.contains(&filename)
        {
            return false;
        }
    }

    true
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Helper: create a temp project directory with a deterministic file tree.
    fn create_test_tree(root: &Path) {
        // -- ALWAYS-skipped dirs --
        std::fs::create_dir_all(root.join("vendor")).unwrap();
        std::fs::write(root.join("vendor/pkg.rs"), "fn vendored() {}").unwrap();

        // -- MODE-specific dirs (included in Full, skipped in Moderate/Fast) --
        std::fs::create_dir_all(root.join("tests")).unwrap();
        std::fs::write(root.join("tests/test.rs"), "fn test_something() {}").unwrap();
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::write(root.join("docs/readme.md"), "# Docs").unwrap();
        std::fs::create_dir_all(root.join("examples")).unwrap();
        std::fs::write(root.join("examples/hello.rs"), "fn main() {}").unwrap();
        std::fs::create_dir_all(root.join("generated")).unwrap();
        std::fs::write(root.join("generated/output.rs"), "// auto-gen").unwrap();
        std::fs::create_dir_all(root.join("assets")).unwrap();
        std::fs::write(root.join("assets/style.css"), "body {}").unwrap();
        std::fs::create_dir_all(root.join("public")).unwrap();
        std::fs::write(root.join("public/script.js"), "console.log(1)").unwrap();

        // -- Always-ingestible files --
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
        std::fs::write(root.join("src/lib.rs"), "pub fn lib() {}").unwrap();

        // -- Root-level files --
        std::fs::write(root.join("main.rs"), "fn main() {}").unwrap();

        // -- Binary files (always excluded by is_ingestible) --
        std::fs::write(root.join("image.png"), "fake-png-bytes").unwrap();
        std::fs::write(root.join("binary.wasm"), "fake-wasm-bytes").unwrap();

        // -- Mode-specific filenames at root --
        // Cargo.lock is NOT ingestible (".lock" is a binary extension), so it
        // never appears in any mode — use package-lock.json for the ingestible test.
        std::fs::write(root.join("package-lock.json"), "{}\n").unwrap();
        std::fs::write(root.join("go.sum"), "# sum\n").unwrap();
        std::fs::write(root.join("LICENSE"), "MIT\n").unwrap();

        // -- Large file (exceed max_file_size) --
        let mut giant = std::fs::File::create(root.join("giant.rs")).unwrap();
        giant
            .write_all(b"// giant\n".repeat(500_000).as_slice())
            .unwrap();

        // -- Hidden dirs --
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/config"), "[core]\n").unwrap();
    }

    /// Count discovered files whose rel_path contains (or equals) `sub`.
    fn count_containing(files: &[DiscoveredFile], sub: &str) -> usize {
        files.iter().filter(|f| f.rel_path.contains(sub)).count()
    }

    /// Count discovered files whose rel_path equals `name`.
    fn count_exact(files: &[DiscoveredFile], name: &str) -> usize {
        files.iter().filter(|f| f.rel_path == name).count()
    }

    // ------------------------------------------------------------------
    // FULL mode
    // ------------------------------------------------------------------

    #[test]
    fn full_mode_includes_all_dirs() {
        let dir = tempfile::tempdir().unwrap();
        create_test_tree(dir.path());
        let config = DiscoveryConfig {
            mode: IndexingMode::Full,
            max_file_size: 2 * 1024 * 1024,
        };
        let files = discover_files(dir.path(), &config).unwrap();

        // Mode-specific dirs present in FULL.
        assert!(
            count_containing(&files, "tests/") > 0,
            "tests/ should be in FULL"
        );
        assert!(
            count_containing(&files, "docs/") > 0,
            "docs/ should be in FULL"
        );
        assert!(
            count_containing(&files, "examples/") > 0,
            "examples/ should be in FULL"
        );
        assert!(
            count_containing(&files, "generated/") > 0,
            "generated/ should be in FULL"
        );
        assert!(
            count_containing(&files, "assets/") > 0,
            "assets/ should be in FULL"
        );
        assert!(
            count_containing(&files, "public/") > 0,
            "public/ should be in FULL"
        );
    }

    #[test]
    fn full_mode_includes_skip_filenames() {
        let dir = tempfile::tempdir().unwrap();
        create_test_tree(dir.path());
        let config = DiscoveryConfig {
            mode: IndexingMode::Full,
            max_file_size: 2 * 1024 * 1024,
        };
        let files = discover_files(dir.path(), &config).unwrap();

        // Cargo.lock is NOT ingestible (".lock" is a binary extension), so
        // only ingestible FAST_SKIP_FILENAMES entries are asserted here.
        assert_eq!(count_exact(&files, "go.sum"), 1);
        assert_eq!(count_exact(&files, "LICENSE"), 1);
        assert_eq!(count_exact(&files, "package-lock.json"), 1);
    }

    // ------------------------------------------------------------------
    // MODERATE mode
    // ------------------------------------------------------------------

    #[test]
    fn moderate_excludes_fast_skip_dirs() {
        let dir = tempfile::tempdir().unwrap();
        create_test_tree(dir.path());
        let config = DiscoveryConfig {
            mode: IndexingMode::Moderate,
            max_file_size: 2 * 1024 * 1024,
        };
        let files = discover_files(dir.path(), &config).unwrap();

        assert_eq!(
            count_containing(&files, "tests/"),
            0,
            "tests/ excluded in Moderate"
        );
        assert_eq!(
            count_containing(&files, "docs/"),
            0,
            "docs/ excluded in Moderate"
        );
        assert_eq!(
            count_containing(&files, "examples/"),
            0,
            "examples/ excluded in Moderate"
        );
        assert_eq!(
            count_containing(&files, "generated/"),
            0,
            "generated/ excluded in Moderate"
        );
        assert_eq!(
            count_containing(&files, "assets/"),
            0,
            "assets/ excluded in Moderate"
        );
        assert_eq!(
            count_containing(&files, "public/"),
            0,
            "public/ excluded in Moderate"
        );
    }

    #[test]
    fn moderate_excludes_skip_filenames() {
        let dir = tempfile::tempdir().unwrap();
        create_test_tree(dir.path());
        let config = DiscoveryConfig {
            mode: IndexingMode::Moderate,
            max_file_size: 2 * 1024 * 1024,
        };
        let files = discover_files(dir.path(), &config).unwrap();

        assert_eq!(count_exact(&files, "go.sum"), 0);
        assert_eq!(count_exact(&files, "LICENSE"), 0);
        assert_eq!(count_exact(&files, "package-lock.json"), 0);
    }

    // ------------------------------------------------------------------
    // FAST mode
    // ------------------------------------------------------------------

    #[test]
    fn fast_excludes_same_as_moderate() {
        // FAST should exclude everything MODERATE excludes.
        let dir = tempfile::tempdir().unwrap();
        create_test_tree(dir.path());
        let config = DiscoveryConfig {
            mode: IndexingMode::Fast,
            max_file_size: 2 * 1024 * 1024,
        };
        let files = discover_files(dir.path(), &config).unwrap();

        assert_eq!(count_containing(&files, "tests/"), 0);
        assert_eq!(count_containing(&files, "docs/"), 0);
        assert_eq!(count_containing(&files, "examples/"), 0);
        assert_eq!(count_containing(&files, "go.sum"), 0);
        assert_eq!(count_containing(&files, "LICENSE"), 0);
        assert_eq!(count_containing(&files, "package-lock.json"), 0);
    }

    // ------------------------------------------------------------------
    // Always-skipped dirs (all modes)
    // ------------------------------------------------------------------

    #[test]
    fn all_modes_skip_always_skip_dirs() {
        let dir = tempfile::tempdir().unwrap();
        create_test_tree(dir.path());
        for mode in [
            IndexingMode::Full,
            IndexingMode::Moderate,
            IndexingMode::Fast,
        ] {
            let config = DiscoveryConfig {
                mode,
                max_file_size: 2 * 1024 * 1024,
            };
            let files = discover_files(dir.path(), &config).unwrap();
            assert_eq!(
                count_containing(&files, "vendor/"),
                0,
                "{mode:?} should skip vendor/"
            );
            assert_eq!(
                count_containing(&files, ".git/"),
                0,
                "{mode:?} should skip .git/"
            );
        }
    }

    // ------------------------------------------------------------------
    // Binary exclusion (all modes)
    // ------------------------------------------------------------------

    #[test]
    fn all_modes_exclude_binary_suffixes() {
        let dir = tempfile::tempdir().unwrap();
        create_test_tree(dir.path());
        for mode in [
            IndexingMode::Full,
            IndexingMode::Moderate,
            IndexingMode::Fast,
        ] {
            let config = DiscoveryConfig {
                mode,
                max_file_size: 2 * 1024 * 1024,
            };
            let files = discover_files(dir.path(), &config).unwrap();
            assert_eq!(
                count_exact(&files, "image.png"),
                0,
                "{mode:?} should skip .png"
            );
            assert_eq!(
                count_exact(&files, "binary.wasm"),
                0,
                "{mode:?} should skip .wasm"
            );
        }
    }

    // ------------------------------------------------------------------
    // max_file_size filtering
    // ------------------------------------------------------------------

    #[test]
    fn max_file_size_excludes_large_files() {
        let dir = tempfile::tempdir().unwrap();
        create_test_tree(dir.path());

        // Tiny limit: only small files exclude.
        let config = DiscoveryConfig {
            mode: IndexingMode::Full,
            max_file_size: 100, // bytes
        };
        let files = discover_files(dir.path(), &config).unwrap();

        // giant.rs is ~500k lines × 7 bytes = ~3.5 MB → definitely excluded.
        assert_eq!(count_exact(&files, "giant.rs"), 0);
        // Small files (src/main.rs) should still be present.
        assert_eq!(count_exact(&files, "src/main.rs"), 1);
    }

    // ------------------------------------------------------------------
    // Relative path normalisation
    // ------------------------------------------------------------------

    #[test]
    fn rel_paths_use_forward_slash() {
        let dir = tempfile::tempdir().unwrap();
        create_test_tree(dir.path());
        let config = DiscoveryConfig {
            mode: IndexingMode::Full,
            max_file_size: 2 * 1024 * 1024,
        };
        let files = discover_files(dir.path(), &config).unwrap();

        for f in &files {
            assert!(
                !f.rel_path.contains('\\'),
                "rel_path should not contain backslash: {}",
                f.rel_path
            );
        }
    }

    // ------------------------------------------------------------------
    // Is_ingestible gate is active
    // ------------------------------------------------------------------

    #[test]
    fn uningestible_suffixes_are_filtered() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        // .pyc is in ALWAYS_IGNORED_SUFFIXES but NOT in BINARY_EXTS
        // (ingestion.rs does not treat .pyc as binary, so only our skip list
        // excludes it).
        std::fs::write(dir.path().join("src/mod.pyc"), "not-bytecode").unwrap();
        // .pdf is in FAST_IGNORED_SUFFIXES but IS ingestible (PDF has a
        // dedicated extractor in ingestion.rs → IngestKind::Document).
        std::fs::write(dir.path().join("src/report.pdf"), "fake-pdf-text").unwrap();
        // .min.js is in FAST_IGNORED_SUFFIXES and IS ingestible (code file).
        std::fs::write(dir.path().join("src/script.min.js"), "console.log(1)").unwrap();
        // .rs should survive in all modes.
        std::fs::write(dir.path().join("src/lib.rs"), "pub fn x() {}").unwrap();

        // FULL mode: .pyc excluded by ALWAYS_IGNORED_SUFFIXES;
        // .pdf and .min.js are included because FAST_IGNORED only applies in
        // Moderate/Fast.
        let cfg_full = DiscoveryConfig {
            mode: IndexingMode::Full,
            max_file_size: 1_000_000,
        };
        let files_full = discover_files(dir.path(), &cfg_full).unwrap();
        assert_eq!(
            count_containing(&files_full, "mod.pyc"),
            0,
            ".pyc always excluded by ALWAYS_IGNORED_SUFFIXES"
        );
        assert_eq!(
            count_exact(&files_full, "src/report.pdf"),
            1,
            ".pdf included in Full mode"
        );
        assert_eq!(
            count_exact(&files_full, "src/script.min.js"),
            1,
            ".min.js included in Full mode"
        );
        assert_eq!(count_exact(&files_full, "src/lib.rs"), 1);

        // MODERATE mode: .pyc still excluded, .pdf and .min.js now excluded
        // via FAST_IGNORED_SUFFIXES.
        let cfg_mod = DiscoveryConfig {
            mode: IndexingMode::Moderate,
            max_file_size: 1_000_000,
        };
        let files_mod = discover_files(dir.path(), &cfg_mod).unwrap();
        assert_eq!(
            count_containing(&files_mod, "mod.pyc"),
            0,
            ".pyc always excluded"
        );
        assert_eq!(
            count_exact(&files_mod, "src/report.pdf"),
            0,
            ".pdf excluded in Moderate via FAST_IGNORED_SUFFIXES"
        );
        assert_eq!(
            count_exact(&files_mod, "src/script.min.js"),
            0,
            ".min.js excluded in Moderate via FAST_IGNORED_SUFFIXES"
        );
        assert_eq!(count_exact(&files_mod, "src/lib.rs"), 1);
    }

    // ------------------------------------------------------------------
    // Deterministic ordering
    // ------------------------------------------------------------------

    #[test]
    fn discover_files_returns_sorted_order() {
        let dir = tempfile::tempdir().unwrap();
        create_test_tree(dir.path());
        let config = DiscoveryConfig {
            mode: IndexingMode::Full,
            max_file_size: 2 * 1024 * 1024,
        };
        let files = discover_files(dir.path(), &config).unwrap();

        let paths: Vec<&str> = files.iter().map(|f| f.rel_path.as_str()).collect();
        let mut sorted = paths.clone();
        sorted.sort_unstable();
        assert_eq!(paths, sorted, "files must be sorted by rel_path");
    }
}
