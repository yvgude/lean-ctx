// SPDX-License-Identifier: Apache-2.0
//! Request ledger of a serving process (#2197).
//!
//! A worker that stops answering looks the same from outside whether it is
//! idle, saturated, or stuck in one handler. The ledger tells them apart
//! without touching the request queue: it counts every admitted request and
//! exactly one outcome per request, keeps the oldest pending request's method
//! and stage, and remembers the last `tools/call` completion separately from
//! the last `initialize`.
//!
//! Privacy: only method names (`tools/call:ctx_read`, never arguments, paths
//! or content) are kept, truncated, and the pending set is bounded.
//!
//! A serving process publishes its snapshot to `<data_dir>/requests/<pid>.json`
//! from a dedicated thread, so `lean-ctx doctor` can read it even when every
//! request worker is busy. In-process, the HTTP `/v1/health` route serves the
//! same snapshot.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Pending requests whose method and stage are tracked; beyond that a request
/// is still counted, but its stage is reported as unknown.
const MAX_TRACKED_PENDING: usize = 4_096;
/// Longest method name kept (bytes, cut at a character boundary).
const MAX_METHOD_BYTES: usize = 64;
/// How often the publisher thread looks for a change.
const PUBLISH_INTERVAL: Duration = Duration::from_secs(2);
/// A live publisher rewrites its snapshot at least this often, so a reader
/// can tell a running process from a file a dead one left behind.
pub const HEARTBEAT: Duration = Duration::from_secs(10);
/// Snapshots older than this belong to a process that is gone.
pub const STALE_AFTER: Duration = Duration::from_secs(120);

/// Where a pending request is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// Admitted, not yet handed to the tool handler.
    PreDispatch,
    /// Inside the tool handler.
    Handler,
    /// The handler returned; the response is being finalized.
    Finalizing,
}

impl Stage {
    pub fn label(self) -> &'static str {
        match self {
            Self::PreDispatch => "pre-dispatch",
            Self::Handler => "handler",
            Self::Finalizing => "finalizing",
        }
    }
}

