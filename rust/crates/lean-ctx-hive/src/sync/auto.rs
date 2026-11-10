// SPDX-License-Identifier: Apache-2.0
//! Background vault sync (3.12.3): the daemon syncs every shared vault of
//! this machine without a manual `lean-ctx hive sync`.
//!
//! One `hive-sync` thread per machine, started beside the scheduler
//! ([`super::super::scheduler::spawn`]). A vault syncs when
//! - its local data changed (the project's knowledge journal moved, or its
//!   replica was written: docs, task and knowledge events queued there),
//!   `DEBOUNCE` after the last change, at the latest `MAX_HOLD` after the
//!   first one of a stream of changes;
//! - Hive said its log or trust advanced (`log.advanced`,
//!   `trust.advanced` on the bus, [`nudge_place`]); a purge at once;
//! - or `[hive] pull_interval_minutes` passed since its last attempt (the
//!   fallback that works without any bus frame).
//!
//! The process that wrote a fact pushes it itself within about a second
//! ([`super::on_write`]); this thread is the fallback for writes of other
//! processes and the receiving side of a bus hint.
//!
//! Bounded: one sync at a time, at most `MAX_PER_PASS` vaults per pass,
//! at most one attempt per `MIN_SPACING` and, after a burst of `BURST_MAX`,
//! one per `BURST_EVERY` per vault (a purge excepted), and a failed vault
//! backs off
//! exponentially up to `BACKOFF_MAX`, or as long as Hive's `Retry-After`
//! asks. The burst cap and the backoff are shared with every other process
//! of the machine that syncs ([`super::budget`]). Offline only backs off:
//! pending events stay in the replica and go out with the next sync that
//! reaches Hive.
//!
//! Never unshared data: only vaults Hive already resolved for this machine
//! (`projects.json`) sync, only against a C0 2.x Hive, and this thread never
//! maps a new project (no resolve); capture keeps its gate and screening.
//!
//! The thread writes `<data>/hive/sync/status.json` (kind `hive.sync`).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::HiveError;
use crate::config::HiveConfig;
use crate::state::{HiveStore, write_private};
use crate::vault::{VaultMap, VaultRecord};

/// How often the thread looks for local changes.
const POLL: Duration = Duration::from_secs(2);
/// Quiet time after the last local change before a sync.
pub(crate) const DEBOUNCE: Duration = Duration::from_secs(1);
/// A stream of local changes syncs at the latest this long after its first
/// change, quiet or not.
pub(crate) const MAX_HOLD: Duration = Duration::from_secs(5);
/// Least time between two attempts on one vault (a purge excepted). It was
/// 30 s, which held a fact or a bus hint up to 30 s after the vault's last
/// sync: past the 10 s a fact needs to reach another Instance (#270).
pub(crate) const MIN_SPACING: Duration = Duration::from_secs(2);
/// The burst budget of one vault (a purge excepted): a burst of up to
/// `BURST_MAX` changes or hints is answered at once, a steady stream is
/// held to `BURST_MAX` attempts per `BURST_WINDOW`, one per `BURST_EVERY`
/// ([`burst_take`]).
pub(crate) const BURST_MAX: usize = 6;
/// The time an empty burst budget takes to fill again.
pub(crate) const BURST_WINDOW: Duration = Duration::from_secs(60);
/// After a spent burst, one more attempt per this long.
pub(crate) const BURST_EVERY: Duration =
    Duration::from_secs(BURST_WINDOW.as_secs() / BURST_MAX as u64);

/// The burst budget, a token bucket (GCRA): `BURST_MAX` turns, one back
/// every `BURST_EVERY`. Its whole state is `ahead`, how long from now
/// until the bucket is full again (zero: full). Returns the `ahead` after
/// one more attempt when the budget lets it start now (a purge always
/// starts and is still charged), `None` when it is spent.
///
/// A spent burst holds a vault for `BURST_EVERY` at most, never until the
/// oldest attempt has left a whole `BURST_WINDOW`: that blackout of up to a
/// minute kept a vault from pulling a peer's change in time. The state is
/// kept, not rebuilt from the attempts within the last `BURST_WINDOW`:
/// rebuilt, the bucket refilled as the first burst aged out and a flood
/// got a second full burst every window (11 attempts a minute, not 6;
/// review of 5!235). `ahead` is cut to an empty bucket's refill, so a run
/// of purges or a stepped clock holds a vault no longer than that.
pub(crate) fn burst_take(ahead: Duration, purge: bool) -> Option<Duration> {
    let ahead = ahead.min(BURST_WINDOW);
    let spent = ahead > BURST_WINDOW.saturating_sub(BURST_EVERY);
    (purge || !spent).then(|| (ahead + BURST_EVERY).min(BURST_WINDOW))
}

