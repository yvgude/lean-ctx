// SPDX-License-Identifier: Apache-2.0
//! Session read set: which files a LeanCTX session actually read, relative
//! to one project root.
//!
//! This is the `observed_reads` pillar of the ECCP change envelope
//! (`docs/contracts/change-envelope-v1.md`): only LeanCTX knows what an agent
//! read while it produced a change. `lean-ctx session readset --json` prints
//! [`SessionReadSet`]; `scripts/eccp-envelope write --observed-reads-from-session`
//! records it.
//!
//! The source is the persisted session (`files_touched`). A session keeps a
//! bounded number of files; when older entries were evicted the read set is
//! marked `complete: false`, so a consumer never mistakes a truncated set for
//! the whole one.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use crate::core::session::SessionState;

/// Schema id of the JSON printed by `lean-ctx session readset --json`.
pub const SCHEMA: &str = "leanctx.session-readset/v1";

/// One file the session read, merged across the selected sessions.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct ReadEntry {
    /// Path relative to the project root, `/`-separated.
    pub path: String,
    /// How often the file was read (sum over sessions).
    pub read_count: u32,
    /// The session also wrote the file.
    pub modified: bool,
    /// Read mode of the last read (`full`, `map`, `lines:N-M`, ...).
    pub last_mode: String,
}

/// The read set of one or more sessions of a project.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct SessionReadSet {
    pub schema: &'static str,
    /// Session ids the set was taken from, in the order given.
    pub sessions: Vec<String>,
    /// Files under the project root, sorted by path.
    pub reads: Vec<ReadEntry>,
    /// Reads outside the project root; left out of `reads`.
    pub outside_root: usize,
    /// False when a session evicted older file entries (bounded history).
    pub complete: bool,
}

/// Errors of [`load`].
#[derive(Debug, PartialEq, Eq)]
pub enum ReadSetError {
    /// No saved session for the project root (or the given id is unknown).
    NoSession(String),
    /// The session with this id belongs to another project root.
    OtherProject(String),
}

impl std::fmt::Display for ReadSetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoSession(what) => write!(f, "no saved LeanCTX session {what}"),
            Self::OtherProject(id) => write!(f, "session {id} belongs to another project root"),
        }
    }
}

impl std::error::Error for ReadSetError {}

/// Loads the sessions (the latest one for `root` when `ids` is empty) and
/// returns their read set relative to `root`.
pub fn load(root: &Path, ids: &[String]) -> Result<SessionReadSet, ReadSetError> {
    let root_text = root.to_string_lossy();
    let sessions = if ids.is_empty() {
        vec![
            SessionState::load_latest_for_project_root(&root_text)
                .ok_or_else(|| ReadSetError::NoSession(format!("for {root_text}")))?,
        ]
    } else {
        ids.iter()
            .map(
                |id| match SessionState::load_by_id_for_project_root(id, &root_text) {
                    Ok(Some(session)) => Ok(session),
                    Ok(None) => Err(ReadSetError::NoSession(id.clone())),
                    Err(_) => Err(ReadSetError::OtherProject(id.clone())),
                },
            )
            .collect::<Result<Vec<_>, _>>()?
    };
    Ok(from_sessions(root, &sessions))
}

/// The read set of `sessions` relative to `root`. Pure: no disk access
/// beyond canonicalizing `root`.
pub fn from_sessions(root: &Path, sessions: &[SessionState]) -> SessionReadSet {
    let roots = root_spellings(root);
    let mut merged: BTreeMap<String, ReadEntry> = BTreeMap::new();
    let mut outside_root = 0;
    let mut complete = true;
    for session in sessions {
        let kept: u64 = session
            .files_touched
            .iter()
            .map(|file| u64::from(file.read_count))
            .sum();
        if u64::from(session.stats.files_read) > kept {
            complete = false;
        }
        for file in &session.files_touched {
            let Some(path) = relative_to(&roots, &file.path) else {
                outside_root += 1;
                continue;
            };
            merged
                .entry(path.clone())
                .and_modify(|entry| {
                    entry.read_count = entry.read_count.saturating_add(file.read_count);
                    entry.modified |= file.modified;
                    entry.last_mode.clone_from(&file.last_mode);
                })
                .or_insert_with(|| ReadEntry {
                    path,
                    read_count: file.read_count,
                    modified: file.modified,
                    last_mode: file.last_mode.clone(),
                });
        }
    }
    SessionReadSet {
        schema: SCHEMA,
        sessions: sessions.iter().map(|s| s.id.clone()).collect(),
        reads: merged.into_values().collect(),
        outside_root,
        complete,
    }
}

fn root_spellings(root: &Path) -> Vec<PathBuf> {
    let canonical = crate::core::pathutil::safe_canonicalize_or_self(root);
    if canonical == root {
        vec![root.to_path_buf()]
    } else {
        vec![root.to_path_buf(), canonical]
    }
}

/// `path` relative to one of `roots`, `/`-separated; `None` outside them.
/// A relative path is taken as relative to the root already.
fn relative_to(roots: &[PathBuf], path: &str) -> Option<String> {
    let path = Path::new(path);
    let relative = if path.is_absolute() {
        roots.iter().find_map(|root| path.strip_prefix(root).ok())?
    } else {
        path
    };
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_str()?.to_string()),
            Component::CurDir => {}
            _ => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str, files: &[(&str, &str)]) -> SessionState {
        let mut s = SessionState::new();
        s.id = id.to_string();
        for (path, mode) in files {
            s.touch_file(path, None, mode, 10);
        }
        s
    }

    #[test]
    fn read_set_is_relative_merged_and_sorted() {
        let root = Path::new("/nonexistent-readset-root/repo");
        let mut a = session(
            "a",
            &[
                ("/nonexistent-readset-root/repo/src/b.rs", "full"),
                ("/nonexistent-readset-root/repo/src/a.rs", "map"),
                ("/etc/hosts", "full"),
                ("/nonexistent-readset-root/repo/../other/x.rs", "full"),
            ],
        );
        a.mark_modified("/nonexistent-readset-root/repo/src/a.rs");
        let b = session(
            "b",
            &[
                ("src/a.rs", "lines:1-9"),
                ("./docs/c.md", "full"),
                ("../escape.rs", "full"),
            ],
        );

        let set = from_sessions(root, &[a, b]);
        assert_eq!(set.schema, SCHEMA);
        assert_eq!(set.sessions, vec!["a", "b"]);
        assert_eq!(set.outside_root, 3);
        assert!(set.complete);
        let paths: Vec<&str> = set.reads.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(paths, vec!["docs/c.md", "src/a.rs", "src/b.rs"]);
        let a_rs = &set.reads[1];
        assert_eq!(a_rs.read_count, 2);
        assert!(a_rs.modified);
        assert_eq!(a_rs.last_mode, "lines:1-9");
    }

    #[test]
    fn evicted_history_marks_the_set_incomplete() {
        let files: Vec<(String, &str)> = (0..60).map(|i| (format!("f{i:02}.rs"), "full")).collect();
        let refs: Vec<(&str, &str)> = files.iter().map(|(p, m)| (p.as_str(), *m)).collect();
        let set = from_sessions(
            Path::new("/nonexistent-readset-root"),
            &[session("s", &refs)],
        );
        assert!(!set.complete);
        assert!(set.reads.len() < 60);
    }
}