/// How a request ended. Each admitted request ends exactly once.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Completed,
    Errored,
    /// The client cancelled or disconnected before a response.
    Abandoned,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingRequest {
    pub method: String,
    pub stage: Stage,
    pub admitted_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub age_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionRecord {
    pub method: String,
    pub outcome: Outcome,
    pub finished_at_ms: u64,
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub age_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhaseRecord {
    pub name: String,
    pub running_ms: u64,
}

/// One point-in-time view of the ledger. `None` fields are unknown or did not
/// happen yet; renderers spell them out instead of guessing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RequestLedgerSnapshot {
    pub pid: u32,
    /// `mcp-stdio`, `http` or `daemon`.
    pub kind: String,
    /// The process's producer identity, distinct per start.
    pub generation: String,
    pub started_at_ms: u64,
    pub snapshot_at_ms: u64,
    pub admitted: u64,
    pub completed: u64,
    pub errored: u64,
    pub refused: u64,
    pub abandoned: u64,
    pub pending: u64,
    /// Pending requests beyond the tracked bound (their stage is unknown).
    pub untracked_pending: u64,
    pub oldest_pending: Option<PendingRequest>,
    pub last_tool_completion: Option<CompletionRecord>,
    pub last_initialize: Option<CompletionRecord>,
    pub current_phase: Option<PhaseRecord>,
    pub observations: crate::core::events::DetachedObservationStats,
}

struct Tracked {
    method: String,
    stage: Stage,
    admitted_at_ms: u64,
}

struct Ledger {
    pending: BTreeMap<u64, Tracked>,
    last_tool_completion: Option<CompletionRecord>,
    last_initialize: Option<CompletionRecord>,
}

static LEDGER: Mutex<Ledger> = Mutex::new(Ledger {
    pending: BTreeMap::new(),
    last_tool_completion: None,
    last_initialize: None,
});
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static ADMITTED: AtomicU64 = AtomicU64::new(0);
static COMPLETED: AtomicU64 = AtomicU64::new(0);
static ERRORED: AtomicU64 = AtomicU64::new(0);
static REFUSED: AtomicU64 = AtomicU64::new(0);
static ABANDONED: AtomicU64 = AtomicU64::new(0);
static STARTED_AT_MS: AtomicU64 = AtomicU64::new(0);
static SERVING: AtomicBool = AtomicBool::new(false);
static DIRTY: AtomicBool = AtomicBool::new(false);
static KIND: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();

fn ledger() -> std::sync::MutexGuard<'static, Ledger> {
    LEDGER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

/// Method names as the ledger keeps them: printable ASCII only, bounded.
fn sanitize_method(method: &str) -> String {
    method
        .chars()
        .filter(char::is_ascii_graphic)
        .take(MAX_METHOD_BYTES)
        .collect()
}

/// Mark this process as serving requests of `kind`: observations no longer
/// block dispatch (#2196) and the snapshot publisher starts. Idempotent.
pub fn start_serving(kind: &'static str) {
    crate::core::events::enable_detached_observations();
    if SERVING.swap(true, Ordering::AcqRel) {
        return;
    }
    let _ = KIND.set(kind);
    STARTED_AT_MS.store(now_ms(), Ordering::Relaxed);
    DIRTY.store(true, Ordering::Release);
    if let Some(dir) = snapshot_dir() {
        let _ = std::thread::Builder::new()
            .name("lean-ctx-request-ledger".to_string())
            .spawn(move || publish_loop(&dir));
    }
}

pub fn serving() -> bool {
    SERVING.load(Ordering::Acquire)
}

/// Count a request turned away before admission (rate or concurrency limit).
pub fn refused() {
    REFUSED.fetch_add(1, Ordering::Relaxed);
    DIRTY.store(true, Ordering::Release);
}

/// Admit one request. The returned guard records exactly one outcome: the
/// one passed to [`RequestGuard::finish`], or `Abandoned` when it is dropped
/// unfinished (the request future was cancelled or the client went away).
#[must_use = "the request is abandoned when the guard drops unfinished"]
pub fn begin(method: &str) -> RequestGuard {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let admitted_at_ms = now_ms();
    let method = sanitize_method(method);
    ADMITTED.fetch_add(1, Ordering::Relaxed);
    {
        let mut ledger = ledger();
        if ledger.pending.len() < MAX_TRACKED_PENDING {
            ledger.pending.insert(
                id,
                Tracked {
                    method: method.clone(),
                    stage: Stage::PreDispatch,
                    admitted_at_ms,
                },
            );
        }
    }
    DIRTY.store(true, Ordering::Release);
    RequestGuard {
        id,
        method,
        admitted_at_ms,
        finished: false,
    }
}

pub struct RequestGuard {
    id: u64,
    method: String,
    admitted_at_ms: u64,
    finished: bool,
}

impl RequestGuard {
    pub fn stage(&self, stage: Stage) {
        if let Some(tracked) = ledger().pending.get_mut(&self.id) {
            tracked.stage = stage;
        }
        DIRTY.store(true, Ordering::Release);
    }

    pub fn finish(mut self, outcome: Outcome) {
        self.record(outcome);
    }

    fn record(&mut self, outcome: Outcome) {
        if std::mem::replace(&mut self.finished, true) {
            return;
        }
        match outcome {
            Outcome::Completed => &COMPLETED,
            Outcome::Errored => &ERRORED,
            Outcome::Abandoned => &ABANDONED,
        }
        .fetch_add(1, Ordering::Relaxed);
        let finished_at_ms = now_ms();
        let record = CompletionRecord {
            method: std::mem::take(&mut self.method),
            outcome,
            finished_at_ms,
            duration_ms: finished_at_ms.saturating_sub(self.admitted_at_ms),
            age_ms: None,
        };
        {
            let mut ledger = ledger();
            ledger.pending.remove(&self.id);
            if record.method == "initialize" {
                ledger.last_initialize = Some(record);
            } else if record.method.starts_with("tools/call") {
                ledger.last_tool_completion = Some(record);
            }
        }
        DIRTY.store(true, Ordering::Release);
    }
}

impl Drop for RequestGuard {
    fn drop(&mut self) {
        self.record(Outcome::Abandoned);
    }
}

/// The ledger now, without ages (a snapshot is a cacheable fact; ages are
/// the reader's arithmetic, see [`RequestLedgerSnapshot::with_ages`]).
pub fn snapshot() -> RequestLedgerSnapshot {
    let (oldest_pending, tracked, last_tool_completion, last_initialize) = {
        let ledger = ledger();
        let oldest = ledger
            .pending
            .values()
            .min_by_key(|t| t.admitted_at_ms)
            .map(|t| PendingRequest {
                method: t.method.clone(),
                stage: t.stage,
                admitted_at_ms: t.admitted_at_ms,
                age_ms: None,
            });
        (
            oldest,
            ledger.pending.len() as u64,
            ledger.last_tool_completion.clone(),
            ledger.last_initialize.clone(),
        )
    };
    // Outcome counters are read after the admitted counter would race; read
    // admitted last so `pending` never underflows.
    let completed = COMPLETED.load(Ordering::Relaxed);
    let errored = ERRORED.load(Ordering::Relaxed);
    let abandoned = ABANDONED.load(Ordering::Relaxed);
    let admitted = ADMITTED.load(Ordering::Relaxed);
    let pending = admitted.saturating_sub(completed + errored + abandoned);
    RequestLedgerSnapshot {
        pid: std::process::id(),
        kind: KIND.get().copied().unwrap_or("unknown").to_string(),
        generation: crate::core::context_os::observation::producer_instance().to_string(),
        started_at_ms: STARTED_AT_MS.load(Ordering::Relaxed),
        snapshot_at_ms: now_ms(),
        admitted,
        completed,
        errored,
        refused: REFUSED.load(Ordering::Relaxed),
        abandoned,
        pending,
        untracked_pending: pending.saturating_sub(tracked),
        oldest_pending,
        last_tool_completion,
        last_initialize,
        current_phase: crate::core::phase_timing::current().map(|(name, running)| PhaseRecord {
            name: name.to_string(),
            running_ms: u64::try_from(running.as_millis()).unwrap_or(u64::MAX),
        }),
        observations: crate::core::events::detached_observation_stats(),
    }
}

impl RequestLedgerSnapshot {
    /// Fill the `age_ms` fields relative to `now_ms`.
    #[must_use]
    pub fn with_ages(mut self, now_ms: u64) -> Self {
        if let Some(pending) = self.oldest_pending.as_mut() {
            pending.age_ms = Some(now_ms.saturating_sub(pending.admitted_at_ms));
        }
        for record in [
            self.last_tool_completion.as_mut(),
            self.last_initialize.as_mut(),
        ]
        .into_iter()
        .flatten()
        {
            record.age_ms = Some(now_ms.saturating_sub(record.finished_at_ms));
        }
        self
    }

    /// One human line for `lean-ctx doctor`.
    pub fn render(&self, now_ms: u64) -> String {
        let age = |at: u64| format_age(now_ms.saturating_sub(at));
        let oldest = if self.pending == 0 {
            "none".to_string()
        } else {
            match &self.oldest_pending {
                Some(p) => format!(
                    "{} {} in {}",
                    p.method,
                    age(p.admitted_at_ms),
                    p.stage.label()
                ),
                None => "unknown (untracked)".to_string(),
            }
        };
        let last = |record: &Option<CompletionRecord>| match record {
            Some(r) => format!(
                "{} ago ({}, {}ms)",
                age(r.finished_at_ms),
                outcome_label(r.outcome),
                r.duration_ms
            ),
            None => "none yet".to_string(),
        };
        let mut line = format!(
            "pid {} ({}), up {}: admitted {} · completed {} · errored {} · refused {} · abandoned {} · pending {}; oldest pending {}; last tool completion {}; last initialize {}",
            self.pid,
            self.kind,
            age(self.started_at_ms),
            self.admitted,
            self.completed,
            self.errored,
            self.refused,
            self.abandoned,
            self.pending,
            oldest,
            last(&self.last_tool_completion),
            last(&self.last_initialize),
        );
        if let Some(phase) = &self.current_phase {
            line.push_str(&format!(
                "; phase {} {}",
                phase.name,
                format_age(phase.running_ms)
            ));
        }
        if self.observations.dropped > 0 || self.observations.failed > 0 {
            line.push_str(&format!(
                "; observations dropped {} · failed {}",
                self.observations.dropped, self.observations.failed
            ));
        }
        let snapshot_age = now_ms.saturating_sub(self.snapshot_at_ms);
        if snapshot_age > HEARTBEAT.as_millis() as u64 * 2 {
            line.push_str(&format!("; snapshot {} old", format_age(snapshot_age)));
        }
        line
    }

    /// Age of the oldest pending request at `now_ms`, if one is tracked.
    pub fn oldest_pending_age(&self, now_ms: u64) -> Option<Duration> {
        self.oldest_pending
            .as_ref()
            .map(|p| Duration::from_millis(now_ms.saturating_sub(p.admitted_at_ms)))
    }
}

fn outcome_label(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Completed => "completed",
        Outcome::Errored => "errored",
        Outcome::Abandoned => "abandoned",
    }
}