const BACKOFF_FIRST: Duration = Duration::from_secs(30);
pub(crate) const BACKOFF_MAX: Duration = Duration::from_mins(30);
/// The longest any vault is held after a failure: the longest backoff
/// (`BACKOFF_MAX` plus 20 % jitter). It also caps Hive's `Retry-After`
/// and any hold the shared budget file keeps, so a server asking for
/// hours or a wall clock stepped back cannot stop every process for longer.
pub(crate) const WAIT_MAX: Duration = Duration::from_secs(BACKOFF_MAX.as_secs() / 5 * 6);
/// Vaults synced in one pass at most; the rest follow in the next pass.
pub(crate) const MAX_PER_PASS: usize = 16;
/// How often the status file is rewritten while nothing happens, so a
/// reader can tell a live thread from a stale file.
const HEARTBEAT: Duration = Duration::from_secs(30);
/// A status older than this many seconds is shown as `running: false`.
const STALE_AFTER_S: i64 = 120;

const SYNC_DIR: &str = "sync";
const STATUS_FILE: &str = "status.json";
const STATUS_LOCK: &str = "status.lock";

/// What started a sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Trigger {
    Local,
    Bus,
    /// `log.advanced` with priority `purge`: no debounce, no spacing.
    BusPurge,
    Periodic,
    Manual,
}

impl Trigger {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Bus | Self::BusPurge => "bus",
            Self::Periodic => "periodic",
            Self::Manual => "manual",
        }
    }
}

// ---------------------------------------------------------------------------
// Wake-ups from the bus (same process: the daemon runs the relay too).

static WANTS: Mutex<BTreeMap<String, Trigger>> = Mutex::new(BTreeMap::new());
static WAKE: Condvar = Condvar::new();
static STOPPED: AtomicBool = AtomicBool::new(false);
static RUNNING: AtomicBool = AtomicBool::new(false);

fn wants() -> MutexGuard<'static, BTreeMap<String, Trigger>> {
    WANTS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The vault id of a bus place (`vault:<uuid>`); `None` for other places.
pub(crate) fn vault_of_place(place: &str) -> Option<String> {
    let id = place.strip_prefix("vault:")?;
    (!id.is_empty()).then(|| super::place::place_id(id))
}

/// Hive announced a change of `place` (`log.advanced`, `trust.advanced`):
/// sync that vault soon. A hint only: the sync verifies what it fetches.
pub fn nudge_place(place: &str, trigger: Trigger) {
    let Some(vault) = vault_of_place(place) else {
        return;
    };
    {
        let mut pending = wants();
        let slot = pending.entry(vault).or_insert(trigger);
        // A purge wins over a plain advance.
        if trigger == Trigger::BusPurge {
            *slot = trigger;
        }
    }
    WAKE.notify_all();
}

/// End the thread (the scheduler stopped: disconnected, revoked, lapsed).
pub fn stop() {
    STOPPED.store(true, Ordering::SeqCst);
    WAKE.notify_all();
}

fn wait(timeout: Duration) -> BTreeMap<String, Trigger> {
    let mut pending = wants();
    if pending.is_empty() && !STOPPED.load(Ordering::SeqCst) {
        pending = WAKE
            .wait_timeout(pending, timeout)
            .unwrap_or_else(PoisonError::into_inner)
            .0;
    }
    std::mem::take(&mut *pending)
}

