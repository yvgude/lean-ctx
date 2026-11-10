// SPDX-License-Identifier: Apache-2.0
//! One project's tasks on disk: `tasks.json` (the board, written
//! crash-atomically, 0600) and `journal.jsonl` (every transition, appended
//! before the board is written, bounded). A write holds an in-process lock
//! and an exclusive file lock (`tasks.lock`, bounded wait) across
//! load → change → journal → write, so concurrent writers of any process
//! queue instead of losing each other's transitions. `notices.json` keeps
//! the journal sequence the bus has taken every notice through.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::board::{BOARD_SCHEMA, Board, Transition};
use crate::{Change, TaskError};

const BOARD_FILE: &str = "tasks.json";
const JOURNAL_FILE: &str = "journal.jsonl";
const LOCK_FILE: &str = "tasks.lock";
/// The journal sequence the bus has taken every notice through.
const NOTICES_FILE: &str = "notices.json";
/// How long a write waits for another writer.
const LOCK_WAIT: Duration = Duration::from_secs(10);
/// The journal is trimmed to its newest [`JOURNAL_KEEP`] lines past this.
const MAX_JOURNAL_BYTES: u64 = 1024 * 1024;
const JOURNAL_KEEP: usize = 2000;

/// Notices taken from the store under its locks, posted after them.
struct Due {
    /// The journal sequence the cursor moves to once they are posted.
    through: u64,
    changes: Vec<Change>,
}

/// The task store of one project directory.
#[derive(Debug, Clone)]
pub struct Store {
    dir: PathBuf,
}

fn store_error(what: &str, error: impl std::fmt::Display) -> TaskError {
    TaskError::Store(format!("local tasks: {what}: {error}"))
}

/// One in-process lock per store directory (file locks alone would make
/// threads of one process spin on each other).
fn process_lock(dir: &Path) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
    let mut map = LOCKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    map.entry(dir.to_path_buf()).or_default().clone()
}