fn format_age(ms: u64) -> String {
    match ms {
        0..1_000 => format!("{ms}ms"),
        1_000..120_000 => format!("{}s", ms / 1_000),
        120_000..7_200_000 => format!("{}m", ms / 60_000),
        _ => format!("{}h", ms / 3_600_000),
    }
}

/// Milliseconds since the Unix epoch, the clock snapshots are stamped with.
pub fn epoch_ms() -> u64 {
    now_ms()
}

fn snapshot_dir() -> Option<std::path::PathBuf> {
    crate::core::data_dir::lean_ctx_data_dir()
        .ok()
        .map(|dir| dir.join("requests"))
}

fn publish_loop(dir: &std::path::Path) {
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    sweep_stale(dir, now_ms());
    let path = dir.join(format!("{}.json", std::process::id()));
    let mut last_write_ms = 0u64;
    loop {
        let now = now_ms();
        let heartbeat_due = now.saturating_sub(last_write_ms) >= HEARTBEAT.as_millis() as u64;
        if DIRTY.swap(false, Ordering::AcqRel) || heartbeat_due {
            if let Ok(bytes) = serde_json::to_vec(&snapshot()) {
                let _ = crate::core::atomic_fs::try_atomic_write(&path, &bytes, None);
            }
            last_write_ms = now;
        }
        std::thread::sleep(PUBLISH_INTERVAL);
    }
}

