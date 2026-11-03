// SPDX-License-Identifier: Apache-2.0
//! Bus controls from Hive (`hold`, `stop`, `veto`, `resume`) and the
//! admission answers derived from them.
//!
//! The relay keeps the controls whose signature verifies against a Hive
//! key of purpose `control` in `state_v2.json` ([`StateV2`]); tool
//! admission, the lease and handoff paths and `hive status` only read
//! them. Rules:
//!
//! - `hold` and `stop` pause an agent: a worker control the agents
//!   attached to that worker, a bus control every agent here (a tool call
//!   names no vault). `stop` wins over `hold`; `resume` ends both.
//! - `veto` refuses the lease or handoff its `ref` names ([`super::veto`]).
//! - A control only pauses Hive traffic: Hive tools (`ctx_tasks`), bus
//!   messages, the Hive side of a lease and a handoff through Hive. Local
//!   tools, local leases and handoffs on this machine are never refused
//!   (connector rule "local work is never gated"), online or offline.
//! - Controls apply only while this machine is enrolled and its lease is
//!   valid (plan C rule 2: local work never depends on Hive state);
//!   `hive status` names the reason when they do not.
//!
//! Bus controls are org coordination between cooperating agents, not a
//! security boundary: whoever controls this machine can edit these files.
//! Enforcement that must hold belongs to the runtime/policy path (org policy,
//! role and policy guards). That is also why the lapse check reads the
//! stored lease without re-verifying its signature: it can only switch
//! controls off, never on.
//!
//! A damaged `state_v2.json`, or one of a version this build does not know,
//! is never read as "no controls" (RB8 M12): until Hive's replay of the
//! controls in force after the relay's next hello is complete, Hive traffic waits under
//! a "waiting for Hive" hold; local work goes on.
//!
//! Network-free and deterministic.

use super::relay_v2::StateV2;
use super::v2::{Control, ControlAction, ControlTarget};
use crate::state::HiveStore;
use lean_ctx_protocol::UtcTimestamp;
use lean_ctx_protocol::hive::lease::LeaseState;

/// Target of a control on every agent of this machine.
pub const ALL_AGENTS: &str = "*";

/// Why a call is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// `*` or the agent id the control holds.
    pub target: String,
    pub action: ControlAction,
    pub reason: String,
    pub issued_by: String,
}

impl Refusal {
    /// One line for the agent, without internal ids.
    pub fn message(&self) -> String {
        let who = if self.target == ALL_AGENTS {
            "every agent on this machine".to_string()
        } else {
            format!("agent {}", self.target)
        };
        let what = match self.action {
            ControlAction::Stop => "stopped: end the current task",
            ControlAction::Hold => "on hold until resumed",
            ControlAction::Veto => "vetoed",
            ControlAction::Resume => "resumed",
        };
        format!(
            "Hive control: {who} is {what} (by {}: {}). Hive traffic waits; local tools keep working.",
            self.issued_by, self.reason
        )
    }
}

/// A control that is in force, for `hive status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveControl {
    /// The worker it holds (`wkr_…`); `None`: every agent of this machine.
    pub target_agent: Option<String>,
    pub action: ControlAction,
    pub target_ref: Option<String>,
    pub reason: String,
    pub issued_by: String,
    pub since: UtcTimestamp,
}

/// Why the controls kept here do not apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Inactive {
    /// This machine is not enrolled (disconnected).
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
        .read_config::<crate::state::LeaseRecord>(crate::state::LEASE_FILE)
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

/// What the refusals of a pending rebuild say (M12).
const REBUILD_REASON: &str =
    "the local control state was damaged; waiting for Hive to resend the controls";
/// Who a pending rebuild's hold is from.
const REBUILD_ISSUER: &str = "lean-ctx";

/// The controls kept for this machine (a `resume` is never kept, oldest
/// first) and, while `state_v2.json` was found damaged or of an unknown
/// version and Hive has not answered a hello since, since when (M12).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Kept {
    pub controls: Vec<Control>,
    pub rebuild_since: Option<u64>,
}

