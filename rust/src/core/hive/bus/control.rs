// SPDX-License-Identifier: Apache-2.0
//! Control frames from Hive (`hold`, `veto`, `stop`, `resume`) and the
//! admission answers derived from them.
//!
//! The relay records every `control` frame in `<data>/hive/bus/control.json`
//! ([`record`]); tool admission and the lease/handoff paths only read it
//! ([`admission`], [`vetoed`]). Rules:
//!
//! - State is kept per target: an agent id, or `*` for every agent of this
//!   instance. A frame whose `seq` is not newer than the target's last one is
//!   ignored, so a replay after reconnect never undoes a later control.
//! - `hold` and `stop` set the target's mode; `resume` without `target_ref`
//!   clears it. `veto` refuses the lease or handoff named by `target_ref`;
//!   `resume` with that `target_ref` lifts it.
//! - An agent is refused when its own entry or `*` refuses it; `stop` wins
//!   over `hold`. Resuming one agent does not lift a control on `*`.
//! - A control only pauses Hive traffic: Hive tools (`ctx_tasks`), bus
//!   messages, the Hive side of a lease and a handoff through Hive. Local
//!   tools, local leases and handoffs on this machine are never refused
//!   (connector rule "local work is never gated"), online or offline.
//! - The state belongs to the enrolled instance it arrived for: after
//!   `hive disconnect --keep-local` or a new enrollment it no longer applies,
//!   and while the lease is lapsed it is inactive (plan C rule 2: local work
//!   never depends on Hive state). `hive status` names the reason.
//!
//! Bus controls are org coordination between cooperating agents, not a
//! security boundary: whoever controls this machine can edit these files.
//! Enforcement that must hold belongs to the runtime/policy path (org policy,
//! role and policy guards). That is also why the lapse check reads the
//! stored lease without re-verifying its signature: it can only switch
//! controls off, never on.
//!
//! Network-free and deterministic; the file is read only when it exists.
//!
//! A damaged `control.json`, or one of a version this build does not know,
//! is moved aside and never read as "no controls" (RB8 M12): until the relay
//! has received Hive's replay of the active controls after its `hello`
//! ([`rebuilt`]), Hive traffic waits under a "waiting for Hive" hold; local
//! work goes on. Changes go through `control.json.lock` (M11).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::core::hive::HiveError;
use crate::core::hive::state::{HiveStore, load_or_quarantine, lock_state, write_private};
use lean_ctx_protocol::UtcTimestamp;
use lean_ctx_protocol::hive::bus::{BusFrameV1, ControlAction};
use lean_ctx_protocol::hive::lease::LeaseState;

const CONTROL_DIR: &str = "bus";
const CONTROL_FILE: &str = "control.json";
const CONTROL_VERSION: u32 = 1;
/// Target of a control without `target_agent`.
pub const ALL_AGENTS: &str = "*";

/// One control as it arrived.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlEntry {
    pub action: ControlAction,
    pub control_id: String,
    pub reason: String,
    pub issued_by: String,
    pub seq: u64,
    pub server_time: UtcTimestamp,
}

/// State of one target (an agent id or [`ALL_AGENTS`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetControl {
    /// The active `hold` or `stop`.
    #[serde(default)]
    pub mode: Option<ControlEntry>,
    /// Active vetoes by lease or handoff id.
    #[serde(default)]
    pub vetoes: BTreeMap<String, ControlEntry>,
    #[serde(default)]
    pub last_seq: u64,
}

/// `control.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlState {
    pub version: u32,
    /// The instance the controls were issued for.
    #[serde(default)]
    pub instance_id: Option<String>,
    #[serde(default)]
    pub targets: BTreeMap<String, TargetControl>,
    /// Since when the file was found damaged: refuse until the relay has
    /// the replay (M12).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rebuild_pending: Option<UtcTimestamp>,
    /// While a rebuild is pending: the agents (agent id, project id) of the
    /// hellos whose replay Hive has confirmed since. Hive replays a
    /// project's controls only for the hello that first announces it, so
    /// these agents run again while the others wait for theirs; the rebuild
    /// ends once every announced project is confirmed.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub rebuilt_for: BTreeSet<(String, String)>,
}

fn stamp_now() -> UtcTimestamp {
    UtcTimestamp::new(chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .expect("a formatted UTC time is a valid timestamp")
}

/// What a pending rebuild refuses with.
const REBUILD_REASON: &str =
    "the local control state was damaged; waiting for Hive to resend the controls";

/// Why a call is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// `*` or the agent id the control names.
    pub target: String,
    pub entry: ControlEntry,
}

