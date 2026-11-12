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
//! (`super::capacity::rebuild`): the files are the truth, the ledger a
//! cache. It then removes receipts past the retention window and brings the
//! store down to `TARGET_PERCENT` of its ceiling, oldest first; a receipt
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
//! **Orphans** go too, once they are older than the protected window: a
//! trace sidecar or observation whose receipt is gone, an egress decision
//! link whose receipt is gone, an egress admission whose decision receipt is
//! gone, the attempt links and observations of an admission that is gone,
//! and an output index none of whose receipts is left. Nothing is an orphan
//! because it cannot be read: a damaged record is left as it is. The audit
//! trail (`<data>/audit`) and the execution ledger live outside the store and
//! are never touched; the store's own ledger is bookkeeping, rebuilt from
//! the count.
//!
//! **A dry run** ([`plan_in`]) walks the same way and lists what a pass
//! would remove or pack — kind, path, bytes — without changing a byte: no
//! removal, no packing, no lock or stamp file, no ledger rebuild, no cursor.
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
pub const RETENTION_DAYS: u64 = 30;
/// Receipts younger than this are never removed
/// (`LEAN_CTX_GATEWAY_PROTECT_HOURS`).
pub const PROTECT_HOURS: u64 = 24;
/// A pass that has to make room brings the store down to this share of its
/// ceiling, so it does not run again at the next delivery.
pub(crate) const TARGET_PERCENT: u64 = 80;
/// The bound of one receipt: a larger one is neither written nor loaded.
pub(crate) const MAX_RECEIPT_BYTES: u64 = 64 << 20;
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
/// Written when a pass starts and removed when it is stamped: a pass the
/// process could not finish still counts as an attempt, so a crash or a kill
/// mid-walk does not make the next process count the whole store again at
/// once.
const STARTED: &str = ".maintenance.started";

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
pub struct Policy {
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
    pub fn current() -> Self {
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
pub struct Maintenance {
    pub packed: u64,
    pub removed: u64,
    pub removed_tasks: u64,
    pub freed_bytes: u64,
    /// The store's size as counted, after the pass.
    pub bytes: u64,
    /// The protected evidence alone leaves no room for a lifecycle.
    pub full: bool,
    /// Another process was on it: nothing was done here.
    pub skipped: bool,
    /// Orphaned sidecars, egress records and output indexes removed.
    pub removed_orphans: u64,
    /// The largest receipt file the pass counted, in stored bytes.
    pub largest_receipt_bytes: u64,
    /// A dry run: nothing was changed; [`Self::planned`] lists what a pass
    /// would have done, and the counts above are what it would have counted.
    pub dry_run: bool,
    /// What a dry run found to remove or pack. Empty after a real pass.
    pub planned: Vec<Planned>,
}

/// One file a pass would remove or pack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Planned {
    pub action: Action,
    pub path: PathBuf,
    /// The file's stored size: what removing it frees (packing frees part).
    pub bytes: u64,
}

/// Why a pass touches a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Action {
    /// A receipt past the retention window or over the store's target.
    Receipt,
    /// The trace sidecar or observation of a receipt removed with it.
    Sidecar,
    /// A task index none of whose receipts is left (or its marker).
    TaskIndex,
    /// A trace sidecar or observation whose receipt is gone.
    OrphanSidecar,
    /// An egress decision link, admission, attempt link or observation
    /// whose lifecycle is gone.
    OrphanEgress,
    /// An output index none of whose receipts is left.
    OrphanOutput,
    /// A crashed write's temporary file.
    Temporary,
    /// A plain receipt that would be packed in place.
    Pack,
}

impl Action {
    pub const ALL: [Self; 8] = [
        Self::Receipt,
        Self::Sidecar,
        Self::TaskIndex,
        Self::OrphanSidecar,
        Self::OrphanEgress,
        Self::OrphanOutput,
        Self::Temporary,
        Self::Pack,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Receipt => "receipt",
            Self::Sidecar => "sidecar",
            Self::TaskIndex => "task_index",
            Self::OrphanSidecar => "orphan_sidecar",
            Self::OrphanEgress => "orphan_egress",
            Self::OrphanOutput => "orphan_output",
            Self::Temporary => "temporary",
            Self::Pack => "pack",
        }
    }
}

