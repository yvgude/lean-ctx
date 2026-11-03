// SPDX-License-Identifier: Apache-2.0
//! The bus v2 relay's decisions (C0 2.0 section 3; SPEC-ENGINE §3.7):
//! which vault buses this machine subscribes, the hello with its presence,
//! which local posts become `place.publish` frames, how deliveries land and
//! which Hive-signed controls hold a worker.
//!
//! - Only a post addressed `#<ws>/<vault>` to a vault linked here leaves
//!   (D6); its type is the post's category when that is a bus type, a
//!   plain status line is a `progress` step.
//! - Deliveries are written into the local bus with the idempotency key
//!   `hive:<address>:<seq>` and are never forwarded again (no bridging).
//! - A control counts only when its signature verifies against a Hive
//!   key of purpose `control`; others are ignored and counted.
//!
//! Network-free and deterministic; the socket loop (`ws`) moves the frames.

// The socket half is used by the relay loop, built with `http-server`.
#![cfg_attr(not(feature = "http-server"), allow(dead_code))]

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::address::Address;
use super::v2::{
    BUS_SCHEMA, Control, ControlAction, ControlKeys, ControlRejection, ControlTarget, MessageType,
    PresenceEntry, PresenceState, Publish, ulid_for,
};
use crate::HiveError;
use crate::knowledge::screening::Screen;
use crate::state::{load_or_quarantine, write_private};
use crate::vault::{VaultMap, VaultRecord};
use lean_ctx::core::a2a::message::{MessagePriority, PrivacyLevel};
use lean_ctx::core::agents::{AgentEntry, AgentRegistry};

const STATE_FILE: &str = "state_v2.json";
/// Vault buses one hello subscribes at most.
pub const MAX_SUBSCRIBE: usize = 64;
/// Remembered decided local message ids (oldest drop first).
const MAX_DECIDED: usize = 5000;
/// Idempotency-key prefix of messages that arrived through Hive.
pub(crate) use lean_ctx::core::agents::HIVE_INBOUND_KEY_PREFIX as INBOUND_KEY_PREFIX;
/// A `progress` step is at most this long.
const STEP_MAX: usize = 300;

/// True for a local bus entry another machine's worker delivered through
/// Hive: it is shown fenced as foreign data and never published again.
pub fn is_inbound(entry: &lean_ctx::core::agents::ScratchpadEntry) -> bool {
    entry.is_hive_inbound()
}

/// Cursors, decided posts, in-flight publishes and verified controls, in
/// `<data_dir>/hive/bus/state_v2.json`. Only the relay holding the relay
/// lock writes it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateV2 {
    /// Last `seq` held per subscribed address (`#ws/vault`).
    #[serde(default)]
    pub cursors: BTreeMap<String, u64>,
    /// Local message ids already published or deliberately kept local.
    #[serde(default)]
    pub decided: VecDeque<String>,
    /// `client_msg_id` → local message id, until `message.stored`.
    #[serde(default)]
    pub in_flight: BTreeMap<String, String>,
    /// Posts screening or the contract kept on this machine.
    #[serde(default)]
    pub withheld: u64,
    /// Verified controls in force, by control id.
    #[serde(default)]
    pub controls: BTreeMap<String, Control>,
    /// Controls ignored because their signature or key failed.
    #[serde(default)]
    pub sig_invalid: u64,
    /// Places Hive denied (`place_ref` → code); the relay stops posting there.
    #[serde(default)]
    pub denied: BTreeMap<String, String>,
    /// The last task change announced per vault task (C0 2.2 §10.1 catch-up).
    /// Lost with a damaged file: the next catch-up announces the open tasks
    /// again, which repeats a notice but never loses one.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tasks: super::task_events::TasksSeen,
    /// The shape of this file: absent (0) in this build's; a file that
    /// names a version is newer and is not read.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub version: u32,
    /// Since when (unix ms) the file was found damaged or of an unknown
    /// version (RB8 M12): its controls are lost, so Hive traffic waits under
    /// a hold until Hive's replay after the next hello is complete
    /// (`note_replay`). Local work goes on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rebuild_pending: Option<u64>,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

impl StateV2 {
    fn path(data_dir: &Path) -> std::path::PathBuf {
        data_dir.join("bus").join(STATE_FILE)
    }