impl Refusal {
    /// One line for the agent, without internal ids.
    pub fn message(&self) -> String {
        let who = if self.target == ALL_AGENTS {
            "every agent on this machine".to_string()
        } else {
            format!("agent {}", self.target)
        };
        let what = match self.entry.action {
            ControlAction::Stop => "stopped: end the current task",
            ControlAction::Hold => "on hold until resumed",
            ControlAction::Veto => "vetoed",
            ControlAction::Resume => "resumed",
        };
        format!(
            "Hive control: {who} is {what} (by {}: {}). Hive traffic waits; local tools keep working.",
            self.entry.issued_by, self.entry.reason
        )
    }
}

/// A control that is in force, for `hive status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveControl {
    /// `None`: every agent of this instance.
    pub target_agent: Option<String>,
    pub action: ControlAction,
    pub target_ref: Option<String>,
    pub reason: String,
    pub issued_by: String,
    pub since: UtcTimestamp,
}

impl ControlState {
    /// Read `<data>/bus/control.json` without changing anything (the tool
    /// side): missing = empty; damaged, an unknown version or unreadable =
    /// `Err` with the file's change time, which the caller treats as a
    /// pending rebuild since then (M12).
    fn read(data_dir: &Path) -> Result<Self, Option<UtcTimestamp>> {
        let path = data_dir.join(CONTROL_DIR).join(CONTROL_FILE);
        let changed = || {
            let at: chrono::DateTime<chrono::Utc> =
                std::fs::metadata(&path).ok()?.modified().ok()?.into();
            UtcTimestamp::new(at.format("%Y-%m-%dT%H:%M:%SZ").to_string()).ok()
        };
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<Self>(&bytes)
                .ok()
                .filter(|state| state.version == CONTROL_VERSION)
                .ok_or_else(changed),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(_) => Err(changed()),
        }
    }

    /// Load `<data>/bus/control.json` for a change (under the lock):
    /// missing = empty; a damaged file or an unknown version is moved aside
    /// and an error (M12): see `load_for`.
    pub fn load(data_dir: &Path) -> Result<Self, HiveError> {
        load_or_quarantine(
            &data_dir.join(CONTROL_DIR).join(CONTROL_FILE),
            |state: &Self| state.version == CONTROL_VERSION,
        )
        .map(Option::unwrap_or_default)
    }

    pub fn save(&self, data_dir: &Path) -> Result<(), HiveError> {
        let mut bytes =
            serde_json::to_vec_pretty(self).map_err(|e| HiveError::Local(e.to_string()))?;
        bytes.push(b'\n');
        write_private(&data_dir.join(CONTROL_DIR), CONTROL_FILE, &bytes)
    }

    /// Apply one `control` frame for `instance_id`; `false` when it changed
    /// nothing (not a control frame, or not newer than the target's last).
    pub fn apply(&mut self, instance_id: Option<&str>, frame: &BusFrameV1) -> bool {
        let BusFrameV1::Control {
            control_id,
            target_agent,
            target_ref,
            action,
            reason,
            issued_by,
            seq,
            server_time,
        } = frame
        else {
            return false;
        };
        if self.instance_id.as_deref() != instance_id {
            // Controls of an earlier enrollment do not carry over.
            self.targets.clear();
            self.instance_id = instance_id.map(str::to_string);
        }
        self.version = CONTROL_VERSION;
        let target = self
            .targets
            .entry(target_agent.clone().unwrap_or_else(|| ALL_AGENTS.into()))
            .or_default();
        if *seq <= target.last_seq {
            return false;
        }
        target.last_seq = *seq;
        let entry = ControlEntry {
            action: *action,
            control_id: control_id.clone(),
            reason: reason.clone(),
            issued_by: issued_by.clone(),
            seq: *seq,
            server_time: server_time.clone(),
        };
        match (action, target_ref) {
            (ControlAction::Hold | ControlAction::Stop, _) => target.mode = Some(entry),
            (ControlAction::Veto, Some(reference)) => {
                target.vetoes.insert(reference.clone(), entry);
            }
            // A veto must name what it refuses; without one there is nothing to apply.
            (ControlAction::Veto, None) => {}
            (ControlAction::Resume, Some(reference)) => {
                target.vetoes.remove(reference);
            }
            (ControlAction::Resume, None) => target.mode = None,
        }
        true
    }

    /// The targets that apply to `agent_id`: `*`, then the agent itself.
    fn targets_for<'a>(
        &'a self,
        agent_id: Option<&'a str>,
    ) -> impl Iterator<Item = (&'a str, &'a TargetControl)> {
        std::iter::once(ALL_AGENTS)
            .chain(agent_id.filter(|id| *id != ALL_AGENTS))
            .filter_map(|name| self.targets.get(name).map(|t| (name, t)))
    }

    /// The hold of a pending rebuild, for every agent.
    fn pending(&self) -> Option<Refusal> {
        self.rebuild_pending.clone().map(|since| Refusal {
            target: ALL_AGENTS.into(),
            entry: ControlEntry {
                action: ControlAction::Hold,
                control_id: "local".into(),
                reason: REBUILD_REASON.into(),
                issued_by: "lean-ctx".into(),
                seq: 0,
                server_time: since,
            },
        })
    }

    /// The pending rebuild's hold for `agent_id`, unless its replay is
    /// confirmed already.
    fn pending_for(&self, agent_id: Option<&str>) -> Option<Refusal> {
        if agent_id.is_some_and(|id| self.rebuilt_for.iter().any(|(agent, _)| agent == id)) {
            return None;
        }
        self.pending()
    }

    /// Whether `agent_id` (`None`: an unknown caller, bound by `*` only) may
    /// run a mutating tool. `stop` wins over `hold`; a pending rebuild
    /// refuses unless a stop already does.
    pub fn admission(&self, agent_id: Option<&str>) -> Option<Refusal> {
        self.targets_for(agent_id)
            .filter_map(|(name, target)| {
                target.mode.as_ref().map(|entry| Refusal {
                    target: name.to_string(),
                    entry: entry.clone(),
                })
            })
            .chain(self.pending_for(agent_id))
            .max_by_key(|refusal| {
                (
                    refusal.entry.action == ControlAction::Stop,
                    refusal.entry.seq,
                )
            })
    }

    /// Whether the lease or handoff `target_ref` is vetoed for `agent_id`.
    pub fn vetoed(&self, agent_id: Option<&str>, target_ref: &str) -> Option<Refusal> {
        self.targets_for(agent_id)
            .filter_map(|(name, target)| {
                target.vetoes.get(target_ref).map(|entry| Refusal {
                    target: name.to_string(),
                    entry: entry.clone(),
                })
            })
            .max_by_key(|refusal| refusal.entry.seq)
            .or_else(|| self.pending_for(agent_id))
    }

    /// Every control in force, `*` first, then by agent and `target_ref`.
    pub fn active(&self) -> Vec<ActiveControl> {
        let mut out: Vec<ActiveControl> = self
            .pending()
            .map(|p| ActiveControl {
                target_agent: None,
                action: p.entry.action,
                target_ref: None,
                reason: p.entry.reason,
                issued_by: p.entry.issued_by,
                since: p.entry.server_time,
            })
            .into_iter()
            .collect();
        for (name, target) in &self.targets {
            let target_agent = (name != ALL_AGENTS).then(|| name.clone());
            let view = |entry: &ControlEntry, target_ref: Option<&String>| ActiveControl {
                target_agent: target_agent.clone(),
                action: entry.action,
                target_ref: target_ref.cloned(),
                reason: entry.reason.clone(),
                issued_by: entry.issued_by.clone(),
                since: entry.server_time.clone(),
            };
            if let Some(entry) = &target.mode {
                out.push(view(entry, None));
            }
            for (reference, entry) in &target.vetoes {
                out.push(view(entry, Some(reference)));
            }
        }
        out
    }
}