impl Kept {
    fn is_empty(&self) -> bool {
        self.controls.is_empty() && self.rebuild_since.is_none()
    }

    /// The hold of a pending rebuild, for every agent.
    fn pending(&self) -> Option<Refusal> {
        self.rebuild_since.map(|_| Refusal {
            target: ALL_AGENTS.into(),
            action: ControlAction::Hold,
            reason: REBUILD_REASON.into(),
            issued_by: REBUILD_ISSUER.into(),
        })
    }
}

/// The controls kept for this machine and whether they apply at
/// `local_now`. Empty and active when there are none. A damaged or unknown
/// file is read, without changing it, as a pending rebuild: never as "no
/// controls".
pub fn current_at(
    store: &HiveStore,
    local_now: chrono::DateTime<chrono::Utc>,
) -> (Kept, Option<Inactive>) {
    let state = StateV2::read(store.data_dir()).unwrap_or_else(|since| StateV2 {
        rebuild_pending: Some(since),
        ..StateV2::default()
    });
    let mut controls: Vec<Control> = state
        .controls
        .into_values()
        .filter(|c| c.action != ControlAction::Resume)
        .collect();
    controls.sort_by_key(|c| c.created_at);
    let kept = Kept {
        controls,
        rebuild_since: state.rebuild_pending,
    };
    if kept.is_empty() {
        return (kept, None);
    }
    if !matches!(store.instance(), Ok(Some(_))) {
        return (kept, Some(Inactive::NotEnrolled));
    }
    if lease_lapsed(store, local_now) {
        return (kept, Some(Inactive::LeaseLapsed));
    }
    (kept, None)
}

/// What is in force at `local_now`; nothing when it does not apply.
fn in_force(store: &HiveStore, local_now: chrono::DateTime<chrono::Utc>) -> Kept {
    match current_at(store, local_now) {
        (kept, None) => kept,
        (_, Some(_)) => Kept::default(),
    }
}

/// Admission for a mutating tool of `agent_id` on this machine: a hold or
/// stop on its worker or on a whole vault bus, or the hold of a pending
/// rebuild. The tool call names no vault, so a bus control of any vault
/// linked here holds the Hive traffic of every agent; a stop wins over a
/// hold.
pub fn admission(store: &HiveStore, agent_id: Option<&str>) -> Option<Refusal> {
    admission_at(store, agent_id, chrono::Utc::now())
}

fn admission_at(
    store: &HiveStore,
    agent_id: Option<&str>,
    local_now: chrono::DateTime<chrono::Utc>,
) -> Option<Refusal> {
    let kept = in_force(store, local_now);
    let controlled = held(store, agent_id, &kept.controls);
    if controlled
        .as_ref()
        .is_some_and(|r| r.action == ControlAction::Stop)
    {
        return controlled;
    }
    kept.pending().or(controlled)
}

/// The hold or stop of `controls` on `agent_id`'s worker or on a bus.
pub fn held(store: &HiveStore, agent_id: Option<&str>, controls: &[Control]) -> Option<Refusal> {
    let holds = || {
        controls
            .iter()
            .filter(|c| matches!(c.action, ControlAction::Hold | ControlAction::Stop))
    };
    // The worker is looked up only when a control names one.
    let worker = holds()
        .any(|c| c.target == ControlTarget::Worker)
        .then(|| agent_id.and_then(|a| super::messages_v2::known_worker_of(store, a)))
        .flatten();
    let control = holds()
        .filter(|c| match c.target {
            ControlTarget::Bus => true,
            ControlTarget::Worker => worker.is_some() && c.worker_id == worker,
        })
        .max_by_key(|c| (c.action == ControlAction::Stop, c.created_at))?;
    let target = match control.target {
        ControlTarget::Bus => ALL_AGENTS.to_string(),
        ControlTarget::Worker => agent_id.unwrap_or(ALL_AGENTS).to_string(),
    };
    Some(Refusal {
        target,
        action: control.action,
        reason: control.reason.clone(),
        issued_by: issuer(control),
    })
}

