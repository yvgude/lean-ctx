// SPDX-License-Identifier: Apache-2.0
//! The corpus walk: which source files of a project the BM25 index covers.

use std::path::Path;

use super::{CorpusRules, build, is_safe_bm25_root};

pub(super) fn list_code_files(root: &Path) -> Vec<String> {
    list_code_files_with(root, &CorpusRules::load())
}

pub(super) fn list_code_files_with(root: &Path, rules: &CorpusRules) -> Vec<String> {
    list_code_files_bounded(root, rules, false).unwrap_or_default()
}

pub(super) fn admitted_scoped_source_paths(
    root: &Path,
    rules: &CorpusRules,
    subdir: Option<&str>,
    in_scope: &dyn Fn(&str) -> bool,
) -> Result<Vec<String>, String> {
    if !is_safe_bm25_root(root) {
        return Err("source root is not safe to scan".into());
    }
    let discovered = list_scoped_code_files(root, rules, true, subdir, in_scope)?;
    let mut admitted = Vec::with_capacity(discovered.len());
    for relative in discovered {
        let metadata = std::fs::symlink_metadata(root.join(&relative))
            .map_err(|_| "source inventory changed during discovery")?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_file() && metadata.len() <= build::MAX_FILE_SIZE_BYTES {
            admitted.push(relative);
        }
    }
    Ok(admitted)
}

/// The persisted-index corpus walk. `bounded` fails closed on a walk or file
/// budget instead of silently truncating.
///
/// Secret-like paths (`.env`, `*.pem`, `.ssh/`, …) never enter the persisted
/// index, whatever the role or gateway setting (GHSA-rxv5-4c3x-g4q6 A): the
/// index is shared across roles, served by dense/hybrid search and uploaded
/// by `sync index push`. Store admission already refuses them while the
/// context gateway is on; this keeps the guarantee when it is switched off.
/// Role-aware fresh views use [`list_scoped_code_files`] and admit per read.
fn list_code_files_bounded(
    root: &Path,
    rules: &CorpusRules,
    bounded: bool,
) -> Result<Vec<String>, String> {
    list_scoped_code_files(root, rules, bounded, None, &|rel| {
        crate::core::io_boundary::is_secret_like(Path::new(rel)).is_none()
    })
}

/// [`list_code_files_bounded`] restricted to the caller's scope: the walk
/// starts at `subdir` (project-relative) and only paths `in_scope` accepts
/// count against the walk and file budgets. A search scoped to a 20-file
/// directory used to walk the whole project and fail its 5000-file budget
/// before the scope was ever applied. Returned paths stay project-relative.
pub(super) fn list_scoped_code_files(
    root: &Path,
    rules: &CorpusRules,
    bounded: bool,
    subdir: Option<&str>,
    in_scope: &dyn Fn(&str) -> bool,
) -> Result<Vec<String>, String> {
    let walk_root = subdir
        .map(|s| root.join(s))
        .filter(|p| p.is_dir())
        .unwrap_or_else(|| root.to_path_buf());
    let walker = rules
        .walker(&walk_root)
        .filter_entry(crate::core::walk_filter::keep_entry)
        .build();

    let mut files: Vec<String> = Vec::new();
    let mut filtered_out = 0usize;
    for (visited, entry) in walker.enumerate() {
        if bounded && visited >= 100_000 {
            return Err("source discovery budget exceeded; narrow the search scope".into());
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) if bounded => return Err("source discovery could not be completed".into()),
            Err(_) => continue,
        };
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if !crate::core::ingestion::is_ingestible(path) {
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();
        if rel.is_empty() {
            continue;
        }
        if rules.ignore_pattern_for(&rel).is_some() {
            continue;
        }
        if rules.filter_excludes(&rel) {
            filtered_out += 1;
            continue;
        }
        if !in_scope(&rel) {
            continue;
        }
        if files.len() >= rules.max_files {
            if bounded {
                return Err(format!(
                    "source file budget exceeded ({} files); narrow the search with path=, \
                     languages= or a glob, or raise bm25_max_files (0 = unlimited)",
                    rules.max_files
                ));
            }
            tracing::warn!(
                "[bm25] file cap reached ({}), skipping remaining files in {}. \
                 Set bm25_max_files in config (0 = unlimited) to index more.",
                rules.max_files,
                root.display()
            );
            break;
        }
        files.push(rel);
    }

    if filtered_out > 0 {
        tracing::info!(
            "[bm25] index filter excluded {filtered_out} files ({})",
            rules.filter.summary().unwrap_or_default()
        );
    }

    files.sort();
    files.dedup();
    Ok(files)
}

pub fn is_code_file(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    matches!(
        ext.as_str(),
        "rs" | "ts"
            | "tsx"
            | "js"
            | "jsx"
            | "py"
            | "go"
            | "java"
            | "c"
            | "cc"
            | "cpp"
            | "h"
            | "hpp"
            | "rb"
            | "cs"
            | "kt"
            | "swift"
            | "php"
            | "scala"
            | "sql"
            | "ex"
            | "exs"
            | "zig"
            | "lua"
            | "dart"
            | "vue"
            | "svelte"
    )
}
