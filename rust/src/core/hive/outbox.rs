// SPDX-License-Identifier: Apache-2.0
//! Durable outbox: everything bound for Hive is written here first and
//! sent later by the scheduler (or `lean-ctx hive sync`), so Hive never
//! blocks a tool call and nothing is lost while offline.
//!
//! `<data_dir>/hive/outbox/<channel>/<id>.json`, one 0600 file per item,
//! ids sort by creation time and double as `Idempotency-Key`. At most
//! 10 000 items / 50 MB: when full, the oldest metrics give way; knowledge
//! is never dropped silently (enqueueing fails instead). An item Hive
//! refuses for good moves to `outbox/failed/<channel>/` and stays visible
//! in `hive status`; each channel keeps the newest [`MAX_FAILED`] there and
//! the newest [`MAX_DAMAGED`] damaged files (RB8 M14).

use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::HiveError;
use super::state::write_private;

pub const CHANNEL_KNOWLEDGE: &str = "knowledge";
pub const CHANNEL_METRICS: &str = "metrics";
pub const CHANNEL_RECEIPTS: &str = "receipts";
/// Task proposals (C0 1.9 §1): agent tasks and contradictions.
pub const CHANNEL_TASKS: &str = "tasks";
const CHANNELS: [&str; 4] = [
    CHANNEL_KNOWLEDGE,
    CHANNEL_METRICS,
    CHANNEL_RECEIPTS,
    CHANNEL_TASKS,
];

const MAX_ITEMS: usize = 10_000;
const MAX_BYTES: u64 = 50 * 1024 * 1024;
/// Refused items kept per channel under `failed/`, newest first.
pub const MAX_FAILED: usize = 500;
/// Damaged item files (`*.damaged`) kept per channel, newest first.
pub const MAX_DAMAGED: usize = 20;
const BACKOFF_START_SECONDS: i64 = 30;
const BACKOFF_MAX_SECONDS: i64 = 3600;
/// A `Retry-After` beyond this is a broken or hostile answer: waiting is
/// capped, never computed past chrono's range (which panics).
const RETRY_AFTER_MAX_SECONDS: i64 = 7 * 24 * 3600;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutboxItem {
    pub id: String,
    pub channel: String,
    pub created_at: String,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_attempt_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// The wait is a policy refusal's (`policy_denied`): a newer policy in
    /// force ends it at once ([`Outbox::release_policy_holds`]).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub policy_hold: bool,
    pub payload: Value,
}

impl OutboxItem {
    /// Whether the item may be sent at `now`.
    pub fn due(&self, now: DateTime<Utc>) -> bool {
        self.next_attempt_at
            .as_deref()
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            .is_none_or(|at| at.with_timezone(&Utc) <= now)
    }
}

/// Counts for `hive status`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OutboxDepth {
    pub pending: usize,
    pub failed: usize,
}

pub struct Outbox {
    root: PathBuf,
}

fn valid_channel(channel: &str) -> bool {
    matches!(
        channel,
        CHANNEL_KNOWLEDGE | CHANNEL_METRICS | CHANNEL_RECEIPTS | CHANNEL_TASKS
    )
}

