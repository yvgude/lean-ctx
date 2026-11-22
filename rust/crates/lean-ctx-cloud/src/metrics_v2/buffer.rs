// SPDX-License-Identifier: Apache-2.0
//! The offline buffer of engine metrics and its state, under
//! `<data_dir>/cloud/metrics-v2/` (0600 files, written atomically):
//!
//! - `state.json` ([`State`]): the workspace it belongs to, the open hour and
//!   the cumulative counters at its start, the rev counter, back-off, and the
//!   refusals Cloud answered (kept until the lease or policy changes);
//! - `buffer/<hour>.json` ([`Item`]): one hour: its sampled part (frozen
//!   once the hour closed), the bucket last built and the rev Cloud acked.
//!
//! At most [`MAX_ITEMS`] hours and [`MAX_BYTES`]; when full the oldest hour
//! gives way and is counted in `dropped_buckets`. An hour Cloud acked is
//! removed once it is older than the previous hour (closed hours are never
//! rebuilt), so the buffer holds only what still has to go.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use lean_ctx_time::{OffsetDateTime, UtcDateTime};
use serde::{Deserialize, Serialize};

use super::collect::{Sampled, Snapshot};
use super::gate::OffReason;
use crate::CloudError;
use crate::state::write_private;
use lean_ctx_protocol::cloud::metrics_v2::{BucketV2, HourStamp, PolicyLevel, SharingStateV2};

pub(crate) const DIR: &str = "metrics-v2";
const STATE_FILE: &str = "state.json";
const BUFFER_DIR: &str = "buffer";
const STATE_SCHEMA: &str = "leanctx.cloud.engine-metrics-state/v1";
/// 30 days of hours.
pub(crate) const MAX_ITEMS: usize = 720;
pub(crate) const MAX_BYTES: u64 = 8 * 1024 * 1024;

/// A refusal Cloud answered, in force until what it names changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub(crate) enum Refusal {
    /// `403 policy_denied` under this policy version.
    Policy {
        policy_version: u64,
        level: Option<PolicyLevel>,
    },
    /// `403 upload_disabled` (or `forbidden`) under this lease.
    Lease { lease_id: String, reason: OffReason },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct State {
    #[serde(default)]
    pub(crate) schema: String,
    #[serde(default)]
    pub(crate) org_id: String,
    /// The hour being counted and the cumulative counters at its start.
    #[serde(default)]
    pub(crate) open_hour: Option<HourStamp>,
    #[serde(default)]
    pub(crate) open_start: Snapshot,
    /// The first hour that may be built (first connect: 30 days back;
    /// after sharing was off: the hour it came back on).
    #[serde(default)]
    pub(crate) collect_from: Option<HourStamp>,
    /// Hours before this were built once and are closed.
    #[serde(default)]
    pub(crate) built_until: Option<HourStamp>,
    #[serde(default)]
    pub(crate) next_rev: u32,
    #[serde(default)]
    pub(crate) last_sent_at: Option<String>,
    #[serde(default)]
    pub(crate) dropped_buckets: u64,
    #[serde(default)]
    pub(crate) refused_slices: u64,
    /// Back-off after 429/5xx: no send before this (unix seconds).
    #[serde(default)]
    pub(crate) retry_at: Option<i64>,
    #[serde(default)]
    pub(crate) attempts: u32,
    #[serde(default)]
    pub(crate) last_run: Option<i64>,
    #[serde(default)]
    pub(crate) refusal: Option<Refusal>,
    /// The feature Cloud switched off (`404 feature_disabled`, e.g.
    /// `instance_metrics`): nothing is sent before `retry_at`, not even by
    /// a manual sync; the hours stay buffered for when it is on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) switched_off: Option<String>,
    /// The sharing state Cloud last answered for `org_id`.
    #[serde(default)]
    pub(crate) sharing: Option<SharingStateV2>,
}

/// One hour in the buffer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Item {
    pub(crate) hour: Option<HourStamp>,
    /// The part from cumulative counters, per wire key.
    #[serde(default)]
    pub(crate) sampled: BTreeMap<String, Sampled>,
    /// The bucket as last built (`None`: nothing with activity).
    #[serde(default)]
    pub(crate) bucket: Option<BucketV2>,
    #[serde(default)]
    pub(crate) acked_rev: u32,
}

impl Item {
    pub(crate) fn pending(&self) -> bool {
        self.bucket.as_ref().is_some_and(|b| b.rev > self.acked_rev)
    }
}

