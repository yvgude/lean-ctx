// SPDX-License-Identifier: Apache-2.0
//! Collaboration friction per hour (V16.1, RESOLUTIONS R128): the local
//! friction log (`core::agents::friction`) summed for one closed hour per
//! vault into the C0 2.0 counts of a collaboration `finding`
//! (`about: collaboration`, `counts`, `window`; CONTRACT §3.19.2). Counts
//! and ids only, never text; sealed vaults get none.

use std::collections::BTreeMap;
use std::io::Write as _;

use chrono::{DateTime, Duration, DurationRound as _, Utc};

use crate::core::agents::AgentEntry;
use crate::core::agents::friction::{self, Event};

use super::HiveStore;
use super::bus::v2::{self, About, CollabCounts, MessageType, Publish, Window};
use super::vault::VaultMap;

/// `<data>/hive/friction.jsonl`: one `{hour, worker_id, place, counts}`
/// line per finding sent (R128; counts and ids only).
const HOURS_FILE: &str = "friction.jsonl";
/// The last hour (`from_ms`) whose findings were queued.
const STATE_FILE: &str = "friction-state.json";
/// Closed hours caught up at most after a pause (the relay was off).
const CATCH_UP_HOURS: i64 = 24;

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct EmitState {
    last_hour_ms: Option<i64>,
}

