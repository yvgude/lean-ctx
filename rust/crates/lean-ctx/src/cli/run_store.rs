// SPDX-License-Identifier: Apache-2.0
//! The records of `lean-ctx run`: one directory per run under
//! `<data dir>/runs/<id>/` with the record (`run.json`), the event log
//! (`events.jsonl`), the task text (`task.txt`, owner-only) and the agent's
//! output (`agent.log`).
//!
//! The record of a run started with `--cloud` lives twice: on the computer
//! (where the agent runs, `place: local` there) and on the machine that
//! started it (`place: computer`, refreshed over SSH). Neither copy is ever
//! sent to LeanCTX Cloud.

use std::io::{BufRead as _, Write as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The schema of `run.json`.
pub(crate) const RECORD_SCHEMA: &str = "leanctx.run/v1";
/// Most events one page carries.
pub(crate) const MAX_EVENTS_PAGE: usize = 500;
/// Longest task text accepted (bytes).
pub(crate) const MAX_TASK_BYTES: usize = 64 * 1024;

/// Where a run's agent works.
pub(crate) const PLACE_LOCAL: &str = "local";
pub(crate) const PLACE_COMPUTER: &str = "computer";

/// Run states.
pub(crate) const STATE_STARTING: &str = "starting";
pub(crate) const STATE_RUNNING: &str = "running";
pub(crate) const STATE_FINISHED: &str = "finished";
pub(crate) const STATE_FAILED: &str = "failed";
pub(crate) const STATE_STOPPED: &str = "stopped";

/// Whether `state` is final.
pub(crate) fn is_final(state: &str) -> bool {
    matches!(state, STATE_FINISHED | STATE_FAILED | STATE_STOPPED)
}

/// A run id: `run_` and 16 lowercase hex digits.
pub(crate) fn new_id() -> String {
    let hex = uuid::Uuid::new_v4().simple().to_string();
    format!("run_{}", &hex[..16])
}

/// Whether `id` is a run id (safe in a path, a ref name and a shell word).
pub(crate) fn valid_id(id: &str) -> bool {
    id.strip_prefix("run_").is_some_and(|hex| {
        (8..=32).contains(&hex.len())
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

/// The branch a run's agent commits on.
pub(crate) fn branch_of(id: &str) -> String {
    format!("leanctx/run/{id}")
}

/// One run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RunRecord {
    pub schema: String,
    pub id: String,
    /// `local` (the agent runs on this machine) or `computer`.
    pub place: String,
    /// The LeanCTX computer's name, for `place: computer`.
    pub computer: Option<String>,
    /// The project the run was started from.
    pub project_root: String,
    pub agent: String,
    /// `own` (the agent CLI's own login) or `balance` (the LeanCTX balance
    /// through the gateway).
    pub model_access: String,
    pub state: String,
    /// When the run started and ended, by this machine's clock: kept for
    /// ordering `run list`, never in a document or an event (#498).
    pub started_at: String,
    pub ended_at: Option<String>,
    pub branch: String,
    /// The commit the run started from.
    pub base: String,
    /// Commits on `branch` after `base`, oldest first.
    pub commits: Vec<String>,
    pub tool_calls: u64,
    pub exit_code: Option<i32>,
    /// `user`, `balance`, `agent_missing`, …; set for `stopped` and `failed`.
    pub stop_reason: Option<String>,
    /// Where the agent works (a git worktree), on the machine that runs it.
    pub worktree: Option<String>,
    /// The agent process (unix process group leader), while it runs.
    pub pid: Option<u32>,
    /// The systemd user unit, when the agent runs under one.
    pub unit: Option<String>,
}

impl RunRecord {
    /// The Local Read row of the run (`run.list`, `run.status`, `run.ack`):
    /// no process ids, no paths of another machine.
    pub(crate) fn view(&self) -> Value {
        let mut row = Map::new();
        let mut put = |k: &str, v: Value| {
            row.insert(k.to_owned(), v);
        };
        put("id", self.id.clone().into());
        put("place", self.place.clone().into());
        put("computer", self.computer.clone().into());
        put("project_root", self.project_root.clone().into());
        put("agent", self.agent.clone().into());
        put("model_access", self.model_access.clone().into());
        put("state", self.state.clone().into());
        put("branch", self.branch.clone().into());
        put("base", self.base.clone().into());
        put("commits", self.commits.clone().into());
        put("tool_calls", self.tool_calls.into());
        put("exit_code", self.exit_code.into());
        put("stop_reason", self.stop_reason.clone().into());
        Value::Object(row)
    }
}

/// The run directories under one root (`<data dir>/runs`).
#[derive(Debug, Clone)]
pub(crate) struct Store {
    root: PathBuf,
}

impl Store {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// `<data dir>/runs`.
    pub(crate) fn open() -> Result<Self, String> {
        let data = lean_ctx::core::data_dir::lean_ctx_data_dir()?;
        Ok(Self::new(data.join("runs")))
    }

    pub(crate) fn dir(&self, id: &str) -> PathBuf {
        self.root.join(id)
    }

    pub(crate) fn task_path(&self, id: &str) -> PathBuf {
        self.dir(id).join("task.txt")
    }

    pub(crate) fn log_path(&self, id: &str) -> PathBuf {
        self.dir(id).join("agent.log")
    }

    fn record_path(&self, id: &str) -> PathBuf {
        self.dir(id).join("run.json")
    }

    fn events_path(&self, id: &str) -> PathBuf {
        self.dir(id).join("events.jsonl")
    }

    /// Create the run's directory (owner-only) and write its task text.
    pub(crate) fn create(&self, record: &RunRecord, task: &str) -> Result<(), String> {
        if !valid_id(&record.id) {
            return Err(format!("'{}' is not a run id", record.id));
        }
        if self.record_path(&record.id).exists() {
            return Err(format!("run {} exists already", record.id));
        }
        write_private(&self.task_path(&record.id), task.as_bytes())?;
        self.save(record)
    }

    pub(crate) fn save(&self, record: &RunRecord) -> Result<(), String> {
        let mut text = serde_json::to_string_pretty(record).map_err(|e| e.to_string())?;
        text.push('\n');
        write_private(&self.record_path(&record.id), text.as_bytes())
    }

    /// The run `id`; `Ok(None)` when there is none.
    pub(crate) fn load(&self, id: &str) -> Result<Option<RunRecord>, String> {
        if !valid_id(id) {
            return Err(format!("'{id}' is not a run id (run_…)"));
        }
        let path = self.record_path(id);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
        };
        let record: RunRecord = serde_json::from_str(&text)
            .map_err(|e| format!("{} is not a run record: {e}", path.display()))?;
        if record.schema != RECORD_SCHEMA || record.id != id {
            return Err(format!("{} is not a record of run {id}", path.display()));
        }
        Ok(Some(record))
    }

    /// Every readable run, newest first (by start time, then id).
    pub(crate) fn list(&self) -> Result<Vec<RunRecord>, String> {
        let entries = match std::fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(format!("cannot read {}: {e}", self.root.display())),
        };
        let mut runs: Vec<RunRecord> = entries
            .filter_map(Result::ok)
            .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
            .filter(|name| valid_id(name))
            .filter_map(|id| self.load(&id).ok().flatten())
            .collect();
        runs.sort_by(|a, b| {
            b.started_at
                .cmp(&a.started_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(runs)
    }

    /// Append one event (`seq` is its line number, from 1) and return it.
    pub(crate) fn append_event(
        &self,
        id: &str,
        kind: &str,
        fields: Map<String, Value>,
    ) -> Result<Value, String> {
        let path = self.events_path(id);
        let seq = count_lines(&path)? + 1;
        let mut event = Map::new();
        event.insert("seq".into(), seq.into());
        event.insert("type".into(), kind.into());
        for (key, value) in fields {
            event.entry(key).or_insert(value);
        }
        let event = Value::Object(event);
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options
            .open(&path)
            .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
        writeln!(file, "{event}").map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        Ok(event)
    }

    /// The events after `since` (at most `limit`), in order.
    pub(crate) fn events(&self, id: &str, since: u64, limit: usize) -> Result<Vec<Value>, String> {
        let path = self.events_path(id);
        let file = match std::fs::File::open(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
        };
        let mut out = Vec::new();
        for line in std::io::BufReader::new(file).lines() {
            let line = line.map_err(|e| format!("cannot read {}: {e}", path.display()))?;
            let Ok(event) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if event["seq"].as_u64().is_some_and(|seq| seq > since) {
                out.push(event);
                if out.len() >= limit {
                    break;
                }
            }
        }
        Ok(out)
    }
}

fn count_lines(path: &Path) -> Result<u64, String> {
    match std::fs::read(path) {
        // `split` yields one piece more than there are newlines.
        Ok(bytes) => Ok((bytes.split(|b| *b == b'\n').count() - 1) as u64),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

/// Now, RFC 3339 in seconds.
pub(crate) fn now() -> String {
    lean_ctx_time::UtcDateTime::now().to_rfc3339_opts(lean_ctx_time::SecondsFormat::Secs, true)
}

/// Write `bytes` owner-only, through a temporary file and a rename.
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    super::super::cloud_cmd::cloud_computer_cmd::ssh::write_private(path, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: &str, started_at: &str) -> RunRecord {
        RunRecord {
            schema: RECORD_SCHEMA.into(),
            id: id.into(),
            place: PLACE_LOCAL.into(),
            computer: None,
            project_root: "/p".into(),
            agent: "claude".into(),
            model_access: "own".into(),
            state: STATE_RUNNING.into(),
            started_at: started_at.into(),
            ended_at: None,
            branch: branch_of(id),
            base: "0123456789abcdef0123456789abcdef01234567".into(),
            commits: Vec::new(),
            tool_calls: 0,
            exit_code: None,
            stop_reason: None,
            worktree: Some("/w".into()),
            pid: Some(42),
            unit: None,
        }
    }

    #[test]
    fn ids_are_checked() {
        let id = new_id();
        assert!(valid_id(&id), "{id}");
        assert!(!valid_id("run_../../x"));
        assert!(!valid_id("run_ABCDEF0123456789"));
        assert!(!valid_id("job_0123456789abcdef"));
        assert!(!valid_id("run_01"));
        assert_eq!(
            branch_of("run_0123456789abcdef"),
            "leanctx/run/run_0123456789abcdef"
        );
    }

    #[test]
    fn records_round_trip_and_list_newest_first() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::new(dir.path().join("runs"));
        let old = record("run_00000000000000aa", "2026-11-19T10:00:00Z");
        let new = record("run_00000000000000bb", "2026-11-19T11:00:00Z");
        store.create(&old, "fix the old test").expect("create");
        store.create(&new, "fix the new test").expect("create");
        assert!(
            store.create(&new, "again").is_err(),
            "a run is created once"
        );
        assert_eq!(store.load(&old.id).expect("load"), Some(old.clone()));
        assert_eq!(store.load("run_00000000000000cc").expect("load"), None);
        let ids: Vec<String> = store
            .list()
            .expect("list")
            .into_iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(ids, [new.id.clone(), old.id.clone()]);
        assert_eq!(
            std::fs::read_to_string(store.task_path(&old.id)).expect("task"),
            "fix the old test"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(store.task_path(&old.id))
                .expect("meta")
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0, "the task text is owner-only");
        }
    }

    #[test]
    fn events_are_numbered_and_paged() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::new(dir.path().to_path_buf());
        let run = record("run_00000000000000aa", "2026-11-19T10:00:00Z");
        store.create(&run, "t").expect("create");
        store
            .append_event(&run.id, "started", Map::new())
            .expect("event");
        let mut commit = Map::new();
        commit.insert("sha".into(), "abc".into());
        let event = store
            .append_event(&run.id, "commit", commit)
            .expect("event");
        assert_eq!(event["seq"], 2);
        assert_eq!(event["sha"], "abc");
        let all = store.events(&run.id, 0, 10).expect("events");
        assert_eq!(all.len(), 2);
        assert_eq!(all[0]["type"], "started");
        let later = store.events(&run.id, 1, 10).expect("events");
        assert_eq!(later.len(), 1);
        assert_eq!(later[0]["type"], "commit");
        assert_eq!(store.events(&run.id, 0, 1).expect("events").len(), 1);
    }

    #[test]
    fn the_view_leaves_out_process_facts() {
        let view = record("run_00000000000000aa", "2026-11-19T10:00:00Z").view();
        assert!(view.get("pid").is_none());
        assert!(view.get("worktree").is_none());
        assert!(view.get("unit").is_none());
        assert_eq!(view["branch"], "leanctx/run/run_00000000000000aa");
        assert!(
            view.get("started_at").is_none() && view.get("ended_at").is_none(),
            "no local clock in output (#498)"
        );
    }
}
