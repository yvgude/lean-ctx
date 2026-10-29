// SPDX-License-Identifier: Apache-2.0
//! Size bound of the gateway evidence store (G09).
//!
//! `<data>/gateway` holds receipts, trace sidecars, hand-over observations,
//! egress records, task signals and their indexes. Each file is bounded;
//! their number was not. This module bounds the store as a whole: a write
//! that does not fit first makes room ([`super::retention`]) and is refused
//! only when protected evidence fills the store.
//!
//! **One writer at a time.** Every write of a file of the store goes through
//! [`write`]: under the store's ledger lock it measures the file as it is,
//! decides whether the change fits, performs the write and books it — in
//! that order, in one critical section. There is therefore no moment in
//! which bytes are booked but not on disk or on disk but not booked, and a
//! count of the files taken under the lock is exact. The lock is the
//! innermost one: it is taken after the index lock where both are held and
//! nothing is locked while it is held.
//!
//! **The ceiling** is `LEAN_CTX_GATEWAY_STORE_MAX_MB` (`0` = none), else the
//! value the configuration gives ([`note_configured_ceiling`]: 10 % of the
//! `max_disk_mb` budget, else [`DEFAULT_STORE_MB`]). The configuration is
//! not read here: a store write happens deep inside calls that may hold
//! locks the configuration loader needs. The value is noted where a call
//! takes its policy snapshot.
//!
//! **What fits.** After every admitted write
//! `booked bytes + held reservations ≤ ceiling`:
//! - A lifecycle is ENTERED with its first record — a Decision Receipt, an
//!   egress admission ([`Kind::Entry`]). It needs room for the record and
//!   for [`LIFECYCLE_RESERVE`], which is then held as a reservation for the
//!   records that follow (sidecar, observation, pointers, index entries).
//! - Every other write ([`Kind::FollowUp`]) takes its growth from the
//!   reservations still held; what they do not cover must fit under the
//!   ceiling itself. A reservation is held for [`RESERVATION_SECONDS`] — a
//!   lifecycle has no end the store could observe — and the pool is shared:
//!   a late record can use room another delivery reserved, and is then held
//!   to the ceiling like any late write.
//!
//! **A refusal** is the store's ordinary failure, with the caller's existing
//! meaning: a refused receipt is missing (delivered and logged without
//! protection, withheld under it — `ReceiptRecord::withholds`); a refused
//! sidecar fails the preparation and withholds a mandatory release; a
//! refused observation leaves the hand-over unconfirmed; a refused task
//! signal leaves that task's signals unknown.
//!
//! **Room is made, not taken.** A write that would not fit starts the
//! store's maintenance in the background ([`super::retention`]): it removes
//! the oldest receipts that are not protected (recent, listed by a project's
//! latest index, or of a task in progress) and brings the store down below
//! the ceiling. The write is refused only when the last pass over THIS store
//! counted its files and found the protected evidence alone filling it
//! ([`super::retention::proven_full`]); until then it is admitted and
//! booked.
//!
//! **The ledger is a cache, the files are the truth.** Every maintenance
//! pass counts the files and rebuilds the ledger from that count
//! ([`rebuild`]). A missing, damaged or stale ledger is never counted on the
//! write path — a store of 100 000 files would hold the lock for seconds —
//! it is used as it is, cannot prove the store full, and the next pass
//! rebuilds it. A store whose lock cannot be taken within [`LOCK_WAIT`] is
//! written to all the same and marked stale: maintenance holds the lock only
//! for short bookings, and a receipt is never refused for a lock.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

/// Default ceiling of the store when neither the override nor a disk budget
/// is set.
pub(crate) const DEFAULT_STORE_MB: u64 = 1024;
/// Room reserved at entry for the records a lifecycle writes afterwards: one
/// trace sidecar (≤ 256 KiB), one observation, the egress records and index
/// entries.
pub(crate) const LIFECYCLE_RESERVE: u64 = 512 * 1024;
/// How long an entry's reservation is held.
pub(crate) const RESERVATION_SECONDS: u64 = 30;

const LEDGER: &str = ".usage.json";
const LEDGER_TEMPORARY: &str = ".usage.json.tmp";
/// Prefix of the stale markers (`.usage.stale.<process>-<n>`).
const STALE: &str = ".usage.stale";
const LOCK: &str = ".usage.lock";
const LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(2);
const MAX_LEDGER_BYTES: u64 = 16 * 1024;

