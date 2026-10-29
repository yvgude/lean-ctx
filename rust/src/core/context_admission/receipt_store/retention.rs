// SPDX-License-Identifier: Apache-2.0
//! Bounded retention of the gateway evidence store (G09).
//!
//! Receipts are stored zstd-packed (`receipts/<sha256>.json.zst`); the
//! digest is the canonical JSON's, so packing never weakens the tamper
//! check, and plain `<sha256>.json` files of older versions still load.
//!
//! A maintenance pass — in the background of a long-lived process (the MCP
//! server, the daemon), at most hourly and one process at a time, sooner
//! when a write did not fit under the ceiling — first counts every file of
//! the store and rebuilds its ledger from that count
//! ([`super::capacity::rebuild`]): the files are the truth, the ledger a
//! cache. It then removes receipts past the retention window and brings the
//! store down to [`TARGET_PERCENT`] of its ceiling, oldest first; a receipt
//! goes together with its trace sidecar and its observation, a task index
//! none of whose receipts is left goes too. Plain receipts that stay are
//! packed last, so nothing is packed only to be removed.
//!
//! **Protected**, whatever their age or size: receipts younger than the
//! protected window, receipts a project's latest index lists, and the
//! receipts of a task whose index changed within that window. Only when the
//! protected evidence alone fills the ceiling, as a pass counted it, are
//! writes refused ([`proven_full`]). A task some of whose receipts were
//! removed reports itself incomplete, as for any missing receipt.
//!
//! **Progress persists.** Every removal is final and booked in batches, and
//! the task indexes are visited in name order behind a cursor saved per
//! batch, so a pass a process could not finish is continued — not
//! restarted — by the next one. Only a finished pass is stamped.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

/// How long a receipt is kept at most (`LEAN_CTX_GATEWAY_RETENTION_DAYS`).
pub(crate) const RETENTION_DAYS: u64 = 30;
/// Receipts younger than this are never removed
/// (`LEAN_CTX_GATEWAY_PROTECT_HOURS`).
pub(crate) const PROTECT_HOURS: u64 = 24;
/// A pass that has to make room brings the store down to this share of its
/// ceiling, so it does not run again at the next delivery.
pub(crate) const TARGET_PERCENT: u64 = 80;
/// A decompressed receipt larger than this is not loaded.
const MAX_RECEIPT_BYTES: u64 = 64 << 20;
const ZSTD_LEVEL: i32 = 3;
/// Plain receipts younger than this are left for a later pass.
const PACK_AFTER: Duration = Duration::from_mins(10);
const MAINTAIN_EVERY: Duration = Duration::from_hours(1);
/// A pass a full store asks for runs at most this often.
const URGENT_EVERY: Duration = Duration::from_mins(1);
/// Removals booked against the store's ledger at once, and task indexes
/// visited between two saves of the cursor.
const BOOK_EVERY: u64 = 256;

/// The prefix of the maintenance files in the store's top directory.
pub(super) const BOOKKEEPING_PREFIX: &str = ".maintenance";
const STAMP: &str = ".maintenance";
const LOCK: &str = ".maintenance.lock";
const CURSOR: &str = ".maintenance.cursor";

pub(super) fn pack(bytes: &[u8]) -> Result<Vec<u8>, String> {
    zstd::encode_all(bytes, ZSTD_LEVEL).map_err(|e| e.to_string())
}

pub(super) fn unpack(file: std::fs::File) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    zstd::stream::read::Decoder::new(file)?
        .take(MAX_RECEIPT_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_RECEIPT_BYTES {
        return Err(std::io::Error::other("receipt exceeds its size bound"));
    }
    Ok(bytes)
}

/// What a pass may remove.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Policy {
    /// Older receipts are removed unless protected; `None`: kept by age.
    pub(crate) retention: Option<Duration>,
    /// Younger receipts are never removed.
    pub(crate) protect: Duration,
    /// The store is brought down to this many bytes; `None`: no ceiling.
    pub(crate) target_bytes: Option<u64>,
    /// The store's ceiling: a store the protected evidence keeps above it
    /// (with room for one lifecycle) is full.
    pub(crate) ceiling_bytes: Option<u64>,
}

