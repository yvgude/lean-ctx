// SPDX-License-Identifier: Apache-2.0
//! Who is where: the agents of other instances, as Hive reports them in
//! `presence_update` frames. `ctx_agent handoff` uses it to tell a handoff to
//! an agent on another machine from a local one (C5c).
//!
//! Besides the newest report per agent id, the map remembers every instance
//! that announced an agent id within [`SEEN_WINDOW_HOURS`]: an agent id is
//! only a name, and another instance may announce the same one. A direct
//! message binds to the recipient's instance only when exactly one remote
//! instance announced it ([`PresenceMap::instances_of`]).
//!
//! Kept in `<data>/hive/bus/presence.json`; the relay is its only writer.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::core::hive::HiveError;
use crate::core::hive::state::{load_or_quarantine, lock_state, write_private};
use lean_ctx_protocol::hive::bus::{BusFrameV1, PresenceState};

const PRESENCE_FILE: &str = "presence.json";
/// How long an instance's announcement of an agent id is remembered.
pub const SEEN_WINDOW_HOURS: i64 = 24;
/// Instances remembered per agent id (the newest stay).
const MAX_SEEN_PER_AGENT: usize = 16;
/// An agent's presence or an announcement older than this is dropped, and
/// at most [`MAX_AGENTS`] agent ids are kept (the newest), so the file does
/// not grow with every remote agent id ever seen (RB8 M14).
const KEPT: chrono::Duration = chrono::Duration::days(7);
const MAX_AGENTS: usize = 1000;
/// `<data>/hive/bus/announce`: a local agent registered or changed its
/// role; the running relay announces it at once instead of on its next tick.
const ANNOUNCE_FILE: &str = "announce";

/// Ask the running relay to announce this machine's agents now (after a
/// `ctx_agent register` or a role change), so a claim right after it finds
/// the agent on the bus. Local only and best effort: it never waits for
/// the network and never fails the registration. No-op when this machine
/// is not connected to Hive.
pub fn request_announce() {
    if let Ok(store) = crate::core::hive::state::HiveStore::open() {
        request_announce_in(&store);
    }
}

fn request_announce_in(store: &crate::core::hive::state::HiveStore) {
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

/// One agent's last reported presence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Presence {
    pub instance_id: String,
    /// On disk as `project_id` (pre-VA name) for downgrades and mixed-version teams.
    #[serde(rename = "project_id", alias = "vault_id")]
    pub vault_id: String,
    pub role: String,
    pub state: PresenceState,
    pub server_time: String,
}

/// `presence.json`: agent id → presence.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresenceMap {
    #[serde(default)]
    pub agents: BTreeMap<String, Presence>,
    /// Agent id → instance id → newest `server_time` it announced the id.
    #[serde(default)]
    pub seen: BTreeMap<String, BTreeMap<String, String>>,
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
        self.agents
            .retain(|_, p| at(&p.server_time).is_some_and(|t| t >= cutoff));
        for seen in self.seen.values_mut() {
            seen.retain(|_, stamp| at(stamp).is_some_and(|t| t >= cutoff));
        }
        self.seen.retain(|_, seen| !seen.is_empty());
        let excess = self.agents.len().saturating_sub(MAX_AGENTS);
        if excess > 0 {
            let mut by_age: Vec<_> = self
                .agents
                .iter()
                .map(|(id, p)| (at(&p.server_time), id.clone()))
                .collect();
            by_age.sort();
            for (_, id) in by_age.into_iter().take(excess) {
                self.agents.remove(&id);
            }
        }
        let excess = self.seen.len().saturating_sub(MAX_AGENTS);
        if excess > 0 {
            let mut by_age: Vec<_> = self
                .seen
                .iter()
                .map(|(id, seen)| (seen.values().filter_map(|s| at(s)).max(), id.clone()))
                .collect();
            by_age.sort();
            for (_, id) in by_age.into_iter().take(excess) {
                self.seen.remove(&id);
            }
        }
    }

    /// Record a `presence_update`; `false` for any other frame or an older report.
    pub fn apply(&mut self, frame: &BusFrameV1) -> bool {
        let BusFrameV1::PresenceUpdate {
            agent_id,
            instance_id,
            role,
            vault_id,
            state,
            server_time,
            ..
        } = frame
        else {
            return false;
        };
        // Every announcement counts, a late one too: it says this instance
        // used the name.
        let seen = self.seen.entry(agent_id.clone()).or_default();
        let at = seen.entry(instance_id.clone()).or_default();
        let seen_changed = server_time.as_str() > at.as_str();
        if seen_changed {
            *at = server_time.as_str().to_string();
            while seen.len() > MAX_SEEN_PER_AGENT {
                let oldest = seen
                    .iter()
                    .min_by(|a, b| a.1.cmp(b.1))
                    .map(|(id, _)| id.clone());
                oldest.map(|id| seen.remove(&id));
            }
        }
        if self
            .agents
            .get(agent_id)
            .is_some_and(|known| known.server_time.as_str() > server_time.as_str())
        {
            return seen_changed;
        }
        self.agents.insert(
            agent_id.clone(),
            Presence {
                instance_id: instance_id.clone(),
                vault_id: vault_id.clone(),
                role: role.clone(),
                state: *state,
                server_time: server_time.as_str().to_string(),
            },
        );
        true
    }

    /// The instances other than `own_instance` that announced `agent` within
    /// [`SEEN_WINDOW_HOURS`] before `now` (`YYYY-MM-DDTHH:MM:SSZ`), sorted.
    /// More than one means the name is ambiguous: a sender must pick.
    pub fn instances_of(&self, agent: &str, own_instance: &str, now: &str) -> Vec<String> {
        let cutoff = chrono::DateTime::parse_from_rfc3339(now)
            .map(|now| {
                (now - chrono::Duration::hours(SEEN_WINDOW_HOURS))
                    .to_utc()
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
            })
            .unwrap_or_default();
        self.seen
            .get(agent)
            .into_iter()
            .flatten()
            .filter(|(instance, at)| {
                instance.as_str() != own_instance && at.as_str() >= cutoff.as_str()
            })
            .map(|(instance, _)| instance.clone())
            .collect()
    }

    /// `agent` as an open agent on an instance other than `own_instance`.
    pub fn remote<'a>(&'a self, agent: &str, own_instance: &str) -> Option<&'a Presence> {
        self.agents
            .get(agent)
            .filter(|p| p.instance_id != own_instance && p.state != PresenceState::Closed)
    }
}