    /// The relay's state. A damaged file, or one of a version this build
    /// does not know, is moved aside and never read as "no controls"
    /// (M12): the state starts over with a pending rebuild, saved at once.
    pub fn load(data_dir: &Path) -> Self {
        match load_or_quarantine(&Self::path(data_dir), |s: &Self| s.version == 0) {
            Ok(state) => state.unwrap_or_default(),
            Err(error) => {
                tracing::warn!("hive bus v2 state: {error}");
                let state = Self {
                    rebuild_pending: Some(now_ms()),
                    ..Self::default()
                };
                if let Err(error) = state.save(data_dir) {
                    tracing::warn!("hive bus v2 state: {error}");
                }
                state
            }
        }
    }

    /// The state as the tool side reads it, changing nothing: missing is
    /// empty; damaged, of an unknown version or unreadable is `Err` with
    /// since when (unix ms), which the caller treats as a pending rebuild.
    pub fn read(data_dir: &Path) -> Result<Self, u64> {
        let path = Self::path(data_dir);
        let changed = || {
            std::fs::metadata(&path)
                .and_then(|m| m.modified())
                .ok()
                .map(|t| chrono::DateTime::<chrono::Utc>::from(t).timestamp_millis())
                .and_then(|ms| u64::try_from(ms).ok())
                .unwrap_or_else(now_ms)
        };
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<Self>(&bytes)
                .ok()
                .filter(|s| s.version == 0)
                .ok_or_else(changed),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(_) => Err(changed()),
        }
    }

    pub fn save(&self, data_dir: &Path) -> Result<(), HiveError> {
        let bytes = serde_json::to_vec(self).map_err(|e| HiveError::Local(e.to_string()))?;
        write_private(&data_dir.join("bus"), STATE_FILE, &bytes)
    }

    fn decide(&mut self, entry_id: &str) {
        if !self.decided.iter().any(|id| id == entry_id) {
            self.decided.push_back(entry_id.to_string());
            while self.decided.len() > MAX_DECIDED {
                self.decided.pop_front();
            }
        }
    }

    fn was_decided(&self, entry_id: &str) -> bool {
        self.decided.iter().any(|id| id == entry_id)
    }

    /// `message.stored`: the post is out.
    pub fn stored(&mut self, client_msg_id: &str) {
        if let Some(entry) = self.in_flight.remove(client_msg_id) {
            self.decide(&entry);
        }
    }

    /// A refused publish (`error` with its `client_msg_id`) is not retried.
    pub fn refused(&mut self, client_msg_id: &str) {
        if let Some(entry) = self.in_flight.remove(client_msg_id) {
            self.decide(&entry);
            self.withheld += 1;
        }
    }

    /// Apply a `control` frame; `false` when it was ignored.
    pub fn apply_control(&mut self, lifted: bool, control: Control, keys: &ControlKeys) -> bool {
        match control.verify(lifted, keys) {
            Ok(()) => {}
            Err(ControlRejection::SigInvalid) => {
                self.sig_invalid += 1;
                return false;
            }
            Err(ControlRejection::Invalid) => return false,
        }
        if lifted || control.action == ControlAction::Resume {
            self.controls.remove(&control.id);
            // A resume ends the holds and stops it answers.
            if control.action == ControlAction::Resume {
                self.controls.retain(|_, c| {
                    !(c.place == control.place
                        && c.target == control.target
                        && c.worker_id == control.worker_id
                        && matches!(c.action, ControlAction::Hold | ControlAction::Stop))
                });
            }
        } else {
            self.controls.insert(control.id.clone(), control);
        }
        true
    }

    /// The hold or stop in force for `worker_id` (or its whole bus) in
    /// `place_ref`; a stop wins over a hold.
    pub fn held(&self, place_ref: Option<&str>, worker_id: &str) -> Option<&Control> {
        self.controls
            .values()
            .filter(|c| matches!(c.action, ControlAction::Hold | ControlAction::Stop))
            .filter(|c| place_ref.is_none_or(|p| c.place == p))
            .filter(|c| match c.target {
                ControlTarget::Bus => true,
                ControlTarget::Worker => c.worker_id.as_deref() == Some(worker_id),
            })
            .max_by_key(|c| (c.action == ControlAction::Stop, c.created_at))
    }

    /// The veto on `ref_id` (a claim, handoff or lease), if any.
    pub fn vetoed(&self, ref_id: &str) -> Option<&Control> {
        self.controls.values().find(|c| {
            c.action == ControlAction::Veto && c.target_ref.as_ref().is_some_and(|r| r.id == ref_id)
        })
    }
}

