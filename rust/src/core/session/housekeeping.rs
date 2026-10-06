// SPDX-License-Identifier: Apache-2.0
//! Bounded upkeep of the session store, run off the MCP handshake (GH #2006).
//!
//! Nothing pruned the store automatically, and every save left its
//! `.<id>.save.lock` behind, so long-lived installs collected thousands of
//! session files (1,617 sessions and 894 orphaned locks in the report). Once a
//! day, from the server's background housekeeping:
//! - lock files whose session JSON is gone are removed, if nobody holds them;
//! - a store above [`AUTO_PRUNE_MIN_SESSIONS`] is pruned with the explicit
//!   cleanup rules (`session_retention_days`; the newest session per project
//!   and the global latest pointer are always kept, and facts are persisted
//!   before a session is removed).

use std::path::Path;

use super::SessionState;

/// Stores at or below this size are left alone: the explicit cleanup command
/// stays the only path that removes their sessions.
pub(crate) const AUTO_PRUNE_MIN_SESSIONS: usize = 300;

const DAY_MARKER: &str = "session_housekeeping_day";

/// What one housekeeping pass did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct HousekeepingReport {
    pub orphan_locks_removed: usize,
    pub sessions_pruned: u32,
}

/// Run the pass at most once per UTC day. `None` when it already ran today or
/// there is no session store.
pub(crate) fn run_daily() -> Option<HousekeepingReport> {
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let marker = crate::core::paths::state_dir().ok()?.join(DAY_MARKER);
    if std::fs::read_to_string(&marker).is_ok_and(|day| day.trim() == today) {
        return None;
    }
    let dir = super::paths::sessions_dir()?;
    let retention = crate::core::config::Config::load().session_retention_days_effective();
    let report = run_in(&dir, retention);
    if let Some(parent) = marker.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&marker, format!("{today}\n"));
    Some(report)
}

fn run_in(dir: &Path, retention_days: u32) -> HousekeepingReport {
    let mut report = HousekeepingReport {
        orphan_locks_removed: remove_orphan_locks(dir),
        ..HousekeepingReport::default()
    };
    if auto_prune_allowed(count_sessions(dir), retention_days) {
        report.sessions_pruned = SessionState::cleanup_old_sessions(i64::from(retention_days));
    }
    report
}

/// Only large stores, and only with a retention of at least one day. The
/// cleanup removes sessions last updated before `now - retention_days`, so a
/// session in use (saved within that window) and everything newer survive; a
/// retention of 0 would move that cutoff to "now", which an automatic pass
/// must never do — `0` stays an explicit-cleanup-only setting.
fn auto_prune_allowed(sessions: usize, retention_days: u32) -> bool {
    sessions > AUTO_PRUNE_MIN_SESSIONS && retention_days >= 1
}

/// Session JSON files, counted by name only (no parsing).
fn count_sessions(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| {
                    let name = entry.file_name();
                    let name = name.to_string_lossy();
                    name.ends_with(".json") && !name.starts_with('.') && name != "latest.json"
                })
                .count()
        })
        .unwrap_or(0)
}

/// Remove `.<id>.save.lock` files whose `<id>.json` no longer exists. A lock
/// that another process holds is kept; a held lock means a save is running.
fn remove_orphan_locks(dir: &Path) -> usize {
    use fs2::FileExt;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(id) = name
            .strip_prefix('.')
            .and_then(|rest| rest.strip_suffix(".save.lock"))
        else {
            continue;
        };
        if id.is_empty() || dir.join(format!("{id}.json")).exists() {
            continue;
        }
        let path = entry.path();
        let Ok(lock) = std::fs::OpenOptions::new().write(true).open(&path) else {
            continue;
        };
        if lock.try_lock_exclusive().is_ok() && std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orphan_locks_are_removed_and_live_ones_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::write(dir.join("alive.json"), "{}").unwrap();
        std::fs::write(dir.join(".alive.save.lock"), "").unwrap();
        std::fs::write(dir.join(".gone.save.lock"), "").unwrap();
        std::fs::write(dir.join(".other.tmp"), "").unwrap();

        assert_eq!(remove_orphan_locks(dir), 1);
        assert!(dir.join(".alive.save.lock").exists());
        assert!(!dir.join(".gone.save.lock").exists());
        assert!(dir.join(".other.tmp").exists());
    }

    #[test]
    fn a_held_orphan_lock_is_kept() {
        use fs2::FileExt;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".busy.save.lock");
        let holder = std::fs::File::create(&path).unwrap();
        holder.lock_exclusive().unwrap();

        assert_eq!(remove_orphan_locks(tmp.path()), 0);
        assert!(path.exists());
    }

    #[test]
    fn auto_prune_needs_a_large_store_and_a_positive_retention() {
        assert!(!auto_prune_allowed(AUTO_PRUNE_MIN_SESSIONS, 7));
        assert!(auto_prune_allowed(AUTO_PRUNE_MIN_SESSIONS + 1, 7));
        assert!(auto_prune_allowed(AUTO_PRUNE_MIN_SESSIONS + 1, 1));
        assert!(
            !auto_prune_allowed(10_000, 0),
            "retention 0 would prune up to now; never automatically"
        );
    }

    #[test]
    fn small_stores_are_never_pruned_automatically() {
        let tmp = tempfile::tempdir().unwrap();
        for i in 0..AUTO_PRUNE_MIN_SESSIONS {
            std::fs::write(tmp.path().join(format!("s{i}.json")), "{}").unwrap();
        }
        std::fs::write(tmp.path().join("latest.json"), "{}").unwrap();
        assert_eq!(count_sessions(tmp.path()), AUTO_PRUNE_MIN_SESSIONS);
        assert_eq!(run_in(tmp.path(), 7).sessions_pruned, 0);
    }
}
