//! Claude Code transcript import.

use std::path::{Path, PathBuf};

use super::{ImportResult, ImportSource, home_dir, push_error, read_jsonl_files};

/// Imports Claude Code project histories from `~/.claude/projects`.
#[must_use]
pub fn import() -> ImportResult {
    let Some(home) = home_dir() else {
        return ImportResult::default();
    };
    import_from_root(&home.join(".claude/projects"))
}

/// Imports JSONL histories below a Claude Code projects directory.
///
/// This is public to keep filesystem discovery separate from transcript parsing.
#[must_use]
pub fn import_from_root(root: &Path) -> ImportResult {
    let projects = match std::fs::read_dir(root) {
        Ok(projects) => projects,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return ImportResult::default();
        }
        Err(error) => {
            let mut result = ImportResult::default();
            push_error(&mut result, format!("{}: {error}", root.display()));
            return result;
        }
    };

    let mut files = Vec::new();
    for project in projects {
        let project = match project {
            Ok(project) => project,
            Err(error) => {
                let mut result = ImportResult::default();
                push_error(&mut result, format!("{}: {error}", root.display()));
                return result;
            }
        };
        let path = project.path();
        if !path.is_dir() {
            continue;
        }
        match std::fs::read_dir(&path) {
            Ok(entries) => collect_jsonl_files(entries, &mut files),
            Err(error) => {
                let mut result = read_jsonl_files(ImportSource::ClaudeCode, root, files);
                push_error(&mut result, format!("{}: {error}", path.display()));
                return result;
            }
        }
    }
    files.sort();
    read_jsonl_files(ImportSource::ClaudeCode, root, files)
}

fn collect_jsonl_files(entries: std::fs::ReadDir, files: &mut Vec<PathBuf>) {
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file()
            && path
                .extension()
                .is_some_and(|extension| extension == "jsonl")
        {
            files.push(path);
        }
    }
}