fn now_ms() -> u64 {
    u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap_or_default()
}

/// Where Hive's replay after this session's hello stands (M12). Hive sends
/// `hello.ack` first, then the control frames of every subscribed place,
/// then presence and messages: the replay is complete at the first frame
/// after the ack that is no `control` frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Replay {
    /// No `hello.ack` yet.
    #[default]
    AwaitingAck,
    /// Acked; control frames of the replay may still come.
    Running,
    /// The replay is complete.
    Done,
}

/// Follow the replay with `frame`; a pending rebuild ends once it is
/// complete. A hello that subscribed no place gets no control, so its ack
/// completes it. A session that ends earlier leaves the hold in place.
pub(crate) fn note_replay(
    state: &mut StateV2,
    replay: &mut Replay,
    frame: &super::v2::Inbound,
    subscribed_any: bool,
) {
    use super::v2::Inbound;
    *replay = match (*replay, frame) {
        (Replay::AwaitingAck, Inbound::HelloAck(_)) if subscribed_any => Replay::Running,
        // The ack of a hello without places, or the first frame after the
        // ack that is no control: the replay is complete.
        (Replay::AwaitingAck, Inbound::HelloAck(_)) | (Replay::Running, _)
            if !matches!(frame, Inbound::Control { .. }) =>
        {
            Replay::Done
        }
        (other, _) => other,
    };
    if *replay == Replay::Done {
        state.rebuild_pending = None;
    }
}

/// Publishes built in the engine rather than posted by an agent (e.g. the
/// hourly collaboration findings, V16.1): `<data>/hive/bus/publish_queue.json`,
/// written by any process under its lock, drained by the relay.
const QUEUE_FILE: &str = "publish_queue.json";
/// Queued publishes at most; a queue beyond it refuses new ones.
const MAX_QUEUE: usize = 1000;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct PublishQueue {
    items: Vec<Publish>,
}

fn read_queue(data_dir: &Path) -> PublishQueue {
    load_or_quarantine(
        &data_dir.join("bus").join(QUEUE_FILE),
        |_: &PublishQueue| true,
    )
    .ok()
    .flatten()
    .unwrap_or_default()
}

fn change_queue<T>(
    data_dir: &Path,
    change: impl FnOnce(&mut PublishQueue) -> T,
) -> Result<T, HiveError> {
    let dir = data_dir.join("bus");
    let _lock = crate::state::lock_state(&dir, QUEUE_FILE)?;
    let mut queue = read_queue(data_dir);
    let out = change(&mut queue);
    let bytes = serde_json::to_vec(&queue).map_err(|e| HiveError::Local(e.to_string()))?;
    write_private(&dir, QUEUE_FILE, &bytes)?;
    Ok(out)
}

/// Queue a publish for the relay. It is validated first; it survives a
/// relay restart, waits while offline and goes once Hive stored it. A
/// permanent refusal drops it (counted, logged).
pub fn queue_publish(store: &crate::HiveStore, publish: Publish) -> Result<(), HiveError> {
    publish
        .validate()
        .map_err(|e| HiveError::Config(e.to_string()))?;
    change_queue(store.data_dir(), |queue| {
        if queue
            .items
            .iter()
            .any(|p| p.client_msg_id == publish.client_msg_id)
        {
            return Ok(());
        }
        if queue.items.len() >= MAX_QUEUE {
            return Err(HiveError::Local(format!(
                "{MAX_QUEUE} bus messages are still waiting for Hive"
            )));
        }
        queue.items.push(publish);
        Ok(())
    })?
}