/// How long the thread may sleep at `now`: until the spacing or debounce of
/// a vault with a hint or a local change waiting ends, at most `POLL`. A
/// hint that came during the spacing goes out when the spacing ends, not
/// up to a whole `POLL` later. Anything held for another reason (burst,
/// backoff, the shared budget) is looked at again after `POLL`.
pub(crate) fn next_wake<'a>(dues: impl IntoIterator<Item = &'a Due>, now: Instant) -> Duration {
    let mut wake = POLL;
    for due in dues {
        let spaced = due.last_attempt.map(|last| last + MIN_SPACING);
        let mut at = None;
        if due.urgent.is_some() {
            at = spaced;
        }
        if let Some(changed) = due.last_change {
            let quiet = changed + DEBOUNCE;
            let ready = spaced.map_or(quiet, |s| s.max(quiet));
            at = Some(at.map_or(ready, |a: Instant| a.min(ready)));
        }
        if let (Some(a), Some(retry)) = (at, due.retry_at) {
            at = Some(a.max(retry));
        }
        if let Some(at) = at.filter(|at| *at > now) {
            wake = wake.min(at - now);
        }
    }
    wake
}

// ---------------------------------------------------------------------------
// Scheduling decisions (pure, tested).

/// What the thread knows of one vault between passes.
#[derive(Debug, Clone, Default)]
pub(crate) struct Due {
    pub last_attempt: Option<Instant>,
    /// When the burst budget is full again ([`burst_take`]).
    pub full_at: Option<Instant>,
    /// The newest local change not synced yet.
    pub last_change: Option<Instant>,
    /// The first local change not synced yet.
    pub first_change: Option<Instant>,
    pub urgent: Option<Trigger>,
    pub retry_at: Option<Instant>,
}

impl Due {
    /// Whether the spacing and the burst budget let an attempt run `now`.
    fn spaced(&self, now: Instant) -> bool {
        if self
            .last_attempt
            .is_some_and(|last| now.saturating_duration_since(last) < MIN_SPACING)
        {
            return false;
        }
        burst_take(self.ahead(now), false).is_some()
    }

    /// How long from `now` until the burst budget is full again.
    fn ahead(&self, now: Instant) -> Duration {
        self.full_at
            .map_or(Duration::ZERO, |full| full.saturating_duration_since(now))
    }

    /// Record an attempt at `now` (a purge too: it is charged).
    fn attempted(&mut self, now: Instant) {
        self.full_at = burst_take(self.ahead(now), true).map(|ahead| now + ahead);
        self.last_attempt = Some(now);
        self.urgent = None;
        self.last_change = None;
        self.first_change = None;
    }
}

/// Whether the vault syncs now, and why.
pub(crate) fn decide(due: &Due, now: Instant, interval: Duration) -> Option<Trigger> {
    if due.retry_at.is_some_and(|at| at > now) {
        return None;
    }
    if due.urgent == Some(Trigger::BusPurge) {
        return Some(Trigger::BusPurge);
    }
    if !due.spaced(now) {
        return None;
    }
    if let Some(trigger) = due.urgent {
        return Some(trigger);
    }
    let quiet = due
        .last_change
        .is_some_and(|changed| now.saturating_duration_since(changed) >= DEBOUNCE);
    let held = due
        .first_change
        .is_some_and(|first| now.saturating_duration_since(first) >= MAX_HOLD);
    if quiet || held {
        return Some(Trigger::Local);
    }
    match due.last_attempt {
        None => Some(Trigger::Periodic),
        Some(last) if now.duration_since(last) >= interval => Some(Trigger::Periodic),
        Some(_) => None,
    }
}

/// The wait after the `failures`-th failure in a row (`failures >= 1`),
/// `jitter` in `0..1000` spreading it by ±20 %.
pub(crate) fn backoff(failures: u32, jitter: u32) -> Duration {
    let base = BACKOFF_FIRST
        .saturating_mul(1u32 << failures.saturating_sub(1).min(16))
        .min(BACKOFF_MAX);
    let permille = 800 + u64::from(jitter.min(999)) * 400 / 1000;
    let millis = u64::try_from(base.as_millis()).unwrap_or(u64::MAX);
    Duration::from_millis(millis / 1000 * permille)
}

fn jitter() -> u32 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos() % 1000)
}

