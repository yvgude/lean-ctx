// SPDX-License-Identifier: Apache-2.0
//! The on-disk log: `<root>/<project key>/<session key>.jsonl`, one
//! [`Event`] per line, appended under the project's `.lock`.
//!
//! Bounds: a session log over [`MAX_SESSION_BYTES`] keeps its newest half
//! (sequence numbers stay, so the gap shows the trim); a project keeps its
//! [`MAX_SESSIONS_PER_PROJECT`] most recently written sessions.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};

use crate::model::{
    Draft, Event, Kind, MAX_LABEL_BYTES, MAX_ROOT_BYTES, MAX_TARGET_BYTES, MAX_TEXT_BYTES, bound,
    present,
};

/// A session log larger than this keeps its newest half.
pub const MAX_SESSION_BYTES: u64 = 1024 * 1024;
/// Sessions kept per project (most recently written).
pub const MAX_SESSIONS_PER_PROJECT: usize = 64;
/// Events returned when no `limit` is given.
pub const DEFAULT_LIMIT: usize = 200;
/// The largest `limit` a listing accepts.
pub const MAX_LIMIT: usize = 1_000;

/// How long an append waits for the project lock before dropping the event.
const LOCK_TIMEOUT: Duration = Duration::from_millis(250);
/// The tail read to find the last sequence number; larger than any line.
const TAIL_BYTES: u64 = 64 * 1024;

/// What a listing selects. `session` and `project_root` narrow, both may be
/// given; with neither the listing is empty.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub session: Option<String>,
    pub project_root: Option<String>,
    /// Strictly newer than this instant.
    pub since: Option<DateTime<Utc>>,
    /// Empty = every kind.
    pub kinds: Vec<Kind>,
    /// Newest `limit` are kept (`DEFAULT_LIMIT` when 0).
    pub limit: usize,
}

/// A listing: the newest `limit` matches, sorted oldest → newest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listing {
    /// Matches before the limit.
    pub total: usize,
    pub truncated: bool,
    pub events: Vec<Event>,
}

/// The activity store under one root directory.
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

fn key(value: &str) -> String {
    lean_ctx_fs::hasher::hash_short(value)
}

fn label(value: &str) -> String {
    bound(value.trim(), MAX_LABEL_BYTES).0
}

impl Store {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn project_dir(&self, project_root: &str) -> PathBuf {
        self.root.join(key(project_root))
    }

    /// Append `draft` at `now`. `redact` masks secrets in every text field
    /// before anything is bounded or written. Returns the stored event.
    pub fn append(
        &self,
        draft: Draft,
        now: DateTime<Utc>,
        redact: &dyn Fn(&str) -> String,
    ) -> std::io::Result<Event> {
        let session_id = label(&draft.session_id);
        let project_root = bound(draft.project_root.trim(), MAX_ROOT_BYTES).0;
        if session_id.is_empty() || project_root.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "an activity event needs a session id and a project root",
            ));
        }
        let mut truncated = false;
        let mut bounded = |value: Option<String>, max: usize| {
            present(value).map(|text| {
                let (text, cut) = bound(&redact(&text), max);
                truncated |= cut;
                text
            })
        };
        let target = bounded(draft.target, MAX_TARGET_BYTES);
        let text = bounded(draft.text, MAX_TEXT_BYTES);
        let agent = label(&draft.agent);

        let dir = self.project_dir(&project_root);
        std::fs::create_dir_all(&dir)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(".lock"))?;
        lean_ctx_fs::file_lock::acquire_exclusive_timeout(&lock, LOCK_TIMEOUT)
            .map_err(|reason| std::io::Error::new(std::io::ErrorKind::TimedOut, reason))?;

        let path = dir.join(format!("{}.jsonl", key(&session_id)));
        let is_new = !path.exists();
        let event = Event {
            seq: last_seq(&path)? + 1,
            at: now.to_rfc3339_opts(SecondsFormat::Millis, true),
            session_id,
            agent: if agent.is_empty() {
                "agent".to_owned()
            } else {
                agent
            },
            project_root,
            kind: draft.kind,
            source: draft.source,
            tool: present(draft.tool).map(|tool| label(&tool)),
            target,
            outcome: draft.outcome,
            text,
            truncated,
        };
        let mut line = serde_json::to_string(&event).map_err(std::io::Error::other)?;
        line.push('\n');
        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
        file.write_all(line.as_bytes())?;
        let len = file.metadata()?.len();
        drop(file);
        if len > MAX_SESSION_BYTES {
            trim(&path)?;
        }
        if is_new {
            prune_sessions(&dir)?;
        }
        Ok(event)
    }

    /// The events `filter` selects. Read only; unreadable lines are skipped.
    pub fn list(&self, filter: &Filter) -> std::io::Result<Listing> {
        let mut files = Vec::new();
        match (&filter.session, &filter.project_root) {
            (Some(session), Some(root)) => {
                files.push(
                    self.project_dir(root)
                        .join(format!("{}.jsonl", key(session))),
                );
            }
            (Some(session), None) => {
                let name = format!("{}.jsonl", key(session));
                for dir in sorted_entries(&self.root)? {
                    files.push(dir.join(&name));
                }
            }
            (None, Some(root)) => {
                files.extend(
                    sorted_entries(&self.project_dir(root))?
                        .into_iter()
                        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl")),
                );
            }
            (None, None) => {}
        }

        let mut events = Vec::new();
        for path in files {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            for line in text.lines() {
                let Ok(event) = serde_json::from_str::<Event>(line) else {
                    continue;
                };
                if matches(&event, filter) {
                    events.push(event);
                }
            }
        }
        // Deterministic: time, then session, then sequence.
        events.sort_by(|a, b| {
            (a.at.as_str(), a.session_id.as_str(), a.seq).cmp(&(
                b.at.as_str(),
                b.session_id.as_str(),
                b.seq,
            ))
        });
        let limit = match filter.limit {
            0 => DEFAULT_LIMIT,
            n => n.min(MAX_LIMIT),
        };
        let total = events.len();
        let truncated = total > limit;
        if truncated {
            events.drain(..total - limit);
        }
        Ok(Listing {
            total,
            truncated,
            events,
        })
    }
}