/// The `place.publish` frames of queued publishes not yet in flight; a
/// held worker's wait.
pub(crate) fn queued_frames(data_dir: &Path, state: &mut StateV2) -> Vec<Value> {
    let mut out = Vec::new();
    for publish in read_queue(data_dir).items {
        if state.in_flight.contains_key(&publish.client_msg_id)
            || state.held(None, &publish.worker_id).is_some()
        {
            continue;
        }
        let Ok(Value::Object(mut frame)) = serde_json::to_value(&publish) else {
            continue;
        };
        frame.insert("t".into(), "place.publish".into());
        state.in_flight.insert(
            publish.client_msg_id.clone(),
            format!("queue:{}", publish.client_msg_id),
        );
        out.push(Value::Object(frame));
    }
    out
}

/// Drop a queued publish Hive stored or refused for good.
pub(crate) fn settle_queued(data_dir: &Path, client_msg_id: &str) -> Result<bool, HiveError> {
    change_queue(data_dir, |queue| {
        let before = queue.items.len();
        queue.items.retain(|p| p.client_msg_id != client_msg_id);
        before != queue.items.len()
    })
}

/// Which bus protocol the enrolled Hive speaks, as the instance card
/// found out (E44 exists only on a C0 2.0 Hive): `<config>/bus_protocol.json`.
const PROTOCOL_FILE: &str = "bus_protocol.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct ProtocolRecord {
    bus: u8,
    /// The minor of the C0 2.x protocol Hive answered (`2.2` → 2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    minor: Option<u16>,
}

/// Record that the enrolled Hive speaks bus v2 (`true`) or only 1.x.
pub fn record_protocol(store: &crate::HiveStore, v2: bool) -> Result<(), HiveError> {
    record_protocol_minor(store, v2, None)
}

/// [`record_protocol`] with the 2.x minor Hive answered.
pub fn record_protocol_minor(
    store: &crate::HiveStore,
    v2: bool,
    minor: Option<u16>,
) -> Result<(), HiveError> {
    store.write_config(
        PROTOCOL_FILE,
        &ProtocolRecord {
            bus: if v2 { 2 } else { 1 },
            minor: minor.filter(|_| v2),
        },
    )
}

/// The C0 2.x minor the enrolled Hive answered, as last recorded.
pub fn protocol_minor(store: &crate::HiveStore) -> Option<u16> {
    store
        .read_config::<ProtocolRecord>(PROTOCOL_FILE)
        .ok()
        .flatten()
        .filter(|r| r.bus == 2)
        .and_then(|r| r.minor)
}

/// True when the enrolled Hive answered C0 2.2 or newer: doc nodes and
/// decision anchors may be written.
pub fn speaks_2_2(store: &crate::HiveStore) -> bool {
    protocol_minor(store).is_some_and(|minor| minor >= 2)
}

/// True when the relay should open a C0 2.0 session.
pub fn speaks_v2(store: &crate::HiveStore) -> bool {
    store
        .read_config::<ProtocolRecord>(PROTOCOL_FILE)
        .ok()
        .flatten()
        .is_some_and(|r| r.bus == 2)
}

/// `#<ns>/<name>` of a linked vault.
pub fn address_of(record: &VaultRecord) -> String {
    format!("#{}/{}", record.ns, record.name)
}

fn record_for<'a>(map: &'a VaultMap, root: &str) -> Option<&'a VaultRecord> {
    map.vaults.values().find(|record| {
        record
            .roots
            .iter()
            .any(|known| lean_ctx::core::pathutil::same_project(known, root))
    })
}

/// The vault buses to subscribe: linked vaults that have an active local
/// session in one of their roots (SPEC-ENGINE §3.7.3: presence only where
/// a session is bound), sorted, at most [`MAX_SUBSCRIBE`].
pub(crate) fn subscriptions(map: &VaultMap, agents: &[&AgentEntry]) -> Vec<String> {
    let set: BTreeSet<String> = agents
        .iter()
        .filter_map(|agent| record_for(map, &agent.project_root))
        .map(address_of)
        // Only slugs that fit the bus address grammar.
        .filter(|address| Address::parse(Some(address)).is_ok_and(|a| a.is_remote()))
        .collect();
    set.into_iter().take(MAX_SUBSCRIBE).collect()
}

/// The C0 2.0 hello: subscriptions, their cursors, and the presence of the
/// local workers (`held` while a control holds them).
pub fn hello(subscribe: &[String], state: &StateV2, presence: &[PresenceEntry]) -> Value {
    let cursors: BTreeMap<&String, u64> = subscribe
        .iter()
        .filter_map(|a| state.cursors.get(a).map(|seq| (a, *seq)))
        .collect();
    json!({
        "t": "hello",
        "protocol": "2.0",
        "subscribe": subscribe,
        "cursors": cursors,
        "presence": presence,
    })
}