/// What a finished pass found, as stamped.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Stamp {
    /// The store the pass counted (see [`identity`]); a copied or restored
    /// store is another one.
    store: Option<String>,
    full: bool,
    /// Finished passes in a row that freed nothing while the store was full
    /// (or that failed): an urgent pass backs off by this count.
    #[serde(default)]
    fruitless: u32,
}

/// A pass that started and was not stamped (yet).
#[derive(Debug, Default, Serialize, Deserialize)]
struct Started {
    store: Option<String>,
    /// Earlier passes in a row that started and never finished.
    #[serde(default)]
    aborted: u32,
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
        let _ = meta;
        std::fs::canonicalize(dir)
            .ok()
            .map(|path| path.display().to_string())
    }
}

/// The last finished pass over THIS store and how long ago it ended.
fn own_stamp(dir: &Path) -> Option<(Stamp, Duration)> {
    let (stamp, age) = read_record::<Stamp>(&dir.join(STAMP))?;
    (stamp.store.is_some() && stamp.store == identity(dir)).then_some((stamp, age))
}

/// The pass over THIS store that started and was not stamped, and how long
/// ago it started.
fn own_started(dir: &Path) -> Option<(Started, Duration)> {
    let (started, age) = read_record::<Started>(&dir.join(STARTED))?;
    (started.store.is_some() && started.store == identity(dir)).then_some((started, age))
}

/// A small bookkeeping record and the age of its file.
fn read_record<T: serde::de::DeserializeOwned>(path: &Path) -> Option<(T, Duration)> {
    let age = std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()?
        .elapsed()
        .unwrap_or_default();
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(4096)
        .read_to_end(&mut bytes)
        .ok()?;
    Some((serde_json::from_slice(&bytes).ok()?, age))
}

/// How long after the last attempt a pass may run again: hourly; an urgent
/// one after a minute, doubled for every fruitless or aborted attempt in a
/// row, never later than the hourly pass.
fn interval(urgent: bool, misses: u32) -> Duration {
    if !urgent {
        return MAINTAIN_EVERY;
    }
    URGENT_EVERY
        .saturating_mul(1u32 << misses.min(16))
        .min(MAINTAIN_EVERY)
}

/// Whether a pass over `dir` is due: the last attempt — a finished pass, or
/// a later one that started and never finished — lies further back than
/// [`interval`].
fn pass_due(dir: &Path, urgent: bool) -> bool {
    let stamp = own_stamp(dir);
    // A started record younger than the stamp is a pass that never finished.
    let started = own_started(dir).filter(|(_, started_age)| {
        stamp
            .as_ref()
            .is_none_or(|(_, stamp_age)| started_age < stamp_age)
    });
    let fruitless = stamp.as_ref().map_or(0, |(stamp, _)| stamp.fruitless);
    let aborted = started
        .as_ref()
        .map_or(0, |(started, _)| started.aborted + 1);
    let last = match (&stamp, &started) {
        (_, Some((_, age))) | (Some((_, age)), None) => *age,
        (None, None) => return true,
    };
    last >= interval(urgent, fruitless.saturating_add(aborted))
}

/// The last pass over this store, which ended within the hour, counted its
/// files and found the protected evidence alone filling the ceiling.
pub fn proven_full(dir: &Path) -> bool {
    own_stamp(dir).is_some_and(|(stamp, age)| stamp.full && age < MAINTAIN_EVERY)
}

/// Background passes are run by long-lived processes only: a hook process
/// that lives for milliseconds would start a pass only to abandon it.
static ALLOWED: AtomicBool = AtomicBool::new(false);
static MAINTAINING: AtomicBool = AtomicBool::new(false);

