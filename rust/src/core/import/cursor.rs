//! Cursor agent-transcript import.

use std::path::{Path, PathBuf};

use super::{ImportResult, ImportSource, home_dir, push_error, read_jsonl_files};

/// Imports Cursor transcripts from `~/.cursor/projects/*/agent-transcripts`.
#[must_use]
pub fn import() -> ImportResult {
    let Some(home) = home_dir() else {
        return ImportResult::default();
    };
    import_from_root(&home.join(".cursor/projects"))
}

/// Imports agent transcript JSONL files from a Cursor projects directory.
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
    let mut errors = Vec::new();
    for project in projects.flatten() {
        let transcript_dir = project.path().join("agent-transcripts");
        if !transcript_dir.is_dir() {
            continue;
        }
        match std::fs::read_dir(&transcript_dir) {
            Ok(entries) => collect_jsonl_files(entries, &mut files),
            Err(error) => errors.push(format!("{}: {error}", transcript_dir.display())),
        }
    }
    files.sort();
    let mut result = read_jsonl_files(ImportSource::Cursor, root, files);
    for error in errors {
        push_error(&mut result, error);
    }
    result
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