// ---------------------------------------------------------------------------
// Status (kind `hive.sync`).

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct VaultSyncStatus {
    pub vault_id: String,
    /// `@ns/name`.
    pub vault: String,
    pub last_sync_at: Option<String>,
    pub last_ok_at: Option<String>,
    pub last_trigger: Option<String>,
    /// Signed events waiting in the replica for Hive.
    pub pending: u64,
    /// Of the last successful sync.
    pub stored: u64,
    pub pushed: u64,
    pub failures: u32,
    pub error: Option<String>,
    pub next_attempt_at: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncStatus {
    pub interval_s: u64,
    pub last_pass_at: Option<String>,
    /// Rewritten at least every 30 s by a live thread.
    pub heartbeat_at: Option<String>,
    pub vaults: Vec<VaultSyncStatus>,
}

impl SyncStatus {
    /// Whether a live thread wrote this status (heartbeat recent).
    pub fn running_at(&self, now: chrono::DateTime<Utc>) -> bool {
        self.heartbeat_at
            .as_deref()
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .is_some_and(|at| (now - at.with_timezone(&Utc)).num_seconds() < STALE_AFTER_S)
    }
}

pub(crate) fn sync_dir(data_dir: &Path) -> PathBuf {
    data_dir.join(SYNC_DIR)
}

/// Read, change and write the status file under its own lock, so two
/// processes noting a sync never drop each other's rows (or a backoff).
/// Without the lock (it cannot be created) the change is still made.
fn with_status<T>(data_dir: &Path, change: impl FnOnce(&mut SyncStatus) -> T) -> T {
    use fs2::FileExt as _;
    let dir = sync_dir(data_dir);
    let lock = std::fs::create_dir_all(&dir).ok().and_then(|()| {
        std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(STATUS_LOCK))
            .ok()
    });
    if let Some(file) = &lock {
        let _ = file.lock_exclusive();
    }
    // Unlocked when dropped (see `VaultLock`).
    let lock = lock.map(VaultLock);
    let mut status = load_status(data_dir);
    let out = change(&mut status);
    save_status(data_dir, &status);
    drop(lock);
    out
}

/// Write the thread's `status`, keeping what other processes noted since
/// (see [`merge_manual`]), under the status lock.
fn store_status(data_dir: &Path, status: &mut SyncStatus, shared: &VaultMap) {
    with_status(data_dir, |disk| {
        merge_manual(status, disk, shared);
        disk.clone_from(status);
    });
}

pub fn load_status(data_dir: &Path) -> SyncStatus {
    std::fs::read(sync_dir(data_dir).join(STATUS_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_status(data_dir: &Path, status: &SyncStatus) {
    if let Ok(bytes) = serde_json::to_vec_pretty(status) {
        let _ = write_private(&sync_dir(data_dir), STATUS_FILE, &bytes);
    }
}

fn stamp(at: chrono::DateTime<Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

// ---------------------------------------------------------------------------
// Single flight with `lean-ctx hive sync`.

/// A held vault sync lock (or the status lock). Dropping it unlocks the
/// file explicitly before closing it: closing alone does not release an
/// `flock` while a child process forked meanwhile by another thread (a
/// `git` spawn, say) still holds a copy of the descriptor, and the next
/// sync of the vault would find it busy until that child execs or exits.
#[derive(Debug)]
pub struct VaultLock(std::fs::File);

impl Drop for VaultLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.0);
    }
}

/// The advisory lock of one vault's sync. `wait`: block until free (the
/// CLI); otherwise `Ok(None)` when another sync holds it (the thread skips).
pub fn lock_vault(
    data_dir: &Path,
    vault_id: &str,
    wait: bool,
) -> Result<Option<VaultLock>, HiveError> {
    use fs2::FileExt as _;
    let local = |e: std::io::Error| HiveError::Local(format!("sync lock: {e}"));
    let dir = sync_dir(data_dir);
    std::fs::create_dir_all(&dir).map_err(local)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(format!("{}.lock", lock_name(vault_id))))
        .map_err(local)?;
    if wait {
        file.lock_exclusive().map_err(local)?;
        return Ok(Some(VaultLock(file)));
    }
    match file.try_lock_exclusive() {
        Ok(()) => Ok(Some(VaultLock(file))),
        Err(_) => Ok(None),
    }
}

