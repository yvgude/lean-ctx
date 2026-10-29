// SPDX-License-Identifier: Apache-2.0

//! OpenCode session-history import.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use rusqlite::params;
use serde_json::{Map, Value};

use super::database::{DatabaseSnapshot, snapshot_history_database};
use super::{
    AdmittedValue, ImportResult, ImportSource, SourceRefusal, finish_result, push_error,
    session_label, staging,
};

/// OpenCode's history: `<platform data dir>/opencode/opencode.db`.
const HISTORY_DIR: &str = "opencode";
const DATABASE_FILE: &str = "opencode.db";
/// What a line about the database calls it — never its path.
const DATABASE_LABEL: &str = "OpenCode history database";

const MAX_SESSIONS: i64 = 1_000;
const MAX_SESSION_ROWS_SCANNED: i64 = 10_000;
const MAX_PARTS: i64 = 10_000;

/// Imports OpenCode history for the current project.
#[must_use]
pub fn import() -> ImportResult {
    let Ok(project_root) = std::env::current_dir() else {
        return ImportResult::default();
    };
    let Some(data_dir) = dirs::data_dir() else {
        return ImportResult::default();
    };
    let history_root = data_dir.join(HISTORY_DIR);
    import_from_history(
        &history_root,
        &history_root.join(DATABASE_FILE),
        &project_root,
    )
}

/// Imports current-project sessions from an OpenCode SQLite database that
/// lies inside a named root.
///
/// `history_root` is OpenCode's data directory as the caller NAMES it (in
/// product code `<platform data dir>/opencode`, see [`import`]). `db_path`
/// is opened only if it resolves — symlinks resolved — inside that root or
/// inside the request's project, AND the path passes the decision a file
/// read gets before its file is opened (`admit_history_database`: path
/// jail, protected store, secret-like path). No authority is derived from
/// `db_path` itself. The file is then opened ONCE, checked on its descriptor
/// and read from a private snapshot of that descriptor — the path is never
/// opened again, so a file exchanged after the decision is not read
/// (`database::snapshot_history_database`). A refused database is not read:
/// the result carries the count (`sources_refused`, `sources_skipped`) and no
/// fact. None of these checks says anything about content — see
/// `read_database`.
#[must_use]
pub fn import_from_history(
    history_root: &Path,
    db_path: &Path,
    project_root: &Path,
) -> ImportResult {
    if !db_path.is_file() {
        return ImportResult::default();
    }
    let mut result = ImportResult::default();
    match snapshot_history_database(history_root, db_path) {
        Ok(snapshot) => read_database(history_root, &snapshot, project_root, result),
        Err(refusal) => {
            result.refuse(refusal);
            result
        }
    }
}

/// Test-only entry with the former signature, for a fixture database: the
/// fixture's own directory is named as the root (the test-only counterpart
/// of `admitted_sources::fixture_root`). Product code names OpenCode's data
/// directory and has no way to this function.
#[cfg(test)]
#[must_use]
pub fn import_from_db(db_path: &Path, project_root: &Path) -> ImportResult {
    let Some(fixture_root) = db_path.parent() else {
        return ImportResult::default();
    };
    import_from_history(fixture_root, db_path, project_root)
}

/// Read the sessions of `project_root` from the snapshot of a verified
/// OpenCode database. A database is not text: no byte of the file is admitted
/// as such. It is opened read-only, and the ORIGINAL column values that can
/// carry content (`message.data`, `part.data`) are admitted as wholes
/// BEFORE anything is selected from them (`AdmittedValue::admit`, G01): a
/// withheld or restricted value yields nothing, a masked one is only seen
/// masked. `importable_part` and the fact builder take admitted values
/// only. The fact built from a selection is admitted again in `push_fact`;
/// the session id is kept as a label only if it passes admission unchanged
/// (`session_label`).
///
/// The database is ONE source, all or nothing: everything its rows yield is
/// held pending and taken over only when every session and row was read
/// (`staging::read_database`, one policy snapshot for the whole read). A
/// database that fails anywhere — it does not open, its schema is not the
/// expected one, a session or history row cannot be stepped or decoded —
/// counts once (`sources_skipped`, or `sources_refused` when the policy
/// changed under the read) and contributes nothing else — no fact of an
/// earlier row, no session count — but one line: `DATABASE_LABEL` and the
/// fixed sentence of the typed refusal. Not a failure of the read, as
/// before: a row whose `data` is text but not JSON is skipped with a fixed
/// sentence, and a value that admission refuses is counted.
fn read_database(
    history_root: &Path,
    snapshot: &DatabaseSnapshot,
    project_root: &Path,
    mut result: ImportResult,
) -> ImportResult {
    let mut sessions = 0;
    let (pending, read) = staging::read_database(history_root, snapshot.source_path(), |pending| {
        sessions = read_sessions(snapshot, project_root, pending)?;
        Ok(())
    });
    let mut touched = HashSet::new();
    let mut seen = HashSet::new();
    // The fallback label is never used: every row is staged under the label
    // of its own session.
    match pending.settle(
        read,
        ImportSource::OpenCode,
        "opencode:unnamed-session",
        &mut result,
        &mut touched,
        &mut seen,
    ) {
        Ok(()) => result.sessions_found = sessions,
        // The reason, as a fixed sentence chosen by the typed refusal. No
        // path: this importer has never printed one.
        Err(refusal) => push_error(&mut result, staging::not_imported(DATABASE_LABEL, refusal)),
    }
    finish_result(&mut result, &touched);
    result
}