fn matches(event: &Event, filter: &Filter) -> bool {
    filter
        .session
        .as_ref()
        .is_none_or(|session| &event.session_id == session)
        && filter
            .project_root
            .as_ref()
            .is_none_or(|root| &event.project_root == root)
        && (filter.kinds.is_empty() || filter.kinds.contains(&event.kind))
        && filter.since.is_none_or(|since| {
            DateTime::parse_from_rfc3339(&event.at).is_ok_and(|at| at.with_timezone(&Utc) > since)
        })
}

/// Entries of `dir` sorted by name; none when it does not exist.
fn sorted_entries(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut out: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    out.sort();
    Ok(out)
}

/// The sequence number of the last line of `path`, 0 when there is none.
fn last_seq(path: &Path) -> std::io::Result<u64> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    let len = file.metadata()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(TAIL_BYTES)))?;
    let mut tail = Vec::new();
    file.read_to_end(&mut tail)?;
    let tail = String::from_utf8_lossy(&tail);
    Ok(tail
        .lines()
        .rev()
        .find_map(|line| serde_json::from_str::<Event>(line).ok())
        .map_or(0, |event| event.seq))
}

/// Keep the newest half (by bytes) of a session log, whole lines only.
fn trim(path: &Path) -> std::io::Result<()> {
    let text = std::fs::read_to_string(path)?;
    let cut = text.len() / 2;
    let start = text[cut..].find('\n').map_or(text.len(), |i| cut + i + 1);
    lean_ctx_fs::atomic_fs::try_atomic_write(path, &text.as_bytes()[start..], None)
}

