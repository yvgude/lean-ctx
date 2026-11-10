// SPDX-License-Identifier: Apache-2.0
//! The sync budget of one vault, shared by every process of this machine
//! (#270 review): the daemon's background sync, the push on write of each
//! MCP server and anything else that syncs a vault on its own.
//!
//! `<data>/hive/sync/<vault>.budget.json` holds the burst budget's state
//! (when its bucket is full again) and the time before which no attempt may start (the
//! backoff after a failure, or Hive's `Retry-After`). It is only read and
//! written while the caller holds the vault's sync lock
//! ([`super::auto::lock_vault`]), so two processes never lose each other's
//! attempts.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::auto::{BURST_WINDOW, WAIT_MAX, burst_take};
use crate::state::write_private;

/// What the budget said to an attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Admit {
    /// Go: the attempt is recorded.
    Go,
    /// A failure or Hive's `Retry-After` holds the vault until later.
    Backoff,
    /// The burst budget is spent; a turn comes back within `BURST_EVERY`
    /// ([`super::auto::burst_take`]).
    Burst,
}

/// One vault's budget (milliseconds since the Unix epoch).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Budget {
    /// When the burst budget is full again ([`burst_take`]); `None`: full.
    /// Kept, not rebuilt from recent attempts (review of 5!235).
    pub full_at: Option<i64>,
    /// No attempt starts before this.
    pub not_before: Option<i64>,
}

fn window_ms() -> i64 {
    i64::try_from(BURST_WINDOW.as_millis()).unwrap_or(i64::MAX)
}

fn wait_max_ms() -> i64 {
    i64::try_from(WAIT_MAX.as_millis()).unwrap_or(i64::MAX)
}

impl Budget {
    /// Whether an attempt may start at `now_ms`; records it when it may.
    /// A purge passes the burst cap (never the backoff), as in the daemon.
    pub(crate) fn admit(&mut self, purge: bool, now_ms: i64) -> Admit {
        // A hold is never longer than `WAIT_MAX` from now: one stamped
        // further out (the wall clock was stepped back during a backoff)
        // is cut to that, so every process is held for the wait at most,
        // not for the skew as well.
        let latest = now_ms.saturating_add(wait_max_ms());
        if self.not_before.is_some_and(|at| at > latest) {
            self.not_before = Some(latest);
        }
        // The same for the burst budget: never further than an empty
        // bucket's refill from now, and the cut sticks.
        let latest = now_ms.saturating_add(window_ms());
        if self.full_at.is_some_and(|at| at > latest) {
            self.full_at = Some(latest);
        }
        if self.not_before.is_some_and(|at| at > now_ms) {
            return Admit::Backoff;
        }
        if self.take(purge, now_ms) {
            Admit::Go
        } else {
            Admit::Burst
        }
    }

    /// A manual sync that ran without asking (a person's sync is never
    /// held): it still counts against the burst cap of every other sync,
    /// and its result sets or clears the backoff.
    pub(crate) fn record(&mut self, wait: Option<Duration>, now_ms: i64) {
        self.take(true, now_ms);
        self.settle(wait, now_ms);
    }

    /// One turn of the burst budget ([`burst_take`]); `purge` takes it even
    /// when the budget is spent. Whether the turn was taken.
    fn take(&mut self, purge: bool, now_ms: i64) -> bool {
        let ahead = self
            .full_at
            .map_or(0, |at| at.saturating_sub(now_ms).max(0));
        let ahead = Duration::from_millis(u64::try_from(ahead).unwrap_or(0));
        let Some(ahead) = burst_take(ahead, purge) else {
            return false;
        };
        let ahead = i64::try_from(ahead.as_millis()).unwrap_or(i64::MAX);
        self.full_at = Some(now_ms.saturating_add(ahead));
        true
    }

    /// How the attempt ended: `wait` after a failure (at most `WAIT_MAX`),
    /// `None` on success.
    pub(crate) fn settle(&mut self, wait: Option<Duration>, now_ms: i64) {
        self.not_before = wait.map(|wait| {
            let wait = i64::try_from(wait.as_millis()).unwrap_or(i64::MAX);
            now_ms.saturating_add(wait.min(wait_max_ms()))
        });
    }
}

fn path(data_dir: &Path, vault_id: &str) -> PathBuf {
    super::auto::sync_dir(data_dir)
        .join(format!("{}.budget.json", super::auto::lock_name(vault_id)))
}

fn load(data_dir: &Path, vault_id: &str) -> Budget {
    std::fs::read(path(data_dir, vault_id))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save(data_dir: &Path, vault_id: &str, budget: &Budget) {
    let file = path(data_dir, vault_id);
    let (Some(dir), Some(name)) = (file.parent(), file.file_name().and_then(|n| n.to_str())) else {
        return;
    };
    if let Ok(bytes) = serde_json::to_vec(budget) {
        let _ = write_private(dir, name, &bytes);
    }
}

/// [`Budget::admit`] on the shared file. Call with the vault's sync lock.
pub(crate) fn admit(data_dir: &Path, vault_id: &str, purge: bool, now: DateTime<Utc>) -> Admit {
    let mut budget = load(data_dir, vault_id);
    let before = budget.clone();
    let admit = budget.admit(purge, now.timestamp_millis());
    // A Go, or a hold cut to `WAIT_MAX`: the cut must stick, or it would
    // move along with the clock and never end.
    if budget != before {
        save(data_dir, vault_id, &budget);
    }
    admit
}

/// [`Budget::record`] on the shared file. Call with the vault's sync lock.
pub(crate) fn record(data_dir: &Path, vault_id: &str, wait: Option<Duration>, now: DateTime<Utc>) {
    let mut budget = load(data_dir, vault_id);
    budget.record(wait, now.timestamp_millis());
    save(data_dir, vault_id, &budget);
}

/// [`Budget::settle`] on the shared file. Call with the vault's sync lock.
pub(crate) fn settle(data_dir: &Path, vault_id: &str, wait: Option<Duration>, now: DateTime<Utc>) {
    let mut budget = load(data_dir, vault_id);
    budget.settle(wait, now.timestamp_millis());
    save(data_dir, vault_id, &budget);
}

#[cfg(test)]
#[path = "budget_tests.rs"]
mod tests;