/// Stage what the sessions of `project_root` yield and return their number.
/// `Err` for any failure of the read — the caller then takes over nothing.
fn read_sessions(
    snapshot: &DatabaseSnapshot,
    project_root: &Path,
    pending: &mut staging::Pending,
) -> Result<usize, SourceRefusal> {
    let unreadable = |_: rusqlite::Error| SourceRefusal::DatabaseUnreadable;
    // A statement that does not prepare: the tables or columns are not there.
    let unsupported = |_: rusqlite::Error| SourceRefusal::UnsupportedSchema;
    let connection = snapshot.open().map_err(unreadable)?;

    let project_root = canonical_or_lexical(project_root);
    let mut session_ids = Vec::new();
    {
        let mut statement = connection
            .prepare(
                "SELECT s.id, p.worktree
                 FROM session s JOIN project p ON p.id = s.project_id
                 ORDER BY s.time_updated DESC, s.id
                 LIMIT ?1",
            )
            .map_err(unsupported)?;
        let rows = statement
            .query_map([MAX_SESSION_ROWS_SCANNED], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(unreadable)?;
        for row in rows {
            let (id, worktree) = row.map_err(unreadable)?;
            if canonical_or_lexical(Path::new(&worktree)) == project_root {
                session_ids.push(id);
                if session_ids.len() >= MAX_SESSIONS as usize {
                    break;
                }
            }
        }
    }

    let mut parts_remaining = MAX_PARTS;
    for session_id in &session_ids {
        if parts_remaining == 0 {
            break;
        }
        let label = session_label(ImportSource::OpenCode, session_id, pending.rules());
        pending.label(label);
        let mut statement = connection
            .prepare(
                "SELECT m.data, p.data
                 FROM message m JOIN part p ON p.message_id = m.id
                 WHERE m.session_id = ?1
                 ORDER BY p.time_created, p.id
                 LIMIT ?2",
            )
            .map_err(unsupported)?;
        let rows = statement
            .query_map(params![session_id, parts_remaining], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(unreadable)?;
        for row in rows {
            parts_remaining -= 1;
            let (message, part) = row.map_err(unreadable)?;
            let (Ok(message), Ok(part)) = (
                serde_json::from_str::<Value>(&message),
                serde_json::from_str::<Value>(&part),
            ) else {
                pending.error("OpenCode contains a malformed history row".to_owned());
                continue;
            };
            // Admission of the originals comes first; one refused original
            // ends this row (counted once), and nothing of it is selected.
            let Some(message) = pending.admit(message) else {
                continue;
            };
            let Some(part) = pending.admit(part) else {
                continue;
            };
            if let Some(safe_part) = importable_part(&message, &part, &project_root) {
                staging::select(&safe_part, |found| pending.stage(found));
            }
        }
    }
    Ok(session_ids.len())
}

/// Select the importable pieces of one admitted part. The result is derived
/// from admitted values and inherits their verdict (`AdmittedValue::derive`).
fn importable_part(
    message: &AdmittedValue,
    admitted_part: &AdmittedValue,
    project_root: &Path,
) -> Option<AdmittedValue> {
    let message = message.value();
    let part = admitted_part.value();
    let part_type = part.get("type")?.as_str()?.to_owned();
    let mut safe_part = Map::new();
    safe_part.insert("type".to_owned(), Value::String(part_type.clone()));

    match part_type.as_str() {
        "text" => {
            let text = safe_text(part.get("text")?.as_str()?)?;
            safe_part.insert("text".to_owned(), Value::String(text));
        }
        "tool" => {
            let state = part.get("state")?.as_object()?;
            let mut safe_state = Map::new();
            if let Some(error) = state
                .get("error")
                .and_then(Value::as_str)
                .and_then(safe_tool_error)
            {
                safe_state.insert("error".to_owned(), Value::String(error));
            }
            if let Some(input) = state.get("input").and_then(Value::as_object) {
                let mut safe_input = Map::new();
                for key in ["file_path", "filePath", "path", "filename", "file_name"] {
                    if let Some(path) = input
                        .get(key)
                        .and_then(Value::as_str)
                        .and_then(|path| project_relative(Path::new(path), project_root))
                    {
                        safe_input.insert(key.to_owned(), Value::String(path));
                    }
                }
                if !safe_input.is_empty() {
                    safe_state.insert("input".to_owned(), Value::Object(safe_input));
                }
            }
            if safe_state.is_empty() {
                return None;
            }
            safe_part.insert("state".to_owned(), Value::Object(safe_state));
        }
        "patch" => {
            let files = part
                .get("files")
                .and_then(Value::as_array)?
                .iter()
                .filter_map(Value::as_str)
                .filter_map(|path| project_relative(Path::new(path), project_root))
                .map(|path| serde_json::json!({ "file_path": path }))
                .collect::<Vec<_>>();
            if files.is_empty() {
                return None;
            }
            safe_part.insert("files".to_owned(), Value::Array(files));
        }
        _ => return None,
    }

    let mut combined = Map::new();
    combined.insert(
        "role".to_owned(),
        message
            .get("role")
            .cloned()
            .unwrap_or(Value::String("unknown".to_owned())),
    );
    combined.insert("part".to_owned(), Value::Object(safe_part));
    Some(admitted_part.derive(Value::Object(combined)))
}

fn safe_text(text: &str) -> Option<String> {
    let safe = text
        .split(['\n', '.', '!', '?'])
        .map(str::trim)
        .filter(|snippet| !contains_sensitive_material(snippet))
        .collect::<Vec<_>>()
        .join(". ");
    (!safe.is_empty()).then_some(safe)
}

fn safe_tool_error(error: &str) -> Option<String> {
    let error = error.trim();
    (!error.is_empty() && !contains_sensitive_material(error)).then(|| error.to_owned())
}

fn contains_sensitive_material(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let has_secret_marker = [
        "api key",
        "api-key",
        "api_key",
        "apikey",
        "access token",
        "access-token",
        "access_token",
        "authorization:",
        "bearer ",
        "password",
        "passwd",
        "private key",
        "secret",
        "token ",
        "token=",
        "token:",
    ]
    .iter()
    .any(|marker| lower.contains(marker));
    has_secret_marker || contains_absolute_path(text)
}

fn contains_absolute_path(text: &str) -> bool {
    text.char_indices().any(|(index, character)| {
        if character == '/' {
            let previous = text[..index].chars().next_back();
            let next = text[index + 1..].chars().next();
            let starts_path = previous.is_none_or(|character| {
                character.is_whitespace()
                    || matches!(character, '`' | '\'' | '"' | '(' | '[' | '{' | '=' | ':')
            });
            let is_url = previous == Some(':') && next == Some('/');
            starts_path && next.is_some_and(|character| character != '/') && !is_url
        } else {
            let candidate = &text[index..];
            looks_like_windows_absolute(candidate)
                && text[..index].chars().next_back().is_none_or(|character| {
                    character.is_whitespace()
                        || matches!(character, '`' | '\'' | '"' | '(' | '[' | '{' | '=' | ':')
                })
        }
    })
}

fn looks_like_windows_absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    (bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/'))
        || path.starts_with("\\\\")
}

/// Whether `path` resolves inside `project_root`, following symlinks.
///
/// Both sides must be canonical or the comparison is meaningless: on macOS the
/// temp tree is reached through `/var -> /private/var`, and on Windows
/// `canonicalize` returns a `\\?\` verbatim path. Canonicalising only the
/// left-hand side made every path look like an escape on those two platforms.
/// The root is canonicalised by the caller, so this stays a pure comparison.
fn existing_path_is_inside(path: &Path, project_root: &Path) -> bool {
    let mut existing = path;
    while !existing.exists() {
        let Some(parent) = existing.parent() else {
            return false;
        };
        existing = parent;
    }
    std::fs::canonicalize(existing).is_ok_and(|canonical| canonical.starts_with(project_root))
}

fn project_relative(path: &Path, project_root: &Path) -> Option<String> {
    // Self-defending: `import_from_db` already canonicalises, but this is the
    // containment check for untrusted paths out of someone else's database, so
    // it must not depend on a caller having done that. `canonical_or_lexical`
    // is idempotent on an already-canonical root.
    let project_root = &canonical_or_lexical(project_root);
    let raw = path.to_string_lossy();
    if cfg!(not(windows)) && looks_like_windows_absolute(&raw) {
        return None;
    }
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        project_root.join(path)
    };
    let normalized = lexical_normalize(&joined)?;
    if !existing_path_is_inside(&normalized, project_root) {
        return None;
    }
    let relative = normalized.strip_prefix(project_root).ok()?;
    if relative.as_os_str().is_empty() {
        return None;
    }
    Some(relative.to_string_lossy().replace('\\', "/"))
}

fn canonical_or_lexical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path)
        .ok()
        .or_else(|| lexical_normalize(path))
        .unwrap_or_else(|| path.to_path_buf())
}