/// When the last pass over THIS store ended, if one did: the stamp of a
/// store this one was copied from does not count.
pub fn last_pass(dir: &Path) -> Option<SystemTime> {
    own_stamp(dir)?;
    std::fs::metadata(dir.join(STAMP))
        .and_then(|meta| meta.modified())
        .ok()
}

/// Called once by a long-lived process: it runs the store's maintenance in
/// the background from now on, and starts a pass now when none ended within
/// the hour, so an upgraded installation brings an old store within its
/// bound before it is needed.
pub fn maintain_at_startup() {
    ALLOWED.store(true, Ordering::Release);
    if let Some(dir) = super::gateway_dir() {
        maintain_in_background(dir, false);
    }
}

/// Run a pass on a background thread when one is due ([`pass_due`]: none
/// over this store was attempted in the last hour, or with `urgent` in the
/// last minute, backing off while passes free nothing or do not finish);
/// never blocks the caller. Only in a process that called
/// [`maintain_at_startup`].
pub(crate) fn maintain_in_background(dir: PathBuf, urgent: bool) {
    if !ALLOWED.load(Ordering::Acquire) {
        return;
    }
    if !pass_due(&dir, urgent) || MAINTAINING.swap(true, Ordering::AcqRel) {
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

pub fn log(outcome: &Result<Maintenance, String>) {
    match outcome {
        Ok(done)
            if done.packed + done.removed + done.removed_tasks + done.removed_orphans > 0
                || done.full =>
        {
            tracing::info!(
                "gateway evidence store: {} packed, {} receipts, {} task indexes and {} orphans removed, {} MB freed, {} MB held{}",
                done.packed,
                done.removed,
                done.removed_tasks,
                done.removed_orphans,
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
pub fn maintain_in(dir: &Path, now: SystemTime, policy: Policy) -> Result<Maintenance, String> {
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
    let store = identity(dir);
    // Recorded before the walk: a pass this process cannot finish still
    // counts as an attempt (see [`pass_due`]).
    let started = Started {
        store: store.clone(),
        aborted: own_started(dir)
            .filter(|(_, started_age)| {
                own_stamp(dir).is_none_or(|(_, stamp_age)| *started_age < stamp_age)
            })
            .map_or(0, |(previous, _)| previous.aborted.saturating_add(1)),
    };
    if let Ok(bytes) = serde_json::to_vec(&started) {
        let _ = crate::core::atomic_fs::try_atomic_write(&dir.join(STARTED), &bytes, None);
    }
    let previous = own_stamp(dir).map(|(stamp, _)| stamp);
    let result = maintain_locked(dir, now, policy, false);
    // Stamped when the pass ends, also when it failed (then never as full),
    // so a broken store is not retried on every delivery; an interrupted
    // pass stays unstamped and its start record stays.
    let fruitless = match &result {
        Ok(done) if !(done.full && done.freed_bytes == 0 && done.packed == 0) => 0,
        _ => previous
            .as_ref()
            .map_or(0, |stamp| stamp.fruitless)
            .saturating_add(1),
    };
    let stamp = Stamp {
        store,
        full: result.as_ref().is_ok_and(|done| done.full),
        fruitless,
    };
    if let Ok(bytes) = serde_json::to_vec(&stamp) {
        let _ = crate::core::atomic_fs::try_atomic_write(&dir.join(STAMP), &bytes, None);
    }
    let _ = std::fs::remove_file(dir.join(STARTED));
    result
}

/// What one pass over `dir` at `now` under `policy` would do, changing
/// nothing: the same walk and the same decisions as [`maintain_in`], with
/// every removal and packing listed in [`Maintenance::planned`] instead of
/// performed. No lock is taken (a lock file would be a change); a pass of
/// another process running meanwhile can make the plan stale, never wrong
/// about what this call did, which is nothing.
pub fn plan_in(dir: &Path, now: SystemTime, policy: Policy) -> Result<Maintenance, String> {
    if !dir.is_dir() {
        return Ok(Maintenance {
            dry_run: true,
            ..Maintenance::default()
        });
    }
    maintain_locked(dir, now, policy, true)
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
    /// Trace sidecars and observations by path: how large, changed when.
    sidecars: HashMap<PathBuf, (u64, SystemTime)>,
    /// The files under `tasks/` by name: changed when, how large.
    tasks: BTreeMap<String, (SystemTime, u64)>,
    /// The files under `egress/` and `outputs/`.
    linked: Vec<Linked>,
    /// Crashed writes' temporaries a dry run left in place (not counted).
    temporaries: Vec<(PathBuf, u64)>,
}

/// A file of the store that belongs to another record's lifecycle.
struct Linked {
    /// Its directory below the store, `/`-separated (`egress/admissions`).
    sub: String,
    /// Its file name without `.json`.
    stem: String,
    path: PathBuf,
    modified: SystemTime,
    size: u64,
}

/// Count every file of the store — complete or an error, like the ledger's
/// own count — and note the receipts, sidecars and task files. A file that
/// vanishes while it is counted (an atomic write's temporary, a removal) was
/// not there. A dry run removes no crashed write's temporary: it lists it.
fn walk(dir: &Path, now: SystemTime, dry_run: bool) -> Result<Walk, String> {
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
            if crate::core::atomic_fs::is_orphaned_temp(&name, modified, now) {
                if dry_run {
                    walk.temporaries.push((path, meta.len()));
                    continue;
                }
                if std::fs::remove_file(&path).is_ok() {
                    continue;
                }
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
                    walk.sidecars.insert(path, (size, modified));
                }
                "tasks" => {
                    walk.tasks.insert(name.into_owned(), (modified, size));
                }
                sub if sub == "outputs" || sub.starts_with("egress") => {
                    if let Some(stem) = name.strip_suffix(".json") {
                        walk.linked.push(Linked {
                            sub: sub.replace(std::path::MAIN_SEPARATOR, "/"),
                            stem: stem.to_owned(),
                            path,
                            modified,
                            size,
                        });
                    }
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

/// Removals not yet booked against the store's ledger. In a dry run nothing
/// is removed or booked: each removal is listed instead.
#[derive(Default)]
struct Pending {
    bytes: u64,
    objects: u64,
    dry_run: bool,
    /// Every path removed (or, in a dry run, listed) in this pass: no file
    /// is counted twice.
    done: HashSet<PathBuf>,
    planned: Vec<Planned>,
}

impl Pending {
    fn new(dry_run: bool) -> Self {
        Self {
            dry_run,
            ..Self::default()
        }
    }

    /// Remove `path`, known to hold `size` bytes, for `action`; `false`: it
    /// is not there (or was already removed in this pass).
    fn remove(&mut self, action: Action, path: &Path, size: u64) -> bool {
        if self.done.contains(path) {
            return false;
        }
        if self.dry_run {
            if std::fs::symlink_metadata(path).is_err() {
                return false;
            }
            self.planned.push(Planned {
                action,
                path: path.to_path_buf(),
                bytes: size,
            });
        } else if std::fs::remove_file(path).is_err() {
            return false;
        }
        self.done.insert(path.to_path_buf());
        self.bytes += size;
        self.objects += 1;
        true
    }

    /// Book the removals when a batch is full (or with `force`); `true`:
    /// booked. A dry run books nothing.
    fn book(&mut self, dir: &Path, force: bool) -> bool {
        if self.objects > 0 && (force || self.objects >= BOOK_EVERY) {
            if !self.dry_run {
                super::capacity::book_removal(dir, self.bytes, self.objects);
            }
            self.bytes = 0;
            self.objects = 0;
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

fn maintain_locked(
    dir: &Path,
    now: SystemTime,
    policy: Policy,
    dry_run: bool,
) -> Result<Maintenance, String> {
    let mut report = Maintenance {
        dry_run,
        ..Maintenance::default()
    };
    // The truth first: count the files, rebuild the ledger from the count.
    let start = (!dry_run).then(|| super::capacity::snapshot(dir)).flatten();
    let mut walked = walk(dir, now, dry_run)?;
    if let Some(start) = &start {
        super::capacity::rebuild(dir, start, (walked.bytes, walked.objects));
    }
    let mut pending = Pending::new(dry_run);
    for (path, size) in &walked.temporaries {
        pending.remove(Action::Temporary, path, *size);
    }
    report.largest_receipt_bytes = walked
        .receipts
        .iter()
        .map(|item| item.size)
        .max()
        .unwrap_or(0);
    let mut total = walked.bytes;
    let fresh_tasks = tasks_in_progress(dir, &walked, now, policy.protect);
    let mut keep = latest_listed(dir);
    keep.extend(fresh_tasks);

    // Orphans of receipts already gone first: what they free is not taken
    // from receipts that could stay.
    let mut gone: HashSet<String> = HashSet::new();
    let freed = sweep_orphans(
        &walked,
        &gone,
        now,
        policy.protect,
        &mut pending,
        &mut report,
    );
    total = total.saturating_sub(freed);

    walked.receipts.sort_by_key(|item| item.modified);
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
        if !pending.remove(Action::Receipt, &item.path, item.size) {
            continue;
        }
        let mut freed = item.size;
        for sidecar in [
            dir.join("traces").join(format!("{}.json", item.hex)),
            dir.join("observed").join(format!("{}.json", item.hex)),
        ] {
            if let Some((size, _)) = walked.sidecars.get(&sidecar).copied()
                && pending.remove(Action::Sidecar, &sidecar, size)
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
    // The receipts removed now leave orphans of their own.
    if !gone.is_empty() {
        let freed = sweep_orphans(
            &walked,
            &gone,
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
            if dry_run {
                // What packing frees is not known before it is done.
                pending.planned.push(Planned {
                    action: Action::Pack,
                    path: item.path.clone(),
                    bytes: item.size,
                });
                report.packed += 1;
                continue;
            }
            let before = item.size;
            // A receipt that cannot be packed now stays plain for a later pass.
            if pack_in_place(dir, &receipts, item).is_ok() {
                report.packed += 1;
                total = total.saturating_sub(before.saturating_sub(item.size));
            }
        }
    }
    report.planned = std::mem::take(&mut pending.planned);

    report.bytes = total;
    report.full = policy
        .ceiling_bytes
        .is_some_and(|ceiling| total.saturating_add(super::capacity::LIFECYCLE_RESERVE) > ceiling);
    Ok(report)
}

/// The digest hex a small JSON record of the store names under `field`.
/// `None` when the record cannot be read, is not a record of a known shape
/// or names none: such a record is never taken for an orphan.
fn named_digest(path: &Path, field: &str) -> Option<String> {
    let bytes = super::read_egress_record(path).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let hex = value.get(field)?.as_str()?.strip_prefix("sha256:")?;
    super::is_digest_hex(hex).then(|| hex.to_owned())
}

fn remove_orphan(
    pending: &mut Pending,
    report: &mut Maintenance,
    action: Action,
    path: &Path,
    size: u64,
) -> u64 {
    if pending.remove(action, path, size) {
        report.removed_orphans += 1;
        report.freed_bytes += size;
        size
    } else {
        0
    }
}

/// Remove the orphans older than `protect`, given the receipts of the walk
/// without those `gone` in this pass. Returns the bytes freed.
///
/// - `traces/<r>.json`, `observed/<r>.json`, `egress/decisions/<r>.json`:
///   receipt `r` is gone.
/// - `egress/admissions/<a>.json`: the decision receipt it names is gone (an
///   admission that names none, a failed one, is kept).
/// - `egress/attempts/<a>/<n>.json`: admission `a` is gone or an orphan.
/// - `egress/observations/<o>.json`: the admission it names is gone or an
///   orphan.
/// - `outputs/<d>.json`: none of the receipts it lists is left.
///
/// A record that cannot be read is kept, whatever its age.
fn sweep_orphans(
    walked: &Walk,
    gone: &HashSet<String>,
    now: SystemTime,
    protect: Duration,
    pending: &mut Pending,
    report: &mut Maintenance,
) -> u64 {
    let receipts: HashSet<&str> = walked
        .receipts
        .iter()
        .map(|item| item.hex.as_str())
        .filter(|hex| !gone.contains(*hex))
        .collect();
    let settled = |at: SystemTime| age(now, at) >= protect;
    let mut freed = 0;

    let mut sidecars: Vec<_> = walked.sidecars.iter().collect();
    sidecars.sort();
    for (path, (size, modified)) in sidecars {
        let receipt = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".json"))
            .filter(|hex| super::is_digest_hex(hex));
        if let Some(hex) = receipt
            && !receipts.contains(hex)
            && settled(*modified)
        {
            freed += remove_orphan(pending, report, Action::OrphanSidecar, path, *size);
        }
    }

    // The admissions that stay: every one but those whose receipt is gone.
    let mut admissions: HashSet<&str> = HashSet::new();
    for item in walked
        .linked
        .iter()
        .filter(|item| item.sub == "egress/admissions")
    {
        // Removed by the first sweep of this pass: no longer readable, and
        // no longer an admission that stays.
        if pending.done.contains(&item.path) {
            continue;
        }
        let orphan = settled(item.modified)
            && named_digest(&item.path, "decision_receipt_digest")
                .is_some_and(|hex| !receipts.contains(hex.as_str()));
        if orphan {
            freed += remove_orphan(pending, report, Action::OrphanEgress, &item.path, item.size);
        } else {
            admissions.insert(item.stem.as_str());
        }
    }

    let mut emptied: Vec<PathBuf> = Vec::new();
    for item in &walked.linked {
        if !settled(item.modified) {
            continue;
        }
        let orphan = match item.sub.as_str() {
            "egress/decisions" => {
                super::is_digest_hex(&item.stem) && !receipts.contains(item.stem.as_str())
            }
            "egress/observations" => named_digest(&item.path, "admission_receipt_digest")
                .is_some_and(|hex| !admissions.contains(hex.as_str())),
            "outputs" => super::read_index(&item.path).is_ok_and(|index| {
                !index
                    .receipts
                    .iter()
                    .any(|hex| receipts.contains(hex.as_str()))
            }),
            sub => match sub.strip_prefix("egress/attempts/") {
                Some(admission) if super::is_digest_hex(admission) => {
                    let orphan = !admissions.contains(admission);
                    if orphan && let Some(parent) = item.path.parent() {
                        emptied.push(parent.to_path_buf());
                    }
                    orphan
                }
                _ => false,
            },
        };
        if orphan {
            let action = if item.sub == "outputs" {
                Action::OrphanOutput
            } else {
                Action::OrphanEgress
            };
            freed += remove_orphan(pending, report, action, &item.path, item.size);
        }
    }
    // An attempts directory left empty goes too (directories are not counted).
    if !pending.dry_run {
        emptied.sort();
        emptied.dedup();
        for directory in emptied {
            let _ = std::fs::remove_dir(directory);
        }
    }
    freed
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
            if pending.remove(Action::TaskIndex, &path, *size) {
                report.removed_tasks += 1;
                freed += size;
                let marker = format!("{stem}.incomplete");
                if let Some((_, marker_size)) = walked.tasks.get(&marker)
                    && pending.remove(Action::TaskIndex, &tasks.join(&marker), *marker_size)
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
            if !pending.dry_run {
                save_cursor(dir, name);
            }
            batch_done();
        }
    }
    if !pending.dry_run {
        let _ = std::fs::remove_file(dir.join(CURSOR));
    }
    report.freed_bytes += freed;
    freed
}

// The tests in retention_tests.rs run as `retention::tests::*`.
#[cfg(test)]
#[path = "retention_tests.rs"]
mod tests;
