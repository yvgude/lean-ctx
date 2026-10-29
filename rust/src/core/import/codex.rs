//! Codex session-history import.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, types::ValueRef};
use serde_json::Value;

use super::database::{DatabaseSnapshot, snapshot_history_database};
use super::{
    ImportResult, ImportSource, SourceRefusal, finish_result, home_dir, push_error, read_json_file,
    read_jsonl_files, session_id, staging,
};

const MAX_SQLITE_ROWS_PER_TABLE: usize = 10_000;
const MAX_SCAN_DEPTH: usize = 8;

/// Imports Codex history and session files from `~/.codex`.
#[must_use]
pub fn import() -> ImportResult {
    let Some(home) = home_dir() else {
        return ImportResult::default();
    };
    import_from_root(&home.join(".codex"))
}

/// Imports Codex JSONL, JSON, and SQLite session data from a Codex directory.
#[must_use]
pub fn import_from_root(root: &Path) -> ImportResult {
    if !root.exists() {
        return ImportResult::default();
    }

    let mut jsonl = Vec::new();
    let mut json = Vec::new();
    let mut sqlite = Vec::new();
    let mut errors = Vec::new();
    collect_files(root, 0, &mut jsonl, &mut json, &mut sqlite, &mut errors);
    jsonl.sort();
    json.sort();
    sqlite.sort();

    let mut result = read_jsonl_files(ImportSource::Codex, root, jsonl);
    for path in json {
        result.merge(read_json_file(ImportSource::Codex, root, &path));
    }
    for path in sqlite {
        // A database is not text the admitted reader can read; its root
        // rule is applied here, and every fact mined from its rows is
        // admitted where it is built (`push_fact`).
        // Root containment, then the path decision a file read gets, then ONE
        // open of that file: it is checked on its descriptor and read from a
        // private snapshot of that descriptor, never from its path again
        // (`database`). Content is admitted value by value in
        // `read_sqlite_file`, and the database is one source: all of it, or
        // nothing but its one refusal count.
        match snapshot_history_database(root, &path) {
            Ok(snapshot) => result.merge(read_sqlite_file(root, &snapshot)),
            Err(refusal) => result.refuse(refusal),
        }
    }
    for error in errors {
        push_error(&mut result, error);
    }
    result
}

fn collect_files(
    dir: &Path,
    depth: usize,
    jsonl: &mut Vec<PathBuf>,
    json: &mut Vec<PathBuf>,
    sqlite: &mut Vec<PathBuf>,
    errors: &mut Vec<String>,
) {
    if depth > MAX_SCAN_DEPTH {
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            errors.push(format!("{}: {error}", dir.display()));
            return;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                errors.push(format!("{}: {error}", dir.display()));
                continue;
            }
        };
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, depth + 1, jsonl, json, sqlite, errors);
        } else if path.is_file() {
            match path.extension().and_then(|extension| extension.to_str()) {
                Some("jsonl") => jsonl.push(path),
                Some("json") if is_named_session_file(&path) => json.push(path),
                Some("sqlite" | "sqlite3" | "db") => sqlite.push(path),
                _ => {}
            }
        }
    }
}

fn is_named_session_file(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let name = name.to_ascii_lowercase();
    ["history", "session", "rollout", "transcript"]
        .iter()
        .any(|marker| name.contains(marker))
}

/// One history database, as ONE source: everything its tables yield is held
/// pending and taken over only when every table and row was read
/// (`staging::read_database`, one policy snapshot for the whole read). A
/// database that fails anywhere counts once (`sources_skipped`, or
/// `sources_refused` when the policy changed under the read) and contributes
/// nothing else — no fact of an earlier row or table — but one line: its path
/// and the fixed sentence of the typed refusal. Never the engine's message:
/// that names the private snapshot or quotes the schema.
fn read_sqlite_file(root: &Path, snapshot: &DatabaseSnapshot) -> ImportResult {
    // The history path names the session; what is opened is the snapshot of
    // the verified descriptor.
    let path = snapshot.source_path();
    let (pending, read) =
        staging::read_database(root, path, |pending| read_rows(snapshot, pending));
    let session = session_id(ImportSource::Codex, path, pending.rules());
    let mut result = ImportResult::default();
    let mut touched = HashSet::new();
    let mut seen = HashSet::new();
    match pending.settle(
        read,
        ImportSource::Codex,
        &session,
        &mut result,
        &mut touched,
        &mut seen,
    ) {
        Ok(()) => result.sessions_found = 1,
        // The reason, as a fixed sentence chosen by the typed refusal, under
        // the label this importer has always printed for a database.
        Err(refusal) => {
            let label = path.display().to_string();
            push_error(&mut result, staging::not_imported(&label, refusal));
        }
    }
    finish_result(&mut result, &touched);
    result
}

/// Stage what every TEXT column value of every table yields. `Err` for any
/// failure of the read — the caller then takes over nothing.
fn read_rows(
    snapshot: &DatabaseSnapshot,
    pending: &mut staging::Pending,
) -> Result<(), SourceRefusal> {
    let unreadable = |_: rusqlite::Error| SourceRefusal::DatabaseUnreadable;
    let connection = snapshot.open().map_err(unreadable)?;
    for table in tables(&connection).map_err(unreadable)? {
        let query = format!(
            "SELECT * FROM {} LIMIT {MAX_SQLITE_ROWS_PER_TABLE}",
            quote_identifier(&table)
        );
        let mut statement = connection.prepare(&query).map_err(unreadable)?;
        let columns = statement.column_count();
        let mut rows = statement.query([]).map_err(unreadable)?;
        while let Some(row) = rows.next().map_err(unreadable)? {
            for column in 0..columns {
                let Ok(ValueRef::Text(bytes)) = row.get_ref(column) else {
                    continue;
                };
                let text = String::from_utf8_lossy(bytes);
                let original = serde_json::from_str::<Value>(&text)
                    .unwrap_or_else(|_| Value::String(text.into_owned()));
                // The ORIGINAL column value is admitted as a whole before
                // anything is selected from it; a refused value yields
                // nothing (counted when the database is taken over).
                let Some(value) = pending.admit(original) else {
                    continue;
                };
                staging::select(&value, |found| pending.stage(found));
            }
        }
    }
    Ok(())
}

fn tables(connection: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut statement = connection.prepare(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
    )?;
    statement
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()
}

fn quote_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('\"', "\"\""))
}