impl Policy {
    /// The policy of this process: the environment overrides, else the
    /// defaults; the target from the store's ceiling.
    pub(crate) fn current() -> Self {
        let number = |name: &str, default: u64| {
            std::env::var(name)
                .ok()
                .and_then(|value| value.trim().parse::<u64>().ok())
                .unwrap_or(default)
        };
        let days = number("LEAN_CTX_GATEWAY_RETENTION_DAYS", RETENTION_DAYS);
        let hours = number("LEAN_CTX_GATEWAY_PROTECT_HOURS", PROTECT_HOURS);
        let ceiling = super::capacity::ceiling_bytes();
        Self {
            retention: (days > 0).then(|| Duration::from_hours(days.saturating_mul(24))),
            protect: Duration::from_hours(hours),
            // Room for new lifecycles too: each entry reserves its follow-up.
            target_bytes: ceiling.map(|limit| {
                (limit / 100 * TARGET_PERCENT)
                    .min(limit.saturating_sub(2 * super::capacity::LIFECYCLE_RESERVE))
            }),
            ceiling_bytes: ceiling,
        }
    }
}

/// What one pass did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Maintenance {
    pub(crate) packed: u64,
    pub(crate) removed: u64,
    pub(crate) removed_tasks: u64,
    pub(crate) freed_bytes: u64,
    /// The store's size as counted, after the pass.
    pub(crate) bytes: u64,
    /// The protected evidence alone leaves no room for a lifecycle.
    pub(crate) full: bool,
    /// Another process was on it: nothing was done here.
    pub(crate) skipped: bool,
}

/// What a finished pass found, as stamped.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Stamp {
    /// The store the pass counted (see [`identity`]); a copied or restored
    /// store is another one.
    store: Option<String>,
    full: bool,
}

/// The identity of the store directory: a copy of it is a new directory.
fn identity(dir: &Path) -> Option<String> {
    let meta = std::fs::metadata(dir).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some(format!("{}:{}", meta.dev(), meta.ino()))
    }
    #[cfg(not(unix))]
    {
        drop(meta);
        std::fs::canonicalize(dir)
            .ok()
            .map(|path| path.display().to_string())
    }
}

/// The last finished pass over THIS store and how long ago it ended.
fn own_stamp(dir: &Path) -> Option<(Stamp, Duration)> {
    let path = dir.join(STAMP);
    let age = std::fs::metadata(&path)
        .and_then(|meta| meta.modified())
        .ok()?
        .elapsed()
        .unwrap_or_default();
    let mut bytes = Vec::new();
    std::fs::File::open(&path)
        .ok()?
        .take(4096)
        .read_to_end(&mut bytes)
        .ok()?;
    let stamp: Stamp = serde_json::from_slice(&bytes).ok()?;
    (stamp.store.is_some() && stamp.store == identity(dir)).then_some((stamp, age))
}

/// The last pass over this store, which ended within the hour, counted its
/// files and found the protected evidence alone filling the ceiling.
pub(crate) fn proven_full(dir: &Path) -> bool {
    own_stamp(dir).is_some_and(|(stamp, age)| stamp.full && age < MAINTAIN_EVERY)
}

/// Background passes are run by long-lived processes only: a hook process
/// that lives for milliseconds would start a pass only to abandon it.
static ALLOWED: AtomicBool = AtomicBool::new(false);
static MAINTAINING: AtomicBool = AtomicBool::new(false);

/// When the last pass over THIS store ended, if one did: the stamp of a
/// store this one was copied from does not count.
pub(crate) fn last_pass(dir: &Path) -> Option<SystemTime> {
    own_stamp(dir)?;
    std::fs::metadata(dir.join(STAMP))
        .and_then(|meta| meta.modified())
        .ok()
}