pub(crate) fn dir_in(cloud_data: &Path) -> PathBuf {
    cloud_data.join(DIR)
}

impl State {
    pub(crate) fn load(cloud_data: &Path) -> Self {
        std::fs::read(dir_in(cloud_data).join(STATE_FILE))
            .ok()
            .and_then(|b| serde_json::from_slice::<Self>(&b).ok())
            .filter(|s| s.schema == STATE_SCHEMA)
            .unwrap_or_default()
    }

    pub(crate) fn store(&mut self, cloud_data: &Path) -> Result<(), CloudError> {
        self.schema = STATE_SCHEMA.into();
        let bytes = serde_json::to_vec(self).map_err(|e| CloudError::Local(e.to_string()))?;
        write_private(&dir_in(cloud_data), STATE_FILE, &bytes)
    }

    /// The refusal in force under `lease_id` and `policy_version`, if any. A
    /// newer policy (or lease) lifts it.
    pub(crate) fn refused(
        &self,
        lease_id: &str,
        policy_version: u64,
    ) -> Option<(OffReason, Option<PolicyLevel>)> {
        match self.refusal.as_ref()? {
            Refusal::Policy {
                policy_version: at,
                level,
            } if *at >= policy_version => Some((OffReason::Policy, *level)),
            Refusal::Lease {
                lease_id: at,
                reason,
            } if at == lease_id => Some((*reason, None)),
            _ => None,
        }
    }

    pub(crate) fn rev(&mut self) -> u32 {
        self.next_rev = self.next_rev.saturating_add(1).max(1);
        self.next_rev
    }
}

/// The buffered hours under `cloud_data`.
pub(crate) struct Buffer {
    dir: PathBuf,
}

impl Buffer {
    pub(crate) fn at(cloud_data: &Path) -> Self {
        Self {
            dir: dir_in(cloud_data).join(BUFFER_DIR),
        }
    }

    fn path(&self, hour: &HourStamp) -> PathBuf {
        // `2026-10-26T17:00:00Z` → `2026-10-26T17.json` (no `:` in names).
        self.dir.join(format!("{}.json", &hour.as_str()[..13]))
    }