/// The file name stem of a vault's sync files (lock, budget).
pub(crate) fn lock_name(vault_id: &str) -> String {
    super::place::place_id(vault_id)
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect()
}

// ---------------------------------------------------------------------------
// Local change signals.

/// What a vault's local data looks like: the knowledge journal's head and
/// the replica database's modification times. A change of either is a
/// local write to sync.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Mark {
    knowledge: u64,
    replica: Option<SystemTime>,
}

impl Mark {
    /// The baseline a sync that started at `self` leaves, given the marks
    /// `after` it: the replica as the sync wrote it (its own writes are not
    /// a local change), the knowledge journal as the sync found it. A fact
    /// remembered while the sync ran was not captured by it; taking the
    /// journal head from `after` hid that write until the next unrelated
    /// change or the interval, and push on write, held by the sync's lock
    /// or the burst budget, leaves such a write to this thread.
    pub(crate) fn after_sync(&self, after: &Mark) -> Mark {
        Mark {
            knowledge: self.knowledge,
            replica: after.replica,
        }
    }
}

fn mark_of(data_dir: &Path, record: &VaultRecord, root: &str) -> Mark {
    let knowledge = lean_ctx::core::knowledge::knowledge_dir_of(root)
        .map_or(0, |dir| lean_ctx::core::knowledge::journal::head_lsn(&dir));
    let dir = crate::replica::store::place_dir(data_dir, &record.vault_id);
    let replica = ["replica.db", "replica.db-wal"]
        .iter()
        .filter_map(|name| std::fs::metadata(dir.join(name)).ok()?.modified().ok())
        .max();
    Mark { knowledge, replica }
}

/// The local checkout to sync `record` from: its first recorded root that
/// still exists.
pub fn root_of(record: &VaultRecord) -> Option<String> {
    record
        .roots
        .iter()
        .find(|root| Path::new(root.as_str()).is_dir())
        .cloned()
}

// ---------------------------------------------------------------------------
// The thread.

#[derive(Default)]
struct VaultRun {
    due: Due,
    mark: Option<Mark>,
    failures: u32,
}

/// Start the thread once per process (the daemon). A second call while it
/// runs does nothing.
pub fn spawn(store: HiveStore, config: HiveConfig) {
    STOPPED.store(false, Ordering::SeqCst);
    if RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    let started = std::thread::Builder::new()
        .name("hive-sync".into())
        .spawn(move || {
            run(&store, config);
            RUNNING.store(false, Ordering::SeqCst);
        });
    if started.is_err() {
        RUNNING.store(false, Ordering::SeqCst);
    }
}

fn run(store: &HiveStore, mut config: HiveConfig) {
    let data = store.data_dir().to_path_buf();
    let mut status = load_status(&data);
    let mut runs: BTreeMap<String, VaultRun> = BTreeMap::new();
    let mut last_write: Option<Instant> = None;
    let mut reloaded = Instant::now();
    let mut sleep = POLL;
    while !STOPPED.load(Ordering::SeqCst) {
        let nudged = wait(sleep);
        if STOPPED.load(Ordering::SeqCst) {
            break;
        }
        if reloaded.elapsed() >= Duration::from_secs(60) {
            config = lean_ctx::core::config::Config::load().hive.clone();
            reloaded = Instant::now();
        }
        let interval = Duration::from_secs(u64::from(config.pull_interval()) * 60);
        status.interval_s = interval.as_secs();
        let map = current_map(store);
        let active = config.enabled != Some(false) && crate::bus::relay_v2::speaks_v2(store);
        let due_now = plan_pass(
            &map,
            &mut runs,
            &mut status,
            &nudged,
            Instant::now(),
            interval,
            active,
            &|record, root| mark_of(&data, record, root),
        );
        let mut wrote = false;
        if !due_now.is_empty() {
            match pass(store, &config, &due_now, &mut runs, &mut status) {
                Ok(()) => {}
                Err(PassEnd::Stop(reason)) => {
                    store.record_error(&format!("background sync: {reason}"));
                    store_status(&data, &mut status, &map);
                    break;
                }
            }
            status.last_pass_at = Some(stamp(Utc::now()));
            wrote = true;
        }
        if wrote || last_write.is_none_or(|at| at.elapsed() >= HEARTBEAT) {
            status.heartbeat_at = Some(stamp(Utc::now()));
            store_status(&data, &mut status, &map);
            last_write = Some(Instant::now());
        }
        sleep = next_wake(runs.values().map(|run| &run.due), Instant::now());
    }
    status.heartbeat_at = None;
    store_status(&data, &mut status, &current_map(store));
}