/// The controls on disk for the enrolled instance. A damaged file becomes
/// a saved pending rebuild for that instance (M12), so the refusal survives
/// the next read instead of reading as "no controls".
fn load_for(store: &HiveStore) -> Result<ControlState, HiveError> {
    match ControlState::load(store.data_dir()) {
        Ok(state) => Ok(state),
        Err(damaged) => {
            tracing::warn!("hive bus controls: {damaged}");
            let state = ControlState {
                version: CONTROL_VERSION,
                instance_id: store.instance()?.map(|i| i.instance_id),
                targets: BTreeMap::new(),
                rebuild_pending: Some(stamp_now()),
                rebuilt_for: BTreeSet::new(),
            };
            state.save(store.data_dir())?;
            Ok(state)
        }
    }
}

/// Load, change and save the controls under their lock (M11).
fn mutate<T>(
    store: &HiveStore,
    change: impl FnOnce(&mut ControlState) -> (bool, T),
) -> Result<T, HiveError> {
    let _lock = lock_state(&store.data_dir().join(CONTROL_DIR), CONTROL_FILE)?;
    let mut state = load_for(store)?;
    let (changed, out) = change(&mut state);
    if changed {
        state.save(store.data_dir())?;
    }
    Ok(out)
}

/// The relay's write path: apply `frame` and persist when it changed.
pub fn record(store: &HiveStore, frame: &BusFrameV1) -> Result<bool, HiveError> {
    let instance = store.instance()?.map(|i| i.instance_id);
    mutate(store, |state| {
        let changed = state.apply(instance.as_deref(), frame);
        (changed, changed)
    })
}