/// The ceiling (MB) the configuration gives, as last noted by a call.
static CONFIGURED_MB: AtomicU64 = AtomicU64::new(DEFAULT_STORE_MB);

/// Remember the configured ceiling. Called where a call takes its policy
/// snapshot — a point at which loading the configuration is safe.
pub(crate) fn note_configured_ceiling(megabytes: u64) {
    CONFIGURED_MB.store(megabytes, Ordering::Relaxed);
}

/// What a write is to the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// The first record of a new evidence lifecycle.
    Entry,
    /// Any other file of the store.
    FollowUp,
}

/// One second's worth of reservations: `bytes` are held until `expires`
/// (seconds since the Unix epoch).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reservation {
    expires: u64,
    bytes: u64,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ledger {
    schema_version: u32,
    bytes: u64,
    objects: u64,
    #[serde(default)]
    reserved: Vec<Reservation>,
}

impl Ledger {
    fn drop_lapsed(&mut self, now: u64) {
        self.reserved.retain(|held| held.expires > now);
    }

    fn held(&self, now: u64) -> u64 {
        self.reserved
            .iter()
            .filter(|held| held.expires > now)
            .fold(0u64, |sum, held| sum.saturating_add(held.bytes))
    }

    /// Take up to `bytes` out of the reservations still held, oldest first.
    fn consume(&mut self, now: u64, bytes: u64) {
        self.reserved.sort_by_key(|held| held.expires);
        let mut wanted = bytes;
        for held in self.reserved.iter_mut().filter(|held| held.expires > now) {
            let taken = held.bytes.min(wanted);
            held.bytes -= taken;
            wanted -= taken;
            if wanted == 0 {
                break;
            }
        }
        self.reserved.retain(|held| held.bytes > 0);
    }

    fn reserve(&mut self, now: u64, bytes: u64) {
        let expires = now.saturating_add(RESERVATION_SECONDS);
        match self
            .reserved
            .iter_mut()
            .find(|held| held.expires == expires)
        {
            Some(held) => held.bytes = held.bytes.saturating_add(bytes),
            None => self.reserved.push(Reservation { expires, bytes }),
        }
    }
}

/// What the store holds and what it may hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Usage {
    pub(crate) bytes: u64,
    pub(crate) objects: u64,
    /// Room held for lifecycles that entered within the last
    /// [`RESERVATION_SECONDS`].
    pub(crate) reserved: u64,
    /// `None`: no ceiling applies.
    pub(crate) limit_bytes: Option<u64>,
}

fn full_message(usage: &Usage) -> String {
    format!(
        "gateway evidence store is full ({} of {} MB, {} objects, {} KiB reserved): the write is \
         refused. The evidence left is protected (recent, latest-indexed or of a task in \
         progress); raise LEAN_CTX_GATEWAY_STORE_MAX_MB (or max_disk_mb) or lower \
         LEAN_CTX_GATEWAY_PROTECT_HOURS",
        usage.bytes / 1_048_576,
        usage.limit_bytes.unwrap_or(0) / 1_048_576,
        usage.objects,
        usage.reserved / 1024
    )
}

/// The ceiling in bytes: the environment override, else the noted
/// configuration value. No configuration is loaded here.
fn limit_bytes() -> Option<u64> {
    let megabytes = std::env::var("LEAN_CTX_GATEWAY_STORE_MAX_MB")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or_else(|| {
            // The noted value is process-wide. Tests run many configurations
            // in one process, each in its own data directory: there only the
            // override and the default apply, so no test's budget becomes
            // another test's ceiling.
            if cfg!(test) {
                DEFAULT_STORE_MB
            } else {
                CONFIGURED_MB.load(Ordering::Relaxed)
            }
        });
    megabytes_to_limit(megabytes)
}

fn megabytes_to_limit(megabytes: u64) -> Option<u64> {
    (megabytes > 0).then(|| megabytes.saturating_mul(1_048_576))
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

fn is_stale_marker(name: &std::ffi::OsStr) -> bool {
    name.to_str().is_some_and(|name| name.starts_with(STALE))
}

/// A file of the store's top directory that is bookkeeping, not evidence.
pub(super) fn is_bookkeeping(name: &std::ffi::OsStr) -> bool {
    name == LEDGER
        || name == LEDGER_TEMPORARY
        || name == LOCK
        || name == ".index.lock"
        || is_stale_marker(name)
        || name
            .to_str()
            .is_some_and(|name| name.starts_with(super::retention::BOOKKEEPING_PREFIX))
}

/// The stale markers present now. Each is its own file: a holder of the
/// lock removes exactly those it saw BEFORE it counted, so a marker set
/// while it counted survives and the next writer counts again.
fn stale_markers(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let unreadable = |error: std::io::Error| {
        format!(
            "gateway evidence store cannot be measured ({}: {error})",
            dir.display()
        )
    };
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(unreadable(error)),
    };
    let mut markers = Vec::new();
    for entry in entries {
        let entry = entry.map_err(unreadable)?;
        if is_stale_marker(&entry.file_name()) {
            markers.push(entry.path());
        }
    }
    Ok(markers)
}

