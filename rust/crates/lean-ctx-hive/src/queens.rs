// SPDX-License-Identifier: Apache-2.0
//! Queen sessions: the sessions LeanCTX | Hive Desktop started as a Queen,
//! registered by the app over the 0600 local socket (an app-only call).
//!
//! A bus role is self-declared, so it never grants anything. A session may
//! set a task's priority and labels through `ctx_tasks` only when the
//! launching app's session id of its MCP server (`LEAN_CTX_SESSION_ID`) is
//! registered here. The app removes the id when the session ends.
//!
//! Stored 0600 in the Instance's data dir; at most [`MAX_QUEENS`] ids.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::HiveError;
use crate::state::write_private;

pub const QUEENS_FILE: &str = "queen-sessions.json";
pub const MAX_QUEENS: usize = 64;
const MAX_NAME_CHARS: usize = 80;

/// One registered Queen session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueenSession {
    pub session_id: String,
    pub name: Option<String>,
    /// When it was first registered (RFC 3339, UTC).
    pub since: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct QueensFile {
    sessions: Vec<QueenSession>,
}

/// 8-128 of `[A-Za-z0-9._:-]`.
pub fn validate_session_id(id: &str) -> Result<(), HiveError> {
    let ok = (8..=128).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'));
    if ok {
        Ok(())
    } else {
        Err(HiveError::Config(
            "session_id is 8-128 of [A-Za-z0-9._:-]".into(),
        ))
    }
}

fn clean_name(name: Option<&str>) -> Result<Option<String>, HiveError> {
    let Some(name) = name.map(str::trim).filter(|n| !n.is_empty()) else {
        return Ok(None);
    };
    if name.chars().count() > MAX_NAME_CHARS || name.chars().any(char::is_control) {
        return Err(HiveError::Config(
            "name is at most 80 characters, no control characters".into(),
        ));
    }
    Ok(Some(name.to_owned()))
}

fn load(dir: &Path) -> QueensFile {
    std::fs::read(dir.join(QUEENS_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save(dir: &Path, file: &QueensFile) -> Result<(), HiveError> {
    let bytes = serde_json::to_vec_pretty(file).map_err(|e| HiveError::Local(e.to_string()))?;
    write_private(dir, QUEENS_FILE, &bytes)
}

/// The registered sessions in `dir`, sorted by id.
#[must_use]
pub fn list_in(dir: &Path) -> Vec<QueenSession> {
    let mut sessions = load(dir).sessions;
    sessions.sort_by(|a, b| a.session_id.cmp(&b.session_id));
    sessions
}

/// Register `session_id` (again: it keeps its `since`, the name is updated).
pub fn register_in(
    dir: &Path,
    session_id: &str,
    name: Option<&str>,
    now: &str,
) -> Result<Vec<QueenSession>, HiveError> {
    validate_session_id(session_id)?;
    let name = clean_name(name)?;
    let mut file = load(dir);
    if let Some(known) = file
        .sessions
        .iter_mut()
        .find(|s| s.session_id == session_id)
    {
        known.name = name;
    } else {
        if file.sessions.len() >= MAX_QUEENS {
            return Err(HiveError::Refused(format!(
                "at most {MAX_QUEENS} Queen sessions are registered; remove ended ones first"
            )));
        }
        file.sessions.push(QueenSession {
            session_id: session_id.to_owned(),
            name,
            since: now.to_owned(),
        });
    }
    save(dir, &file)?;
    Ok(list_in(dir))
}

/// Remove `session_id` (no error when it is not registered).
pub fn remove_in(dir: &Path, session_id: &str) -> Result<Vec<QueenSession>, HiveError> {
    validate_session_id(session_id)?;
    let mut file = load(dir);
    let before = file.sessions.len();
    file.sessions.retain(|s| s.session_id != session_id);
    if file.sessions.len() != before {
        save(dir, &file)?;
    }
    Ok(list_in(dir))
}

/// Whether `session_id` is a registered Queen session in `dir`.
#[must_use]
pub fn is_queen_in(dir: &Path, session_id: &str) -> bool {
    validate_session_id(session_id).is_ok()
        && load(dir)
            .sessions
            .iter()
            .any(|s| s.session_id == session_id)
}

/// The Instance's data dir, where the registrations live.
pub fn data_dir() -> Result<PathBuf, HiveError> {
    lean_ctx::core::data_dir::lean_ctx_data_dir().map_err(HiveError::Local)
}

/// Whether this process runs for a registered Queen session: the launching
/// app's session id (`LEAN_CTX_SESSION_ID`) of this process is registered.
#[must_use]
pub fn this_session_is_queen() -> bool {
    let Some(id) = lean_ctx::core::activity::session_id_from_env() else {
        return false;
    };
    data_dir().is_ok_and(|dir| is_queen_in(&dir, &id))
}

/// The refusal for a session that may read but not set priority.
pub const NOT_A_QUEEN: &str = "priority and labels are set by a person or a Queen session the \
     app registered; this session can read them";

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: &str = "2026-11-05T12:00:00Z";

    #[test]
    fn a_registered_session_is_a_queen_until_removed() {
        let dir = tempfile::tempdir().expect("temp dir");
        let id = "sess-0f3a9c2e";
        assert!(!is_queen_in(dir.path(), id));
        let listed = register_in(dir.path(), id, Some("LeanCTX core"), NOW).expect("ok");
        assert_eq!(listed.len(), 1);
        assert!(is_queen_in(dir.path(), id));
        // Again: idempotent, keeps since, updates the name.
        let again = register_in(dir.path(), id, None, "2026-11-05T13:00:00Z").expect("ok");
        assert_eq!(again[0].since, NOW);
        assert_eq!(again[0].name, None);
        assert!(remove_in(dir.path(), id).expect("ok").is_empty());
        assert!(!is_queen_in(dir.path(), id));
        assert!(remove_in(dir.path(), id).expect("ok").is_empty());
    }

    #[test]
    fn a_bad_id_or_name_is_refused_and_never_matches() {
        let dir = tempfile::tempdir().expect("temp dir");
        for bad in ["short", "has space here", "semi;colon-id", &"x".repeat(129)] {
            assert!(register_in(dir.path(), bad, None, NOW).is_err(), "{bad}");
            assert!(!is_queen_in(dir.path(), bad));
        }
        assert!(register_in(dir.path(), "sess-12345678", Some("a\u{1b}[31m"), NOW).is_err());
        assert!(register_in(dir.path(), "sess-12345678", Some(&"n".repeat(81)), NOW).is_err());
        assert!(list_in(dir.path()).is_empty());
    }

    #[test]
    fn registrations_are_bounded() {
        let dir = tempfile::tempdir().expect("temp dir");
        for n in 0..MAX_QUEENS {
            register_in(dir.path(), &format!("sess-{n:08}"), None, NOW).expect("ok");
        }
        assert!(register_in(dir.path(), "sess-overflow", None, NOW).is_err());
        assert!(register_in(dir.path(), "sess-00000003", Some("again"), NOW).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_private() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().expect("temp dir");
        register_in(dir.path(), "sess-12345678", None, NOW).expect("ok");
        let mode = std::fs::metadata(dir.path().join(QUEENS_FILE))
            .expect("ok")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