/// The vault map without stale resolutions (dropped from the file; when
/// that write fails, still left out of this pass): a pass never plans a
/// vault the checkout left.
fn current_map(store: &HiveStore) -> VaultMap {
    match crate::resolution::load_current(store.data_dir()) {
        Ok((map, _)) => map,
        Err(error) => {
            store.record_error(&format!("background sync: {error}"));
            crate::resolution::current_view(store.data_dir())
        }
    }
}

/// Keep the rows a manual `hive sync` wrote to the file since this thread
/// last wrote it (newer `last_sync_at`). A row of a vault no longer shared
/// on this machine (not in `shared`) is not brought back.
pub(crate) fn merge_manual(status: &mut SyncStatus, disk: &SyncStatus, shared: &VaultMap) {
    let shared: BTreeSet<String> = shared
        .vaults
        .values()
        .map(|record| super::place::place_id(&record.vault_id))
        .collect();
    status.vaults.retain(|v| shared.contains(&v.vault_id));
    for row in disk.vaults.iter().filter(|v| shared.contains(&v.vault_id)) {
        match status
            .vaults
            .iter_mut()
            .find(|v| v.vault_id == row.vault_id)
        {
            Some(mine) if row.last_sync_at > mine.last_sync_at => *mine = row.clone(),
            Some(_) => {}
            None => status.vaults.push(row.clone()),
        }
    }
    status.vaults.sort_by(|a, b| a.vault_id.cmp(&b.vault_id));
}

/// One pass's plan: look at every shared vault's local marks (`mark`), take
/// the bus nudges, and return the vaults to sync now (purges first, then the
/// oldest attempt; at most [`MAX_PER_PASS`]). Nothing is planned while not
/// `active` (Hive off, not C0 2.x), but local changes are still noted.
#[allow(clippy::too_many_arguments)]
fn plan_pass(
    map: &VaultMap,
    runs: &mut BTreeMap<String, VaultRun>,
    status: &mut SyncStatus,
    nudged: &BTreeMap<String, Trigger>,
    now: Instant,
    interval: Duration,
    active: bool,
    mark: &dyn Fn(&VaultRecord, &str) -> Mark,
) -> Vec<(VaultRecord, String, Trigger)> {
    let mut due_now: Vec<(VaultRecord, String, Trigger)> = Vec::new();
    let mut seen = BTreeSet::new();
    for record in map.vaults.values() {
        let vault = super::place::place_id(&record.vault_id);
        if !seen.insert(vault.clone()) {
            continue;
        }
        let Some(root) = root_of(record) else {
            continue;
        };
        let run = runs.entry(vault.clone()).or_default();
        if let Some(trigger) = nudged.get(&vault) {
            run.due.urgent = Some(run.due.urgent.map_or(*trigger, |t| t.max(*trigger)));
        }
        let now_mark = mark(record, &root);
        match &run.mark {
            // The first look is the baseline, not a change.
            None => run.mark = Some(now_mark),
            Some(previous) if *previous != now_mark => {
                run.due.last_change = Some(now);
                run.due.first_change.get_or_insert(now);
                run.mark = Some(now_mark);
            }
            Some(_) => {}
        }
        if !active {
            continue;
        }
        if let Some(trigger) = decide(&run.due, now, interval) {
            due_now.push((record.clone(), root, trigger));
        }
    }
    // Vaults no longer shared on this machine leave the status.
    runs.retain(|vault, _| seen.contains(vault));
    status.vaults.retain(|v| seen.contains(&v.vault_id));
    // Purges first, then the oldest attempt.
    due_now.sort_by_key(|(record, _, trigger)| {
        let vault = super::place::place_id(&record.vault_id);
        (
            *trigger != Trigger::BusPurge,
            runs.get(&vault).and_then(|r| r.due.last_attempt),
        )
    });
    due_now.truncate(MAX_PER_PASS);
    due_now
}