/// Hive confirmed (pong to the barrier ping behind a `hello`) that it sent
/// the replay of the active controls for the agents that hello announced
/// (`confirmed`: agent id, project id). Those agents run again; a pending
/// rebuild (M12) ends once every project the socket announces (`projects`)
/// is confirmed. Only those: a project without a local agent is never
/// announced and would hold callers without an agent id for good. `true`
/// when it ended.
pub fn rebuilt(
    store: &HiveStore,
    confirmed: &BTreeSet<(String, String)>,
    projects: &BTreeSet<String>,
) -> Result<bool, HiveError> {
    mutate(store, |state| {
        if state.rebuild_pending.is_none() {
            return (false, false);
        }
        let before = state.rebuilt_for.len();
        state.rebuilt_for.extend(confirmed.iter().cloned());
        let covered: BTreeSet<&str> = state.rebuilt_for.iter().map(|(_, p)| p.as_str()).collect();
        if !projects.iter().all(|p| covered.contains(p.as_str())) {
            return (state.rebuilt_for.len() != before, false);
        }
        state.rebuild_pending = None;
        state.rebuilt_for.clear();
        (true, true)
    })
}

/// Why recorded controls do not apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Inactive {
    /// Disconnected, or enrolled anew since they arrived.
    NotEnrolled,
    /// The lease is lapsed (or missing/damaged): rule 2.
    LeaseLapsed,
}

impl Inactive {
    /// The `hive status` line.
    pub fn status_line(self) -> &'static str {
        match self {
            Self::NotEnrolled => "controls inactive: not enrolled",
            Self::LeaseLapsed => "controls inactive: lease lapsed",
        }
    }
}

/// Whether the stored lease is lapsed at `local_now` (service clock via the
/// stored offset). Not signature-verified on purpose; see the module docs.
fn lease_lapsed(store: &HiveStore, local_now: chrono::DateTime<chrono::Utc>) -> bool {
    let record = store
        .read_config::<crate::core::hive::state::LeaseRecord>(crate::core::hive::state::LEASE_FILE)
        .ok()
        .flatten();
    let Some(record) = record else {
        return true;
    };
    let lease = serde_json::from_value::<lean_ctx_protocol::hive::lease::HiveLeaseEnvelopeV1>(
        record.envelope,
    )
    .ok()
    .and_then(|envelope| {
        serde_json::from_value::<lean_ctx_protocol::hive::lease::HiveLeaseV1>(envelope.lease).ok()
    });
    let Some(lease) = lease else {
        return true;
    };
    let service = (local_now + chrono::Duration::seconds(record.offset_seconds)).fixed_offset();
    !matches!(
        lease.state_at(service),
        Ok(LeaseState::Valid | LeaseState::Grace)
    )
}

/// The controls recorded for this machine and whether they apply at
/// `local_now`. Empty and active when nothing was recorded.
pub fn current_at(
    store: &HiveStore,
    local_now: chrono::DateTime<chrono::Utc>,
) -> (ControlState, Option<Inactive>) {
    if !store
        .data_dir()
        .join(CONTROL_DIR)
        .join(CONTROL_FILE)
        .exists()
    {
        return (ControlState::default(), None);
    }
    // Damaged, unknown or unreadable: refuse rather than allow. Only the
    // relay's next change moves the file aside (under the lock).
    let state = ControlState::read(store.data_dir()).unwrap_or_else(|since| ControlState {
        version: CONTROL_VERSION,
        instance_id: store.instance().ok().flatten().map(|i| i.instance_id),
        targets: BTreeMap::new(),
        rebuild_pending: Some(since.unwrap_or_else(stamp_now)),
        rebuilt_for: BTreeSet::new(),
    });
    if state.targets.is_empty() && state.rebuild_pending.is_none() {
        return (state, None);
    }
    let enrolled = store.instance().ok().flatten().map(|i| i.instance_id);
    if state.instance_id.is_none() || state.instance_id != enrolled {
        return (state, Some(Inactive::NotEnrolled));
    }
    if lease_lapsed(store, local_now) {
        return (state, Some(Inactive::LeaseLapsed));
    }
    (state, None)
}