/// Called once by a long-lived process: it runs the store's maintenance in
/// the background from now on, and starts a pass now when none ended within
/// the hour, so an upgraded installation brings an old store within its
/// bound before it is needed.
pub(crate) fn maintain_at_startup() {
    ALLOWED.store(true, Ordering::Release);
    if let Some(dir) = super::gateway_dir() {
        maintain_in_background(dir, false);
    }
}

/// Run a pass on a background thread when one is due — none over this store
/// ended in the last hour, or with `urgent` in the last minute; never blocks
/// the caller. Only in a process that called [`maintain_at_startup`].
pub(crate) fn maintain_in_background(dir: PathBuf, urgent: bool) {
    if !ALLOWED.load(Ordering::Acquire) {
        return;
    }
    let every = if urgent { URGENT_EVERY } else { MAINTAIN_EVERY };
    let recent = own_stamp(&dir).is_some_and(|(_, age)| age < every);
    if recent || MAINTAINING.swap(true, Ordering::AcqRel) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("receipt-maintenance".into())
        .spawn(move || {
            log(&maintain_in(&dir, SystemTime::now(), Policy::current()));
            MAINTAINING.store(false, Ordering::Release);
        });
    if spawned.is_err() {
        MAINTAINING.store(false, Ordering::Release);
    }
}

fn log(outcome: &Result<Maintenance, String>) {
    match outcome {
        Ok(done) if done.packed + done.removed + done.removed_tasks > 0 || done.full => {
            tracing::info!(
                "gateway evidence store: {} packed, {} receipts and {} task indexes removed, {} MB freed, {} MB held{}",
                done.packed,
                done.removed,
                done.removed_tasks,
                done.freed_bytes / 1_048_576,
                done.bytes / 1_048_576,
                if done.full {
                    " — full of protected evidence"
                } else {
                    ""
                }
            );
        }
        Ok(_) => {}
        Err(error) => tracing::warn!("gateway evidence store maintenance skipped: {error}"),
    }
}

/// One pass over `dir` at `now` under `policy`.
pub(crate) fn maintain_in(
    dir: &Path,
    now: SystemTime,
    policy: Policy,
) -> Result<Maintenance, String> {
    use fs2::FileExt;
    if !dir.is_dir() {
        return Ok(Maintenance::default());
    }
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(LOCK))
        .map_err(|e| e.to_string())?;
    if lock.try_lock_exclusive().is_err() {
        return Ok(Maintenance {
            skipped: true,
            ..Maintenance::default()
        });
    }
    let result = maintain_locked(dir, now, policy);
    // Stamped when the pass ends, also when it failed (then never as full),
    // so a broken store is not retried on every delivery; an interrupted
    // pass stays unstamped.
    let stamp = Stamp {
        store: identity(dir),
        full: result.as_ref().is_ok_and(|done| done.full),
    };
    if let Ok(bytes) = serde_json::to_vec(&stamp) {
        let _ = crate::core::atomic_fs::try_atomic_write(&dir.join(STAMP), &bytes, None);
    }
    result
}

struct Stored {
    hex: String,
    path: PathBuf,
    modified: SystemTime,
    size: u64,
    plain: bool,
}

/// One count of the store, with what a pass needs of it: nothing is
/// measured twice.
#[derive(Default)]
struct Walk {
    bytes: u64,
    objects: u64,
    receipts: Vec<Stored>,
    /// Trace sidecars and observations by path, with their sizes.
    sidecars: HashMap<PathBuf, u64>,
    /// The files under `tasks/` by name: changed when, how large.
    tasks: BTreeMap<String, (SystemTime, u64)>,
}