enum PassEnd {
    Stop(String),
}

fn pass(
    store: &HiveStore,
    config: &HiveConfig,
    due: &[(VaultRecord, String, Trigger)],
    runs: &mut BTreeMap<String, VaultRun>,
    status: &mut SyncStatus,
) -> Result<(), PassEnd> {
    let data = store.data_dir().to_path_buf();
    let mut screen = crate::knowledge::screening::Screen::for_installation();
    // A screening rule that does not compile pauses every content channel.
    let session = match screen.problem() {
        Some(problem) => Err(HiveError::Local(problem)),
        None => crate::scheduler::connected_session(store, config),
    };
    let session = match session {
        Ok(session) => session,
        Err(error @ (HiveError::Revoked(_) | HiveError::LeaseLapsed | HiveError::NotConnected)) => {
            return Err(PassEnd::Stop(error.to_string()));
        }
        Err(error) => {
            // Offline or a local refusal: every due vault waits.
            for (record, _, trigger) in due {
                note(record, *trigger, Err(&error), runs, status, None);
            }
            return Ok(());
        }
    };
    for (record, root, trigger) in due {
        let Ok(lock) = lock_vault(&data, &record.vault_id, false) else {
            continue;
        };
        // A manual `hive sync` holds it: its result is just as good.
        let Some(_lock) = lock else {
            continue;
        };
        // The budget every process of this machine shares: a push on write
        // that just ran (or failed and backs off) counts here too. Held
        // back, the vault stays due and goes out once the budget allows.
        let purge = *trigger == Trigger::BusPurge;
        if super::budget::admit(&data, &record.vault_id, purge, Utc::now())
            != super::budget::Admit::Go
        {
            continue;
        }
        // The journal as this sync starts: it captures what is written up
        // to here, nothing written while it runs.
        let before = mark_of(&data, record, root);
        let done = super::place::sync_vault(&session, record, Path::new(root), &mut screen);
        let pending = pending_events(store, record);
        let wait = match done {
            Ok(done) => {
                let wait = note(record, *trigger, Ok(&done), runs, status, pending);
                // A fresh session's `ctx_tasks` answers from this cache.
                if let Err(error) = crate::tasks::cache::warm(Path::new(root)) {
                    tracing::debug!(
                        "hive: task lists of {} not warmed: {error}",
                        record.vault_id
                    );
                }
                wait
            }
            Err(error @ (HiveError::Revoked(_) | HiveError::LeaseLapsed)) => {
                let wait = note(record, *trigger, Err(&error), runs, status, pending);
                super::budget::settle(&data, &record.vault_id, wait, Utc::now());
                return Err(PassEnd::Stop(error.to_string()));
            }
            Err(error) => note(record, *trigger, Err(&error), runs, status, pending),
        };
        super::budget::settle(&data, &record.vault_id, wait, Utc::now());
        // What this sync wrote itself is not a local change; a fact
        // remembered while it ran is.
        if let Some(run) = runs.get_mut(&super::place::place_id(&record.vault_id)) {
            run.mark = Some(before.after_sync(&mark_of(&data, record, root)));
        }
    }
    // Task proposals queued locally go out with the vault sync.
    for problem in crate::tasks::propose::drain(&session).problems {
        store.record_error(&format!("tasks: {problem}"));
    }
    // Workers no session used for a day, and those of ended app sessions,
    // leave the book and are retired on Hive (owner 09.11.: workers that
    // are not active do not count).
    if let Err(error) = crate::workers::register::sweep(&session) {
        store.record_error(&format!("worker retirement: {error}"));
    }
    Ok(())
}

/// Signed events of `record` still waiting for Hive; `None` when the
/// replica cannot be read (no replica yet, no key).
fn pending_events(store: &HiveStore, record: &VaultRecord) -> Option<u64> {
    if !crate::replica::store::place_dir(store.data_dir(), &record.vault_id).is_dir() {
        return Some(0);
    }
    let (mk, _) = crate::replica::crypt::master_key(store.config_dir(), false).ok()?;
    let (replica, _) =
        crate::replica::Replica::open(store.data_dir(), &record.vault_id, &mk).ok()?;
    replica
        .pending()
        .ok()
        .map(|p| u64::try_from(p.len()).unwrap_or(u64::MAX))
}