#[cfg(test)]
mod vault_rename_tests {
    use super::*;

    /// `Presence` of the engine before the VA rename.
    #[derive(Deserialize)]
    #[allow(dead_code)]
    struct PreVaPresence {
        instance_id: String,
        project_id: String,
        role: String,
        state: PresenceState,
        server_time: String,
    }

    #[test]
    fn presence_keeps_pre_va_names() {
        let old = r#"{"instance_id":"ins_1","project_id":"v1","role":"coder","state":"active","server_time":"2026-10-22T10:00:00Z"}"#;
        let read = crate::core::hive::vault::assert_pre_va_round_trip::<Presence, PreVaPresence>(
            old,
            "project_id",
            "vault_id",
        );
        assert_eq!(read.vault_id, "v1");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lean_ctx_protocol::UtcTimestamp;

    fn update(agent: &str, instance: &str, state: PresenceState, at: &str) -> BusFrameV1 {
        BusFrameV1::PresenceUpdate {
            agent_id: agent.into(),
            instance_id: instance.into(),
            role: "dev".into(),
            vault_id: "0123456789abcdef0123456789abcdef".into(),
            state,
            server_time: UtcTimestamp::new(at).unwrap(),
            profile_id: None,
            profile: None,
        }
    }

    #[test]
    fn an_announce_request_wakes_the_relay_once_and_only_when_connected() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            crate::core::hive::state::HiveStore::at(dir.path().join("c"), dir.path().join("d"));
        request_announce_in(&store);
        assert!(!take_announce(store.data_dir()), "not connected: nothing");
        crate::core::hive::bus::control::enroll_for_tests(&store, 1);
        request_announce_in(&store);
        assert!(take_announce(store.data_dir()));
        assert!(!take_announce(store.data_dir()), "taken once");
    }

    #[test]
    fn newest_report_wins_and_closed_or_local_agents_are_not_remote() {
        let mut map = PresenceMap::default();
        assert!(map.apply(&update(
            "beta",
            "inst_b",
            PresenceState::Active,
            "2026-10-21T10:00:00Z"
        )));
        assert_eq!(map.remote("beta", "inst_a").unwrap().instance_id, "inst_b");
        assert!(map.remote("beta", "inst_b").is_none(), "own instance");
        assert!(map.remote("gamma", "inst_a").is_none());
        // An older report arriving late changes nothing.
        assert!(!map.apply(&update(
            "beta",
            "inst_b",
            PresenceState::Closed,
            "2026-10-21T09:00:00Z"
        )));
        assert!(map.remote("beta", "inst_a").is_some());
        assert!(map.apply(&update(
            "beta",
            "inst_b",
            PresenceState::Closed,
            "2026-10-21T11:00:00Z"
        )));
        assert!(map.remote("beta", "inst_a").is_none());
        assert!(!map.apply(&BusFrameV1::Ping { nonce: 1 }));
    }