/// Who issued `control`, as its signed label says, else their user id.
fn issuer(control: &Control) -> String {
    control
        .issued_by
        .label
        .clone()
        .unwrap_or_else(|| control.issued_by.user_id.clone())
}

fn stamp_ms(ms: u64) -> UtcTimestamp {
    let at = i64::try_from(ms)
        .ok()
        .and_then(chrono::DateTime::from_timestamp_millis)
        .unwrap_or_else(chrono::Utc::now);
    UtcTimestamp::new(at.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .expect("a formatted UTC time is a valid timestamp")
}

/// For `hive status`: a pending rebuild's hold first, then the controls in
/// force, oldest first (a worker control names its worker, a bus control
/// every agent), or why the ones kept here do not apply (`None` when there
/// is nothing to say).
pub fn status_at(
    store: &HiveStore,
    local_now: chrono::DateTime<chrono::Utc>,
) -> (Vec<ActiveControl>, Option<Inactive>) {
    let (kept, inactive) = current_at(store, local_now);
    if inactive.is_some() {
        return (Vec::new(), inactive);
    }
    let rebuild = kept.rebuild_since.map(|since| ActiveControl {
        target_agent: None,
        action: ControlAction::Hold,
        target_ref: None,
        reason: REBUILD_REASON.into(),
        issued_by: REBUILD_ISSUER.into(),
        since: stamp_ms(since),
    });
    let active = rebuild
        .into_iter()
        .chain(kept.controls.iter().map(|c| ActiveControl {
            target_agent: match c.target {
                ControlTarget::Bus => None,
                ControlTarget::Worker => c.worker_id.clone(),
            },
            action: c.action,
            target_ref: c.target_ref.as_ref().map(|r| r.id.clone()),
            reason: c.reason.clone(),
            issued_by: issuer(c),
            since: stamp_ms(c.created_at),
        }))
        .collect();
    (active, None)
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

/// `hive status --json`: the controls in force, or why the ones kept here
/// do not apply.
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
/// (`ctx_agent`) pauses only its Hive side ([`veto_refusal`], `messages_v2`).
fn hive_traffic(tool: &str) -> bool {
    crate::HIVE_TOOLS.contains(&tool)
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
/// handoff through Hive: `Some` when a verified veto names any of `refs`
/// (lease id, resource ref, handoff id; V9.5). The caller then skips the
/// Hive request; the local lease or handoff goes ahead.
pub fn veto_refusal(refs: &[&str]) -> Option<String> {
    let store = HiveStore::open().ok()?;
    // A pending rebuild may have lost a veto: the Hive side waits too.
    if let Some(pending) = in_force(&store, chrono::Utc::now()).pending() {
        return Some(pending.message());
    }
    refs.iter()
        .find_map(|reference| super::veto::vetoed_v2(&store, reference))
}

/// Test enrollment: the protocol fixture's instance and lease, with the
/// stored clock offset placing "now" `hours_after_issue` after it was issued
/// (24 h: valid until; 168 h: grace until; later: lapsed).
#[cfg(any(test, feature = "test-support"))]
#[allow(clippy::unwrap_used)]
pub fn enroll_for_tests(store: &HiveStore, hours_after_issue: i64) -> String {
    use crate::state::{INSTANCE_FILE, InstanceRecord, LEASE_FILE, LeaseRecord};
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

/// Keep `controls` as the relay does after verifying them (tests).
#[cfg(any(test, feature = "test-support"))]
#[allow(clippy::unwrap_used)]
pub fn keep_for_tests(store: &HiveStore, controls: Vec<Control>) {
    let mut state = StateV2::load(store.data_dir());
    state.controls.clear();
    for control in controls {
        state.controls.insert(control.id.clone(), control);
    }
    state.save(store.data_dir()).unwrap();
}

/// The C0 hold fixture as a `target` (`bus` or `worker`) `action` control
/// with `id` (tests; kept unverified, as admission reads it).
#[cfg(any(test, feature = "test-support"))]
#[allow(clippy::unwrap_used)]
pub fn control_for_tests(target: &str, action: &str, id: &str) -> Control {
    let path = format!(
        "{}/crates/lean-ctx-protocol/tests/fixtures/hive-v2/bus/control.hold.json",
        concat!(env!("CARGO_MANIFEST_DIR"), "/../..")
    );
    let mut control: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    control["target"] = target.into();
    control["action"] = action.into();
    control["id"] = id.into();
    if target == "bus" {
        control.as_object_mut().unwrap().remove("worker_id");
    }
    serde_json::from_value(control).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, HiveStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = HiveStore::at(dir.path().join("c"), dir.path().join("d"));
        (dir, store)
    }

    #[test]
    fn a_lapsed_lease_voids_controls_and_status_says_so() {
        let (dir, store) = store();
        enroll_for_tests(&store, 1);
        keep_for_tests(&store, vec![control_for_tests("bus", "stop", "ctl_1")]);
        assert_eq!(
            admission(&store, Some("a")).unwrap().action,
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
    fn controls_apply_only_while_enrolled() {
        let (_dir, store) = store();
        assert_eq!(status_at(&store, chrono::Utc::now()), (Vec::new(), None));
        keep_for_tests(&store, vec![control_for_tests("bus", "hold", "ctl_1")]);
        assert!(admission(&store, Some("a")).is_none(), "not enrolled");
        assert_eq!(
            status_at(&store, chrono::Utc::now()),
            (Vec::new(), Some(Inactive::NotEnrolled))
        );
        enroll_for_tests(&store, 1);
        let refusal = admission(&store, Some("a")).expect("bus hold");
        assert_eq!(refusal.target, ALL_AGENTS);
        assert_eq!(refusal.action, ControlAction::Hold);
        assert!(admission(&store, None).is_some(), "an unknown caller too");
        let (active_now, inactive) = status_at(&store, chrono::Utc::now());
        assert_eq!((active_now.len(), inactive), (1, None));
        assert_eq!(active_now[0].target_agent, None);
    }

    #[test]
    fn a_stop_wins_over_a_hold() {
        let (_dir, store) = store();
        enroll_for_tests(&store, 1);
        keep_for_tests(
            &store,
            vec![
                control_for_tests("bus", "hold", "ctl_1"),
                control_for_tests("bus", "stop", "ctl_2"),
            ],
        );
        assert_eq!(
            admission(&store, Some("a")).unwrap().action,
            ControlAction::Stop
        );
        assert_eq!(status_at(&store, chrono::Utc::now()).0.len(), 2);
    }

    #[test]
    fn a_worker_hold_spares_agents_of_other_workers() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        let (_dir, store) = store();
        enroll_for_tests(&store, 1);
        keep_for_tests(&store, vec![control_for_tests("worker", "hold", "ctl_1")]);
        assert!(admission(&store, None).is_none());
        assert!(admission(&store, Some("unregistered")).is_none());
        let (active_now, _) = status_at(&store, chrono::Utc::now());
        assert_eq!(
            active_now[0].target_agent.as_deref(),
            Some("wkr_gawyravj36igdp3irdfo")
        );
    }

    /// Local work is never gated: a stop on every agent pauses Hive tools
    /// only, never a local mutating tool.
    #[test]
    fn controls_pause_hive_tools_only() {
        let _iso = lean_ctx::core::data_dir::isolated_data_dir();
        let store = HiveStore::open().unwrap();
        enroll_for_tests(&store, 1);
        keep_for_tests(&store, vec![control_for_tests("bus", "stop", "ctl_1")]);
        for tool in [
            "ctx_edit",
            "ctx_shell",
            "ctx_patch",
            "ctx_agent",
            "ctx_read",
        ] {
            assert_eq!(tool_refusal(Some("a"), tool), None, "{tool}");
        }
        let refused = tool_refusal(Some("a"), "ctx_tasks").expect("Hive traffic waits");
        assert!(refused.contains("local tools keep working"), "{refused}");
    }

    /// RB8 M12: a damaged or unknown `state_v2.json` holds Hive traffic
    /// (never "no controls") while local tools go on; `hive status` names
    /// it; the relay's load keeps the hold, and Hive's complete replay ends it
    /// (`relay_v2::note_replay`).
    #[test]
    fn a_damaged_control_state_holds_hive_traffic_until_hive_answers_a_hello() {
        let _iso = lean_ctx::core::data_dir::isolated_data_dir();
        let store = HiveStore::open().unwrap();
        enroll_for_tests(&store, 1);
        let path = store.data_dir().join("bus/state_v2.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        for broken in [&b"{ not json"[..], br#"{"version": 7, "controls": {}}"#] {
            std::fs::write(&path, broken).unwrap();
            let waiting = tool_refusal(Some("a"), "ctx_tasks").expect("fails closed");
            assert!(waiting.contains(REBUILD_REASON), "{waiting}");
            assert!(waiting.contains("local tools keep working"), "{waiting}");
            assert!(
                tool_refusal(None, "ctx_tasks").is_some(),
                "unknown callers too"
            );
            for tool in ["ctx_edit", "ctx_shell", "ctx_agent", "ctx_read"] {
                assert_eq!(tool_refusal(Some("a"), tool), None, "{tool}");
            }
            assert!(
                veto_refusal(&["bl_7"]).is_some(),
                "the Hive side of a lease"
            );
            let (shown, inactive) = status_at(&store, chrono::Utc::now());
            assert_eq!((shown.len(), inactive), (1, None), "status names it");
            assert_eq!(shown[0].reason, REBUILD_REASON);
            assert_eq!(
                std::fs::read(&path).unwrap(),
                broken,
                "a reader changes nothing"
            );

            // The relay's load moves the file aside and keeps the hold.
            let mut state = StateV2::load(store.data_dir());
            assert!(state.rebuild_pending.is_some());
            assert!(
                tool_refusal(Some("a"), "ctx_tasks").is_some(),
                "still waiting"
            );
            // Hive's replay after the next hello is complete: the hold ends.
            state.rebuild_pending = None;
            state.save(store.data_dir()).unwrap();
            assert_eq!(tool_refusal(Some("a"), "ctx_tasks"), None);
            assert_eq!(veto_refusal(&["bl_7"]), None);
            assert!(status_at(&store, chrono::Utc::now()).0.is_empty());
        }
        assert!(
            store
                .data_dir()
                .join("bus/state_v2.json.damaged-1")
                .exists()
        );
        assert!(
            store
                .data_dir()
                .join("bus/state_v2.json.damaged-2")
                .exists()
        );
    }

    /// A stop Hive replayed already wins over the hold of a pending rebuild.
    #[test]
    fn a_stop_wins_over_a_pending_rebuild() {
        let (_dir, store) = store();
        enroll_for_tests(&store, 1);
        let mut state = StateV2 {
            rebuild_pending: Some(1),
            ..StateV2::default()
        };
        let stop = control_for_tests("bus", "stop", "ctl_1");
        state.controls.insert(stop.id.clone(), stop);
        state.save(store.data_dir()).unwrap();
        assert_eq!(
            admission(&store, Some("a")).unwrap().action,
            ControlAction::Stop
        );
        let (_, inactive) = status_at(&store, chrono::Utc::now());
        assert_eq!(inactive, None);
        assert_eq!(status_at(&store, chrono::Utc::now()).0.len(), 2);
    }

    #[test]
    fn the_refusal_message_names_the_issuer_and_reason_but_no_ids() {
        let (_dir, store) = store();
        enroll_for_tests(&store, 1);
        keep_for_tests(&store, vec![control_for_tests("bus", "stop", "ctl_1")]);
        let message = admission(&store, Some("a")).unwrap().message();
        assert!(message.contains("stopped"), "{message}");
        assert!(
            message.contains("by yves: Pausing the reviewer"),
            "{message}"
        );
        assert!(!message.contains("ctl_1"));
    }
}