/// Presence of one local worker: held under a control, active while one
/// of its sessions is active, else idle.
pub fn presence_state(state: &StateV2, worker_id: &str, any_active: bool) -> PresenceState {
    if state.held(None, worker_id).is_some() {
        PresenceState::Held
    } else if any_active {
        PresenceState::Active
    } else {
        PresenceState::Idle
    }
}

/// A publish built from a local post, or why it stays local.
fn publish_for(
    entry: &lean_ctx::core::agents::ScratchpadEntry,
    to: &str,
    worker_id: &str,
) -> Option<Publish> {
    let kind = MessageType::parse(&entry.category.to_ascii_lowercase());
    let text = entry.message.trim();
    let (kind, body) = match kind {
        Some(MessageType::Question) => (MessageType::Question, json!({"text": text})),
        // Answers need `reply_to`, findings refs and an evidence level,
        // offers and handoffs their fields: those go through
        // `ctx_agent send`, never as a plain post.
        Some(
            MessageType::Answer | MessageType::Finding | MessageType::Offer | MessageType::Handoff,
        ) => return None,
        Some(MessageType::Progress) | None => {
            let step: String = text.chars().take(STEP_MAX).collect();
            (MessageType::Progress, json!({"step": step}))
        }
    };
    let publish = Publish {
        schema: BUS_SCHEMA.into(),
        client_msg_id: ulid_for(
            u64::try_from(entry.timestamp.timestamp_millis()).unwrap_or_default(),
            &entry.id,
        ),
        to: to.to_string(),
        kind,
        worker_id: worker_id.to_string(),
        thread_id: None,
        reply_to: None,
        refs: Vec::new(),
        body,
        ck_wrapped: None,
        epoch: None,
        about: None,
        counts: None,
        window: None,
    };
    publish.validate().ok().map(|()| publish)
}

/// A frame to send and the local post it carries.
#[derive(Debug, Clone, PartialEq)]
pub struct OutgoingV2 {
    pub entry_id: String,
    pub frame: Value,
}

/// The local posts still to publish, oldest first. `worker_of` names the
/// registered worker of a local agent (none: the post waits). Posts that
/// may never leave are decided once.
pub(crate) fn outgoing(
    registry: &AgentRegistry,
    map: &VaultMap,
    state: &mut StateV2,
    screen: &mut Screen,
    worker_of: &dyn Fn(&AgentEntry) -> Option<String>,
) -> Vec<OutgoingV2> {
    let now = chrono::Utc::now();
    let mut entries: Vec<_> = registry
        .scratchpad
        .iter()
        .filter(|e| e.expires_at.is_none_or(|at| at > now))
        .filter(|e| !is_inbound(e))
        .filter(|e| !state.was_decided(&e.id))
        .filter(|e| !state.in_flight.values().any(|id| id == &e.id))
        .collect();
    entries.sort_by_key(|e| e.timestamp);
    let mut out = Vec::new();
    for entry in entries {
        let Some(agent) = registry
            .agents
            .iter()
            .find(|a| a.agent_id == entry.from_agent)
        else {
            continue;
        };
        let record = match Address::parse(entry.to_agent.as_deref()) {
            Ok(Address::Vault { workspace, vault }) => map
                .vaults
                .values()
                .find(|r| r.ns == workspace && r.name == vault),
            // Direct messages come later.
            Ok(Address::Worker(_)) => continue,
            Ok(Address::Local | Address::LocalAgent(_)) | Err(_) => {
                state.decide(&entry.id);
                continue;
            }
        };
        let denied =
            record.is_some_and(|r| state.denied.contains_key(&format!("vault:{}", r.vault_id)));
        let Some(record) = record.filter(|_| entry.privacy != PrivacyLevel::Private && !denied)
        else {
            state.decide(&entry.id);
            state.withheld += 1;
            continue;
        };
        let Some(worker_id) = worker_of(agent) else {
            continue;
        };
        if state.held(None, &worker_id).is_some() {
            continue;
        }
        if screen.check(&entry.message).is_err() {
            state.decide(&entry.id);
            state.withheld += 1;
            continue;
        }
        let Some(publish) = publish_for(entry, &address_of(record), &worker_id) else {
            state.decide(&entry.id);
            state.withheld += 1;
            continue;
        };
        let Ok(Value::Object(mut frame)) = serde_json::to_value(&publish) else {
            continue;
        };
        frame.insert("t".into(), "place.publish".into());
        state
            .in_flight
            .insert(publish.client_msg_id.clone(), entry.id.clone());
        out.push(OutgoingV2 {
            entry_id: entry.id.clone(),
            frame: Value::Object(frame),
        });
    }
    out
}