/// Count every file of the store: `(bytes, objects)`. COMPLETE or an error —
/// a directory or file that cannot be read makes the count unusable, never
/// smaller; one that vanishes while it is counted (an atomic write's
/// temporary, a removal) was not there. The bookkeeping files and the index
/// lock are not evidence.
fn count_files(dir: &Path) -> Result<(u64, u64), String> {
    let gone = |error: &std::io::Error| error.kind() == std::io::ErrorKind::NotFound;
    let unreadable = |what: &Path, error: std::io::Error| {
        format!(
            "gateway evidence store cannot be measured ({}: {error})",
            what.display()
        )
    };
    if !dir.exists() {
        return Ok((0, 0));
    }
    let (mut bytes, mut objects) = (0u64, 0u64);
    let mut pending: Vec<PathBuf> = vec![dir.to_path_buf()];
    while let Some(directory) = pending.pop() {
        #[cfg(test)]
        crate::core::atomic_fs::listings::seen();
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if gone(&error) && directory != dir => continue,
            Err(error) => return Err(unreadable(&directory, error)),
        };
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
            } else if kind.is_file() && !(directory == dir && is_bookkeeping(&entry.file_name())) {
                let meta = match entry.metadata() {
                    Ok(meta) => meta,
                    Err(error) if gone(&error) => continue,
                    Err(error) => return Err(unreadable(&path, error)),
                };
                bytes = bytes.saturating_add(meta.len());
                objects += 1;
            }
        }
    }
    Ok((bytes, objects))
}

/// The ledger file as written, whatever the stale marker says.
fn read_raw(dir: &Path) -> Option<Ledger> {
    let mut bytes = Vec::new();
    std::fs::File::open(dir.join(LEDGER))
        .ok()?
        .take(MAX_LEDGER_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    let ledger: Ledger = serde_json::from_slice(&bytes).ok()?;
    (ledger.schema_version == 1).then_some(ledger)
}

/// The ledger as the write path sees it: never counted here.
struct Loaded {
    /// The ledger as written, or an empty one when there is none.
    ledger: Ledger,
    /// A ledger as written and no stale marker: it describes the files.
    trusted: bool,
    /// Stale markers are present (the ledger stays untrusted without a new
    /// one).
    marked: bool,
}

fn load(dir: &Path) -> Result<Loaded, String> {
    let marked = !stale_markers(dir)?.is_empty();
    let raw = read_raw(dir);
    Ok(Loaded {
        trusted: raw.is_some() && !marked,
        ledger: raw.unwrap_or(Ledger {
            schema_version: 1,
            ..Ledger::default()
        }),
        marked,
    })
}

/// The ledger's counts when it is trusted, else the files counted now —
/// without the lock, for a reader: complete or an error.
fn measured(dir: &Path) -> Result<Ledger, String> {
    let loaded = {
        let _lock = lock(dir);
        load(dir)?
    };
    if loaded.trusted {
        return Ok(loaded.ledger);
    }
    let (bytes, objects) = count_files(dir)?;
    Ok(Ledger {
        bytes,
        objects,
        ..loaded.ledger
    })
}

/// Write `ledger` and remove the stale markers it `answered`; a ledger that
/// cannot be written sets one, so the files are counted next time.
fn store(dir: &Path, ledger: &Ledger, answered: &[PathBuf]) {
    // Renamed into place, not fsynced: the ledger is rebuilt from the files
    // when it is lost. Only the lock holder writes it.
    let temporary = dir.join(LEDGER_TEMPORARY);
    let written = serde_json::to_vec(ledger).is_ok_and(|bytes| {
        std::fs::write(&temporary, bytes).is_ok()
            && std::fs::rename(&temporary, dir.join(LEDGER)).is_ok()
    });
    if written {
        for marker in answered {
            let _ = std::fs::remove_file(marker);
        }
    } else {
        mark_stale(dir);
    }
}

fn lock(dir: &Path) -> Option<std::fs::File> {
    std::fs::create_dir_all(dir).ok()?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(LOCK))
        .ok()?;
    // Windows reports a held lock as ERROR_LOCK_VIOLATION, not `WouldBlock`;
    // the shared helper waits on both instead of giving up at once.
    crate::core::file_lock::acquire_exclusive_timeout(&file, LOCK_WAIT)
        .ok()
        .map(|()| file)
}

