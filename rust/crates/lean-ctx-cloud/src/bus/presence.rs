// SPDX-License-Identifier: Apache-2.0
//! Who works where: the workers of other machines on the vault buses this
//! machine follows, as Cloud reports them in `presence.update` frames.
//! `ctx_agent list` shows them under the local agents; a message or a
//! handoff addresses one by its worker id (`wkr_…`).
//!
//! Kept in `<data>/cloud/bus/presence.json`; the relay is its only writer.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::v2::{PresenceNames, PresenceState};
use crate::CloudError;
use crate::state::{load_or_quarantine, lock_state, write_private};

const PRESENCE_FILE: &str = "presence.json";
/// A report older than this is dropped, and at most [`MAX_WORKERS`] workers
/// are kept (the newest), so the file does not grow with every remote
/// worker ever seen (RB8 M14).
const KEPT: lean_ctx_time::Duration = lean_ctx_time::Duration::days(7);
const MAX_WORKERS: usize = 1000;
/// `<data>/cloud/bus/announce`: a local agent registered or changed its
/// role; the running relay announces it at once instead of on its next tick.
const ANNOUNCE_FILE: &str = "announce";

/// How long `ctx_agent list` shows another machine's worker after its last
/// presence report.
pub const LIST_WINDOW_HOURS: i64 = 24;