/// Display text of a delivered message (fenced by the local bus reader as
/// data from Hive, [`is_inbound`]).
fn delivered_text(message: &Value) -> String {
    let body = &message["body"];
    // A typed message carries its id, thread and addressee, so the agent
    // can answer it (`ctx_agent reply reply_to=<id>`).
    if let Some(line) = super::messages_v2::delivered_line(message) {
        return line;
    }
    ["text", "statement", "step", "summary", "what"]
        .iter()
        .find_map(|field| body[*field].as_str())
        .map_or_else(|| body.to_string(), str::to_string)
}

fn sender_label(message: &Value) -> String {
    let sender = &message["sender"];
    match sender["kind"].as_str() {
        Some("worker") => sender["worker_id"].as_str().unwrap_or("worker").to_string(),
        Some("person") => format!("person:{}", sender["handle"].as_str().unwrap_or("?")),
        _ => "hive".into(),
    }
}

/// Write a `message.deliver` into the local bus. Returns its `(address,
/// seq)`; an unknown vault, an older seq or a message naming a local agent
/// as sender changes nothing but the cursor.
pub(crate) fn apply_delivery(
    registry: &mut AgentRegistry,
    map: &VaultMap,
    state: &mut StateV2,
    message: &Value,
    local_workers: &BTreeSet<String>,
    agents_of: &BTreeMap<String, Vec<String>>,
) -> Result<Option<(String, u64)>, String> {
    let address = message["to"]
        .as_str()
        .ok_or("deliver without to")?
        .to_string();
    let seq = message["seq"].as_u64().ok_or("deliver without seq")?;
    if state.cursors.get(&address).is_some_and(|last| seq <= *last) {
        return Ok(None);
    }
    let Ok(Address::Vault { workspace, vault }) = Address::parse(Some(&address)) else {
        state.cursors.insert(address.clone(), seq);
        return Ok(Some((address, seq)));
    };
    if super::v2::sender_of(message).is_err() {
        state.cursors.insert(address.clone(), seq);
        return Ok(Some((address, seq)));
    }
    let root = map
        .vaults
        .values()
        .find(|r| r.ns == workspace && r.name == vault)
        .and_then(|r| r.roots.iter().find(|root| Path::new(root).is_dir()));
    let from = sender_label(message);
    let spoofed = registry.agents.iter().any(|a| a.agent_id == from);
    if let Some(root) = root.filter(|_| !spoofed && !local_workers.contains(&from)) {
        let kind = message["type"].as_str().unwrap_or("progress");
        let key = format!("{INBOUND_KEY_PREFIX}{address}:{seq}");
        let text = delivered_text(message);
        // A message for one of this machine's workers goes to the local
        // agents that are that worker; everything else to every agent of
        // the project, as a vault post.
        let addressed: &[String] = message["body"]["to_worker"]
            .as_str()
            .and_then(|w| agents_of.get(w))
            .map_or(&[], Vec::as_slice);
        let targets: Vec<Option<&str>> = if addressed.is_empty() {
            vec![None]
        } else {
            addressed.iter().map(|a| Some(a.as_str())).collect()
        };
        for (i, to) in targets.into_iter().enumerate() {
            let key = if i == 0 {
                key.clone()
            } else {
                format!("{key}:{i}")
            };
            registry.post_message_keyed(
                Some(root),
                &from,
                to,
                kind,
                &text,
                PrivacyLevel::Team,
                MessagePriority::Normal,
                None,
                Some(&key),
            )?;
        }
    }
    state.cursors.insert(address.clone(), seq);
    Ok(Some((address, seq)))
}

#[cfg(test)]
#[path = "relay_v2_tests.rs"]
mod tests;