/// The ledger no longer describes the files: the next writer counts them.
/// Set AFTER the write it reports, under a name of its own (see
/// [`stale_markers`]).
fn mark_stale(dir: &Path) {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let name = format!(
        "{STALE}.{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    let _ = std::fs::write(dir.join(name), b"");
}

/// Set the zero-byte marker `path` of the store. Booked like every write
/// where that is possible; where it is not (store busy, unmeasurable, over
/// its ceiling) the marker is still set and the files are counted again: it
/// says that something is NOT known, holds no bytes — it cannot take the
/// store over a ceiling in bytes — and must not be lost to the bound.
pub(crate) fn mark(dir: &Path, path: &Path) -> std::io::Result<()> {
    let set = || {
        std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)
            .map(drop)
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if write(dir, path, 0, Kind::FollowUp, || {
        set().map_err(|e| e.to_string())
    })
    .is_ok()
    {
        return Ok(());
    }
    let marked = set();
    mark_stale(dir);
    marked
}

/// What the store under `dir` holds against a ceiling of `megabytes`
/// (`0` = none), for the operator's view. `Err`: it cannot be measured.
pub(crate) fn usage_in(dir: &Path, megabytes: u64) -> Result<Usage, String> {
    usage_under(dir, megabytes_to_limit(megabytes), now())
}

fn usage_under(dir: &Path, limit_bytes: Option<u64>, now: u64) -> Result<Usage, String> {
    let ledger = if dir.is_dir() {
        measured(dir)?
    } else {
        Ledger::default()
    };
    Ok(Usage {
        bytes: ledger.bytes,
        objects: ledger.objects,
        reserved: ledger.held(now),
        limit_bytes,
    })
}

/// Why a write was not performed.
enum Refused {
    /// No room under the ceiling, as the ledger books it.
    Full(String),
    /// Anything else; `perform` may have run.
    Failed(String),
}

impl Refused {
    fn into_message(self) -> String {
        match self {
            Self::Full(message) | Self::Failed(message) => message,
        }
    }
}

/// Perform ONE write of the store's file `path`, which will hold `after`
/// bytes when `perform` succeeded: measured, decided, written and booked in
/// one critical section (see the module text). `perform` must write exactly
/// that file and take no lock. `Err`: refused or failed — a refusal happens
/// before `perform` runs, so nothing was written.
pub(crate) fn write<T>(
    dir: &Path,
    path: &Path,
    after: u64,
    kind: Kind,
    perform: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let limit = limit_bytes();
    let mut perform = Some(perform);
    match decide_and_write(dir, path, after, kind, limit, now(), &mut perform) {
        Ok(value) => Ok(value),
        // No room as booked: room is made in the background. The refusal
        // stands only when the last pass counted this store's files and
        // found protected evidence filling it.
        Err(Refused::Full(message)) => {
            super::retention::maintain_in_background(dir.to_path_buf(), true);
            if super::retention::proven_full(dir) {
                tracing::warn!("gateway evidence store write refused: {message}");
                return Err(message);
            }
            decide_and_write(dir, path, after, kind, None, now(), &mut perform)
                .map_err(Refused::into_message)
        }
        Err(Refused::Failed(message)) => Err(message),
    }
}

/// The store's ceiling in bytes; `None`: no ceiling.
pub(crate) fn ceiling_bytes() -> Option<u64> {
    limit_bytes()
}

/// The bytes the store holds: the ledger's when it is trusted, else counted.
#[cfg(test)]
pub(super) fn booked_bytes(dir: &Path) -> Result<u64, String> {
    Ok(measured(dir)?.bytes)
}

/// Book that `bytes` in `objects` files were removed from the store. An
/// untrusted ledger is left alone: the next rebuild counts the files.
pub(super) fn book_removal(dir: &Path, bytes: u64, objects: u64) {
    let Some(_lock) = lock(dir) else {
        mark_stale(dir);
        return;
    };
    match load(dir) {
        Ok(loaded) if loaded.trusted => {
            let mut ledger = loaded.ledger;
            ledger.bytes = ledger.bytes.saturating_sub(bytes);
            ledger.objects = ledger.objects.saturating_sub(objects);
            store(dir, &ledger, &[]);
        }
        Ok(_) => {}
        Err(_) => mark_stale(dir),
    }
}

/// The ledger as written and the stale markers present when a count of the
/// files begins: what [`rebuild`] reconciles that count with.
pub(super) struct Snapshot {
    ledger: Option<Ledger>,
    markers: Vec<PathBuf>,
}

/// Take the snapshot a recount starts from; `None`: the store is busy or
/// unreadable.
pub(super) fn snapshot(dir: &Path) -> Option<Snapshot> {
    let _lock = lock(dir)?;
    Some(Snapshot {
        ledger: read_raw(dir),
        markers: stale_markers(dir).ok()?,
    })
}

/// Rebuild the ledger from `counted` — `(bytes, objects)` of a complete
/// count of the files that began at `start` and ran WITHOUT the lock. What
/// writers booked meanwhile is added to the count (a file the count already
/// saw is then counted twice: the ledger may overstate by the writes during
/// the count, never understate), reservations are kept, and the stale
/// markers present at `start` are answered. `false`: nothing was rebuilt.
pub(super) fn rebuild(dir: &Path, start: &Snapshot, counted: (u64, u64)) -> bool {
    let Some(_lock) = lock(dir) else {
        return false;
    };
    // A process of an older version recounted meanwhile: its ledger is
    // newer than this count.
    if start.markers.iter().any(|marker| !marker.exists()) {
        return false;
    }
    let current = read_raw(dir);
    let shift = |counted: u64, before: u64, now: u64| {
        let value = i128::from(counted) + i128::from(now) - i128::from(before);
        u64::try_from(value.max(0)).unwrap_or(u64::MAX)
    };
    let (before, now_ledger) = match (&start.ledger, &current) {
        (Some(before), Some(now)) => (before.clone(), now.clone()),
        // No ledger before: writers meanwhile booked from zero.
        (None, Some(now)) => (Ledger::default(), now.clone()),
        // Removed meanwhile: nothing was booked that the count missed.
        (_, None) => (Ledger::default(), Ledger::default()),
    };
    let mut ledger = Ledger {
        schema_version: 1,
        bytes: shift(counted.0, before.bytes, now_ledger.bytes),
        objects: shift(counted.1, before.objects, now_ledger.objects),
        reserved: now_ledger.reserved,
    };
    ledger.drop_lapsed(now());
    store(dir, &ledger, &start.markers);
    true
}

/// The files of the store as counted, for tests that check the ledger.
#[cfg(test)]
pub(super) fn count_for_test(dir: &Path) -> (u64, u64) {
    count_files(dir).expect("countable store")
}

/// The ledger no longer describes the files: the next pass counts them.
pub(super) fn recount_later(dir: &Path) {
    mark_stale(dir);
}

#[cfg(test)]
fn write_under<T>(
    dir: &Path,
    path: &Path,
    after: u64,
    kind: Kind,
    limit: Option<u64>,
    now: u64,
    perform: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    decide_and_write(dir, path, after, kind, limit, now, &mut Some(perform))
        .map_err(Refused::into_message)
}

/// [`write_under`] that leaves `perform` in place when the write is refused
/// for room, so the caller may decide again.
fn decide_and_write<T, F: FnOnce() -> Result<T, String>>(
    dir: &Path,
    path: &Path,
    after: u64,
    kind: Kind,
    limit: Option<u64>,
    now: u64,
    perform: &mut Option<F>,
) -> Result<T, Refused> {
    let run = |perform: &mut Option<F>| match perform.take() {
        Some(perform) => perform().map_err(Refused::Failed),
        None => Err(Refused::Failed(
            "gateway evidence store write was already performed".to_owned(),
        )),
    };
    // A store that cannot be locked or read is still written to; the next
    // pass counts the files. A receipt is never refused for a lock.
    let unbooked = |perform: &mut Option<F>| {
        let outcome = run(perform);
        mark_stale(dir);
        super::retention::maintain_in_background(dir.to_path_buf(), true);
        outcome
    };
    let Some(_lock) = lock(dir) else {
        return unbooked(perform);
    };
    let measured = load(dir).and_then(|loaded| match std::fs::metadata(path) {
        Ok(meta) => Ok((loaded, Some(meta.len()))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok((loaded, None)),
        Err(error) => Err(format!(
            "gateway evidence store cannot be measured ({}: {error})",
            path.display()
        )),
    });
    let Ok((loaded, before)) = measured else {
        return unbooked(perform);
    };
    let Loaded {
        mut ledger,
        trusted,
        marked,
    } = loaded;
    ledger.drop_lapsed(now);
    let previous = before.unwrap_or(0);
    let growth = after.saturating_sub(previous);
    let held = ledger.held(now);
    // Only a trusted ledger can say the store is full.
    if let Some(limit) = limit.filter(|_| trusted) {
        let needed = match kind {
            Kind::Entry => growth.saturating_add(LIFECYCLE_RESERVE),
            Kind::FollowUp => growth.saturating_sub(held),
        };
        // A write that needs no new room (a shrinking file, a marker, growth
        // its reservation covers) is never what makes a store too large.
        if needed > 0 && ledger.bytes.saturating_add(held).saturating_add(needed) > limit {
            let usage = Usage {
                bytes: ledger.bytes,
                objects: ledger.objects,
                reserved: held,
                limit_bytes: Some(limit),
            };
            // The lapsed reservations are dropped on disk too.
            store(dir, &ledger, &[]);
            return Err(Refused::Full(full_message(&usage)));
        }
    }
    match run(perform) {
        Ok(value) => {
            ledger.bytes = ledger
                .bytes
                .saturating_add(growth)
                .saturating_sub(previous.saturating_sub(after));
            if before.is_none() {
                ledger.objects += 1;
            }
            match kind {
                Kind::Entry => ledger.reserve(now, LIFECYCLE_RESERVE),
                Kind::FollowUp => ledger.consume(now, growth),
            }
            store(dir, &ledger, &[]);
            if !trusted {
                // A ledger booked from nothing must not pass for a count.
                if !marked {
                    mark_stale(dir);
                }
                super::retention::maintain_in_background(dir.to_path_buf(), true);
            }
            Ok(value)
        }
        Err(failed) => {
            // What the failed write left behind is unknown: count again.
            mark_stale(dir);
            Err(failed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KIB: u64 = 1024;

    fn store_dir() -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().expect("store");
        let dir = directory.path().to_path_buf();
        std::fs::create_dir_all(dir.join("receipts")).expect("objects");
        // A counted store, as a maintenance pass leaves it.
        rebuild_now(&dir);
        (directory, dir)
    }

    /// A maintenance pass's recount: snapshot, count, rebuild.
    fn rebuild_now(dir: &Path) {
        let start = snapshot(dir).expect("snapshot");
        assert!(rebuild(dir, &start, count_files(dir).expect("countable")));
    }

    /// One real file write of `size` bytes through the store's one writer.
    fn put(
        dir: &Path,
        name: &str,
        size: u64,
        kind: Kind,
        limit: Option<u64>,
        now: u64,
    ) -> Result<(), String> {
        let path = dir.join("receipts").join(name);
        write_under(dir, &path, size, kind, limit, now, || {
            std::fs::write(&path, vec![b'x'; size as usize]).map_err(|e| e.to_string())
        })
    }

    /// The ledger agrees with the files on disk and respects the ceiling.
    fn within(dir: &Path, limit: u64, now: u64) -> Usage {
        let usage = usage_under(dir, Some(limit), now).expect("measurable");
        assert_eq!(
            (usage.bytes, usage.objects),
            count_files(dir).expect("countable"),
            "the ledger is what is on disk"
        );
        assert!(usage.bytes + usage.reserved <= limit, "{usage:?}");
        usage
    }

    /// Entries arriving together at the ceiling: room for exactly three
    /// lifecycles (record + reserve each) admits exactly three of sixteen
    /// simultaneous entries, however they interleave; the reserved room
    /// comes back when the reservations lapse, the written bytes do not.
    #[test]
    fn simultaneous_entries_at_the_ceiling_share_no_room() {
        const RECORD: u64 = 2_000;
        let (_directory, dir) = store_dir();
        let limit = 3 * (RECORD + LIFECYCLE_RESERVE) + RECORD;
        let start = std::sync::Arc::new(std::sync::Barrier::new(16));
        let entries: Vec<_> = (0..16)
            .map(|entry| {
                let (dir, start) = (dir.clone(), start.clone());
                std::thread::spawn(move || {
                    start.wait();
                    let name = format!("{entry}.json");
                    put(&dir, &name, RECORD, Kind::Entry, Some(limit), 1_000).is_ok()
                })
            })
            .collect();
        let admitted = entries
            .into_iter()
            .map(|entry| entry.join().expect("entry"))
            .filter(|admitted| *admitted)
            .count();
        assert_eq!(admitted, 3, "room for three lifecycles admits three");

        let held = within(&dir, limit, 1_000);
        assert_eq!(
            (held.bytes, held.objects, held.reserved),
            (3 * RECORD, 3, 3 * LIFECYCLE_RESERVE)
        );
        // Still held one second before the reservations lapse …
        let last = 1_000 + RESERVATION_SECONDS - 1;
        assert!(put(&dir, "late.json", RECORD, Kind::Entry, Some(limit), last).is_err());
        // … and free afterwards: the written bytes stay, the reserve returns.
        let later = 1_000 + RESERVATION_SECONDS;
        put(&dir, "later.json", RECORD, Kind::Entry, Some(limit), later).expect("room again");
        assert_eq!(within(&dir, limit, later).bytes, 4 * RECORD);
    }

    /// A lifecycle that outlives its reservation: the old one is still live,
    /// its reserve has lapsed, a new lifecycle took the room — and then the
    /// old one's follow-up write arrives. It is held to the real ceiling
    /// like every write; inside a held reservation a follow-up is served
    /// from it. An existing file's growth and shrinkage are booked as such.
    #[test]
    fn a_follow_up_write_after_its_reservation_lapsed_is_held_to_the_ceiling() {
        const RECORD: u64 = 2_000;
        const SIDECAR: u64 = 200_000;
        let (_directory, dir) = store_dir();
        // Room for exactly ONE lifecycle at a time, plus two records.
        let limit = LIFECYCLE_RESERVE + 3 * RECORD;
        let follow = |name: &str, size: u64, now: u64| {
            put(&dir, name, size, Kind::FollowUp, Some(limit), now)
        };

        // The old lifecycle enters at t=1000 and writes nothing more for now.
        put(&dir, "old.json", RECORD, Kind::Entry, Some(limit), 1_000).expect("old enters");
        assert_eq!(within(&dir, limit, 1_000).reserved, LIFECYCLE_RESERVE);
        // While its reservation is held, no second lifecycle fits.
        assert!(put(&dir, "second.json", RECORD, Kind::Entry, Some(limit), 1_010).is_err());

        // Its reservation lapses; a NEW lifecycle enters and takes the room …
        let later = 1_000 + RESERVATION_SECONDS;
        put(&dir, "new.json", RECORD, Kind::Entry, Some(limit), later).expect("new enters");
        // … and writes its own sidecar inside its reservation: served from it.
        follow("new.sidecar", SIDECAR, later + 1).expect("covered by the reservation");
        let held = within(&dir, limit, later + 1);
        assert_eq!(held.bytes, 2 * RECORD + SIDECAR);
        assert_eq!(held.reserved, LIFECYCLE_RESERVE - SIDECAR);

        // The new lifecycle's reservation lapses as well. Now the OLD
        // lifecycle's sidecar arrives: nothing is reserved for it any more,
        // and next to what is written it does not fit under the ceiling.
        let late = later + RESERVATION_SECONDS;
        assert_eq!(within(&dir, limit, late).reserved, 0);
        let room = limit - (2 * RECORD + SIDECAR);
        let refused = follow("old.sidecar", room + 1, late);
        assert!(refused.is_err(), "held to the ceiling: {refused:?}");
        assert!(
            !dir.join("receipts/old.sidecar").exists(),
            "nothing written"
        );
        assert_eq!(within(&dir, limit, late).bytes, 2 * RECORD + SIDECAR);
        // A late write that DOES fit is written — late is not forbidden,
        // only unbounded is. The store is then full to the byte.
        follow("old.sidecar", room, late).expect("fits to the byte");
        assert_eq!(within(&dir, limit, late).bytes, limit);
        // Nothing more: not one byte of growth of an existing file, no
        // lifecycle. Shrinking frees exactly what it frees.
        assert!(follow("old.json", RECORD + 1, late).is_err());
        assert!(put(&dir, "third.json", 1, Kind::Entry, Some(limit), late).is_err());
        follow("old.json", RECORD - 6, late).expect("shrinking always fits");
        assert_eq!(within(&dir, limit, late).bytes, limit - 6);
        follow("old.json", RECORD, late).expect("grows back into the freed room");
        assert_eq!(within(&dir, limit, late).bytes, limit);
    }

    /// The ledger is a cache: a stale one is never counted on the write
    /// path and never refuses — over its ceiling the write is admitted and
    /// booked — and a rebuild from a count of the files is exact, including
    /// a write booked after the count began.
    #[test]
    fn a_stale_ledger_admits_and_a_rebuild_counts_every_write() {
        let (_directory, dir) = store_dir();
        let limit = 1024 * KIB;
        put(&dir, "stock.bin", 800 * KIB, Kind::FollowUp, Some(limit), 0).expect("stock");
        // Over the ceiling as booked: refused while the ledger is trusted …
        assert!(put(&dir, "b.bin", 300 * KIB, Kind::FollowUp, Some(limit), 0).is_err());
        // … admitted once it is stale, which only a recount resolves.
        mark_stale(&dir);
        put(&dir, "b.bin", 300 * KIB, Kind::FollowUp, Some(limit), 0).expect("admitted");
        assert!(!load(&dir).expect("readable").trusted);

        // A recount that began before a write misses its file; the booking
        // during the count is added: nothing is forgotten.
        let start = snapshot(&dir).expect("snapshot");
        let counted = count_files(&dir).expect("countable");
        put(&dir, "c.bin", 10 * KIB, Kind::FollowUp, None, 0).expect("during the count");
        assert!(rebuild(&dir, &start, counted));
        assert!(load(&dir).expect("readable").trusted);
        let usage = usage_under(&dir, Some(limit), 0).expect("measurable");
        assert_eq!(
            (usage.bytes, usage.objects),
            count_files(&dir).expect("countable")
        );
        assert_eq!(usage.bytes, 1110 * KIB);
    }

    /// A ledger that disagrees with the files — copied with a store it does
    /// not describe, or restored from elsewhere — is replaced by the count,
    /// whether it overstates or understates.
    #[test]
    fn a_rebuild_replaces_a_ledger_that_overstates_or_understates() {
        let (_directory, dir) = store_dir();
        put(&dir, "kept.bin", 10 * KIB, Kind::FollowUp, None, 0).expect("kept");
        for claimed in [7 << 30, 0] {
            let wrong = Ledger {
                schema_version: 1,
                bytes: claimed,
                objects: 250_056,
                reserved: Vec::new(),
            };
            store(&dir, &wrong, &[]);
            rebuild_now(&dir);
            assert_eq!(within(&dir, 1024 * KIB, 0).bytes, 10 * KIB);
        }
    }

    /// A store that cannot be measured completely is not certified: the
    /// reader gets no usage and a rebuild is impossible — and a write is
    /// still admitted (a receipt is never refused for what the store's
    /// bookkeeping cannot know). A lost ledger alone is no obstacle: the
    /// files are the truth.
    #[cfg(unix)]
    #[test]
    fn a_partial_count_certifies_nothing_and_refuses_nothing() {
        use std::os::unix::fs::PermissionsExt;
        let (_directory, dir) = store_dir();
        let limit = 1024 * KIB;
        put(&dir, "kept.bin", 10 * KIB, Kind::FollowUp, Some(limit), 0).expect("kept");
        std::fs::remove_file(dir.join(LEDGER)).expect("lost ledger");
        assert_eq!(
            usage_under(&dir, Some(limit), 0).expect("countable").bytes,
            10 * KIB,
            "counted from the files"
        );

        // A directory of the store that cannot be read, and a stale ledger.
        let sealed = dir.join("sealed");
        std::fs::create_dir(&sealed).expect("directory");
        std::fs::write(sealed.join("hidden.bin"), vec![b'h'; 1000]).expect("hidden");
        std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o000)).expect("seal");
        // (A process that may read everything — root — cannot run this case.)
        let sealable = std::fs::read_dir(&sealed).is_err();
        mark_stale(&dir);
        let admitted = put(&dir, "new.bin", KIB, Kind::FollowUp, Some(limit), 0);
        let unmeasured = usage_under(&dir, Some(limit), 0);
        let uncounted = count_files(&dir);
        std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o700)).expect("open");
        assert!(admitted.is_ok(), "{admitted:?}");
        if !sealable {
            return;
        }
        assert!(
            unmeasured.is_err(),
            "no usage from a partial count: {unmeasured:?}"
        );
        assert!(uncounted.is_err());
        // Readable again: counted completely, hidden file included.
        rebuild_now(&dir);
        assert_eq!(within(&dir, limit, 0).bytes, 11 * KIB + 1000);
    }
}