/// Keep the `MAX_SESSIONS_PER_PROJECT` most recently written session logs.
fn prune_sessions(dir: &Path) -> std::io::Result<()> {
    let mut sessions: Vec<(std::time::SystemTime, PathBuf)> = sorted_entries(dir)?
        .into_iter()
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .filter_map(|path| {
            let modified = path.metadata().and_then(|m| m.modified()).ok()?;
            Some((modified, path))
        })
        .collect();
    if sessions.len() <= MAX_SESSIONS_PER_PROJECT {
        return Ok(());
    }
    sessions.sort();
    let excess = sessions.len() - MAX_SESSIONS_PER_PROJECT;
    for (_, path) in sessions.into_iter().take(excess) {
        let _ = std::fs::remove_file(path);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Outcome, Source};
    use chrono::TimeZone;

    fn at(second: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 4, 9, 0, second)
            .single()
            .unwrap_or_default()
    }

    fn mask(text: &str) -> String {
        text.replace("sk-secret", "[REDACTED]")
    }

    fn draft(session: &str, kind: Kind) -> Draft {
        Draft::new(session, "claude", "/code/app", kind, Source::Hook)
    }

    #[test]
    fn append_numbers_redacts_and_bounds_every_event() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::new(dir.path());
        let first = store
            .append(
                draft("s1", Kind::Shell)
                    .tool("Bash")
                    .target("curl -H 'Authorization: sk-secret' x")
                    .outcome(Outcome::Ok),
                at(1),
                &mask,
            )
            .expect("append");
        assert_eq!(first.seq, 1);
        assert_eq!(first.at, "2026-11-04T09:00:01.000Z");
        assert_eq!(
            first.target.as_deref(),
            Some("curl -H 'Authorization: [REDACTED]' x")
        );
        assert!(!first.truncated);

        let long = "é".repeat(MAX_TEXT_BYTES);
        let second = store
            .append(draft("s1", Kind::AgentMessage).text(long), at(2), &mask)
            .expect("append");
        assert_eq!(second.seq, 2);
        assert!(second.truncated);
        let text = second.text.expect("text");
        assert!(text.len() <= MAX_TEXT_BYTES + '…'.len_utf8());
        assert!(text.ends_with('…'));

        let empty = store
            .append(draft("s1", Kind::TurnEnd).text("  "), at(3), &mask)
            .expect("append");
        assert_eq!(empty.text, None);

        let missing = store.append(draft(" ", Kind::TurnEnd), at(4), &mask);
        assert!(missing.is_err());
    }

    #[test]
    fn list_selects_by_session_project_kind_and_since_deterministically() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::new(dir.path());
        for (session, kind, second) in [
            ("s1", Kind::UserMessage, 1),
            ("s2", Kind::FileRead, 1),
            ("s1", Kind::FileWrite, 2),
            ("s2", Kind::TurnEnd, 3),
        ] {
            store
                .append(draft(session, kind), at(second), &mask)
                .expect("append");
        }
        let other = Draft::new("s3", "codex", "/code/other", Kind::Shell, Source::Mcp);
        store.append(other, at(1), &mask).expect("append");

        let project = Filter {
            project_root: Some("/code/app".into()),
            ..Filter::default()
        };
        let listing = store.list(&project).expect("list");
        let order: Vec<(&str, u64)> = listing
            .events
            .iter()
            .map(|e| (e.session_id.as_str(), e.seq))
            .collect();
        assert_eq!(order, [("s1", 1), ("s2", 1), ("s1", 2), ("s2", 2)]);
        assert_eq!(store.list(&project).expect("list"), listing);

        let session = Filter {
            session: Some("s3".into()),
            ..Filter::default()
        };
        let listing = store.list(&session).expect("list");
        assert_eq!(listing.events.len(), 1);
        assert_eq!(listing.events[0].project_root, "/code/other");

        let narrowed = Filter {
            since: Some(at(1)),
            kinds: vec![Kind::TurnEnd, Kind::FileRead],
            limit: 1,
            ..project
        };
        let listing = store.list(&narrowed).expect("list");
        assert_eq!(listing.total, 1);
        assert!(!listing.truncated);
        assert_eq!(listing.events[0].kind, Kind::TurnEnd);

        let limited = Filter {
            project_root: Some("/code/app".into()),
            limit: 2,
            ..Filter::default()
        };
        let listing = store.list(&limited).expect("list");
        assert_eq!((listing.total, listing.truncated), (4, true));
        assert_eq!(listing.events[1].kind, Kind::TurnEnd);

        assert!(
            store
                .list(&Filter::default())
                .expect("list")
                .events
                .is_empty()
        );
    }

    #[test]
    fn an_oversized_session_keeps_its_newest_half_and_its_numbering() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::new(dir.path());
        let text = "x".repeat(MAX_TEXT_BYTES);
        let lines = (MAX_SESSION_BYTES / MAX_TEXT_BYTES as u64) + 10;
        for _ in 0..lines {
            store
                .append(
                    draft("s1", Kind::UserMessage).text(text.clone()),
                    at(1),
                    &mask,
                )
                .expect("append");
        }
        let path = store
            .project_dir("/code/app")
            .join(format!("{}.jsonl", key("s1")));
        assert!(std::fs::metadata(&path).expect("log").len() <= MAX_SESSION_BYTES);
        let listing = store
            .list(&Filter {
                session: Some("s1".into()),
                limit: MAX_LIMIT,
                ..Filter::default()
            })
            .expect("list");
        let last = listing.events.last().expect("events");
        assert_eq!(last.seq, lines);
        assert!(listing.events[0].seq > 1);
    }
}
