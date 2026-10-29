// SPDX-License-Identifier: Apache-2.0
//! Sealed segments of the audit trail.
//!
//! The active trail (`trail.jsonl`) is sealed once it reaches
//! [`SEGMENT_MAX_BYTES`] or the UTC day of its last entry has passed: the
//! writer appends a signed [`SEAL_EVENT`] entry naming the segment number and
//! renames the file to `trail.segments/<seq>.jsonl`, all under the exclusive
//! trail lock. The next append starts a new active file whose first entry
//! links to the seal, so the chain runs unbroken through every segment in
//! order. Sealed segments are never written again; nothing is deleted.
//!
//! A trail written before segmenting becomes segment 0 at its first seal,
//! and only once this build has verified it (a valid checkpoint exists):
//! an unverified trail is never sealed. Trails outside the data dir have no
//! checkpoint and are never sealed.
//!
//! Downgrade: a reader that predates segmenting (3.11.x) sees only the
//! active file, whose first entry links to a seal instead of `genesis`, and
//! reports the chain invalid. Audit verification after a downgrade from
//! 3.12.0 with a sealed trail is not supported.

use std::fs;
use std::path::{Path, PathBuf};

/// Size at which the active trail is sealed.
pub(super) const SEGMENT_MAX_BYTES: u64 = 64 * 1024 * 1024;
/// Event type of the entry that closes a segment. In the `x-` namespace, so
/// a reader that predates segmenting counts it as an unknown additive event.
pub(super) const SEAL_EVENT: &str = "x-segment_sealed";

/// `<dir>/<stem>.segments` next to the active trail.
pub(super) fn dir(trail: &Path) -> PathBuf {
    let stem = trail.file_stem().map_or_else(
        || "trail".into(),
        |stem| stem.to_string_lossy().into_owned(),
    );
    trail.with_file_name(format!("{stem}.segments"))
}

/// Path of sealed segment `seq`.
pub(super) fn path(trail: &Path, seq: u64) -> PathBuf {
    dir(trail).join(format!("{seq:06}.jsonl"))
}

/// The sealed segments of `trail`, oldest first. Only names this module
/// writes count; anything else in the directory is ignored.
pub(super) fn sealed(trail: &Path) -> Vec<(u64, PathBuf)> {
    let Ok(entries) = fs::read_dir(dir(trail)) else {
        return Vec::new();
    };
    let mut found: Vec<(u64, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name();
            let digits = name.to_str()?.strip_suffix(".jsonl")?;
            if digits.len() < 6 || !digits.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            Some((digits.parse().ok()?, entry.path()))
        })
        .collect();
    found.sort_unstable_by_key(|(seq, _)| *seq);
    found
}

/// Every file of the trail in chain order: the sealed segments, then the
/// active trail.
pub(super) fn chain(trail: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = sealed(trail).into_iter().map(|(_, path)| path).collect();
    files.push(trail.to_path_buf());
    files
}

/// The `action` of the seal entry for segment `seq`.
pub(super) fn seal_action(seq: u64) -> String {
    format!("segment={seq}")
}

/// The segment number a seal entry's `action` names.
pub(super) fn sealed_seq(action: Option<&str>) -> Option<u64> {
    action?.strip_prefix("segment=")?.parse().ok()
}

/// Whether an active trail of `len` bytes whose last entry was written at
/// `last` (RFC 3339) is due to be sealed at `now`.
pub(super) fn due(len: u64, last: Option<&str>, now: chrono::DateTime<chrono::Utc>) -> bool {
    if len == 0 {
        return false;
    }
    if len >= SEGMENT_MAX_BYTES {
        return true;
    }
    last.and_then(|stamp| chrono::DateTime::parse_from_rfc3339(stamp).ok())
        .is_some_and(|last| last.with_timezone(&chrono::Utc).date_naive() < now.date_naive())
}

/// How long a process waits before sealing again after a failed rename, so
/// a persistent failure does not add a seal to every append of a full trail.
const BACK_OFF_SECS: i64 = 60 * 60;
static BACK_OFF_UNTIL: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

pub(super) fn back_off() {
    BACK_OFF_UNTIL.store(
        chrono::Utc::now().timestamp() + BACK_OFF_SECS,
        std::sync::atomic::Ordering::Relaxed,
    );
}

pub(super) fn backing_off() -> bool {
    chrono::Utc::now().timestamp() < BACK_OFF_UNTIL.load(std::sync::atomic::Ordering::Relaxed)
}

/// Move the sealed active trail to segment `seq`. Rename only: a reader
/// that has the file open keeps reading the same bytes. Refuses to replace
/// an existing segment.
pub(super) fn retire(trail: &Path, seq: u64) -> Result<(), String> {
    let dir = dir(trail);
    fs::create_dir_all(&dir).map_err(|e| format!("audit segments dir: {e}"))?;
    crate::core::data_dir::ensure_dir_permissions(&dir);
    let target = path(trail, seq);
    if target.exists() {
        return Err(format!("audit segment {seq} already exists"));
    }
    fs::rename(trail, &target).map_err(|e| format!("audit segment rename: {e}"))?;
    #[cfg(unix)]
    for synced in [
        &dir,
        &trail.parent().map_or_else(PathBuf::new, Path::to_path_buf),
    ] {
        if let Ok(handle) = fs::File::open(synced) {
            let _ = handle.sync_all();
        }
    }
    Ok(())
}