fn stamp(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

impl Outbox {
    pub fn at(data_dir: &Path) -> Self {
        Self {
            root: data_dir.join("outbox"),
        }
    }

    fn dir(&self, channel: &str) -> PathBuf {
        self.root.join(channel)
    }

    fn failed_dir(&self, channel: &str) -> PathBuf {
        self.root.join("failed").join(channel)
    }

    /// Queue `payload` on `channel`; returns the item id.
    pub fn enqueue(
        &self,
        channel: &str,
        payload: Value,
        now: DateTime<Utc>,
    ) -> Result<String, HiveError> {
        if !valid_channel(channel) {
            return Err(HiveError::Local(format!(
                "unknown outbox channel {channel:?}"
            )));
        }
        let mut random = [0u8; 8];
        getrandom::fill(&mut random).map_err(|e| HiveError::Local(format!("random: {e}")))?;
        let id = format!(
            "{:013}{}",
            now.timestamp_millis().max(0),
            hex::encode(random)
        );
        let item = OutboxItem {
            id: id.clone(),
            channel: channel.to_string(),
            created_at: stamp(now),
            attempts: 0,
            next_attempt_at: None,
            last_error: None,
            policy_hold: false,
            payload,
        };
        let bytes = serde_json::to_vec(&item).map_err(|e| HiveError::Local(e.to_string()))?;
        self.make_room(channel, bytes.len() as u64)?;
        write_private(&self.dir(channel), &format!("{id}.json"), &bytes)?;
        Ok(id)
    }

    fn files(dir: &Path) -> Vec<(PathBuf, u64)> {
        let mut files: Vec<(PathBuf, u64)> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".json"))
            .map(|e| (e.path(), e.metadata().map_or(0, |m| m.len())))
            .collect();
        files.sort();
        files
    }

    fn make_room(&self, channel: &str, incoming: u64) -> Result<(), HiveError> {
        let channels = CHANNELS;
        let mut count: usize = channels
            .iter()
            .map(|c| Self::files(&self.dir(c)).len())
            .sum();
        let mut bytes: u64 = channels
            .iter()
            .flat_map(|c| Self::files(&self.dir(c)))
            .map(|(_, size)| size)
            .sum();
        let mut metrics = Self::files(&self.dir(CHANNEL_METRICS)).into_iter();
        while count + 1 > MAX_ITEMS || bytes + incoming > MAX_BYTES {
            let Some((oldest, size)) = metrics.next() else {
                return Err(HiveError::Local(format!(
                    "the Hive outbox is full; {channel} is kept back until Hive is reachable again"
                )));
            };
            let _ = std::fs::remove_file(oldest);
            count -= 1;
            bytes = bytes.saturating_sub(size);
        }
        Ok(())
    }

    /// Pending items of `channel`, oldest first. Damaged files are moved aside.
    pub fn list(&self, channel: &str) -> Vec<OutboxItem> {
        Self::files(&self.dir(channel))
            .into_iter()
            .filter_map(|(path, _)| {
                let parsed = std::fs::read(&path)
                    .ok()
                    .and_then(|b| serde_json::from_slice::<OutboxItem>(&b).ok());
                if parsed.is_none() {
                    let _ = std::fs::rename(&path, path.with_extension("damaged"));
                    keep_newest(&self.dir(channel), ".damaged", MAX_DAMAGED);
                }
                parsed
            })
            .collect()
    }

    pub fn remove(&self, item: &OutboxItem) -> Result<(), HiveError> {
        match std::fs::remove_file(self.dir(&item.channel).join(format!("{}.json", item.id))) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(HiveError::Local(format!("outbox: {e}"))),
        }
    }

    /// Replace an item (e.g. after a rebase) keeping its id.
    pub fn update(&self, item: &OutboxItem) -> Result<(), HiveError> {
        let bytes = serde_json::to_vec(item).map_err(|e| HiveError::Local(e.to_string()))?;
        write_private(
            &self.dir(&item.channel),
            &format!("{}.json", item.id),
            &bytes,
        )
    }

    /// Record a transient failure: exponential backoff 30 s → 1 h with
    /// jitter, or Hive' `Retry-After` when given.
    pub fn retry_later(
        &self,
        item: &mut OutboxItem,
        error: &str,
        retry_after: Option<u64>,
        now: DateTime<Utc>,
    ) -> Result<(), HiveError> {
        item.policy_hold = false;
        self.wait(item, error, retry_after, now)
    }

    /// [`Self::retry_later`] for a policy refusal: the item waits
    /// `retry_after` (or the backoff), or until a newer policy is in force.
    pub fn hold_for_policy(
        &self,
        item: &mut OutboxItem,
        error: &str,
        retry_after: Option<u64>,
        now: DateTime<Utc>,
    ) -> Result<(), HiveError> {
        item.policy_hold = true;
        self.wait(item, error, retry_after, now)
    }

    /// A newer policy is in force: every item waiting on a policy refusal
    /// is due again, on every channel; other waits stay. Returns how many.
    pub fn release_policy_holds(&self) -> usize {
        let mut released = 0;
        for channel in CHANNELS {
            for mut item in self.list(channel).into_iter().filter(|i| i.policy_hold) {
                item.policy_hold = false;
                item.next_attempt_at = None;
                if self.update(&item).is_ok() {
                    released += 1;
                }
            }
        }
        released
    }

    fn wait(
        &self,
        item: &mut OutboxItem,
        error: &str,
        retry_after: Option<u64>,
        now: DateTime<Utc>,
    ) -> Result<(), HiveError> {
        item.attempts = item.attempts.saturating_add(1);
        let exponent = item.attempts.saturating_sub(1).min(7);
        let backoff = (BACKOFF_START_SECONDS << exponent).min(BACKOFF_MAX_SECONDS);
        let mut jitter = [0u8; 2];
        let _ = getrandom::fill(&mut jitter);
        let jitter = i64::from(u16::from_le_bytes(jitter)) % (backoff / 4 + 1);
        let wait = retry_after
            .map_or(backoff + jitter, |s| i64::try_from(s).unwrap_or(i64::MAX))
            .clamp(0, RETRY_AFTER_MAX_SECONDS);
        item.next_attempt_at = Some(stamp(now + Duration::seconds(wait)));
        item.last_error = Some(error.chars().take(300).collect());
        self.update(item)
    }

    /// Hive refused the item for good: keep it visible under `failed/`.
    pub fn fail(&self, item: &mut OutboxItem, error: &str) -> Result<(), HiveError> {
        item.last_error = Some(error.chars().take(300).collect());
        let bytes = serde_json::to_vec(item).map_err(|e| HiveError::Local(e.to_string()))?;
        write_private(
            &self.failed_dir(&item.channel),
            &format!("{}.json", item.id),
            &bytes,
        )?;
        keep_newest(&self.failed_dir(&item.channel), ".json", MAX_FAILED);
        self.remove(item)
    }

    pub fn failed(&self, channel: &str) -> Vec<OutboxItem> {
        Self::files(&self.failed_dir(channel))
            .into_iter()
            .filter_map(|(path, _)| {
                std::fs::read(&path)
                    .ok()
                    .and_then(|b| serde_json::from_slice(&b).ok())
            })
            .collect()
    }

    pub fn depth(&self, channel: &str) -> OutboxDepth {
        OutboxDepth {
            pending: Self::files(&self.dir(channel)).len(),
            failed: Self::files(&self.failed_dir(channel)).len(),
        }
    }
}