/// The state in force now; empty when it does not apply.
pub fn current(store: &HiveStore) -> ControlState {
    match current_at(store, chrono::Utc::now()) {
        (state, None) => state,
        (_, Some(_)) => ControlState::default(),
    }
}

/// Admission for a mutating tool of `agent_id` on this machine.
pub fn admission(store: &HiveStore, agent_id: Option<&str>) -> Option<Refusal> {
    current(store).admission(agent_id)
}

/// Veto check for acquiring/renewing a lease or accepting a handoff.
pub fn vetoed(store: &HiveStore, agent_id: Option<&str>, target_ref: &str) -> Option<Refusal> {
    current(store).vetoed(agent_id, target_ref)
}

/// Controls in force, for `hive status`.
pub fn active(store: &HiveStore) -> Vec<ActiveControl> {
    current(store).active()
}

/// For `hive status`: the controls in force, or why recorded ones are not
/// (`None` when there is nothing to say).
pub fn status_at(
    store: &HiveStore,
    local_now: chrono::DateTime<chrono::Utc>,
) -> (Vec<ActiveControl>, Option<Inactive>) {
    match current_at(store, local_now) {
        (state, None) => (state.active(), None),
        (state, Some(reason)) => (Vec::new(), (!state.active().is_empty()).then_some(reason)),
    }
}

impl From<Inactive> for lean_ctx_protocol::hive::status::HiveBusControlsInactiveV1 {
    fn from(reason: Inactive) -> Self {
        match reason {
            Inactive::NotEnrolled => Self::NotEnrolled,
            Inactive::LeaseLapsed => Self::LeaseLapsed,
        }
    }
}

impl From<ActiveControl> for lean_ctx_protocol::hive::status::HiveStatusBusControlV1 {
    fn from(c: ActiveControl) -> Self {
        use lean_ctx_protocol::hive::status::HiveBusControlActionV1 as Action;
        Self {
            target_agent: c.target_agent,
            action: match c.action {
                ControlAction::Hold => Action::Hold,
                ControlAction::Stop => Action::Stop,
                ControlAction::Veto => Action::Veto,
                // Never kept as active; mapped for completeness.
                ControlAction::Resume => Action::Unknown,
            },
            target_ref: c.target_ref,
            reason: c.reason,
            issued_by: c.issued_by,
            since: c.since,
        }
    }
}

/// `hive status --json`: the controls in force, or why recorded ones are not.
pub fn status_view(
    store: &HiveStore,
    local_now: chrono::DateTime<chrono::Utc>,
) -> (
    Vec<lean_ctx_protocol::hive::status::HiveStatusBusControlV1>,
    Option<lean_ctx_protocol::hive::status::HiveBusControlsInactiveV1>,
) {
    let (active, inactive) = status_at(store, local_now);
    (
        active.into_iter().map(Into::into).collect(),
        inactive.map(Into::into),
    )
}

/// The tools `hold` and `stop` pause: those whose every call is Hive
/// traffic. Every other tool is local work and is never refused; the bus
/// (`ctx_agent`) pauses only its Hive side ([`veto_refusal`], `messages`).
fn hive_traffic(tool: &str) -> bool {
    crate::server::dynamic_tools::HIVE_TOOLS.contains(&tool)
}

/// The admission hook of the tool pipeline: `Some(message)` refuses `tool`
/// for `agent_id`. Only Hive tools are ever refused; missing or unreadable
/// local state never refuses.
pub fn tool_refusal(agent_id: Option<&str>, tool: &str) -> Option<String> {
    if !hive_traffic(tool) {
        return None;
    }
    let store = HiveStore::open().ok()?;
    admission(&store, agent_id).map(|refusal| refusal.message())
}

/// The veto hook of the Hive side of a lease (acquire, renew) and of a
/// handoff through Hive: `Some` when any of `refs` (lease ref, resource
/// ref, handoff id) is vetoed. The caller then skips the Hive request; the
/// local lease or handoff goes ahead.
pub fn veto_refusal(agent_id: Option<&str>, refs: &[&str]) -> Option<String> {
    let store = HiveStore::open().ok()?;
    let state = current(&store);
    refs.iter()
        .find_map(|reference| state.vetoed(agent_id, reference))
        .map(|refusal| refusal.message())
        // C0 2.0: a verified veto control on the lock or handoff (V9.5).
        .or_else(|| {
            refs.iter()
                .find_map(|reference| super::veto::vetoed_v2(&store, reference))
        })
}