    fn files(&self) -> Vec<(PathBuf, u64)> {
        let mut files: Vec<(PathBuf, u64)> = std::fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".json"))
            .map(|e| (e.path(), e.metadata().map_or(0, |m| m.len())))
            .collect();
        files.sort();
        files
    }

    pub(crate) fn get(&self, hour: &HourStamp) -> Option<Item> {
        std::fs::read(self.path(hour))
            .ok()
            .and_then(|b| serde_json::from_slice::<Item>(&b).ok())
            .filter(|i| i.hour.as_ref() == Some(hour))
    }

    pub(crate) fn put(&self, item: &Item) -> Result<(), CloudError> {
        let Some(hour) = item.hour.as_ref() else {
            return Ok(());
        };
        let bytes = serde_json::to_vec(item).map_err(|e| CloudError::Local(e.to_string()))?;
        let name = self
            .path(hour)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        write_private(&self.dir, &name, &bytes)
    }

    pub(crate) fn remove(&self, hour: &HourStamp) {
        let _ = std::fs::remove_file(self.path(hour));
    }

    /// Every hour, oldest first. Damaged files are removed.
    pub(crate) fn items(&self) -> Vec<Item> {
        self.files()
            .into_iter()
            .filter_map(|(path, _)| {
                let item = std::fs::read(&path)
                    .ok()
                    .and_then(|b| serde_json::from_slice::<Item>(&b).ok())
                    .filter(|i| i.hour.is_some());
                if item.is_none() {
                    let _ = std::fs::remove_file(&path);
                }
                item
            })
            .collect()
    }

    /// Hours waiting to be sent or acked.
    pub(crate) fn pending(&self) -> usize {
        self.items().iter().filter(|i| i.pending()).count()
    }

    /// Drop the oldest hours until at most `max_items` / `max_bytes` remain;
    /// returns how many unsent hours were dropped.
    pub(crate) fn enforce_cap(&self, max_items: usize, max_bytes: u64) -> u64 {
        let files = self.files();
        let mut count = files.len();
        let mut bytes: u64 = files.iter().map(|(_, n)| n).sum();
        let mut dropped = 0;
        for (path, size) in files {
            if count <= max_items && bytes <= max_bytes {
                break;
            }
            let unsent = std::fs::read(&path)
                .ok()
                .and_then(|b| serde_json::from_slice::<Item>(&b).ok())
                .is_none_or(|i| i.pending());
            if std::fs::remove_file(&path).is_ok() {
                count -= 1;
                bytes = bytes.saturating_sub(size);
                if unsent {
                    dropped += 1;
                }
            }
        }
        dropped
    }

    /// Delete every buffered hour (sharing off, workspace changed, delete).
    pub(crate) fn clear(&self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Sharing is off: drop the buffer and the counting baseline, keep the
/// refusal, the counters people see, and remember that a restart begins at
/// the hour sharing comes back (no backfill over the off period).
pub(crate) fn drop_all(cloud_data: &Path) -> Result<(), CloudError> {
    note_local_off(cloud_data);
    Buffer::at(cloud_data).clear();
    super::secrets::remove(cloud_data);
    let mut state = State::load(cloud_data);
    if state.open_hour.is_none()
        && state.built_until.is_none()
        && Buffer::at(cloud_data).items().is_empty()
        && state.schema.is_empty()
    {
        return Ok(());
    }
    let kept = State {
        org_id: state.org_id.clone(),
        next_rev: state.next_rev,
        last_sent_at: state.last_sent_at.take(),
        dropped_buckets: state.dropped_buckets,
        refusal: state.refusal.take(),
        sharing: state.sharing.take(),
        ..State::default()
    };
    let mut kept = kept;
    if kept != state {
        kept.store(cloud_data)?;
    }
    Ok(())
}

/// Delete everything this engine holds for the workspace (`cloud metrics
/// delete`, disconnect): buffer, state, fingerprint key and hits.
pub(crate) fn forget(cloud_data: &Path) {
    super::secrets::remove(cloud_data);
    let _ = std::fs::remove_dir_all(dir_in(cloud_data));
}

/// Engine-local "on since" (K2 review 3, NEW-1), next to the metrics dir
/// so [`drop_all`] and [`forget`] keep it (an off is noted by `drop_all`,
/// `receipts::stop_local_recording` and `cloud metrics delete`; `forget`
/// alone also runs for a first pass or another workspace): `off` while this engine holds
/// metrics off, else the time sharing last came back on here. The server's
/// `since` only moves with an enrolment or a policy change, so without it
/// an engine-side off period (`metrics_upload = false`, `enabled = false`,
/// a lapsed lease, a refusal) would be rebuilt from the local stores.
const ON_SINCE_FILE: &str = "metrics-v2.on-since";
const LOCAL_OFF: &str = "off";

/// This engine holds metrics off now. Only where the Cloud data dir exists
/// (a machine that never connected gets no file).
pub(crate) fn note_local_off(cloud_data: &Path) {
    if !cloud_data.is_dir() {
        return;
    }
    let path = cloud_data.join(ON_SINCE_FILE);
    if std::fs::read_to_string(&path).is_ok_and(|t| t.trim() == LOCAL_OFF) {
        return;
    }
    let _ = write_private(cloud_data, ON_SINCE_FILE, LOCAL_OFF.as_bytes());
}

/// A new enrolment: Cloud' `since` starts over and bounds the period.
pub(crate) fn clear_local_on_since(cloud_data: &Path) {
    let _ = std::fs::remove_file(cloud_data.join(ON_SINCE_FILE));
}

/// The start of this engine's current on period, when an off period came
/// before it: after an off, the next whole hour from `now` (no part of an
/// hour that held off time is built), recorded once.
pub(crate) fn local_on_since(cloud_data: &Path, now: UtcDateTime) -> Option<UtcDateTime> {
    let text = std::fs::read_to_string(cloud_data.join(ON_SINCE_FILE)).ok()?;
    let text = text.trim();
    if text == LOCAL_OFF {
        let start = HourStamp::of(now).start();
        let on = if start == now {
            now
        } else {
            start + lean_ctx_time::Duration::hours(1)
        };
        let stamp = on.to_rfc3339_opts(lean_ctx_time::SecondsFormat::Secs, true);
        let _ = write_private(cloud_data, ON_SINCE_FILE, stamp.as_bytes());
        return Some(on);
    }
    OffsetDateTime::parse_from_rfc3339(text)
        .ok()
        .map(lean_ctx_time::OffsetDateTime::to_utc)
}
