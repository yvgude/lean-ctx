// SPDX-License-Identifier: Apache-2.0
//! Who works where: the workers of other machines on the vault buses this
//! machine follows, as Hive reports them in `presence.update` frames.
//! `ctx_agent list` shows them under the local agents; a message or a
//! handoff addresses one by its worker id (`wkr_…`).
//!
//! Kept in `<data>/hive/bus/presence.json`; the relay is its only writer.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::v2::PresenceState;
use crate::HiveError;
use crate::state::{load_or_quarantine, lock_state, write_private};

const PRESENCE_FILE: &str = "presence.json";
/// A report older than this is dropped, and at most [`MAX_WORKERS`] workers
/// are kept (the newest), so the file does not grow with every remote
/// worker ever seen (RB8 M14).
const KEPT: chrono::Duration = chrono::Duration::days(7);
const MAX_WORKERS: usize = 1000;
/// `<data>/hive/bus/announce`: a local agent registered or changed its
/// role; the running relay announces it at once instead of on its next tick.
const ANNOUNCE_FILE: &str = "announce";

/// How long `ctx_agent list` shows another machine's worker after its last
/// presence report.
pub const LIST_WINDOW_HOURS: i64 = 24;

/// The workers of other machines, as `ctx_agent list` shows them under the
/// local ones; empty when Hive reported none.
pub fn render_remote(data_dir: &Path, now: chrono::DateTime<chrono::Utc>) -> String {
    let map = PresenceMap::load(data_dir);
    let rows = map.remote_workers(now, chrono::Duration::hours(LIST_WINDOW_HOURS));
    if rows.is_empty() {
        return String::new();
    }
    let mut out = format!(
        "Other machines ({}), address them with to_agent=<wkr_…>:\n",
        rows.len()
    );
    for (worker, p) in rows {
        let age = chrono::DateTime::parse_from_rfc3339(&p.server_time)
            .map(|t| (now - t.with_timezone(&chrono::Utc)).num_minutes().max(0))
            .unwrap_or(0);
        let state = serde_json::to_value(p.state)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default();
        out.push_str(&format!(
            "  {worker} · {} state={state} on {} (last seen {age}m ago)\n",
            p.role, p.address
        ));
    }
    out
}

/// Ask the running relay to announce this machine's agents now (after a
/// `ctx_agent register` or a role change), so a claim right after it finds
/// the agent on the bus. Local only and best effort: it never waits for
/// the network and never fails the registration. No-op when this machine
/// is not connected to Hive.
pub fn request_announce() {
    if let Ok(store) = crate::state::HiveStore::open() {
        request_announce_in(&store);
    }
}

fn request_announce_in(store: &crate::state::HiveStore) {
    if !matches!(store.instance(), Ok(Some(_))) {
        return;
    }
    if let Err(error) = write_private(&store.data_dir().join("bus"), ANNOUNCE_FILE, b"1\n") {
        tracing::debug!("hive bus: announce request: {error}");
    }
}

/// Whether an announce was requested since the last call (the relay's side).
#[cfg_attr(not(feature = "http-server"), allow(dead_code))]
pub(crate) fn take_announce(data_dir: &Path) -> bool {
    std::fs::remove_file(data_dir.join("bus").join(ANNOUNCE_FILE)).is_ok()
}

/// One worker's last reported presence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Presence {
    /// The vault bus it was reported on (`#ws/vault`).
    pub address: String,
    pub role: String,
    pub state: PresenceState,
    pub server_time: String,
}

/// `presence.json`: worker id → presence.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresenceMap {
    #[serde(default)]
    pub workers: BTreeMap<String, Presence>,
}