fn note(
    record: &VaultRecord,
    trigger: Trigger,
    result: Result<&super::place::VaultSync, &HiveError>,
    runs: &mut BTreeMap<String, VaultRun>,
    status: &mut SyncStatus,
    pending: Option<u64>,
) -> Option<Duration> {
    let vault = super::place::place_id(&record.vault_id);
    let now = Instant::now();
    let at = Utc::now();
    let run = runs.entry(vault.clone()).or_default();
    run.due.attempted(now);
    let index = status
        .vaults
        .iter()
        .position(|v| v.vault_id == vault)
        .unwrap_or_else(|| {
            status.vaults.push(VaultSyncStatus {
                vault_id: vault.clone(),
                ..VaultSyncStatus::default()
            });
            status.vaults.len() - 1
        });
    let row = &mut status.vaults[index];
    row.vault = format!("@{}/{}", record.ns, record.name);
    row.last_sync_at = Some(stamp(at));
    row.last_trigger = Some(trigger.as_str().to_string());
    if let Some(pending) = pending {
        row.pending = pending;
    }
    match result {
        Ok(done) => {
            run.failures = 0;
            run.due.retry_at = None;
            row.failures = 0;
            row.error = None;
            row.next_attempt_at = None;
            row.last_ok_at = Some(stamp(at));
            row.stored = u64::try_from(done.stored).unwrap_or(u64::MAX);
            row.pushed = u64::try_from(done.pushed).unwrap_or(u64::MAX);
        }
        Err(error) => {
            run.failures = row.failures.max(run.failures).saturating_add(1);
            let wait = failure_wait(error, run.failures, at);
            run.due.retry_at = Some(now + wait);
            row.failures = run.failures;
            row.error = Some(error.to_string());
            row.next_attempt_at = chrono::Duration::from_std(wait)
                .ok()
                .map(|wait| stamp(at + wait));
            status.vaults.sort_by(|a, b| a.vault_id.cmp(&b.vault_id));
            return Some(wait);
        }
    }
    status.vaults.sort_by(|a, b| a.vault_id.cmp(&b.vault_id));
    None
}

/// How long a vault waits after its `failures`-th failure in a row: what
/// Hive asked for (`Retry-After` of a 429, a waiting quota's reset), else
/// the exponential backoff; never longer than `WAIT_MAX`.
pub(crate) fn failure_wait(
    error: &HiveError,
    failures: u32,
    now: chrono::DateTime<Utc>,
) -> Duration {
    match error.retry_after_at(now) {
        Some(seconds) => Duration::from_secs(seconds.max(1)),
        None => backoff(failures, jitter()),
    }
    .min(WAIT_MAX)
}

/// Record a manual `lean-ctx hive sync` of `record` in the status, so the
/// app shows it like a background one. Called with the vault's sync lock:
/// a person's sync is never held to the shared budget, but it counts as an
/// attempt there (the burst cap of every other sync sees it), and its
/// result sets (or clears) the backoff every other sync keeps.
pub fn note_manual(
    store: &HiveStore,
    record: &VaultRecord,
    result: Result<&super::place::VaultSync, &HiveError>,
) {
    let wait = note_outside(store, record, Trigger::Manual, result);
    let now = Utc::now();
    super::budget::record(store.data_dir(), &record.vault_id, wait, now);
}

/// Record a sync that ran outside this thread (the CLI, a push on write),
/// under the status lock; the wait a failure asks for (`None` on success).
pub(crate) fn note_outside(
    store: &HiveStore,
    record: &VaultRecord,
    trigger: Trigger,
    result: Result<&super::place::VaultSync, &HiveError>,
) -> Option<Duration> {
    let pending = pending_events(store, record);
    with_status(store.data_dir(), |status| {
        let mut runs = BTreeMap::new();
        note(record, trigger, result, &mut runs, status, pending)
    })
}

#[cfg(test)]
#[path = "auto_tests.rs"]
mod tests;