fn lexical_normalize(path: &Path) -> Option<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::Normal(part) => normalized.push(part),
        }
    }
    Some(normalized)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use rusqlite::Connection;
    use tempfile::tempdir;

    use super::{contains_sensitive_material, import_from_db, project_relative};

    fn create_fixture(path: &Path, current: &Path, other: &Path) {
        let db = Connection::open(path).unwrap();
        db.execute_batch(
            "CREATE TABLE project (id TEXT PRIMARY KEY, worktree TEXT NOT NULL);
             CREATE TABLE session (id TEXT PRIMARY KEY, project_id TEXT NOT NULL, time_updated INTEGER NOT NULL);
             CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT NOT NULL, time_created INTEGER NOT NULL, data TEXT NOT NULL);
             CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT NOT NULL, session_id TEXT NOT NULL, time_created INTEGER NOT NULL, data TEXT NOT NULL);",
        ).unwrap();
        db.execute(
            "INSERT INTO project (id, worktree) VALUES ('p1', ?1), ('p2', ?2)",
            [current.to_str().unwrap(), other.to_str().unwrap()],
        )
        .unwrap();
        db.execute(
            "INSERT INTO session (id, project_id, time_updated) VALUES ('s1', 'p1', 2), ('s2', 'p2', 1)",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO message (id, session_id, time_created, data) VALUES
             ('m1', 's1', 1, '{\"role\":\"assistant\"}'), ('m2', 's2', 2, '{\"role\":\"assistant\"}')",
            [],
        ).unwrap();
        db.execute(
            "INSERT INTO part (id, message_id, session_id, time_created, data) VALUES
             ('a', 'm1', 's1', 1, '{\"type\":\"text\",\"text\":\"We decided to use a bounded cache.\"}'),
             ('b', 'm1', 's1', 2, '{\"type\":\"tool\",\"tool\":\"read\",\"state\":{\"status\":\"completed\",\"input\":{\"filePath\":\"src/lib.rs\",\"token\":\"TOP_SECRET\"},\"output\":\"PRIVATE_OUTPUT\"}}'),
             ('c', 'm1', 's1', 3, '{\"type\":\"tool\",\"tool\":\"bash\",\"state\":{\"status\":\"error\",\"error\":\"Build failed\"}}'),
             ('d', 'm1', 's1', 4, '{\"type\":\"tool\",\"tool\":\"read\",\"state\":{\"input\":{\"filePath\":\"../secret.txt\"}}}'),
             ('e', 'm2', 's2', 5, '{\"type\":\"tool\",\"tool\":\"read\",\"state\":{\"input\":{\"filePath\":\"other.txt\"}}}'),
             ('f', 'm1', 's1', 6, '{\"type\":\"patch\",\"files\":[\"src/patch.rs\",\"../outside.rs\"]}'),
             ('g', 'm1', 's1', 7, '{\"type\":\"snapshot\",\"path\":\"ignored.txt\"}'),
             ('h', 'm1', 's1', 8, 'not-json'),
             ('i', 'm1', 's1', 9, '{\"type\":\"text\",\"text\":\"We decided to use /home/alice/private/key. We chose safe fallback.\"}'),
             ('j', 'm1', 's1', 10, '{\"type\":\"tool\",\"state\":{\"error\":\"Build failed at /home/alice/private/key token=TOP_SECRET\"}}')",
            [],
        ).unwrap();
    }

    #[test]
    fn imports_only_current_project_and_filters_unsafe_paths() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let temp = tempdir().unwrap();
        let current = temp.path().join("current");
        let other = temp.path().join("other");
        std::fs::create_dir_all(current.join("src")).unwrap();
        std::fs::write(current.join("src/lib.rs"), "fixture").unwrap();
        std::fs::write(current.join("src/patch.rs"), "fixture").unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let db = temp.path().join("opencode.db");
        create_fixture(&db, &current, &other);

        let result = import_from_db(&db, &current);

        assert_eq!(result.sessions_found, 1);
        assert_eq!(result.files_touched, 2);
        assert!(
            result
                .facts
                .iter()
                .any(|fact| fact.value == "Touched file: src/lib.rs")
        );
        assert!(
            result
                .facts
                .iter()
                .any(|fact| fact.category == "imported-decision")
        );
        assert!(
            result
                .facts
                .iter()
                .any(|fact| fact.value == "Observed error: Build failed")
        );
        assert!(
            result
                .facts
                .iter()
                .any(|fact| fact.value == "Touched file: src/patch.rs")
        );
        assert!(
            result
                .errors
                .iter()
                .any(|error| error.contains("malformed"))
        );
        assert!(result.facts.iter().all(|fact| {
            ![
                "secret.txt",
                "outside.rs",
                "other.txt",
                "ignored.txt",
                "TOP_SECRET",
                "PRIVATE_OUTPUT",
                "/home/alice/private/key",
            ]
            .iter()
            .any(|private| fact.value.contains(private))
                && fact.source_session == "opencode:s1"
                && fact.imported_from.as_deref() == Some("opencode")
        }));
    }

    #[test]
    fn sensitive_material_filter_handles_punctuation_and_marker_variants() {
        for value in [
            "read `/home/user/key`",
            "read (/home/user/key)",
            "path=/home/user/key",
            "read \"/home/user/key\"",
            "API key abc",
            "api-key=abc",
            "access_token=abc",
            "token abc",
            "read `C:\\Users\\user\\key`",
        ] {
            assert!(contains_sensitive_material(value), "not rejected: {value}");
        }
        for value in [
            "We decided to use a bounded cache",
            "See https://example.com/path",
            "relative/path.rs changed",
        ] {
            assert!(
                !contains_sensitive_material(value),
                "false positive: {value}"
            );
        }
    }

    #[test]
    fn path_filter_rejects_windows_and_symlink_escapes() {
        let temp = tempdir().unwrap();
        let project = temp.path().join("project");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&outside).unwrap();

        assert_eq!(
            project_relative(Path::new("src/new.rs"), &project).as_deref(),
            Some("src/new.rs")
        );
        assert!(project_relative(Path::new("../outside/file.rs"), &project).is_none());
        assert!(project_relative(Path::new(r"C:\outside\file.rs"), &project).is_none());
        assert!(project_relative(Path::new(r"\\server\share\file.rs"), &project).is_none());

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, project.join("link")).unwrap();
            assert!(project_relative(Path::new("link/file.rs"), &project).is_none());
        }
    }

    #[test]
    fn missing_database_is_empty() {
        let temp = tempdir().unwrap();
        let result = import_from_db(&temp.path().join("missing.db"), temp.path());
        assert_eq!(result.sessions_found, 0);
        assert!(result.errors.is_empty(), "{:?}", result.errors);
    }

    /// A project with the two files the fixture's tool parts name, a second
    /// project, and OpenCode's data directory — all under one canonical base.
    fn named_history(base: &Path) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
        let current = base.join("current");
        let other = base.join("other");
        std::fs::create_dir_all(current.join("src")).unwrap();
        std::fs::write(current.join("src/lib.rs"), "fixture").unwrap();
        std::fs::write(current.join("src/patch.rs"), "fixture").unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let history = base.join("data").join("opencode");
        std::fs::create_dir_all(&history).unwrap();
        (current, other, history)
    }

    /// The database is opened only through the named-root rule; inside
    /// OpenCode's named data directory it is read, and every fact is
    /// admitted where it is built.
    #[test]
    fn a_database_inside_the_named_root_is_imported_admitted() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let temp = tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let (current, other, history) = named_history(&base);
        let db = history.join("opencode.db");
        create_fixture(&db, &current, &other);

        let result = super::import_from_history(&history, &db, &current);
        assert_eq!((result.sources_refused, result.sources_skipped), (0, 0));
        assert_eq!(result.sessions_found, 1);
        assert!(
            result
                .facts
                .iter()
                .any(|fact| fact.value.contains("bounded cache")),
            "a clean fact is imported"
        );
        assert!(
            result
                .facts
                .iter()
                .all(|fact| fact.source_session == "opencode:s1"),
            "the session id is the label"
        );
        assert_eq!(result.facts_extracted, result.facts.len());
    }

    /// No authority is derived from the database's own path: a `db_path`
    /// outside every named root is not opened — a typed, content-free
    /// reason, a count, and no fact.
    #[test]
    fn a_database_outside_every_named_root_is_not_opened() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let temp = tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let (current, other, history) = named_history(&base);
        let elsewhere = base.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        let outside = elsewhere.join("opencode.db");
        create_fixture(&outside, &current, &other);

        assert_eq!(
            crate::core::import::resolve_history_source(&history, &outside),
            Err(crate::core::import::SourceRefusal::OutsideRoot)
        );
        let result = super::import_from_history(&history, &outside, &current);
        assert_eq!(result.sources_refused, 1);
        assert_eq!(result.sessions_found, 0);
        assert!(result.facts.is_empty());
        assert!(result.errors.is_empty(), "{:?}", result.errors);
    }

    /// A link inside the named root that points outside it is judged by
    /// where it resolves: not opened.
    #[cfg(unix)]
    #[test]
    fn a_link_inside_the_root_to_a_database_outside_is_not_opened() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let temp = tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let (current, other, history) = named_history(&base);
        let elsewhere = base.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        let outside = elsewhere.join("real.db");
        create_fixture(&outside, &current, &other);
        let link = history.join("opencode.db");
        std::os::unix::fs::symlink(&outside, &link).unwrap();

        let result = super::import_from_history(&history, &link, &current);
        assert_eq!(result.sources_refused, 1);
        assert!(result.facts.is_empty());
        assert!(result.errors.is_empty(), "{:?}", result.errors);
    }

    /// An OpenCode database with one session of `project` and the given
    /// `part.data` documents, in order.
    fn minimal_database(path: &Path, project: &Path, parts: &[&str]) {
        let db = Connection::open(path).unwrap();
        db.execute_batch(
            "CREATE TABLE project (id TEXT PRIMARY KEY, worktree TEXT NOT NULL);
             CREATE TABLE session (id TEXT PRIMARY KEY, project_id TEXT NOT NULL, time_updated INTEGER NOT NULL);
             CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT NOT NULL, time_created INTEGER NOT NULL, data TEXT NOT NULL);
             CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT NOT NULL, session_id TEXT NOT NULL, time_created INTEGER NOT NULL, data TEXT NOT NULL);
             INSERT INTO session (id, project_id, time_updated) VALUES ('s1', 'p1', 1);
             INSERT INTO message (id, session_id, time_created, data) VALUES ('m1', 's1', 1, '{\"role\":\"assistant\"}');",
        )
        .unwrap();
        db.execute(
            "INSERT INTO project (id, worktree) VALUES ('p1', ?1)",
            [project.to_str().unwrap()],
        )
        .unwrap();
        for (index, part) in parts.iter().enumerate() {
            db.execute(
                "INSERT INTO part (id, message_id, session_id, time_created, data) VALUES (?1, 'm1', 's1', ?2, ?3)",
                rusqlite::params![
                    format!("part-{index}"),
                    i64::try_from(index).unwrap(),
                    part
                ],
            )
            .unwrap();
        }
    }

    fn decision_values(result: &crate::core::import::ImportResult) -> Vec<&str> {
        result
            .facts
            .iter()
            .filter(|fact| fact.category == "imported-decision")
            .map(|fact| fact.value.as_str())
            .collect()
    }

    /// G01, the order. `part.data` carries a classification marker in one
    /// field and a clean, importable sentence in another. The selector takes
    /// only the sentence — so the ORIGINAL value is admitted first, and a
    /// refused original yields no fact. (On the old order — select, then
    /// admit the fragment — the sentence was imported.)
    #[test]
    fn a_marked_part_yields_no_fact() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let temp = tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let (current, _other, history) = named_history(&base);
        let db = history.join("opencode.db");
        minimal_database(
            &db,
            &current,
            &[
                r#"{"type":"text","text":"We decided to keep the marked sentence","banner":"CONFIDENTIAL"}"#,
                r#"{"type":"text","text":"We decided to use a bounded cache"}"#,
            ],
        );

        let policy = crate::core::policy::load(
            "name = \"opencode-original-test\"\nversion = \"1.0.0\"\ndescription = \"test\"\n\
             [filters]\nclassification = \"block\"\nblocked_labels = [\"CONFIDENTIAL\"]\n",
        )
        .unwrap();
        let _policy = crate::core::policy::runtime::TestPolicyOverride::set(Some(policy));
        let result = super::import_from_history(&history, &db, &current);

        assert_eq!(
            decision_values(&result),
            vec!["We decided to use a bounded cache"]
        );
        assert_eq!(result.facts_skipped, 1, "the marked original, once");
        assert_eq!((result.sources_refused, result.sources_skipped), (0, 0));
        assert!(
            result
                .facts
                .iter()
                .all(|fact| !fact.value.contains("marked sentence"))
        );

        // All or nothing per database: a row that cannot be decoded (its
        // `data` is a BLOB) AFTER the two rows above. Nothing of the database
        // is taken over — the result is that of an import without it plus
        // its one skip and its one fixed sentence: no fact of the earlier
        // rows, no session, no counted value.
        Connection::open(&db)
            .unwrap()
            .execute(
                "INSERT INTO part (id, message_id, session_id, time_created, data)
                 VALUES ('part-z', 'm1', 's1', 99, X'00')",
                [],
            )
            .unwrap();
        let result = super::import_from_history(&history, &db, &current);
        assert!(result.facts.is_empty(), "{:?}", decision_values(&result));
        assert_eq!((result.sources_skipped, result.sources_refused), (1, 0));
        assert_eq!(
            (
                result.sessions_found,
                result.facts_extracted,
                result.files_touched,
                result.facts_skipped
            ),
            (0, 0, 0, 0)
        );
        assert_eq!(
            result.errors,
            vec!["OpenCode history database: history database could not be read; skipped"]
        );
    }

    /// A key-shaped secret next to an importable sentence: what is selected
    /// is selected from the masked value, so no fact carries the key.
    #[test]
    fn a_keyed_part_never_yields_the_key() {
        const AWS_KEY: &str = concat!("AK", "IAIOSFODNN7EXAMPLE");
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let temp = tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let (current, _other, history) = named_history(&base);
        let db = history.join("opencode.db");
        let keyed = serde_json::json!({
            "type": "text",
            "text": "We decided to deploy today",
            "note": format!("deploy id {AWS_KEY}"),
        })
        .to_string();
        minimal_database(&db, &current, &[&keyed]);

        let result = super::import_from_history(&history, &db, &current);
        assert_eq!(result.sources_refused, 0);
        assert!(
            result
                .facts
                .iter()
                .all(|fact| !fact.value.contains(AWS_KEY))
        );
        if result.facts_skipped == 0 {
            assert_eq!(decision_values(&result), vec!["We decided to deploy today"]);
        } else {
            assert_eq!(result.facts_skipped, 1);
            assert!(decision_values(&result).is_empty());
        }
    }

    /// The path decision before the database is opened: under a secret-like
    /// directory and boundary mode `enforce` it is refused like a file read
    /// — not opened, counted, no fact.
    #[test]
    fn a_database_at_a_refused_path_is_not_opened() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let temp = tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let (current, _other, history) = named_history(&base);
        let secret_dir = history.join(".aws");
        std::fs::create_dir_all(&secret_dir).unwrap();
        let db = secret_dir.join("opencode.db");
        minimal_database(
            &db,
            &current,
            &[r#"{"type":"text","text":"We decided to use a bounded cache"}"#],
        );

        crate::test_env::set_var("LEAN_CTX_IO_BOUNDARY_MODE", "enforce");
        let result = super::import_from_history(&history, &db, &current);
        crate::test_env::remove_var("LEAN_CTX_IO_BOUNDARY_MODE");

        assert_eq!(result.sources_refused, 1);
        assert_eq!(result.facts_skipped, 0);
        assert_eq!(result.sessions_found, 0);
        assert!(result.facts.is_empty());
    }
}