/// Queue the collaboration findings of every hour closed since the last
/// run (at most `CATCH_UP_HOURS`) for the v2 vault bus; returns how many
/// were queued. Nothing on a 1.x Hive. The first run starts with the hour
/// that just closed. Called by the bus relay about once a minute.
pub fn emit_due(store: &HiveStore, now: DateTime<Utc>) -> usize {
    if !super::bus::relay_v2::speaks_v2(store) {
        return 0;
    }
    let Ok(Some(instance)) = store.instance() else {
        return 0;
    };
    let Ok(this_hour) = now.duration_trunc(Duration::hours(1)) else {
        return 0;
    };
    let dir = store.data_dir();
    let state_path = dir.join(STATE_FILE);
    let mut state: EmitState = std::fs::read(&state_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    let first = state
        .last_hour_ms
        .and_then(DateTime::from_timestamp_millis)
        .map_or(this_hour - Duration::hours(1), |last| {
            (last + Duration::hours(1)).max(this_hour - Duration::hours(CATCH_UP_HOURS))
        });
    let registry = crate::core::agents::AgentRegistry::load_or_create();
    let agents = registry.list_all();
    let vaults = VaultMap::load(dir);
    let worker_of = |agent: &AgentEntry| {
        let host = super::workers::attach::detect_host_app(
            agent.transport.as_ref().map(|t| t.host.as_str()),
            Some(agent.agent_type.as_str()),
            std::iter::empty::<(&str, &str)>(),
            None,
        );
        let role = agent
            .role
            .as_deref()
            .unwrap_or(super::workers::GENERAL_ROLE);
        super::workers::attach::attach(store, &instance.instance_id, &host, role)
            .ok()
            .map(|attached| attached.worker.worker_id)
    };
    let now_ms = u64::try_from(now.timestamp_millis()).unwrap_or(0);
    let mut queued = 0;
    let mut hour = first;
    while hour < this_hour {
        let end = hour + Duration::hours(1);
        let events = friction::events(hour, end);
        for finding in hour_findings(&events, agents, &vaults, &worker_of, hour, end) {
            if let Err(error) = super::bus::relay_v2::queue_publish(store, finding.publish(now_ms))
            {
                store.record_error(&format!("collaboration finding: {error}"));
                continue;
            }
            append_hour(dir, &finding);
            queued += 1;
        }
        state.last_hour_ms = Some(hour.timestamp_millis());
        hour = end;
    }
    if let Ok(bytes) = serde_json::to_vec(&state) {
        let _ = super::state::write_private(dir, STATE_FILE, &bytes);
    }
    queued
}

fn append_hour(dir: &std::path::Path, finding: &HourFinding) {
    let line = serde_json::json!({
        "hour": finding.from_ms,
        "worker_id": finding.worker_id,
        "place": finding.to,
        "counts": finding.counts,
    });
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    if let Ok(mut file) = options.open(dir.join(HOURS_FILE)) {
        let _ = writeln!(file, "{line}");
    }
}

/// Count a merge conflict when a bound, non-sealed checkout an active agent
/// works in has unmerged paths that the last sample did not see; `seen`
/// carries the conflicted checkouts from one sample to the next, so one
/// conflict counts once however long it lasts.
pub fn sample_merge_conflicts(store: &HiveStore, seen: &mut std::collections::BTreeSet<String>) {
    let vaults = VaultMap::load(store.data_dir());
    let registry = crate::core::agents::AgentRegistry::load_or_create();
    let mut roots: BTreeMap<&str, &str> = BTreeMap::new();
    for agent in registry.list_active(None) {
        if vaults
            .for_root(&agent.project_root)
            .is_some_and(|record| !record.sealed)
        {
            roots
                .entry(agent.project_root.as_str())
                .or_insert(agent.agent_id.as_str());
        }
    }
    seen.retain(|root| roots.contains_key(root.as_str()));
    for (root, agent) in roots {
        if has_unmerged_paths(root) {
            if seen.insert(root.to_string()) {
                friction::record_for(friction::MERGE_CONFLICT, Some(agent), Some(root));
            }
        } else {
            seen.remove(root);
        }
    }
}

/// Whether `git ls-files -u` lists any path in `root` (an error is "no").
fn has_unmerged_paths(root: &str) -> bool {
    std::process::Command::new("git")
        .args(["-C", root, "ls-files", "-u", "-z"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .is_ok_and(|out| out.status.success() && !out.stdout.is_empty())
}

/// The C0 `counts` keys (enums/friction-signals.json), in that order.
pub const COUNT_KEYS: [&str; 8] = [
    "claim_refused",
    "claim_expired",
    "registration_renewed",
    "wait_timeout",
    "merge_conflict",
    "gate_reruns",
    "agent_seconds",
    "idle_seconds",
];

/// The keys that are friction (all but the two time measures).
const FRICTION_KEYS: [&str; 6] = [
    "claim_refused",
    "claim_expired",
    "registration_renewed",
    "wait_timeout",
    "merge_conflict",
    "gate_reruns",
];

/// The C0 key a local signal counts under; `None` for signals that are
/// not part of the contract.
pub fn count_key(signal: &str) -> Option<&'static str> {
    Some(match signal {
        friction::CLAIM_REFUSED_PRESENCE
        | friction::CLAIM_REFUSED_ROLE
        | friction::CLAIM_REFUSED_PERMIT => "claim_refused",
        friction::CLAIM_EXPIRED => "claim_expired",
        friction::REGISTRATION_RENEWED => "registration_renewed",
        friction::WAIT_TIMEOUT => "wait_timeout",
        friction::MERGE_CONFLICT => "merge_conflict",
        _ => return None,
    })
}

/// One vault's collaboration finding for one closed hour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HourFinding {
    /// `#<ws>/<vault>`.
    pub to: String,
    /// The worker that sends it: the one with the most agent time in the
    /// vault that hour (ties: the smaller id).
    pub worker_id: String,
    pub from_ms: i64,
    pub to_ms: i64,
    /// C0 keys with a non-zero value only.
    pub counts: BTreeMap<&'static str, u32>,
}

impl HourFinding {
    /// The C0 statement template: "Collaboration friction from {from} to
    /// {to}: {n} signals in {h} agent-hours." (times in UTC, `HH:MM`).
    pub fn statement(&self) -> String {
        let hhmm = |ms: i64| {
            DateTime::from_timestamp_millis(ms)
                .map_or_else(String::new, |t| t.format("%H:%M").to_string())
        };
        let signals: u32 = FRICTION_KEYS
            .iter()
            .filter_map(|key| self.counts.get(key))
            .sum();
        let seconds = self.counts.get("agent_seconds").copied().unwrap_or(0);
        format!(
            "Collaboration friction from {} to {}: {signals} signals in {:.1} agent-hours.",
            hhmm(self.from_ms),
            hhmm(self.to_ms),
            f64::from(seconds) / 3600.0
        )
    }

    /// The finding as a v2 vault-bus publish (CONTRACT §3.19.2): type
    /// `finding`, `about: collaboration`, no refs, the counts and window in
    /// plaintext, the statement as an observed finding.
    pub fn publish(&self, now_ms: u64) -> Publish {
        let get = |key: &str| self.counts.get(key).copied();
        Publish {
            schema: v2::BUS_SCHEMA.to_string(),
            client_msg_id: v2::new_ulid(now_ms),
            to: self.to.clone(),
            kind: MessageType::Finding,
            worker_id: self.worker_id.clone(),
            thread_id: None,
            reply_to: None,
            refs: Vec::new(),
            body: serde_json::json!({"statement": self.statement(), "evidence": "observed"}),
            ck_wrapped: None,
            epoch: None,
            about: Some(About::Collaboration),
            counts: Some(CollabCounts {
                registration_renewed: get("registration_renewed"),
                wait_timeout: get("wait_timeout"),
                merge_conflict: get("merge_conflict"),
                claim_refused: get("claim_refused"),
                claim_expired: get("claim_expired"),
                gate_reruns: get("gate_reruns"),
                agent_seconds: get("agent_seconds"),
                idle_seconds: get("idle_seconds"),
            }),
            window: Some(Window {
                from_ms: u64::try_from(self.from_ms).unwrap_or(0),
                to_ms: u64::try_from(self.to_ms).unwrap_or(0),
            }),
        }
    }
}

/// The findings of the hour `[from, to)`: the signals of `events` and the
/// agent time of `agents`, attributed to the vault of their checkout
/// (`vaults`, sealed ones left out) and to the worker `worker_of` names for
/// their agent. A signal without a known agent counts for the vault of its
/// checkout under the vault's sending worker. Vaults without friction and
/// without agent time get no finding.
pub(crate) fn hour_findings(
    events: &[Event],
    agents: &[AgentEntry],
    vaults: &VaultMap,
    worker_of: &dyn Fn(&AgentEntry) -> Option<String>,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Vec<HourFinding> {
    let place_of = |root: &str| {
        vaults
            .for_root(root)
            .filter(|record| !record.sealed)
            .map(super::bus::relay_v2::address_of)
    };
    // vault → worker → agent seconds, and vault → C0 key → count.
    let mut time: BTreeMap<String, BTreeMap<String, i64>> = BTreeMap::new();
    let mut counts: BTreeMap<String, BTreeMap<&'static str, u32>> = BTreeMap::new();
    for agent in agents {
        let (Some(place), Some(worker)) = (place_of(&agent.project_root), worker_of(agent)) else {
            continue;
        };
        let start = agent.started_at.max(from);
        let end = agent.last_active.min(to);
        let seconds = (end - start).num_seconds().max(0);
        *time.entry(place).or_default().entry(worker).or_default() += seconds;
    }
    for event in events {
        let Some(key) = count_key(&event.signal) else {
            continue;
        };
        let agent = event
            .agent
            .as_deref()
            .and_then(|id| agents.iter().find(|a| a.agent_id == id));
        let root = event
            .root
            .as_deref()
            .or_else(|| agent.map(|a| a.project_root.as_str()));
        let Some(place) = root.and_then(place_of) else {
            continue;
        };
        if let Some(worker) = agent.and_then(worker_of) {
            time.entry(place.clone())
                .or_default()
                .entry(worker)
                .or_default();
        }
        let count = counts.entry(place).or_default().entry(key).or_default();
        *count = count.saturating_add(1);
    }
    time.into_iter()
        .filter_map(|(place, workers)| {
            let worker_id = workers
                .iter()
                .max_by(|a, b| a.1.cmp(b.1).then_with(|| b.0.cmp(a.0)))
                .map(|(id, _)| id.clone())?;
            let seconds: i64 = workers.values().sum();
            let mut counts = counts.remove(&place).unwrap_or_default();
            if seconds > 0 {
                counts.insert("agent_seconds", u32::try_from(seconds).unwrap_or(u32::MAX));
            }
            (!counts.is_empty()).then(|| HourFinding {
                to: place,
                worker_id,
                from_ms: from.timestamp_millis(),
                to_ms: to.timestamp_millis(),
                counts,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::agents::AgentStatus;
    use crate::core::hive::vault::VaultRecord;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn agent(id: &str, root: &str, from: &str, to: &str) -> AgentEntry {
        AgentEntry {
            agent_id: id.into(),
            agent_type: "claude".into(),
            role: Some("engine".into()),
            project_root: root.into(),
            started_at: at(from),
            last_active: at(to),
            pid: 1,
            process_identity: None,
            durable_identity_id: None,
            status: AgentStatus::Active,
            status_message: None,
            transport: None,
            role_source: None,
            parent: None,
        }
    }

    fn event(signal: &str, agent: Option<&str>, root: Option<&str>) -> Event {
        Event {
            at: at("2026-10-24T08:30:00Z"),
            signal: signal.into(),
            agent: agent.map(Into::into),
            root: root.map(Into::into),
        }
    }

    fn vaults() -> VaultMap {
        let mut map = VaultMap::default();
        for (id, ns, name, sealed, root) in [
            ("v1", "acme", "payments", false, "/w/pay"),
            ("v2", "acme", "secret", true, "/w/secret"),
        ] {
            map.vaults.insert(
                id.into(),
                VaultRecord {
                    vault_id: id.into(),
                    ns: ns.into(),
                    name: name.into(),
                    sealed,
                    roots: vec![root.into()],
                },
            );
        }
        map
    }

    /// Every local signal counts under exactly one C0 key, and only the
    /// eight C0 keys exist.
    #[test]
    fn local_signals_map_onto_the_contract_keys() {
        for (signal, key) in [
            (friction::CLAIM_REFUSED_PRESENCE, "claim_refused"),
            (friction::CLAIM_REFUSED_ROLE, "claim_refused"),
            (friction::CLAIM_REFUSED_PERMIT, "claim_refused"),
            (friction::CLAIM_EXPIRED, "claim_expired"),
            (friction::REGISTRATION_RENEWED, "registration_renewed"),
            (friction::WAIT_TIMEOUT, "wait_timeout"),
            (friction::MERGE_CONFLICT, "merge_conflict"),
        ] {
            assert_eq!(count_key(signal), Some(key), "{signal}");
            assert!(COUNT_KEYS.contains(&key));
        }
        assert_eq!(count_key("something_else"), None);
    }

    /// One finding per vault and hour: signals and agent time of the
    /// vault's checkouts, sent by the worker with the most agent time;
    /// sealed vaults and other hours get nothing.
    #[test]
    fn an_hour_sums_into_one_finding_per_vault() {
        let agents = [
            agent(
                "mcp-a",
                "/w/pay",
                "2026-10-24T07:00:00Z",
                "2026-10-24T08:30:00Z",
            ),
            agent(
                "mcp-b",
                "/w/pay",
                "2026-10-24T08:30:00Z",
                "2026-10-24T10:00:00Z",
            ),
            agent(
                "mcp-c",
                "/w/secret",
                "2026-10-24T08:00:00Z",
                "2026-10-24T09:00:00Z",
            ),
        ];
        let worker_of = |a: &AgentEntry| Some(format!("wkr_{}", a.agent_id));
        let events = [
            event(friction::CLAIM_REFUSED_ROLE, Some("mcp-a"), None),
            event(
                friction::CLAIM_REFUSED_PRESENCE,
                Some("mcp-b"),
                Some("/w/pay"),
            ),
            event(friction::WAIT_TIMEOUT, None, Some("/w/pay")),
            event(friction::CLAIM_EXPIRED, Some("mcp-c"), None),
            event("unknown_signal", Some("mcp-a"), None),
        ];
        let findings = hour_findings(
            &events,
            &agents,
            &vaults(),
            &worker_of,
            at("2026-10-24T08:00:00Z"),
            at("2026-10-24T09:00:00Z"),
        );
        assert_eq!(
            findings.len(),
            1,
            "the sealed vault gets none: {findings:?}"
        );
        let finding = &findings[0];
        assert_eq!(finding.to, "#acme/payments");
        assert_eq!(
            finding.worker_id, "wkr_mcp-a",
            "a tie of 30 min each goes to the smaller id"
        );
        assert_eq!(
            (finding.from_ms, finding.to_ms),
            (1_792_828_800_000, 1_792_832_400_000)
        );
        assert_eq!(
            finding.counts,
            BTreeMap::from([
                ("agent_seconds", 3600),
                ("claim_refused", 2),
                ("wait_timeout", 1)
            ])
        );
        assert_eq!(
            finding.statement(),
            "Collaboration friction from 08:00 to 09:00: 3 signals in 1.0 agent-hours."
        );
        assert!(finding.counts.keys().all(|key| COUNT_KEYS.contains(key)));
    }

    /// The publish carries exactly what the C0 2.0 collaboration finding
    /// fixture stores (`about`, `counts`, `window`, `to`, `worker_id`,
    /// type, no refs) and passes the engine's own v2 publish rules.
    #[test]
    fn the_publish_matches_the_contract_fixture() {
        let path = format!(
            "{}/crates/lean-ctx-protocol/tests/fixtures/hive-v2/bus/message.finding.collaboration.json",
            env!("CARGO_MANIFEST_DIR")
        );
        let stored: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let counts: BTreeMap<&'static str, u32> = COUNT_KEYS
            .iter()
            .filter_map(|key| {
                let n = stored["counts"][*key].as_u64()?;
                Some((*key, u32::try_from(n).unwrap()))
            })
            .collect();
        let finding = HourFinding {
            to: stored["to"].as_str().unwrap().to_string(),
            worker_id: stored["worker_id"].as_str().unwrap().to_string(),
            from_ms: stored["window"]["from_ms"].as_i64().unwrap(),
            to_ms: stored["window"]["to_ms"].as_i64().unwrap(),
            counts,
        };
        let publish = finding.publish(1_792_803_600_000);
        publish.validate().expect("a valid v2 publish");
        let sent = serde_json::to_value(&publish).unwrap();
        for member in [
            "schema",
            "type",
            "about",
            "counts",
            "window",
            "to",
            "worker_id",
        ] {
            assert_eq!(sent[member], stored[member], "{member}");
        }
        assert!(sent.get("refs").is_none(), "no refs");
        assert_eq!(sent["body"]["evidence"], "observed");
        assert_eq!(
            sent["body"]["statement"],
            "Collaboration friction from 00:00 to 01:00: 8 signals in 1.5 agent-hours.",
            "the window in UTC"
        );
    }

    /// The relay queues each closed hour's findings once, keeps a line per
    /// finding in `friction.jsonl`, and queues nothing for a 1.x Hive.
    #[test]
    fn closed_hours_are_queued_once_and_only_for_a_v2_hive() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        let store = HiveStore::at(dir.path().join("c"), dir.path().join("d"));
        store
            .write_config(
                super::super::state::INSTANCE_FILE,
                &super::super::state::InstanceRecord {
                    instance_id: "inst_a".into(),
                    org_id: "org_a".into(),
                    org_name: "Acme".into(),
                    label: "mac".into(),
                    url: "https://hive.example".into(),
                    org_salt_id: "salt".into(),
                    enrolled_at: "2026-10-24T00:00:00Z".into(),
                    web_url: None,
                    org_kind: None,
                    company_name: None,
                },
            )
            .unwrap();
        let project = tempfile::tempdir().unwrap();
        let root = project.path().to_string_lossy().into_owned();
        let mut map = VaultMap::default();
        map.vaults.insert(
            "v1".into(),
            VaultRecord {
                vault_id: "v1".into(),
                ns: "acme".into(),
                name: "payments".into(),
                sealed: false,
                roots: vec![root.clone()],
            },
        );
        map.save(store.data_dir()).unwrap();
        let mut registry = crate::core::agents::AgentRegistry::new();
        let agent = registry
            .register("claude", Some("engine"), &root, None)
            .unwrap();
        registry.save().unwrap();
        friction::record_for(friction::WAIT_TIMEOUT, Some(&agent), Some(&root));
        let next_hour = Utc::now() + Duration::hours(1);

        super::super::bus::relay_v2::record_protocol(&store, false).unwrap();
        assert_eq!(emit_due(&store, next_hour), 0, "a 1.x Hive gets none");

        super::super::bus::relay_v2::record_protocol(&store, true).unwrap();
        assert_eq!(emit_due(&store, next_hour), 1);
        assert_eq!(emit_due(&store, next_hour), 0, "an hour is queued once");
        let lines = std::fs::read_to_string(store.data_dir().join(HOURS_FILE)).unwrap();
        let line: serde_json::Value = serde_json::from_str(lines.trim()).unwrap();
        assert_eq!(line["place"], "#acme/payments");
        assert_eq!(line["counts"]["wait_timeout"], 1);
        assert!(line["worker_id"].as_str().unwrap().starts_with("wkr_"));
    }

    /// R128 `merge_conflict`: unmerged paths in a bound checkout count once
    /// per conflict, however many samples see it; a resolved checkout
    /// counts again on its next conflict.
    #[test]
    fn a_merge_conflict_counts_once_per_conflict() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        let store = HiveStore::at(dir.path().join("c"), dir.path().join("d"));
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path().to_string_lossy().into_owned();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@t", "-C", &root])
                .args(args)
                .output()
                .unwrap();
            out.status.success()
        };
        assert!(git(&["init", "-q", "-b", "main"]));
        std::fs::write(repo.path().join("f"), "base\n").unwrap();
        assert!(git(&["add", "f"]) && git(&["commit", "-qm", "base"]));
        assert!(git(&["checkout", "-qb", "other"]));
        std::fs::write(repo.path().join("f"), "other\n").unwrap();
        assert!(git(&["commit", "-qam", "other"]) && git(&["checkout", "-q", "main"]));
        std::fs::write(repo.path().join("f"), "main\n").unwrap();
        assert!(git(&["commit", "-qam", "main"]));
        assert!(!git(&["merge", "-q", "other"]), "the merge conflicts");

        let mut map = VaultMap::default();
        map.vaults.insert(
            "v1".into(),
            VaultRecord {
                vault_id: "v1".into(),
                ns: "acme".into(),
                name: "payments".into(),
                sealed: false,
                roots: vec![root.clone()],
            },
        );
        map.save(store.data_dir()).unwrap();
        let mut registry = crate::core::agents::AgentRegistry::new();
        let agent = registry
            .register("claude", Some("engine"), &root, None)
            .unwrap();
        registry.save().unwrap();

        let conflicts = || {
            friction::events(
                Utc::now() - Duration::hours(1),
                Utc::now() + Duration::hours(1),
            )
            .into_iter()
            .filter(|e| e.signal == friction::MERGE_CONFLICT)
            .collect::<Vec<_>>()
        };
        let mut seen = std::collections::BTreeSet::new();
        sample_merge_conflicts(&store, &mut seen);
        sample_merge_conflicts(&store, &mut seen);
        let counted = conflicts();
        assert_eq!(counted.len(), 1, "one conflict, two samples");
        assert_eq!(counted[0].agent.as_deref(), Some(agent.as_str()));
        assert_eq!(counted[0].root.as_deref(), Some(root.as_str()));

        assert!(git(&["merge", "--abort"]));
        sample_merge_conflicts(&store, &mut seen);
        assert!(seen.is_empty(), "resolved");
        assert!(!git(&["merge", "-q", "other"]));
        sample_merge_conflicts(&store, &mut seen);
        assert_eq!(conflicts().len(), 2, "a new conflict counts again");
    }

    #[test]
    fn a_quiet_hour_has_no_finding() {
        let agents = [agent(
            "mcp-a",
            "/w/pay",
            "2026-10-24T05:00:00Z",
            "2026-10-24T06:00:00Z",
        )];
        let findings = hour_findings(
            &[],
            &agents,
            &vaults(),
            &|a: &AgentEntry| Some(a.agent_id.clone()),
            at("2026-10-24T08:00:00Z"),
            at("2026-10-24T09:00:00Z"),
        );
        assert!(findings.is_empty(), "{findings:?}");
    }
}
