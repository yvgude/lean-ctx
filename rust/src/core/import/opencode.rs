//! OpenCode session-history import.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use rusqlite::{Connection, OpenFlags, params};
use serde_json::{Map, Value};

use super::{ImportResult, ImportSource, finish_result, process_value, push_error};

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
    import_from_db(
        &data_dir.join("opencode").join("opencode.db"),
        &project_root,
    )
}

/// Imports current-project sessions from an OpenCode SQLite database.
#[must_use]
pub fn import_from_db(db_path: &Path, project_root: &Path) -> ImportResult {
    if !db_path.is_file() {
        return ImportResult::default();
    }

    let mut result = ImportResult::default();
    let Ok(connection) = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
    else {
        push_error(
            &mut result,
            "OpenCode history database could not be opened".to_owned(),
        );
        return result;
    };

    let project_root = canonical_or_lexical(project_root);
    let mut session_ids = Vec::new();
    let Ok(mut statement) = connection.prepare(
        "SELECT s.id, p.worktree
         FROM session s JOIN project p ON p.id = s.project_id
         ORDER BY s.time_updated DESC, s.id
         LIMIT ?1",
    ) else {
        push_error(
            &mut result,
            "OpenCode history schema is not supported".to_owned(),
        );
        return result;
    };
    let Ok(rows) = statement.query_map([MAX_SESSION_ROWS_SCANNED], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    }) else {
        push_error(
            &mut result,
            "OpenCode sessions could not be read".to_owned(),
        );
        return result;
    };
    for row in rows {
        match row {
            Ok((id, worktree)) if canonical_or_lexical(Path::new(&worktree)) == project_root => {
                session_ids.push(id);
                if session_ids.len() >= MAX_SESSIONS as usize {
                    break;
                }
            }
            Ok(_) => {}
            Err(_) => push_error(
                &mut result,
                "OpenCode contains a malformed session row".to_owned(),
            ),
        }
    }
    drop(statement);
    result.sessions_found = session_ids.len();

    let mut touched = HashSet::new();
    let mut seen = HashSet::new();
    let mut parts_remaining = MAX_PARTS;
    for session_id in session_ids {
        if parts_remaining == 0 {
            break;
        }
        let Ok(mut statement) = connection.prepare(
            "SELECT m.data, p.data
             FROM message m JOIN part p ON p.message_id = m.id
             WHERE m.session_id = ?1
             ORDER BY p.time_created, p.id
             LIMIT ?2",
        ) else {
            push_error(
                &mut result,
                "OpenCode messages could not be read".to_owned(),
            );
            continue;
        };
        let Ok(rows) = statement.query_map(params![session_id, parts_remaining], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        }) else {
            push_error(
                &mut result,
                "OpenCode messages could not be read".to_owned(),
            );
            continue;
        };
        for row in rows {
            parts_remaining -= 1;
            let Ok(row) = row else {
                push_error(
                    &mut result,
                    "OpenCode contains a malformed history row".to_owned(),
                );
                continue;
            };
            let (Ok(message), Ok(mut part)) = (
                serde_json::from_str::<Value>(&row.0),
                serde_json::from_str::<Value>(&row.1),
            ) else {
                push_error(
                    &mut result,
                    "OpenCode contains a malformed history row".to_owned(),
                );
                continue;
            };
            if let Some(safe_part) = importable_part(&message, &mut part, &project_root) {
                process_value(
                    ImportSource::OpenCode,
                    &format!("opencode:{session_id}"),
                    &safe_part,
                    &mut result,
                    &mut touched,
                    &mut seen,
                );
            }
        }
    }
    finish_result(&mut result, &touched);
    result
}

fn importable_part(message: &Value, part: &mut Value, project_root: &Path) -> Option<Value> {
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
    Some(Value::Object(combined))
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
        assert!(result.errors.is_empty());
    }
}