impl PresenceMap {
    pub fn load(data_dir: &Path) -> Self {
        std::fs::read(data_dir.join("bus").join(PRESENCE_FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, data_dir: &Path) -> Result<(), HiveError> {
        let bytes = serde_json::to_vec(self).map_err(|e| HiveError::Local(e.to_string()))?;
        write_private(&data_dir.join("bus"), PRESENCE_FILE, &bytes)
    }

    /// Load, change and save under `presence.lock` (RB8 M11), shared by the
    /// relay and the tool calls; never held across a request. A damaged or
    /// unknown file is moved aside first and the state starts empty, so
    /// the damaged copy is never overwritten (M12); an unreadable one is an
    /// error. Old entries are pruned on every change (M14).
    pub fn mutate<T>(data_dir: &Path, change: impl FnOnce(&mut Self) -> T) -> Result<T, HiveError> {
        let dir = data_dir.join("bus");
        let _lock = lock_state(&dir, PRESENCE_FILE)?;
        let path = dir.join(PRESENCE_FILE);
        let mut state = match load_or_quarantine::<Self>(&path, |_| true) {
            Ok(state) => state.unwrap_or_default(),
            Err(error) if !path.exists() => {
                tracing::warn!("hive bus presence: {error}");
                Self::default()
            }
            Err(error) => return Err(error),
        };
        let before = state.clone();
        let out = change(&mut state);
        state.prune(chrono::Utc::now());
        if state != before {
            state.save(data_dir)?;
        }
        Ok(out)
    }

    /// Times are compared parsed, not as text (fractional seconds or an
    /// offset would sort wrong); one that does not parse counts as too old.
    fn prune(&mut self, now: chrono::DateTime<chrono::Utc>) {
        let at = |stamp: &str| {
            chrono::DateTime::parse_from_rfc3339(stamp)
                .ok()
                .map(|t| t.with_timezone(&chrono::Utc))
        };
        let cutoff = now - KEPT;
        self.workers
            .retain(|_, p| at(&p.server_time).is_some_and(|t| t >= cutoff));
        let excess = self.workers.len().saturating_sub(MAX_WORKERS);
        if excess > 0 {
            let mut by_age: Vec<_> = self
                .workers
                .iter()
                .map(|(id, p)| (at(&p.server_time), id.clone()))
                .collect();
            by_age.sort();
            for (_, id) in by_age.into_iter().take(excess) {
                self.workers.remove(&id);
            }
        }
    }

    /// Record a `presence.update` (a worker on a bus address, `at` in
    /// milliseconds); `false` for an older report.
    pub fn apply(
        &mut self,
        address: &str,
        worker_id: &str,
        role: &str,
        state: PresenceState,
        at_ms: u64,
    ) -> bool {
        let Some(at) =
            chrono::DateTime::from_timestamp_millis(i64::try_from(at_ms).unwrap_or(i64::MAX))
        else {
            return false;
        };
        let server_time = at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        if self
            .workers
            .get(worker_id)
            .is_some_and(|known| known.server_time.as_str() > server_time.as_str())
        {
            return false;
        }
        self.workers.insert(
            worker_id.to_owned(),
            Presence {
                address: address.to_owned(),
                role: role.to_owned(),
                state,
                server_time,
            },
        );
        true
    }

    /// The workers of other machines Hive reported within `window` before
    /// `now`, newest first: what `ctx_agent list` shows.
    pub fn remote_workers(
        &self,
        now: chrono::DateTime<chrono::Utc>,
        window: chrono::Duration,
    ) -> Vec<(&str, &Presence)> {
        let cutoff = (now - window).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let mut out: Vec<(&str, &Presence)> = self
            .workers
            .iter()
            .filter(|(_, p)| p.server_time.as_str() >= cutoff.as_str())
            .map(|(id, p)| (id.as_str(), p))
            .collect();
        out.sort_by(|a, b| b.1.server_time.cmp(&a.1.server_time).then(a.0.cmp(b.0)));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AT: u64 = 1_792_800_060_000;

    fn ms_ago(now: chrono::DateTime<chrono::Utc>, minutes: i64) -> u64 {
        u64::try_from((now - chrono::Duration::minutes(minutes)).timestamp_millis()).unwrap()
    }

    #[test]
    fn other_machines_workers_are_listed_newest_first_within_the_window() {
        let mut map = PresenceMap::default();
        assert!(map.apply(
            "#acme/payments",
            "wkr_b",
            "tester",
            PresenceState::Active,
            AT
        ));
        assert!(
            !map.apply(
                "#acme/payments",
                "wkr_b",
                "tester",
                PresenceState::Idle,
                AT - 1000
            ),
            "older"
        );
        assert!(map.apply(
            "#acme/payments",
            "wkr_c",
            "dev",
            PresenceState::Held,
            AT + 1000
        ));
        let now = chrono::DateTime::from_timestamp_millis(AT as i64 + 120_000).unwrap();
        let rows = map.remote_workers(now, chrono::Duration::hours(1));
        assert_eq!(
            rows.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            ["wkr_c", "wkr_b"]
        );
        assert_eq!(rows[1].1.address, "#acme/payments");
        assert_eq!(rows[1].1.state, PresenceState::Active);
        let late = chrono::DateTime::from_timestamp_millis(AT as i64 + 7_200_000).unwrap();
        assert!(
            map.remote_workers(late, chrono::Duration::hours(1))
                .is_empty()
        );
    }

    #[test]
    fn an_announce_request_wakes_the_relay_once_and_only_when_connected() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::state::HiveStore::at(dir.path().join("c"), dir.path().join("d"));
        request_announce_in(&store);
        assert!(!take_announce(store.data_dir()), "not connected: nothing");
        crate::bus::control::enroll_for_tests(&store, 1);
        request_announce_in(&store);
        assert!(take_announce(store.data_dir()));
        assert!(!take_announce(store.data_dir()), "taken once");
    }

    #[test]
    fn the_map_persists_privately() {
        let dir = tempfile::tempdir().unwrap();
        let mut map = PresenceMap::default();
        map.apply("#acme/payments", "wkr_b", "dev", PresenceState::Idle, AT);
        map.save(dir.path()).unwrap();
        assert_eq!(PresenceMap::load(dir.path()), map);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(dir.path().join("bus/presence.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn a_damaged_file_is_kept_aside_and_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let bus = dir.path().join("bus");
        std::fs::create_dir_all(&bus).unwrap();
        std::fs::write(bus.join(PRESENCE_FILE), b"[1,2").unwrap();
        assert_eq!(PresenceMap::load(dir.path()), PresenceMap::default());
        assert_eq!(std::fs::read(bus.join(PRESENCE_FILE)).unwrap(), b"[1,2");
        let at = ms_ago(chrono::Utc::now(), 0);
        assert!(
            PresenceMap::mutate(dir.path(), |map| map.apply(
                "#acme/payments",
                "wkr_b",
                "dev",
                PresenceState::Active,
                at
            ))
            .unwrap()
        );
        assert_eq!(
            std::fs::read(bus.join("presence.json.damaged-1")).unwrap(),
            b"[1,2"
        );
        assert!(PresenceMap::load(dir.path()).workers.contains_key("wkr_b"));
    }

    #[test]
    fn prune_compares_times_not_text() {
        let now = chrono::Utc::now();
        let mut map = PresenceMap::default();
        // Recent, written with an offset and fractions: parsed, it is kept.
        let recent = (now - chrono::Duration::days(6))
            .with_timezone(&chrono::FixedOffset::east_opt(3600).unwrap())
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, false);
        let presence = |server_time: &str| Presence {
            address: "#acme/payments".into(),
            role: "dev".into(),
            state: PresenceState::Active,
            server_time: server_time.into(),
        };
        map.workers.insert("wkr_b".into(), presence(&recent));
        map.workers.insert("wkr_g".into(), presence("not a time"));
        map.prune(now);
        assert!(map.workers.contains_key("wkr_b"));
        assert!(
            !map.workers.contains_key("wkr_g"),
            "an unreadable time is too old"
        );
    }

    #[test]
    fn old_workers_age_out_and_the_map_is_capped() {
        let now = chrono::Utc::now();
        let mut map = PresenceMap::default();
        let address = "#acme/payments";
        map.apply(
            address,
            "wkr_stale",
            "dev",
            PresenceState::Idle,
            ms_ago(now, 8 * 24 * 60),
        );
        for n in 0..(MAX_WORKERS + 3) {
            let minutes = (MAX_WORKERS + 3 - n) as i64;
            map.apply(
                address,
                &format!("wkr_{n:04}"),
                "dev",
                PresenceState::Active,
                ms_ago(now, minutes),
            );
        }
        map.prune(now);
        assert!(!map.workers.contains_key("wkr_stale"));
        assert_eq!(map.workers.len(), MAX_WORKERS);
        assert!(!map.workers.contains_key("wkr_0002"), "oldest went");
        assert!(map.workers.contains_key("wkr_0003"));
        assert!(
            map.workers
                .contains_key(&format!("wkr_{:04}", MAX_WORKERS + 2))
        );
    }
}