/// Test enrollment: the protocol fixture's instance and lease, with the
/// stored clock offset placing "now" `hours_after_issue` after it was issued
/// (24 h: valid until; 168 h: grace until; later: lapsed).
#[cfg(test)]
pub(crate) fn enroll_for_tests(store: &HiveStore, hours_after_issue: i64) -> String {
    use crate::core::hive::state::{INSTANCE_FILE, InstanceRecord, LEASE_FILE, LeaseRecord};
    let envelope: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../crates/lean-ctx-protocol/tests/fixtures/hive/lease_envelope.response.json"
    ))
    .unwrap();
    let lease = &envelope["lease"];
    let instance_id = lease["instance_id"].as_str().unwrap().to_string();
    let issued = chrono::DateTime::parse_from_rfc3339(lease["issued_at"].as_str().unwrap())
        .unwrap()
        .with_timezone(&chrono::Utc);
    let offset = (issued + chrono::Duration::hours(hours_after_issue)) - chrono::Utc::now();
    store
        .write_config(
            INSTANCE_FILE,
            &InstanceRecord {
                instance_id: instance_id.clone(),
                org_id: lease["org_id"].as_str().unwrap().into(),
                org_name: "Acme AG".into(),
                label: "test-box".into(),
                url: "https://hive.example".into(),
                org_salt_id: "salt".into(),
                enrolled_at: "2026-10-20T12:00:00Z".into(),
                web_url: None,
                org_kind: None,
                company_name: None,
            },
        )
        .unwrap();
    store
        .write_config(
            LEASE_FILE,
            &LeaseRecord {
                envelope: envelope.clone(),
                offset_seconds: offset.num_seconds(),
                received_at: "2026-10-20T12:00:00Z".into(),
            },
        )
        .unwrap();
    instance_id
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lapsed_lease_voids_controls_and_status_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let store = HiveStore::at(dir.path().join("c"), dir.path().join("d"));
        enroll_for_tests(&store, 1);
        assert!(record(&store, &frame(1, None, ControlAction::Stop, None)).unwrap());
        assert_eq!(
            admission(&store, Some("a")).unwrap().entry.action,
            ControlAction::Stop
        );
        // Grace still counts as connected.
        enroll_for_tests(&store, 30);
        assert!(admission(&store, Some("a")).is_some());
        let (active_now, inactive) = status_at(&store, chrono::Utc::now());
        assert_eq!((active_now.len(), inactive), (1, None));
        // Past grace_until: lapsed, controls off, status names the reason.
        enroll_for_tests(&store, 24 * 8);
        assert!(admission(&store, Some("a")).is_none());
        let (active_now, inactive) = status_at(&store, chrono::Utc::now());
        assert!(active_now.is_empty());
        assert_eq!(inactive, Some(Inactive::LeaseLapsed));
        assert_eq!(
            inactive.unwrap().status_line(),
            "controls inactive: lease lapsed"
        );
        // A missing lease counts as lapsed.
        std::fs::remove_file(dir.path().join("c/lease.json")).unwrap();
        assert!(admission(&store, Some("a")).is_none());
    }

    #[test]
    fn damaged_or_newer_controls_hold_every_agent_until_hive_replays() {
        let dir = tempfile::tempdir().unwrap();
        let store = HiveStore::at(dir.path().join("c"), dir.path().join("d"));
        enroll_for_tests(&store, 1);
        assert!(record(&store, &frame(1, None, ControlAction::Stop, None)).unwrap());
        let path = dir.path().join("d/bus/control.json");
        for broken in [&b"{ not json"[..], br#"{"version": 2, "targets": {}}"#] {
            std::fs::write(&path, broken).unwrap();
            // Never "no controls": a hold for every agent, also on leases.
            let refusal = admission(&store, Some("a")).expect("fails closed");
            assert_eq!(refusal.entry.reason, REBUILD_REASON);
            assert!(admission(&store, None).is_some());
            assert!(vetoed(&store, Some("a"), "lease_1").is_some());
            let (shown, inactive) = status_at(&store, chrono::Utc::now());
            assert_eq!((shown.len(), inactive), (1, None), "status names it");
            // The replay after a hello re-applies what is in force; Hive's
            // pong to the barrier behind it frees the agents that hello
            // announced, and the rebuild ends once every project is covered.
            assert!(record(&store, &frame(9, Some("b"), ControlAction::Hold, None)).unwrap());
            assert!(admission(&store, Some("a")).is_some(), "still pending");
            let pair = |agent: &str, project: &str| (agent.to_string(), project.to_string());
            let projects = BTreeSet::from(["p1".to_string(), "p2".to_string()]);
            let first = BTreeSet::from([pair("a", "p1")]);
            assert!(!rebuilt(&store, &first, &projects).unwrap(), "p2 not yet");
            assert!(
                admission(&store, Some("a")).is_none(),
                "a's replay is confirmed"
            );
            assert!(vetoed(&store, Some("a"), "lease_1").is_none());
            assert!(admission(&store, Some("c")).is_some(), "c's project is not");
            assert!(admission(&store, None).is_some());
            // A later hello (an agent of the other project) completes it.
            let later = BTreeSet::from([pair("c", "p2")]);
            assert!(rebuilt(&store, &later, &projects).unwrap());
            assert!(!rebuilt(&store, &later, &projects).unwrap(), "once");
            assert!(admission(&store, Some("c")).is_none());
            assert!(admission(&store, None).is_none());
            assert!(admission(&store, Some("b")).is_some(), "the replayed hold");
        }
        // Both damaged copies are kept beside it.
        assert!(dir.path().join("d/bus/control.json.damaged-1").exists());
        assert!(dir.path().join("d/bus/control.json.damaged-2").exists());
    }

    fn frame(
        seq: u64,
        target_agent: Option<&str>,
        action: ControlAction,
        target_ref: Option<&str>,
    ) -> BusFrameV1 {
        BusFrameV1::Control {
            control_id: format!("ctl_{seq}"),
            target_agent: target_agent.map(str::to_string),
            target_ref: target_ref.map(str::to_string),
            action,
            reason: "Owner review.".into(),
            issued_by: "admin@acme".into(),
            seq,
            server_time: UtcTimestamp::new("2026-10-21T12:00:00Z").unwrap(),
        }
    }

    const I: Option<&str> = Some("inst_1");

    #[test]
    fn hold_and_stop_refuse_until_resume_and_stop_wins() {
        let mut state = ControlState::default();
        assert!(state.apply(I, &frame(1, Some("claude-a"), ControlAction::Hold, None)));
        assert_eq!(
            state.admission(Some("claude-a")).unwrap().entry.action,
            ControlAction::Hold
        );
        assert!(state.admission(Some("claude-b")).is_none());
        assert!(state.admission(None).is_none());
        assert!(state.apply(I, &frame(2, None, ControlAction::Stop, None)));
        let refusal = state.admission(Some("claude-a")).unwrap();
        assert_eq!(refusal.entry.action, ControlAction::Stop, "stop beats hold");
        assert_eq!(refusal.target, ALL_AGENTS);
        assert!(
            state.admission(None).is_some(),
            "unknown callers are bound by *"
        );
        // Resuming one agent leaves the control on every agent in force.
        assert!(state.apply(I, &frame(3, Some("claude-a"), ControlAction::Resume, None)));
        assert_eq!(
            state.admission(Some("claude-a")).unwrap().target,
            ALL_AGENTS
        );
        assert!(state.apply(I, &frame(4, None, ControlAction::Resume, None)));
        assert!(state.admission(Some("claude-a")).is_none());
    }

    #[test]
    fn replayed_or_older_frames_never_undo_a_newer_control() {
        let mut state = ControlState::default();
        assert!(state.apply(I, &frame(10, None, ControlAction::Resume, None)));
        assert!(state.apply(I, &frame(11, None, ControlAction::Hold, None)));
        // Redelivered after reconnect: the earlier resume and the hold itself.
        assert!(!state.apply(I, &frame(10, None, ControlAction::Resume, None)));
        assert!(!state.apply(I, &frame(11, None, ControlAction::Hold, None)));
        assert!(state.admission(Some("x")).is_some());
    }

    #[test]
    fn a_veto_names_one_lease_and_does_not_replace_a_hold() {
        let mut state = ControlState::default();
        state.apply(I, &frame(1, None, ControlAction::Hold, None));
        state.apply(I, &frame(2, None, ControlAction::Veto, Some("lease_7")));
        assert!(
            state.admission(Some("a")).is_some(),
            "hold survives the veto"
        );
        assert!(state.vetoed(Some("a"), "lease_7").is_some());
        assert!(state.vetoed(Some("a"), "lease_8").is_none());
        // A veto without a target_ref refuses nothing.
        state.apply(I, &frame(3, Some("a"), ControlAction::Veto, None));
        assert!(state.vetoed(Some("a"), "").is_none());
        state.apply(I, &frame(4, None, ControlAction::Resume, Some("lease_7")));
        assert!(state.vetoed(Some("a"), "lease_7").is_none());
        assert!(
            state.admission(Some("a")).is_some(),
            "resume of a veto keeps the hold"
        );
    }

    #[test]
    fn active_lists_modes_and_vetoes_for_status() {
        let mut state = ControlState::default();
        state.apply(I, &frame(1, Some("codex-9"), ControlAction::Stop, None));
        state.apply(I, &frame(2, None, ControlAction::Veto, Some("handoff_1")));
        let active = state.active();
        assert_eq!(active.len(), 2);
        assert_eq!(active[0].target_agent, None);
        assert_eq!(active[0].target_ref.as_deref(), Some("handoff_1"));
        assert_eq!(active[1].target_agent.as_deref(), Some("codex-9"));
        assert_eq!(active[1].action, ControlAction::Stop);
        assert_eq!(active[1].since.as_str(), "2026-10-21T12:00:00Z");
    }

    #[test]
    fn controls_belong_to_the_enrollment_and_persist_privately() {
        let dir = tempfile::tempdir().unwrap();
        let store = HiveStore::at(dir.path().join("c"), dir.path().join("d"));
        // Not enrolled: recorded for no instance, and nothing applies.
        assert!(record(&store, &frame(1, None, ControlAction::Stop, None)).unwrap());
        assert!(admission(&store, Some("a")).is_none());
        assert!(active(&store).is_empty());
        assert_eq!(
            status_at(&store, chrono::Utc::now()),
            (Vec::new(), Some(Inactive::NotEnrolled))
        );
        let path = dir.path().join("d/bus/control.json");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        // The flat pre-v1 shape (latest frame per target) is no state of
        // this build: moved aside, never read as "no controls" (RB8 M12).
        std::fs::write(&path, br#"{"*":{"t":"control","action":"stop"}}"#).unwrap();
        assert!(ControlState::load(&dir.path().join("d")).is_err());
        assert!(dir.path().join("d/bus/control.json.damaged-1").exists());
        // A different instance's state resets on the next frame.
        let mut state = ControlState::default();
        state.apply(Some("inst_old"), &frame(5, None, ControlAction::Stop, None));
        state.apply(
            Some("inst_new"),
            &frame(1, Some("a"), ControlAction::Hold, None),
        );
        assert_eq!(state.instance_id.as_deref(), Some("inst_new"));
        assert_eq!(
            state.admission(Some("a")).unwrap().entry.action,
            ControlAction::Hold
        );
        assert!(state.admission(Some("b")).is_none());
    }

    /// Local work is never gated: a stop on every agent and a damaged
    /// control file pause Hive tools only, never a local mutating tool.
    #[test]
    fn controls_and_a_damaged_file_pause_hive_tools_only() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let store = HiveStore::open().unwrap();
        enroll_for_tests(&store, 1);
        assert!(record(&store, &frame(1, None, ControlAction::Stop, None)).unwrap());
        for tool in [
            "ctx_edit",
            "ctx_shell",
            "ctx_patch",
            "ctx_agent",
            "ctx_read",
        ] {
            assert_eq!(tool_refusal(Some("a"), tool), None, "{tool}");
        }
        assert!(tool_refusal(Some("a"), "ctx_tasks").is_some());
        let path = store.data_dir().join("bus/control.json");
        std::fs::write(&path, b"{ not json").unwrap();
        assert_eq!(tool_refusal(None, "ctx_edit"), None, "offline, damaged");
        let waiting = tool_refusal(None, "ctx_tasks").expect("Hive traffic waits");
        assert!(waiting.contains("local tools keep working"), "{waiting}");
    }

    #[test]
    fn the_refusal_message_names_the_issuer_and_reason_but_no_ids() {
        let mut state = ControlState::default();
        state.apply(I, &frame(1, None, ControlAction::Stop, None));
        let message = state.admission(Some("a")).unwrap().message();
        assert!(message.contains("stopped"), "{message}");
        assert!(message.contains("admin@acme") && message.contains("Owner review."));
        assert!(!message.contains("ctl_1"));
    }
}