/// The workers of other machines, as `ctx_agent list` shows them under the
/// local ones; empty when Cloud reported none.
pub fn render_remote(data_dir: &Path, now: lean_ctx_time::UtcDateTime) -> String {
    let map = PresenceMap::load(data_dir);
    let rows = map.remote_workers(now, lean_ctx_time::Duration::hours(LIST_WINDOW_HOURS));
    if rows.is_empty() {
        return String::new();
    }
    let mut out = format!(
        "Other machines ({}), address them with to_agent=<wkr_…>:\n",
        rows.len()
    );
    for (worker, p) in rows {
        let age = lean_ctx_time::OffsetDateTime::parse_from_rfc3339(&p.server_time)
            .map(|t| (now - t.to_utc()).num_minutes().max(0))
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
/// is not connected to Cloud.
pub fn request_announce() {
    if let Ok(store) = crate::state::CloudStore::open() {
        request_announce_in(&store);
    }
}

fn request_announce_in(store: &crate::state::CloudStore) {
    if !matches!(store.instance(), Ok(Some(_))) {
        return;
    }
    if let Err(error) = write_private(&store.data_dir().join("bus"), ANNOUNCE_FILE, b"1\n") {
        tracing::debug!("cloud bus: announce request: {error}");
    }
}

/// Whether an announce was requested since the last call (the relay's side).
#[cfg_attr(not(feature = "http-server"), allow(dead_code))]
pub(crate) fn take_announce(data_dir: &Path) -> bool {
    std::fs::remove_file(data_dir.join("bus").join(ANNOUNCE_FILE)).is_ok()
}

/// `<data>/cloud/bus/announced.txt`: what the relay's last hello announced,
/// in the words `cloud bus relay` prints (#252), so a foreground run that
/// finds the daemon's relay running can still say what was sent.
const ANNOUNCED_FILE: &str = "announced.txt";

/// Keep what the hello Cloud just acknowledged announced (best effort).
#[cfg_attr(not(feature = "http-server"), allow(dead_code))]
pub(crate) fn note_announced(data_dir: &Path, lines: &[String]) {
    let text = lines.join("\n") + "\n";
    if let Err(error) = write_private(&data_dir.join("bus"), ANNOUNCED_FILE, text.as_bytes()) {
        tracing::debug!("cloud bus: announced record: {error}");
    }
}

/// Ask the running relay to announce now and wait up to `wait` for its
/// fresh hello's record: what it announced, or `None` when it did not
/// answer in time (it is not connected to Cloud right now).
pub fn announce_and_wait(wait: std::time::Duration) -> Option<String> {
    let store = crate::state::CloudStore::open().ok()?;
    announce_and_wait_in(&store, wait)
}

fn announce_and_wait_in(
    store: &crate::state::CloudStore,
    wait: std::time::Duration,
) -> Option<String> {
    // The old record goes first: whatever is there afterwards is the
    // fresh hello's.
    let path = store.data_dir().join("bus").join(ANNOUNCED_FILE);
    let _ = std::fs::remove_file(&path);
    request_announce_in(store);
    let deadline = std::time::Instant::now() + wait;
    loop {
        if let Ok(text) = std::fs::read_to_string(&path) {
            return Some(text);
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

/// One worker's last reported presence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Presence {
    /// The vault bus it was reported on (`#ws/vault`).
    pub address: String,
    pub role: String,
    pub state: PresenceState,
    pub server_time: String,
    /// The names the Cloud relayed with the report (none from a Cloud that
    /// sends only id, role and state).
    #[serde(default, flatten)]
    pub names: PresenceNames,
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

    pub fn save(&self, data_dir: &Path) -> Result<(), CloudError> {
        let bytes = serde_json::to_vec(self).map_err(|e| CloudError::Local(e.to_string()))?;
        write_private(&data_dir.join("bus"), PRESENCE_FILE, &bytes)
    }

    /// Load, change and save under `presence.lock` (RB8 M11), shared by the
    /// relay and the tool calls; never held across a request. A damaged or
    /// unknown file is moved aside first and the state starts empty, so
    /// the damaged copy is never overwritten (M12); an unreadable one is an
    /// error. Old entries are pruned on every change (M14).
    pub fn mutate<T>(
        data_dir: &Path,
        change: impl FnOnce(&mut Self) -> T,
    ) -> Result<T, CloudError> {
        let dir = data_dir.join("bus");
        let _lock = lock_state(&dir, PRESENCE_FILE)?;
        let path = dir.join(PRESENCE_FILE);
        let mut state = match load_or_quarantine::<Self>(&path, |_| true) {
            Ok(state) => state.unwrap_or_default(),
            Err(error) if !path.exists() => {
                tracing::warn!("cloud bus presence: {error}");
                Self::default()
            }
            Err(error) => return Err(error),
        };
        let before = state.clone();
        let out = change(&mut state);
        state.prune(lean_ctx_time::UtcDateTime::now());
        if state != before {
            state.save(data_dir)?;
        }
        Ok(out)
    }

    /// Times are compared parsed, not as text (fractional seconds or an
    /// offset would sort wrong); one that does not parse counts as too old.
    fn prune(&mut self, now: lean_ctx_time::UtcDateTime) {
        let at = |stamp: &str| {
            lean_ctx_time::OffsetDateTime::parse_from_rfc3339(stamp)
                .ok()
                .map(lean_ctx_time::OffsetDateTime::to_utc)
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
        self.apply_named(
            address,
            worker_id,
            role,
            state,
            at_ms,
            PresenceNames::default(),
        )
    }

    /// [`Self::apply`] with the names the report carried.
    pub fn apply_named(
        &mut self,
        address: &str,
        worker_id: &str,
        role: &str,
        state: PresenceState,
        at_ms: u64,
        names: PresenceNames,
    ) -> bool {
        let Some(at) = lean_ctx_time::UtcDateTime::from_timestamp_millis(
            i64::try_from(at_ms).unwrap_or(i64::MAX),
        ) else {
            return false;
        };
        let server_time = at.to_rfc3339_opts(lean_ctx_time::SecondsFormat::Secs, true);
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
                names,
            },
        );
        true
    }

    /// The workers of other machines Cloud reported within `window` before
    /// `now`, newest first: what `ctx_agent list` shows.
    pub fn remote_workers(
        &self,
        now: lean_ctx_time::UtcDateTime,
        window: lean_ctx_time::Duration,
    ) -> Vec<(&str, &Presence)> {
        let cutoff = (now - window).to_rfc3339_opts(lean_ctx_time::SecondsFormat::Secs, true);
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

    fn ms_ago(now: lean_ctx_time::UtcDateTime, minutes: i64) -> u64 {
        u64::try_from((now - lean_ctx_time::Duration::minutes(minutes)).timestamp_millis()).unwrap()
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
        let now = lean_ctx_time::UtcDateTime::from_timestamp_millis(AT as i64 + 120_000).unwrap();
        let rows = map.remote_workers(now, lean_ctx_time::Duration::hours(1));
        assert_eq!(
            rows.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            ["wkr_c", "wkr_b"]
        );
        assert_eq!(rows[1].1.address, "#acme/payments");
        assert_eq!(rows[1].1.state, PresenceState::Active);
        let late =
            lean_ctx_time::UtcDateTime::from_timestamp_millis(AT as i64 + 7_200_000).unwrap();
        assert!(
            map.remote_workers(late, lean_ctx_time::Duration::hours(1))
                .is_empty()
        );
    }

    #[test]
    fn an_announce_request_wakes_the_relay_once_and_only_when_connected() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::state::CloudStore::at(dir.path().join("c"), dir.path().join("d"));
        request_announce_in(&store);
        assert!(!take_announce(store.data_dir()), "not connected: nothing");
        crate::bus::control::enroll_for_tests(&store, 1);
        request_announce_in(&store);
        assert!(take_announce(store.data_dir()));
        assert!(!take_announce(store.data_dir()), "taken once");
    }

    /// `cloud bus relay` beside a running relay: it asks for a fresh hello
    /// and prints that hello's record, or says it got none in time.
    #[test]
    fn a_fresh_announce_record_is_waited_for_and_an_old_one_is_not_taken() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::state::CloudStore::at(dir.path().join("c"), dir.path().join("d"));
        crate::bus::control::enroll_for_tests(&store, 1);
        note_announced(store.data_dir(), &["old hello".into()]);
        let wait = std::time::Duration::from_millis(400);
        assert_eq!(
            announce_and_wait_in(&store, wait),
            None,
            "an old record is not an answer"
        );
        assert!(take_announce(store.data_dir()), "the unanswered request");
        let data = store.data_dir().to_path_buf();
        let relay = std::thread::spawn(move || {
            while !take_announce(&data) {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            note_announced(&data, &["fresh hello".into()]);
        });
        assert_eq!(
            announce_and_wait_in(&store, std::time::Duration::from_secs(10)).as_deref(),
            Some("fresh hello\n")
        );
        relay.join().unwrap();
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
        let at = ms_ago(lean_ctx_time::UtcDateTime::now(), 0);
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
        let now = lean_ctx_time::UtcDateTime::now();
        let mut map = PresenceMap::default();
        // Recent, written with an offset and fractions: parsed, it is kept.
        let recent = (now - lean_ctx_time::Duration::days(6))
            .with_offset_seconds(3600)
            .unwrap()
            .to_rfc3339_opts(lean_ctx_time::SecondsFormat::Millis, false);
        let presence = |server_time: &str| Presence {
            address: "#acme/payments".into(),
            role: "dev".into(),
            state: PresenceState::Active,
            server_time: server_time.into(),
            names: PresenceNames::default(),
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
        let now = lean_ctx_time::UtcDateTime::now();
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