/// Remove snapshots whose publisher stopped long ago (its process is gone:
/// a live publisher rewrites its file every [`HEARTBEAT`]).
fn sweep_stale(dir: &std::path::Path, now: u64) {
    for snapshot in read_dir_snapshots(dir) {
        if now.saturating_sub(snapshot.1.snapshot_at_ms) > STALE_AFTER.as_millis() as u64 * 5 {
            let _ = std::fs::remove_file(&snapshot.0);
        }
    }
}

fn read_dir_snapshots(dir: &std::path::Path) -> Vec<(std::path::PathBuf, RequestLedgerSnapshot)> {
    const MAX_BYTES: u64 = 64 * 1024;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut snapshots: Vec<_> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .filter(|path| std::fs::metadata(path).is_ok_and(|m| m.len() <= MAX_BYTES))
        .filter_map(|path| {
            let text = std::fs::read_to_string(&path).ok()?;
            let snapshot = serde_json::from_str(&text).ok()?;
            Some((path, snapshot))
        })
        .collect();
    snapshots.sort_by_key(|(_, s): &(_, RequestLedgerSnapshot)| s.pid);
    snapshots
}

/// Snapshots published by serving processes that are still running, by pid.
pub fn published_snapshots() -> Vec<RequestLedgerSnapshot> {
    let Some(dir) = snapshot_dir() else {
        return Vec::new();
    };
    let now = now_ms();
    read_dir_snapshots(&dir)
        .into_iter()
        .map(|(_, snapshot)| snapshot)
        .filter(|s| now.saturating_sub(s.snapshot_at_ms) <= STALE_AFTER.as_millis() as u64)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counts() -> (u64, u64, u64, u64) {
        (
            ADMITTED.load(Ordering::Relaxed),
            COMPLETED.load(Ordering::Relaxed),
            ERRORED.load(Ordering::Relaxed),
            ABANDONED.load(Ordering::Relaxed),
        )
    }

    /// Acceptance of #2197: completion, error and disconnect each update
    /// their own counter exactly once, the oldest pending request shows its
    /// method and stage, and the last tool completion is kept apart from the
    /// last initialize.
    #[test]
    fn each_request_path_counts_its_outcome_exactly_once() {
        if crate::test_env::run_in_own_process(
            "core::request_ledger::tests::each_request_path_counts_its_outcome_exactly_once",
            &[],
        ) {
            return;
        }
        let init = begin("initialize");
        init.finish(Outcome::Completed);

        let stuck = begin("tools/call:ctx_read");
        stuck.stage(Stage::Handler);
        std::thread::sleep(Duration::from_millis(5));
        let completed = begin("tools/call:ctx_search");
        completed.stage(Stage::Handler);
        completed.stage(Stage::Finalizing);
        completed.finish(Outcome::Completed);
        begin("tools/call:ctx_shell").finish(Outcome::Errored);
        // A cancelled request future drops its guard without finishing.
        drop(begin("tools/call:ctx_tree"));
        refused();

        assert_eq!(counts(), (5, 2, 1, 1));
        let snap = snapshot();
        assert_eq!(snap.pending, 1);
        assert_eq!(snap.refused, 1);
        assert_eq!(snap.untracked_pending, 0);
        let oldest = snap.oldest_pending.clone().expect("pending tracked");
        assert_eq!(
            (oldest.method.as_str(), oldest.stage),
            ("tools/call:ctx_read", Stage::Handler)
        );
        let tool = snap.last_tool_completion.clone().expect("tool completion");
        // The abandoned call is the most recent tools/call outcome.
        assert_eq!(tool.method, "tools/call:ctx_tree");
        assert_eq!(tool.outcome, Outcome::Abandoned);
        assert_eq!(
            snap.last_initialize.as_ref().map(|r| r.outcome),
            Some(Outcome::Completed)
        );

        let now = oldest.admitted_at_ms + 74_000;
        let line = snap.clone().render(now);
        assert!(
            line.contains("oldest pending tools/call:ctx_read 74s in handler"),
            "{line}"
        );
        assert!(
            line.contains(
                "admitted 5 · completed 2 · errored 1 · refused 1 · abandoned 1 · pending 1"
            ),
            "{line}"
        );
        assert_eq!(
            snap.with_ages(now).oldest_pending.and_then(|p| p.age_ms),
            Some(74_000)
        );

        stuck.finish(Outcome::Completed);
        assert_eq!(counts(), (5, 3, 1, 1));
        assert_eq!(snapshot().pending, 0);
    }

    #[test]
    fn method_names_are_bounded_printable_identifiers() {
        let long = format!("tools/call:{}\n{}", "x".repeat(200), "secret path");
        let kept = sanitize_method(&long);
        assert!(kept.len() <= MAX_METHOD_BYTES);
        assert!(kept.chars().all(|c| c.is_ascii_graphic()));
    }
}