#[cfg(unix)]
fn private() -> Option<std::fs::Permissions> {
    use std::os::unix::fs::PermissionsExt as _;
    Some(std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn private() -> Option<std::fs::Permissions> {
    None
}

impl Store {
    /// The store in `dir` (created on the first write).
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The board as written (an empty one before the first write).
    pub fn load(&self) -> Result<Board, TaskError> {
        let path = self.dir.join(BOARD_FILE);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Board::default());
            }
            Err(error) => return Err(store_error("read", error)),
        };
        let board: Board = serde_json::from_str(&text).map_err(|e| store_error("parse", e))?;
        if board.schema != BOARD_SCHEMA {
            return Err(store_error("schema", &board.schema));
        }
        Ok(board)
    }

    /// The board as of `now`: claims that ran out are given back first
    /// (a write), and their changes returned for the caller's notices.
    pub fn read(&self, now: DateTime<Utc>) -> Result<(Board, Vec<Change>), TaskError> {
        let board = self.load()?;
        if !board.has_expired(now) {
            return Ok((board, Vec::new()));
        }
        let mut current = None;
        let ((), expired) = self.write(now, |board| {
            current = Some(board.clone());
            Ok(())
        })?;
        Ok((current.unwrap_or_default(), expired))
    }

    /// Change the board under both locks. Claims that ran out by `now` are
    /// given back before `change` runs. Returns what `change` returned and
    /// every change recorded in this write (expiries first), for notices.
    pub fn write<T>(
        &self,
        now: DateTime<Utc>,
        change: impl FnOnce(&mut Board) -> Result<T, TaskError>,
    ) -> Result<(T, Vec<Change>), TaskError> {
        self.locked(|| {
            let (out, changes, _) = self.write_locked(now, change)?;
            Ok((out, changes))
        })
    }

    /// [`Store::read`], then announce every transition the bus has not
    /// taken yet (see [`Store::write_announced`]).
    pub fn read_announced(
        &self,
        now: DateTime<Utc>,
        post: impl FnOnce(&[Change]) -> bool,
    ) -> Result<(Board, Vec<Change>), TaskError> {
        let board = self.load()?;
        if board.has_expired(now) {
            let mut current = None;
            let ((), expired) = self.write_announced(
                now,
                |board| {
                    current = Some(board.clone());
                    Ok(())
                },
                post,
            )?;
            return Ok((current.unwrap_or_default(), expired));
        }
        if self
            .notice_cursor()
            .is_some_and(|posted| posted >= board.seq)
        {
            return Ok((board, Vec::new()));
        }
        let (board, due) = self.locked(|| {
            let board = self.load()?;
            self.ensure_cursor(board.seq)?;
            let due = self.due_locked(board.seq, &[]);
            Ok((board, due))
        })?;
        self.post_due(due, post);
        Ok((board, Vec::new()))
    }

    /// [`Store::write`], then `post` the transitions the bus has not taken
    /// yet: this write's and any an earlier failed post left behind (from
    /// the journal). `post` runs after both locks are released, so a slow
    /// or contended bus never holds up the next task writer. It returns
    /// whether the bus took the notices; only then does the cursor
    /// (`notices.json`) move past them, so a failed bus write is retried on
    /// the next access instead of being lost (Hive task #39). Notices are
    /// keyed by their journal sequence, so a re-post is idempotent.
    pub fn write_announced<T>(
        &self,
        now: DateTime<Utc>,
        change: impl FnOnce(&mut Board) -> Result<T, TaskError>,
        post: impl FnOnce(&[Change]) -> bool,
    ) -> Result<(T, Vec<Change>), TaskError> {
        let (out, changes, due) = self.locked(|| {
            // The cursor exists before the transition is journaled: if it
            // cannot be saved the write is refused, never left unannounced.
            self.ensure_cursor(self.load()?.seq)?;
            let (out, changes, seq) = self.write_locked(now, change)?;
            let due = self.due_locked(seq, &changes);
            Ok((out, changes, due))
        })?;
        self.post_due(due, post);
        Ok((out, changes))
    }

    /// Without a cursor (a new store, or one from before the cursor) every
    /// notice through `seq` counts as posted: saved now, before anything
    /// newer is journaled, so a later failed post always finds it.
    fn ensure_cursor(&self, seq: u64) -> Result<(), TaskError> {
        if self.notice_cursor().is_some() {
            return Ok(());
        }
        self.save_cursor(seq)
    }

    /// The transitions between the cursor and `seq` (under both locks).
    /// `fresh` (this write's changes) stands in for the journal when it
    /// covers the whole span.
    fn due_locked(&self, seq: u64, fresh: &[Change]) -> Option<Due> {
        let from = self.notice_cursor()?;
        if from >= seq {
            return None;
        }
        let changes = if fresh.first().is_some_and(|c| c.seq == from + 1) {
            fresh.to_vec()
        } else {
            self.journal()
                .iter()
                .filter(|t| t.seq > from && t.seq <= seq)
                .map(Change::from)
                .collect()
        };
        Some(Due {
            through: seq,
            changes,
        })
    }

    /// Post `due` (no lock held), then move the cursor past it if the bus
    /// took it. The cursor only moves forward: a writer that posted less
    /// than another never moves it back. A cursor that cannot be saved
    /// leaves the notices due; the next access re-posts them (idempotent).
    fn post_due(&self, due: Option<Due>, post: impl FnOnce(&[Change]) -> bool) {
        let Some(due) = due else { return };
        if !due.changes.is_empty() && !post(&due.changes) {
            return;
        }
        let _ = self.locked(|| {
            if self
                .notice_cursor()
                .is_some_and(|posted| posted >= due.through)
            {
                return Ok(());
            }
            self.save_cursor(due.through)
        });
    }

    fn save_cursor(&self, through: u64) -> Result<(), TaskError> {
        let bytes = format!("{{\"posted\":{through}}}\n");
        lean_ctx_fs::atomic_fs::try_atomic_write(
            &self.dir.join(NOTICES_FILE),
            bytes.as_bytes(),
            private().as_ref(),
        )
        .map_err(|e| store_error("notices", e))
    }

    /// The journal sequence the bus has taken every notice through.
    fn notice_cursor(&self) -> Option<u64> {
        let text = std::fs::read_to_string(self.dir.join(NOTICES_FILE)).ok()?;
        let value: serde_json::Value = serde_json::from_str(&text).ok()?;
        value.get("posted")?.as_u64()
    }

    /// Run `f` under the in-process lock and `tasks.lock` (bounded wait).
    fn locked<R>(&self, f: impl FnOnce() -> Result<R, TaskError>) -> Result<R, TaskError> {
        let lock = process_lock(&self.dir);
        let _guard = lock.lock().unwrap_or_else(PoisonError::into_inner);
        std::fs::create_dir_all(&self.dir).map_err(|e| store_error("create", e))?;
        let lock_file = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.dir.join(LOCK_FILE))
            .map_err(|e| store_error("lock", e))?;
        lean_ctx_fs::file_lock::acquire_exclusive_timeout(&lock_file, LOCK_WAIT)
            .map_err(|e| store_error("lock", e))?;
        let result = f();
        let _ = fs2::FileExt::unlock(&lock_file);
        result
    }

    /// One write under both locks; also returns the board's journal
    /// sequence after it.
    fn write_locked<T>(
        &self,
        now: DateTime<Utc>,
        change: impl FnOnce(&mut Board) -> Result<T, TaskError>,
    ) -> Result<(T, Vec<Change>, u64), TaskError> {
        let mut board = self.load()?;
        board.expire(now);
        let out = change(&mut board)?;
        let pending = std::mem::take(&mut board.pending);
        let changes = std::mem::take(&mut board.changes);
        // A renewal changes the board without a transition: always written.
        self.append_journal(&pending)?;
        let bytes = serde_json::to_vec(&board).map_err(|e| store_error("encode", e))?;
        lean_ctx_fs::atomic_fs::try_atomic_write(
            &self.dir.join(BOARD_FILE),
            &bytes,
            private().as_ref(),
        )
        .map_err(|e| store_error("write", e))?;
        Ok((out, changes, board.seq))
    }

    fn append_journal(&self, transitions: &[Transition]) -> Result<(), TaskError> {
        if transitions.is_empty() {
            return Ok(());
        }
        let path = self.dir.join(JOURNAL_FILE);
        let mut lines = String::new();
        for transition in transitions {
            let line = serde_json::to_string(transition).map_err(|e| store_error("encode", e))?;
            lines.push_str(&line);
            lines.push('\n');
        }
        let mut options = OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options.open(&path).map_err(|e| store_error("journal", e))?;
        file.write_all(lines.as_bytes())
            .map_err(|e| store_error("journal", e))?;
        drop(file);
        let size = std::fs::metadata(&path).map_or(0, |m| m.len());
        if size > MAX_JOURNAL_BYTES {
            let text = std::fs::read_to_string(&path).map_err(|e| store_error("journal", e))?;
            let all: Vec<&str> = text.lines().collect();
            let kept = all[all.len().saturating_sub(JOURNAL_KEEP)..].join("\n") + "\n";
            lean_ctx_fs::atomic_fs::try_atomic_write(&path, kept.as_bytes(), private().as_ref())
                .map_err(|e| store_error("journal", e))?;
        }
        Ok(())
    }

    /// The journal, oldest first (a line that does not parse is skipped).
    pub fn journal(&self) -> Vec<Transition> {
        std::fs::read_to_string(self.dir.join(JOURNAL_FILE))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }
}