    #[test]
    fn a_second_instance_announcing_the_same_name_makes_it_ambiguous() {
        let now = "2026-10-21T12:00:00Z";
        let mut map = PresenceMap::default();
        map.apply(&update(
            "beta",
            "inst_b",
            PresenceState::Active,
            "2026-10-21T10:00:00Z",
        ));
        assert_eq!(map.instances_of("beta", "inst_a", now), ["inst_b"]);
        assert!(
            map.instances_of("beta", "inst_b", now).is_empty(),
            "own instance"
        );
        // An impostor announces the name later: the newest report is its,
        // but the name now has two instances and binds to neither.
        assert!(map.apply(&update(
            "beta",
            "inst_x",
            PresenceState::Active,
            "2026-10-21T11:00:00Z"
        )));
        assert_eq!(map.remote("beta", "inst_a").unwrap().instance_id, "inst_x");
        assert_eq!(
            map.instances_of("beta", "inst_a", now),
            ["inst_b", "inst_x"]
        );
        // A late (older) announcement still counts, and is saved.
        assert!(map.apply(&update(
            "beta",
            "inst_y",
            PresenceState::Closed,
            "2026-10-21T09:00:00Z"
        )));
        assert_eq!(map.instances_of("beta", "inst_a", now).len(), 3);
        // Announcements older than the window drop out of the choice.
        assert_eq!(
            map.instances_of("beta", "inst_a", "2026-10-22T10:30:00Z"),
            ["inst_x"]
        );
        assert!(map.instances_of("gamma", "inst_a", now).is_empty());
    }

    #[test]
    fn the_seen_list_per_name_is_bounded() {
        let mut map = PresenceMap::default();
        for n in 0..(MAX_SEEN_PER_AGENT + 4) {
            map.apply(&update(
                "beta",
                &format!("inst_{n:02}"),
                PresenceState::Active,
                &format!("2026-10-21T10:{n:02}:00Z"),
            ));
        }
        let seen = &map.seen["beta"];
        assert_eq!(seen.len(), MAX_SEEN_PER_AGENT);
        assert!(!seen.contains_key("inst_00") && seen.contains_key("inst_19"));
    }

    #[test]
    fn the_map_persists_privately() {
        let dir = tempfile::tempdir().unwrap();
        let mut map = PresenceMap::default();
        map.apply(&update(
            "beta",
            "inst_b",
            PresenceState::Idle,
            "2026-10-21T10:00:00Z",
        ));
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
        let at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        assert!(
            PresenceMap::mutate(dir.path(), |map| map.apply(&update(
                "beta",
                "inst_b",
                PresenceState::Active,
                &at
            )))
            .unwrap()
        );
        assert_eq!(
            std::fs::read(bus.join("presence.json.damaged-1")).unwrap(),
            b"[1,2"
        );
        assert!(PresenceMap::load(dir.path()).agents.contains_key("beta"));
    }

    #[test]
    fn prune_compares_times_not_text() {
        let now = chrono::Utc::now();
        let mut map = PresenceMap::default();
        // Recent, written with an offset and fractions: parsed, it is kept.
        let recent = (now - chrono::Duration::days(6))
            .with_timezone(&chrono::FixedOffset::east_opt(3600).unwrap())
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, false);
        map.agents.insert(
            "beta".into(),
            Presence {
                instance_id: "inst_b".into(),
                vault_id: "p".into(),
                role: "dev".into(),
                state: PresenceState::Active,
                server_time: recent.clone(),
            },
        );
        map.seen
            .insert("beta".into(), BTreeMap::from([("inst_b".into(), recent)]));
        map.seen.insert(
            "gamma".into(),
            BTreeMap::from([("inst_g".into(), "not a time".into())]),
        );
        map.prune(now);
        assert!(map.agents.contains_key("beta") && map.seen.contains_key("beta"));
        assert!(
            !map.seen.contains_key("gamma"),
            "an unreadable time is too old"
        );
    }

    #[test]
    fn old_agents_age_out_and_the_map_is_capped() {
        let now = chrono::Utc::now();
        let ago = |minutes: i64| {
            (now - chrono::Duration::minutes(minutes))
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        };
        let mut map = PresenceMap::default();
        map.apply(&update(
            "stale",
            "inst_s",
            PresenceState::Idle,
            &ago(8 * 24 * 60),
        ));
        for n in 0..(MAX_AGENTS + 3) {
            let minutes = (MAX_AGENTS + 3 - n) as i64;
            map.apply(&update(
                &format!("agent_{n:04}"),
                &format!("inst_{n:04}"),
                PresenceState::Active,
                &ago(minutes),
            ));
        }
        map.prune(now);
        assert!(!map.agents.contains_key("stale") && !map.seen.contains_key("stale"));
        assert_eq!(map.agents.len(), MAX_AGENTS);
        assert_eq!(map.seen.len(), MAX_AGENTS);
        assert!(!map.agents.contains_key("agent_0002"), "oldest went");
        assert!(map.agents.contains_key("agent_0003"));
        assert!(
            map.agents
                .contains_key(&format!("agent_{:04}", MAX_AGENTS + 2))
        );
    }
}