/// Remove the oldest files of `dir` ending in `suffix` beyond `keep` (ids,
/// and so names, sort by creation time).
fn keep_newest(dir: &Path, suffix: &str, keep: usize) {
    let mut names: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().ends_with(suffix))
        .map(|e| e.path())
        .collect();
    names.sort();
    let excess = names.len().saturating_sub(keep);
    for old in names.drain(..excess) {
        let _ = std::fs::remove_file(old);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-20T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn items_survive_reopen_in_order_and_keep_their_ids() {
        let dir = tempfile::tempdir().unwrap();
        let outbox = Outbox::at(dir.path());
        let first = outbox
            .enqueue(CHANNEL_KNOWLEDGE, json!({"n": 1}), now())
            .unwrap();
        let second = outbox
            .enqueue(
                CHANNEL_KNOWLEDGE,
                json!({"n": 2}),
                now() + Duration::milliseconds(5),
            )
            .unwrap();
        let items = Outbox::at(dir.path()).list(CHANNEL_KNOWLEDGE);
        assert_eq!(
            items.iter().map(|i| i.id.clone()).collect::<Vec<_>>(),
            vec![first, second]
        );
        outbox.remove(&items[0]).unwrap();
        assert_eq!(
            outbox.depth(CHANNEL_KNOWLEDGE),
            OutboxDepth {
                pending: 1,
                failed: 0
            }
        );
    }

    #[test]
    fn backoff_grows_and_failures_stay_visible() {
        let dir = tempfile::tempdir().unwrap();
        let outbox = Outbox::at(dir.path());
        outbox.enqueue(CHANNEL_KNOWLEDGE, json!({}), now()).unwrap();
        let mut item = outbox.list(CHANNEL_KNOWLEDGE).remove(0);
        outbox
            .retry_later(&mut item, "offline", None, now())
            .unwrap();
        assert!(!item.due(now() + Duration::seconds(29)));
        assert!(item.due(now() + Duration::seconds(38)));
        outbox
            .retry_later(&mut item, "offline", Some(600), now())
            .unwrap();
        assert!(!item.due(now() + Duration::seconds(599)));
        outbox.fail(&mut item, "invalid_request").unwrap();
        assert_eq!(
            outbox.depth(CHANNEL_KNOWLEDGE),
            OutboxDepth {
                pending: 0,
                failed: 1
            }
        );
        assert_eq!(
            outbox.failed(CHANNEL_KNOWLEDGE)[0].last_error.as_deref(),
            Some("invalid_request")
        );
    }

    #[test]
    fn a_huge_retry_after_is_capped_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let outbox = Outbox::at(dir.path());
        outbox.enqueue(CHANNEL_KNOWLEDGE, json!({}), now()).unwrap();
        let mut item = outbox.list(CHANNEL_KNOWLEDGE).remove(0);
        outbox
            .retry_later(&mut item, "busy", Some(u64::MAX), now())
            .unwrap();
        assert!(!item.due(now() + Duration::days(6)));
        assert!(item.due(now() + Duration::days(7) + Duration::seconds(1)));
    }

    #[test]
    fn unknown_channels_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            Outbox::at(dir.path())
                .enqueue("../x", json!({}), now())
                .is_err()
        );
    }
}