/// Count every file of the store — complete or an error, like the ledger's
/// own count — and note the receipts, sidecars and task files. A file that
/// vanishes while it is counted (an atomic write's temporary, a removal) was
/// not there.
fn walk(dir: &Path, now: SystemTime) -> Result<Walk, String> {
    let unreadable = |what: &Path, error: std::io::Error| {
        format!(
            "gateway evidence store cannot be measured ({}: {error})",
            what.display()
        )
    };
    let gone = |error: &std::io::Error| error.kind() == std::io::ErrorKind::NotFound;
    let mut walk = Walk::default();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if gone(&error) && directory != dir => continue,
            Err(error) => return Err(unreadable(&directory, error)),
        };
        let sub = directory
            .strip_prefix(dir)
            .ok()
            .and_then(|sub| sub.to_str())
            .unwrap_or_default()
            .to_owned();
        for entry in entries {
            let entry = entry.map_err(|e| unreadable(&directory, e))?;
            let path = entry.path();
            let kind = match entry.file_type() {
                Ok(kind) => kind,
                Err(error) if gone(&error) => continue,
                Err(error) => return Err(unreadable(&path, error)),
            };
            if kind.is_dir() {
                pending.push(path);
                continue;
            }
            let name = entry.file_name();
            if !kind.is_file() || (sub.is_empty() && super::capacity::is_bookkeeping(&name)) {
                continue;
            }
            let meta = match entry.metadata() {
                Ok(meta) => meta,
                Err(error) if gone(&error) => continue,
                Err(error) => return Err(unreadable(&path, error)),
            };
            let modified = meta.modified().unwrap_or(now);
            let name = name.to_string_lossy();
            // Store writes do not sweep for a crashed write's temp: the walk
            // does, and does not count what it removes.
            if crate::core::atomic_fs::is_orphaned_temp(&name, modified, now)
                && std::fs::remove_file(&path).is_ok()
            {
                continue;
            }
            let size = meta.len();
            walk.bytes = walk.bytes.saturating_add(size);
            walk.objects += 1;
            match sub.as_str() {
                "receipts" => {
                    if let Some((hex, plain)) = digest_of(&name) {
                        walk.receipts.push(Stored {
                            hex: hex.to_owned(),
                            path,
                            modified,
                            size,
                            plain,
                        });
                    }
                }
                "traces" | "observed" => {
                    walk.sidecars.insert(path, size);
                }
                "tasks" => {
                    walk.tasks.insert(name.into_owned(), (modified, size));
                }
                _ => {}
            }
        }
    }
    Ok(walk)
}

fn age(now: SystemTime, at: SystemTime) -> Duration {
    now.duration_since(at).unwrap_or_default()
}

fn digest_of(name: &str) -> Option<(&str, bool)> {
    let (hex, plain) = match name.strip_suffix(".json.zst") {
        Some(hex) => (hex, false),
        None => (name.strip_suffix(".json")?, true),
    };
    (hex.len() == 64 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
        .then_some((hex, plain))
}

/// The digests the indexes under `dir/latest` list. Unreadable indexes list
/// nothing.
fn latest_listed(dir: &Path) -> HashSet<String> {
    let mut digests = HashSet::new();
    let Ok(entries) = std::fs::read_dir(dir.join("latest")) else {
        return digests;
    };
    for entry in entries.flatten() {
        if let Ok(index) = super::read_index(&entry.path()) {
            digests.extend(index.receipts);
        }
    }
    digests
}

/// The digests the task indexes changed within `window` list.
fn tasks_in_progress(
    dir: &Path,
    walk: &Walk,
    now: SystemTime,
    window: Duration,
) -> HashSet<String> {
    let mut digests = HashSet::new();
    for (name, (modified, _)) in &walk.tasks {
        if Path::new(name).extension().is_some_and(|ext| ext == "json")
            && age(now, *modified) < window
            && let Ok(index) = super::read_index(&dir.join("tasks").join(name))
        {
            digests.extend(index.receipts);
        }
    }
    digests
}

/// Removals not yet booked against the store's ledger.
#[derive(Default)]
struct Pending {
    bytes: u64,
    objects: u64,
}

impl Pending {
    /// Remove `path`, known to hold `size` bytes; `false`: it is not there.
    fn remove(&mut self, path: &Path, size: u64) -> bool {
        if std::fs::remove_file(path).is_err() {
            return false;
        }
        self.bytes += size;
        self.objects += 1;
        true
    }

    /// Book the removals when a batch is full (or with `force`); `true`:
    /// booked.
    fn book(&mut self, dir: &Path, force: bool) -> bool {
        if self.objects > 0 && (force || self.objects >= BOOK_EVERY) {
            super::capacity::book_removal(dir, self.bytes, self.objects);
            *self = Self::default();
            return true;
        }
        false
    }
}

#[cfg(test)]
thread_local! {
    /// Tests: the process ends after this many more batches.
    static ENDS_AFTER: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

/// A batch of the pass is done and its progress saved: the point at which a
/// process may end (simulated in tests).
fn batch_done() {
    #[cfg(test)]
    ENDS_AFTER.with(|left| match left.get() {
        Some(1) => {
            left.set(None);
            panic!("the process ended");
        }
        Some(n) => left.set(Some(n - 1)),
        None => {}
    });
}

fn maintain_locked(dir: &Path, now: SystemTime, policy: Policy) -> Result<Maintenance, String> {
    let mut report = Maintenance::default();
    // The truth first: count the files, rebuild the ledger from the count.
    let start = super::capacity::snapshot(dir);
    let mut walked = walk(dir, now)?;
    if let Some(start) = &start {
        super::capacity::rebuild(dir, start, (walked.bytes, walked.objects));
    }
    let mut total = walked.bytes;
    let fresh_tasks = tasks_in_progress(dir, &walked, now, policy.protect);
    let mut keep = latest_listed(dir);
    keep.extend(fresh_tasks);

    walked.receipts.sort_by_key(|item| item.modified);
    let mut pending = Pending::default();
    let mut gone: HashSet<String> = HashSet::new();
    for item in &walked.receipts {
        if keep.contains(&item.hex) || age(now, item.modified) < policy.protect {
            continue;
        }
        let expired = policy
            .retention
            .is_some_and(|window| age(now, item.modified) > window);
        let over = policy.target_bytes.is_some_and(|target| total > target);
        if !(expired || over) {
            continue;
        }
        if !pending.remove(&item.path, item.size) {
            continue;
        }
        let mut freed = item.size;
        for sidecar in [
            dir.join("traces").join(format!("{}.json", item.hex)),
            dir.join("observed").join(format!("{}.json", item.hex)),
        ] {
            if let Some(size) = walked.sidecars.get(&sidecar).copied()
                && pending.remove(&sidecar, size)
            {
                freed += size;
            }
        }
        gone.insert(item.hex.clone());
        report.removed += 1;
        report.freed_bytes += freed;
        total = total.saturating_sub(freed);
        if pending.book(dir, false) {
            batch_done();
        }
    }

    // The task indexes: after removals, when an earlier pass left a cursor,
    // or on the first pass over this store.
    if report.removed > 0 || dir.join(CURSOR).exists() || own_stamp(dir).is_none() {
        let left: HashSet<&str> = walked
            .receipts
            .iter()
            .filter(|item| !gone.contains(&item.hex))
            .map(|item| item.hex.as_str())
            .collect();
        let freed = prune_tasks(
            dir,
            &walked,
            &left,
            now,
            policy.protect,
            &mut pending,
            &mut report,
        );
        total = total.saturating_sub(freed);
    }
    pending.book(dir, true);

    // Packing last: what stays plain and settled is packed.
    let receipts = dir.join("receipts");
    for item in &mut walked.receipts {
        if item.plain && !gone.contains(&item.hex) && age(now, item.modified) >= PACK_AFTER {
            let before = item.size;
            // A receipt that cannot be packed now stays plain for a later pass.
            if pack_in_place(dir, &receipts, item).is_ok() {
                report.packed += 1;
                total = total.saturating_sub(before.saturating_sub(item.size));
            }
        }
    }

    report.bytes = total;
    report.full = policy
        .ceiling_bytes
        .is_some_and(|ceiling| total.saturating_add(super::capacity::LIFECYCLE_RESERVE) > ceiling);
    Ok(report)
}

/// Pack the plain receipt `item` beside itself, keeping its time (retention
/// and the size order go by it), then remove the plain file. Packing only
/// shrinks the store, so it is booked as a removal of the difference and
/// never refused by the ceiling it serves.
fn pack_in_place(dir: &Path, receipts: &Path, item: &mut Stored) -> Result<(), String> {
    let target = receipts.join(format!("{}.json.zst", item.hex));
    let plain = item.size;
    let existed = target.exists();
    if !existed {
        let bytes = std::fs::read(&item.path).map_err(|e| e.to_string())?;
        crate::core::atomic_fs::try_atomic_write_unswept(&target, &pack(&bytes)?)
            .map_err(|e| e.to_string())?;
        if let Ok(file) = std::fs::File::options().write(true).open(&target) {
            let _ = file.set_times(std::fs::FileTimes::new().set_modified(item.modified));
        }
    }
    std::fs::remove_file(&item.path).map_err(|e| e.to_string())?;
    let packed = std::fs::metadata(&target).map_or(0, |meta| meta.len());
    if existed {
        super::capacity::book_removal(dir, plain, 1);
    } else if packed <= plain {
        super::capacity::book_removal(dir, plain - packed, 0);
    } else {
        super::capacity::recount_later(dir);
    }
    item.path = target;
    item.size = packed;
    item.plain = false;
    Ok(())
}

/// The task index a pass this store left unfinished visited last.
fn read_cursor(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join(CURSOR)).ok()?;
    let (store, name) = text.split_once('\n')?;
    (Some(store) == identity(dir).as_deref()).then(|| name.to_owned())
}

fn save_cursor(dir: &Path, name: &str) {
    if let Some(store) = identity(dir) {
        let _ = std::fs::write(dir.join(CURSOR), format!("{store}\n{name}"));
    }
}

/// Remove the task indexes (and their incomplete markers) unchanged within
/// `protect` none of whose receipts is `left`, in name order from the saved
/// cursor on. Returns the bytes freed.
fn prune_tasks(
    dir: &Path,
    walked: &Walk,
    left: &HashSet<&str>,
    now: SystemTime,
    protect: Duration,
    pending: &mut Pending,
    report: &mut Maintenance,
) -> u64 {
    let tasks = dir.join("tasks");
    let cursor = read_cursor(dir);
    let mut freed = 0;
    let mut visited = 0u64;
    let after = cursor
        .as_deref()
        .map_or(std::ops::Bound::Unbounded, |name| {
            std::ops::Bound::Excluded(name.to_owned())
        });
    for (name, (modified, size)) in walked.tasks.range((after, std::ops::Bound::Unbounded)) {
        'visit: {
            let Some(stem) = name.strip_suffix(".json") else {
                break 'visit;
            };
            if age(now, *modified) < protect {
                break 'visit;
            }
            let path = tasks.join(name);
            // A damaged index is left as it is: readers report it incomplete.
            let Ok(index) = super::read_index(&path) else {
                break 'visit;
            };
            if index.receipts.iter().any(|hex| left.contains(hex.as_str())) {
                break 'visit;
            }
            // Changed since the count (a delivery of that task meanwhile): kept.
            let changed = std::fs::metadata(&path)
                .and_then(|meta| meta.modified())
                .is_ok_and(|at| at != *modified);
            if changed {
                break 'visit;
            }
            if pending.remove(&path, *size) {
                report.removed_tasks += 1;
                freed += size;
                let marker = format!("{stem}.incomplete");
                if let Some((_, marker_size)) = walked.tasks.get(&marker)
                    && pending.remove(&tasks.join(&marker), *marker_size)
                {
                    freed += marker_size;
                }
            }
        }
        // Every BOOK_EVERY visits: removals booked, then the cursor saved
        // behind the last index visited.
        visited += 1;
        if visited.is_multiple_of(BOOK_EVERY) {
            pending.book(dir, true);
            save_cursor(dir, name);
            batch_done();
        }
    }
    let _ = std::fs::remove_file(dir.join(CURSOR));
    report.freed_bytes += freed;
    freed
}

#[cfg(test)]
#[path = "retention_tests.rs"]
mod tests;
