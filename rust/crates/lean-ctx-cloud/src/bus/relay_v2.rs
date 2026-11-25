// SPDX-License-Identifier: Apache-2.0
//! The bus v2 relay's decisions (C0 2.0 section 3; SPEC-ENGINE §3.7):
//! which vault buses this machine subscribes, the hello with its presence,
//! which local posts become `place.publish` frames, how deliveries land and
//! which Cloud-signed controls hold a worker.
//!
//! - Only a post addressed `#<ws>/<vault>` to a vault linked here leaves
//!   (D6); its type is the post's category when that is a bus type, a
//!   plain status line is a `progress` step.
//! - Deliveries are written into the local bus with the idempotency key
//!   `cloud:<address>:<seq>` and are never forwarded again (no bridging).
//! - A control counts only when its signature verifies against a Cloud
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
    PERSON_VALIDATION_WORKER, PresenceEntry, PresenceState, Publish, ulid_for,
};
use crate::CloudError;
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
/// Idempotency-key prefix of messages that arrived through Cloud.
pub(crate) use lean_ctx::core::agents::CLOUD_INBOUND_KEY_PREFIX as INBOUND_KEY_PREFIX;
/// A `progress` step is at most this long.
const STEP_MAX: usize = 300;

/// True for a local bus entry another machine's worker delivered through
/// Cloud: it is shown fenced as foreign data and never published again.
pub fn is_inbound(entry: &lean_ctx::core::agents::ScratchpadEntry) -> bool {
    entry.is_cloud_inbound()
}

/// Cursors, decided posts, in-flight publishes and verified controls, in
/// `<data_dir>/cloud/bus/state_v2.json`. Only the relay holding the relay
/// lock writes it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateV2 {
    /// Last `seq` held per subscribed address (`#ws/vault`).
    #[serde(default)]
    pub cursors: BTreeMap<String, u64>,
    /// The place and epoch each cursor was taken in (address → binding).
    /// A seq counts only there: a recreated vault is a new place whose seq
    /// restarts at 1, and a failover epoch bump may reissue seqs. A cursor
    /// without a binding (a file from before, or an older Cloud) keeps its
    /// value until the first ack that names its place binds it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub bound: BTreeMap<String, CursorBinding>,
    /// `#ws/vault` → the unbound cursor its first binding reset to 0: the
    /// replay up to that seq may bring posts already landed under the
    /// legacy key `cloud:<address>:<seq>`, so `apply_delivery` looks them
    /// up there first (see `legacy_key`). Gone once a delivery reaches
    /// that seq or the bus moves again.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub legacy_until: BTreeMap<String, u64>,
    /// Per address in a legacy window: posts matched to a legacy post and
    /// stale posts skipped, for the line logged when the window ends. Kept
    /// in memory only.
    #[serde(skip)]
    pub legacy_tally: BTreeMap<String, LegacyTally>,
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
    /// Places Cloud denied (`place_ref` → code); the relay stops posting there.
    #[serde(default)]
    pub denied: BTreeMap<String, String>,
    /// The last task change announced per vault task (C0 2.2 §10.1 catch-up).
    /// Lost with a damaged file: the next catch-up announces the open tasks
    /// again, which repeats a notice but never loses one.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tasks: super::task_events::TasksSeen,
    /// The plan version applied per vault plan (§37). Lost with a damaged
    /// file: the next catch-up pulls the active versions again; directives
    /// already issued are not repeated (they are keyed per agent).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub plans: super::plan_events::PlansSeen,
    /// The shape of this file: absent (0) in this build's; a file that
    /// names a version is newer and is not read.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub version: u32,
    /// Since when (unix ms) the file was found damaged or of an unknown
    /// version (RB8 M12): its controls are lost, so Cloud traffic waits under
    /// a hold until Cloud's replay after the next hello is complete
    /// (`note_replay`). Local work goes on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rebuild_pending: Option<u64>,
    /// The Cloud accepts a person's publish (`hello.ack` lists the feature
    /// [`PERSON_PUBLISH`]): the person's local posts to a vault bus leave
    /// the machine, attributed by the Cloud to this Instance's owner. Until
    /// then they wait.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub person_publish: bool,
    /// Publishes per worker in the last minute (QN0 rate limit). Not
    /// stored: a restart forgives the last minute.
    #[serde(skip)]
    pub budget: super::queen_kinds::RelayBudget,
    /// LeanCTX Code's chat key (rc4-32): a person's post the app prepared
    /// and signed leaves with the app's signature, checked against it. Not
    /// stored: loaded per connection.
    #[serde(skip)]
    pub chat_key: Option<crate::consent::chat_key::ChatKeyCheck>,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// The local bus sender of a person's post (`lean-ctx bus post`, Cloud
/// Desktop): no registered agent, no worker id.
pub const PERSON_SENDER: &str = "cloud-person";
/// The feature of a person's publish: the hello names it only while this
/// machine can sign one ([`crate::consent::person_sign::PersonSigner`]), and the
/// `hello.ack` names it when the Cloud accepts it.
pub const PERSON_PUBLISH: &str = "person_publish";
/// Domain of the person's signature over a publish (C0 `person_publish.v1`).
pub const PERSON_PUBLISH_DOMAIN: &str = "leanctx.cloud.person_publish.v1\n";
/// Entry metadata of a person's post LeanCTX Code prepared and signed
/// (rc4-32): the exact frame (JSON) as prepared…
pub const PERSON_FRAME_KEY: &str = "person_frame";
/// …and the chat key's signature over it (base64url).
pub const PERSON_SIGNATURE_KEY: &str = "presence_signature";

/// The `place.publish` frame of a person's post `entry` to the vault at
/// `address`, without its signature: exactly what the relay sends for it,
/// so LeanCTX Code can sign it ahead (`chat.prepare`). `None` when such a
/// post never leaves (answers, findings, offers, handoffs).
pub fn person_frame(
    entry: &lean_ctx::core::agents::ScratchpadEntry,
    address: &str,
) -> Option<serde_json::Map<String, Value>> {
    let text = if MessageType::parse(&entry.category.trim().to_ascii_lowercase())
        == Some(MessageType::Question)
    {
        super::person_rules::cut_to_fit(entry.message.trim(), super::v2::TEXT_MAX)
    } else {
        entry.message.clone()
    };
    let publish = publish_for(entry, &text, address, PERSON_VALIDATION_WORKER)?;
    let Ok(Value::Object(mut frame)) = serde_json::to_value(&publish) else {
        return None;
    };
    if let Some(fields) = super::queen_kinds::frame_fields(
        &entry.metadata,
        entry.task_id.as_deref(),
        entry
            .metadata
            .get(super::queen_kinds::THREAD_KEY)
            .map(String::as_str),
    ) {
        frame.extend(fields);
    }
    frame.insert("t".into(), "place.publish".into());
    frame.remove("worker_id");
    frame.insert("sender".into(), json!({"kind": "person"}));
    Some(frame)
}

/// The app-signed frame of a person's post, when the entry carries one.
fn app_signed(
    entry: &lean_ctx::core::agents::ScratchpadEntry,
) -> Option<(serde_json::Map<String, Value>, String)> {
    let frame = entry.metadata.get(PERSON_FRAME_KEY)?;
    let signature = entry.metadata.get(PERSON_SIGNATURE_KEY)?;
    if let Ok(Value::Object(frame)) = serde_json::from_str(frame) {
        return Some((frame, signature.clone()));
    }
    // Withheld (an empty frame never matches), and said so.
    tracing::warn!("cloud bus v2: an app-signed frame that is not a JSON object is withheld");
    Some((serde_json::Map::new(), signature.clone()))
}

/// The hello feature that asks Cloud for `hello.ack.policy_version` and the
/// `policy.advanced` nudge.
pub const POLICY_ADVANCED: &str = "policy_advanced";
/// The hello feature for the `policy.applied` frame: the Instance sends it
/// only when `hello.ack.features` names it too.
pub const POLICY_APPLIED: &str = "policy_applied";
/// The hello feature for team presence names (h7b): the Instance adds the
/// machine name and what each worker is working on to its
/// `presence.update` frames only when `hello.ack.features` names it too,
/// that is when Cloud shows them to members of the same team only, and
/// only while `[cloud] presence_names` is on. Never part of the hello.
pub const PRESENCE_IDENTITY: &str = "presence_identity";
/// The features this Instance always names in its hello (Cloud answers
/// with the ones it speaks too); see [`hello_features`].
pub const HELLO_FEATURES: &[&str] = &[
    POLICY_ADVANCED,
    POLICY_APPLIED,
    PRESENCE_IDENTITY,
    super::plan_events::PLAN_UPDATES,
];

/// The hello's features: [`HELLO_FEATURES`], plus [`PERSON_PUBLISH`] only
/// when a person's post can be signed here. Named without a signature, the
/// Cloud would refuse every person's post (`person_unproven`).
pub fn hello_features(person_signing: bool) -> Vec<&'static str> {
    let mut features = HELLO_FEATURES.to_vec();
    if person_signing {
        features.push(PERSON_PUBLISH);
    }
    features
}

/// Whether a `hello.ack` names `feature` among its `features`.
pub fn acks_feature(ack: &Value, feature: &str) -> bool {
    ack["features"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|f| f.as_str() == Some(feature))
}

/// Whether a `hello.ack` names [`PERSON_PUBLISH`] among its `features`.
pub fn acks_person_publish(ack: &Value) -> bool {
    acks_feature(ack, PERSON_PUBLISH)
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
                tracing::warn!("cloud bus v2 state: {error}");
                let state = Self {
                    rebuild_pending: Some(now_ms()),
                    ..Self::default()
                };
                if let Err(error) = state.save(data_dir) {
                    tracing::warn!("cloud bus v2 state: {error}");
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
                .map(|t| lean_ctx_time::UtcDateTime::from(t).timestamp_millis())
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

    pub fn save(&self, data_dir: &Path) -> Result<(), CloudError> {
        let bytes = serde_json::to_vec(self).map_err(|e| CloudError::Local(e.to_string()))?;
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

    /// A new socket starts: nothing is in flight on it (what the last one
    /// sent without `message.stored` goes again; Cloud deduplicates by
    /// client_msg_id), and a person's post waits for THIS connection's
    /// `hello.ack` to say the Cloud accepts it — the flag of an earlier
    /// connection never lets one leave.
    pub fn begin_connection(&mut self) {
        self.in_flight.clear();
        self.person_publish = false;
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

    /// Whether a person's post waits under a hold: a bus-wide hold or
    /// stop (`outgoing` skips it), or a pending rebuild (Cloud traffic
    /// waits until the control replay is complete, M12).
    pub fn holds_person_posts(&self) -> bool {
        self.rebuild_pending.is_some() || self.held(None, PERSON_VALIDATION_WORKER).is_some()
    }

    /// The veto on `ref_id` (a claim, handoff or lease), if any.
    pub fn vetoed(&self, ref_id: &str) -> Option<&Control> {
        self.controls.values().find(|c| {
            c.action == ControlAction::Veto && c.target_ref.as_ref().is_some_and(|r| r.id == ref_id)
        })
    }
}

fn now_ms() -> u64 {
    u64::try_from(lean_ctx_time::UtcDateTime::now().timestamp_millis()).unwrap_or_default()
}

/// Where Cloud's replay after this session's hello stands (M12). Cloud sends
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
/// hourly collaboration findings, V16.1): `<data>/cloud/bus/publish_queue.json`,
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
) -> Result<T, CloudError> {
    let dir = data_dir.join("bus");
    let _lock = crate::state::lock_state(&dir, QUEUE_FILE)?;
    let mut queue = read_queue(data_dir);
    let out = change(&mut queue);
    let bytes = serde_json::to_vec(&queue).map_err(|e| CloudError::Local(e.to_string()))?;
    write_private(&dir, QUEUE_FILE, &bytes)?;
    Ok(out)
}

/// Queue a publish for the relay. It is validated first; it survives a
/// relay restart, waits while offline and goes once Cloud stored it. A
/// permanent refusal drops it (counted, logged).
pub fn queue_publish(store: &crate::CloudStore, publish: Publish) -> Result<(), CloudError> {
    publish
        .validate()
        .map_err(|e| CloudError::Config(e.to_string()))?;
    change_queue(store.data_dir(), |queue| {
        if queue
            .items
            .iter()
            .any(|p| p.client_msg_id == publish.client_msg_id)
        {
            return Ok(());
        }
        if queue.items.len() >= MAX_QUEUE {
            return Err(CloudError::Local(format!(
                "{MAX_QUEUE} bus messages are still waiting for LeanCTX Cloud"
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

/// The worker of a queued publish, by its `client_msg_id`.
pub(crate) fn queued_worker(data_dir: &Path, client_msg_id: &str) -> Option<String> {
    read_queue(data_dir)
        .items
        .into_iter()
        .find(|p| p.client_msg_id == client_msg_id)
        .map(|p| p.worker_id)
}

/// Drop a queued publish Cloud stored or refused for good.
pub(crate) fn settle_queued(data_dir: &Path, client_msg_id: &str) -> Result<bool, CloudError> {
    change_queue(data_dir, |queue| {
        let before = queue.items.len();
        queue.items.retain(|p| p.client_msg_id != client_msg_id);
        before != queue.items.len()
    })
}

/// Which bus protocol the enrolled Cloud speaks, as the instance card
/// found out (E44 exists only on a C0 2.0 Cloud): `<config>/bus_protocol.json`.
const PROTOCOL_FILE: &str = "bus_protocol.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct ProtocolRecord {
    bus: u8,
    /// The minor of the C0 2.x protocol Cloud answered (`2.2` → 2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    minor: Option<u16>,
}

/// Record that the enrolled Cloud speaks bus v2 (`true`) or only 1.x.
pub fn record_protocol(store: &crate::CloudStore, v2: bool) -> Result<(), CloudError> {
    record_protocol_minor(store, v2, None)
}

/// [`record_protocol`] with the 2.x minor Cloud answered.
pub fn record_protocol_minor(
    store: &crate::CloudStore,
    v2: bool,
    minor: Option<u16>,
) -> Result<(), CloudError> {
    store.write_config(
        PROTOCOL_FILE,
        &ProtocolRecord {
            bus: if v2 { 2 } else { 1 },
            minor: minor.filter(|_| v2),
        },
    )
}

/// The C0 2.x minor the enrolled Cloud answered, as last recorded.
pub fn protocol_minor(store: &crate::CloudStore) -> Option<u16> {
    store
        .read_config::<ProtocolRecord>(PROTOCOL_FILE)
        .ok()
        .flatten()
        .filter(|r| r.bus == 2)
        .and_then(|r| r.minor)
}

/// True when the enrolled Cloud answered C0 2.2 or newer: doc nodes and
/// decision anchors may be written.
pub fn speaks_2_2(store: &crate::CloudStore) -> bool {
    protocol_minor(store).is_some_and(|minor| minor >= 2)
}

/// True when the relay should open a C0 2.0 session.
pub fn speaks_v2(store: &crate::CloudStore) -> bool {
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

pub(crate) fn record_for<'a>(map: &'a VaultMap, root: &str) -> Option<&'a VaultRecord> {
    map.vaults.values().find(|record| {
        record
            .roots
            .iter()
            .any(|known| lean_ctx::core::pathutil::same_project(known, root))
    })
}

/// The vault buses to subscribe: every linked vault, sorted, at most
/// [`MAX_SUBSCRIBE`]. A person reads and writes a vault's chat with no
/// agent running (rc4-82: without one the hello subscribed nothing and the
/// machine never received a message); SPEC-ENGINE §3.7.3 limits presence
/// and workers to agents in linked roots, not what this machine listens to.
pub(crate) fn subscriptions(map: &VaultMap) -> Vec<String> {
    let set: BTreeSet<String> = map
        .vaults
        .values()
        .map(address_of)
        // Only slugs that fit the bus address grammar.
        .filter(|address| Address::parse(Some(address)).is_ok_and(|a| a.is_remote()))
        .collect();
    set.into_iter().take(MAX_SUBSCRIBE).collect()
}

/// The C0 2.0 hello: subscriptions, their cursors, and the presence of the
/// local workers (`held` while a control holds them), and the
/// features this Instance speaks ([`hello_features`]).
pub fn hello(
    subscribe: &[String],
    state: &StateV2,
    presence: &[PresenceEntry],
    person_signing: bool,
) -> Value {
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
        "features": hello_features(person_signing),
    })
}

/// The `GET /v1/capabilities` feature of a Cloud that reads
/// `hello.metrics_sharing` (F29). Without it the hello stays as it was.
pub const FEATURE_METRICS_SHARING: &str = "instance_metrics_sharing";

/// What the hello says about metrics sharing (F29): `[cloud] metrics_upload`
/// (the switch the metrics uploader obeys, `false` after "connect without
/// sharing" or `config set cloud.metrics_upload false`), only while the
/// enrolled Cloud announced [`FEATURE_METRICS_SHARING`]; `None` otherwise.
pub fn metrics_sharing(
    store: &crate::CloudStore,
    config: &crate::config::CloudConfig,
) -> Option<bool> {
    crate::session_ref::announced(store, FEATURE_METRICS_SHARING).then_some(config.metrics_upload)
}

/// `hello` with the top-level `metrics_sharing` member when `sharing` is
/// `Some` ([`metrics_sharing`]); with `None` the frame is unchanged, byte for
/// byte, so a Cloud without the capability sees the hello it always saw.
pub fn with_metrics_sharing(mut hello: Value, sharing: Option<bool>) -> Value {
    if let (Some(sharing), Some(frame)) = (sharing, hello.as_object_mut()) {
        frame.insert("metrics_sharing".into(), Value::Bool(sharing));
    }
    hello
}

/// What [`adopt_ack_cursors`] changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct AckCursors {
    /// The state changed: the caller saves it at once.
    pub changed: bool,
    /// A bus moved to another place or epoch (or held a seq of no recorded
    /// one) and was reset to cursor 0: the caller re-subscribes it (a fresh
    /// hello) so Cloud replays it whole.
    pub resubscribe: bool,
}

/// Take the replay start Cloud names in `hello.ack.cursors` (`#ws/vault` →
/// seq, the per-bus `last` its replay begins after) as the cursor of a bus
/// this hello subscribed. Without it a bus that never delivered here
/// subscribes without a cursor on every reconnect, Cloud starts such a
/// subscription at its head, and posts committed in between are lost.
///
/// A seq is valid only in the (place, epoch) it was taken in: a recreated
/// vault is a new place whose seq restarts at 1, a failover epoch bump may
/// reissue seqs. Only an address Cloud subscribed and names a place for
/// (`ack.places[address]`) with that place's epoch (`ack.epochs[place]`)
/// counts:
/// - Same binding: the held cursor is never touched, so a later ack with an
///   older head cannot move it back; a bus without one takes the start.
/// - Another place or epoch than the bound one, or a cursor not bound yet (a
///   state file from before bindings): the held seq cannot be trusted in
///   the place Cloud names, so the cursor becomes 0, bound to that (place,
///   epoch), never the held seq nor the ack start (Cloud computed that start
///   as `min(sent cursor, head)`, so posts at or below it would be skipped).
///   Unless 0 was already sent, the caller re-subscribes, as a C0 hello is
///   only the first frame of a socket: it closes the session and the
///   immediate reconnect sends cursor 0, so Cloud replays the bus from its
///   start. Delivery keys carry the binding (see [`inbound_key`]), so the
///   new seq space never collides with posts of the old one, and a replay
///   within one binding lands once. A reset unbound cursor `c` is kept in
///   `legacy_until`, so the replay of seqs up to `c` first matches posts
///   landed under the legacy key (see [`legacy_key`]). Gap: frames Cloud sent after this ack on
///   the closing socket are not read, and a crash before the save resends
///   the stale cursor once.
/// - A fresh bus (no cursor, not bound): the ack start is taken and bound,
///   as Cloud starts such a subscription at its head.
/// - No start for the address: a fresh bus or a cursor not bound yet stays
///   as it is (only a moved bound one is reset).
///
/// An ack without `places` (an older Cloud) changes nothing, as before.
pub(crate) fn adopt_ack_cursors(
    state: &mut StateV2,
    subscribe: &[String],
    ack: &Value,
) -> AckCursors {
    let subscribed: BTreeSet<&str> = ack["subscribed"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let mut out = AckCursors::default();
    for address in subscribe {
        if !subscribed.contains(address.as_str()) {
            continue;
        }
        let Some(now) = ack_binding(ack, address) else {
            continue;
        };
        let held = state.cursors.get(address).copied();
        let start = ack["cursors"][address.as_str()].as_u64();
        let next = match (state.bound.get(address), held) {
            (Some(bound), Some(_)) if *bound == now => continue,
            (Some(bound), None) if *bound == now => start,
            (None, None) => start,
            (None, Some(_)) if start.is_none() => continue,
            // Moved, or a seq of a place nobody recorded: replay from 0.
            (bound, held) => {
                // Posts up to an unbound cursor landed under legacy keys.
                let legacy = held.filter(|seq| bound.is_none() && *seq > 0);
                let marker = state.legacy_until.get(address).copied();
                if marker != legacy {
                    match legacy {
                        Some(seq) => state.legacy_until.insert(address.clone(), seq),
                        None => state.legacy_until.remove(address),
                    };
                    state.legacy_tally.remove(address);
                    out.changed = true;
                }
                out.resubscribe |= held != Some(0);
                Some(0)
            }
        };
        let Some(next) = next else {
            continue;
        };
        if held != Some(next) {
            state.cursors.insert(address.clone(), next);
            out.changed = true;
        }
        if state.bound.get(address) != Some(&now) {
            state.bound.insert(address.clone(), now);
            out.changed = true;
        }
    }
    out
}

/// The (place, epoch) this ack reports for `address`, if it names both.
fn ack_binding(ack: &Value, address: &str) -> Option<CursorBinding> {
    let place = ack["places"][address].as_str()?;
    let epoch = ack["epochs"][place].as_u64()?;
    Some(CursorBinding {
        place: place.to_string(),
        epoch,
    })
}

/// `#ws/vault` → `vault:<vault_id>` for every vault the map resolved: the
/// place a bus address lives in (place id = vault id).
pub(crate) fn local_places(map: &VaultMap) -> BTreeMap<String, String> {
    map.vaults
        .values()
        .map(|r| (address_of(r), format!("vault:{}", r.vault_id)))
        .collect()
}

/// Before a hello: a cursor bound to a place the vault map no longer
/// resolves its address to (the vault was recreated) becomes 0 and unbound,
/// so the new place is replayed from its start and the stale seq is never
/// sent. An unbound cursor is sent as before; the epoch and Cloud's own
/// place are checked at the ack.
pub(crate) fn drop_moved_cursors(state: &mut StateV2, local: &BTreeMap<String, String>) -> bool {
    let moved: Vec<String> = state
        .bound
        .iter()
        .filter(|(address, held)| local.get(*address).is_some_and(|p| *p != held.place))
        .map(|(address, _)| address.clone())
        .collect();
    for address in &moved {
        state.cursors.insert(address.clone(), 0);
        state.bound.remove(address);
        state.legacy_until.remove(address);
        state.legacy_tally.remove(address);
    }
    !moved.is_empty()
}

/// Where a cursor's seq is valid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorBinding {
    /// `vault:<uuid>`.
    pub place: String,
    pub epoch: u64,
}

/// The key an unbound address posted `seq` with: `cloud:<address>:<seq>`.
///
/// After the first binding reset an unbound cursor `c` to 0 (state field
/// `legacy_until`), Cloud replays the bus from its start, and posts up to
/// `c` may already have landed under this key. While the replay is at or
/// below `c`, a delivery first looks for a live post of its sender under
/// this key (per target session as usual): the same request (text, which
/// for a typed message holds its id, addressee and kind) answers with that
/// post and writes nothing; another one, a recreated vault's post of the
/// same seq, is posted under its tagged key. The window ends when a
/// delivery reaches `c` or the bus moves again; no lookup after that. Only
/// posts still live locally (scratchpad TTL) can be matched; staler ones
/// are not posted at all (see [`stale_for_legacy_window`]).
fn legacy_key(address: &str, seq: u64) -> String {
    format!("{INBOUND_KEY_PREFIX}{address}:{seq}")
}

/// The idempotency key a delivery of `address` at `seq` is posted with.
///
/// A seq is unique only within one (place, epoch): after a recreate or an
/// epoch bump Cloud replays seq 1, 2, … again with other messages, and a key
/// of address and seq alone would refuse them as a conflicting reuse (the
/// post lost, the cursor past it). So a bound address keys by its current
/// binding: `cloud:<address>:<tag>.<seq>`, `tag` the first 16 hex digits of
/// BLAKE3 over `<place>\0<epoch>`. A place holds ':' (`vault:<uuid>`), the
/// tag does not, so the last ':' still separates the address
/// (`cloud_inbound_address`). Within one binding a replay of the same seq
/// keeps its key and lands once. An unbound address (no placed ack yet: an
/// older Cloud) keeps `cloud:<address>:<seq>`; it never replays a reset seq
/// space, as the first placed ack binds it and resets it to 0 first.
pub(crate) fn inbound_key(state: &StateV2, address: &str, seq: u64) -> String {
    match state.bound.get(address) {
        Some(binding) => {
            let tag = blake3::hash(format!("{}\0{}", binding.place, binding.epoch).as_bytes());
            let tag = tag.to_hex();
            format!("{INBOUND_KEY_PREFIX}{address}:{}.{seq}", &tag[..16])
        }
        None => legacy_key(address, seq),
    }
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
    text: &str,
    to: &str,
    worker_id: &str,
) -> Option<Publish> {
    let kind = MessageType::parse(&entry.category.to_ascii_lowercase());
    let text = text.trim();
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
/// registered worker of a local agent (none: the post waits). A person's
/// post leaves only signed by `person_signer` (none: it waits). Posts that may
/// never leave are decided once.
pub(crate) fn outgoing(
    registry: &AgentRegistry,
    map: &VaultMap,
    state: &mut StateV2,
    screen: &mut Screen,
    worker_of: &dyn Fn(&AgentEntry) -> Option<String>,
    records: &super::person_rules::Records,
    person_signer: Option<&dyn crate::consent::person_sign::PersonSigner>,
) -> Vec<OutgoingV2> {
    let now = lean_ctx_time::UtcDateTime::now();
    let queen_mode = super::person_rules::QueenMode::current();
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
        let agent = registry
            .agents
            .iter()
            .find(|a| a.agent_id == entry.from_agent);
        let person = agent.is_none() && entry.from_agent == PERSON_SENDER;
        if agent.is_none() && !person {
            continue;
        }
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
        // HN0: of this Instance's agents only the Queen the app registered
        // speaks on a vault's bus; every other worker's post stays on the
        // local bus (one carrying a Queen kind is counted as withheld).
        if let Some(agent) = agent
            && !super::person_rules::Speaker::of_registry(registry, Some(&agent.agent_id))
                .registered_queen
        {
            state.decide(&entry.id);
            if entry.metadata.contains_key(super::queen_kinds::KIND_KEY) {
                state.withheld += 1;
            }
            continue;
        }
        // QN2 `orchestrator.network`: while the vault's team policy turns
        // the Orchestrator network off, an Orchestrator kind stays on the
        // local bus (withheld); an untrusted floor makes it wait. A withheld
        // entry is decided: it stays local even if the network is turned
        // back on later, and is not relayed then. Inbound deliveries of a
        // kind are dropped for such a vault in `apply_delivery`.
        if entry.metadata.contains_key(super::queen_kinds::KIND_KEY) {
            match lean_ctx::core::policy::cloud_orchestrator::vault_network_refusal(
                &record.vault_id,
            ) {
                Ok(None) => {}
                Ok(Some(_)) => {
                    state.decide(&entry.id);
                    state.withheld += 1;
                    continue;
                }
                Err(_) => continue,
            }
        }
        // A person's post waits until the Cloud accepts it and this machine
        // can sign it; an agent's until its worker is registered.
        let worker_id = match agent {
            Some(agent) => match worker_of(agent) {
                Some(worker_id) => worker_id,
                None => continue,
            },
            None if state.person_publish
                && (person_signer.is_some() || state.chat_key.is_some()) =>
            {
                PERSON_VALIDATION_WORKER.to_owned()
            }
            None => continue,
        };
        // A bus-wide hold holds a person's post too.
        if state.held(None, &worker_id).is_some() {
            continue;
        }
        // Only the registered Queen appears as the Queen (process v1 §7);
        // `ctx_agent post` checks and logs it first. Here an older post is
        // kept local (`enforce`) or leaves with the receiver's mark (`warn`).
        let queen = agent.map_or(Ok(None), |agent| {
            let sender = super::person_rules::Speaker::of_registry(registry, Some(&agent.agent_id));
            super::person_rules::queen_rule(sender, &[&entry.message], queen_mode)
        });
        // No refused action passed on (process v1 §7), as for `ctx_agent
        // send`: a refused one stays local; unreadable records make it wait.
        if let Some(agent) = agent
            && let Some((kind, body)) =
                super::person_rules::post_body(&entry.category, &entry.message)
            && super::person_rules::asks_to_act(kind)
        {
            let Some(ledger) = &records.ledger else {
                continue;
            };
            let refused = super::person_rules::Refused::for_sender(
                state,
                &records.handoffs,
                ledger,
                map,
                &agent.agent_id,
                &worker_id,
            );
            if super::person_rules::check_forwarding(kind, &[], &body, &refused).is_err() {
                state.decide(&entry.id);
                state.withheld += 1;
                continue;
            }
        }
        let (Ok(queen), Ok(())) = (queen, screen.check(&entry.message)) else {
            state.decide(&entry.id);
            state.withheld += 1;
            continue;
        };
        let text = queen.map_or_else(
            || entry.message.clone(),
            |mark| super::person_rules::marked(&mark, &entry.message),
        );
        // A question queued before its sending tool checked the length (or
        // marked here only) is delivered shortened, with a visible note of
        // the cut, never withheld for its length.
        let text = if MessageType::parse(&entry.category.trim().to_ascii_lowercase())
            == Some(MessageType::Question)
        {
            super::person_rules::cut_to_fit(text.trim(), super::v2::TEXT_MAX)
        } else {
            text
        };
        let Some(publish) = publish_for(entry, &text, &address_of(record), &worker_id) else {
            state.decide(&entry.id);
            state.withheld += 1;
            continue;
        };
        // Over its worker's budget for the minute, a post waits.
        if !state.budget.take(&worker_id, now.timestamp_millis()) {
            continue;
        }
        let Ok(Value::Object(mut frame)) = serde_json::to_value(&publish) else {
            continue;
        };
        // QN0: a post carries its kind, task and thread, each when it has one.
        if let Some(fields) = super::queen_kinds::frame_fields(
            &entry.metadata,
            entry.task_id.as_deref(),
            entry
                .metadata
                .get(super::queen_kinds::THREAD_KEY)
                .map(String::as_str),
        ) {
            frame.extend(fields);
        }
        frame.insert("t".into(), "place.publish".into());
        if person {
            // The Cloud attributes it to the account that owns this
            // Instance; the frame names no worker and no user. The presence
            // key signs the frame as sent (its `t` included); unsigned, the
            // post waits.
            frame.remove("worker_id");
            frame.insert("sender".into(), json!({"kind": "person"}));
            if let Some((signed, signature)) = app_signed(entry) {
                // rc4-32: LeanCTX Code signed this frame with its chat key;
                // it leaves unchanged with that signature, or never (a
                // frame for another vault, or a signature that does not
                // verify). The engine signs nothing here.
                let ours =
                    signed.get("to").and_then(Value::as_str) == Some(address_of(record).as_str());
                let verified = state.chat_key.as_ref().is_some_and(|check| {
                    check.admits(
                        PERSON_PUBLISH_DOMAIN,
                        &Value::Object(signed.clone()),
                        &signature,
                    )
                });
                if !(ours && verified) {
                    state.decide(&entry.id);
                    state.withheld += 1;
                    continue;
                }
                frame = signed;
                frame.insert(
                    crate::consent::person_sign::SIGNATURE_FIELD.into(),
                    signature.into(),
                );
            } else {
                let Some(signer) = person_signer else {
                    continue;
                };
                if !crate::consent::person_sign::sign_frame(
                    signer,
                    PERSON_PUBLISH_DOMAIN,
                    &mut frame,
                ) {
                    continue;
                }
            }
        }
        let client_msg_id = frame
            .get("client_msg_id")
            .and_then(Value::as_str)
            .map_or_else(|| publish.client_msg_id.clone(), str::to_owned);
        state.in_flight.insert(client_msg_id, entry.id.clone());
        out.push(OutgoingV2 {
            entry_id: entry.id.clone(),
            frame: Value::Object(frame),
        });
    }
    out
}

/// Display text of a delivered message (fenced by the local bus reader as
/// data from Cloud, [`is_inbound`]).
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
        _ => "cloud".into(),
    }
}

/// The address, sequence number and linked vault of a delivered
/// `message` while it is new (its seq past the cursor); `None` for a
/// replay or a frame without them.
pub(crate) fn new_delivery<'a>(
    map: &'a VaultMap,
    state: &StateV2,
    message: &Value,
) -> Option<(String, u64, Option<&'a VaultRecord>)> {
    let address = message["to"].as_str()?.to_string();
    let seq = message["seq"].as_u64()?;
    if state.cursors.get(&address).is_some_and(|last| seq <= *last) {
        return None;
    }
    let record = match Address::parse(Some(&address)) {
        Ok(Address::Vault { workspace, vault }) => map
            .vaults
            .values()
            .find(|r| r.ns == workspace && r.name == vault),
        _ => None,
    };
    Some((address, seq, record))
}

/// Skip a delivery refused by the exchange rules (§40): only the cursor
/// moves, nothing reaches the local bus.
pub(crate) fn skip_delivery(state: &mut StateV2, address: &str, seq: u64) {
    state.cursors.insert(address.to_string(), seq);
}

/// Write a `message.deliver` into the local bus. Returns its `(address,
/// seq)`; an unknown vault, an older seq or a message naming a local agent
/// as sender changes nothing but the cursor.
///
/// A message for one worker (`body.to_worker`) goes to the sessions of
/// that worker alive in `registry` now, each its own addressed copy:
/// `worker_of` maps a live agent to its worker at delivery time, not when
/// the relay connected, so a session that reconnected under a new id gets
/// it and a finished one does not (#28). A worker of this machine without
/// a live session keeps the message as a vault post; another machine's
/// worker gets nothing here.
pub(crate) fn apply_delivery(
    registry: &mut AgentRegistry,
    map: &VaultMap,
    state: &mut StateV2,
    message: &Value,
    local_workers: &BTreeSet<String>,
    worker_of: &dyn Fn(&AgentEntry) -> Option<String>,
) -> Result<Option<(String, u64)>, String> {
    let applied = deliver_one(registry, map, state, message, local_workers, worker_of)?;
    if let Some((address, seq)) = &applied {
        end_legacy_window(state, address, *seq);
    }
    Ok(applied)
}

/// What one legacy window matched or skipped.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LegacyTally {
    /// Posts that had already landed under their legacy key.
    pub matched: u64,
    /// Stale posts (purged or older than the local TTL) not posted again.
    pub skipped: u64,
}

/// The legacy window of a first binding ends once a delivery reaches the
/// old cursor: logged with its counts (never a text), then no lookups.
fn end_legacy_window(state: &mut StateV2, address: &str, seq: u64) {
    if state
        .legacy_until
        .get(address)
        .is_some_and(|until| seq >= *until)
    {
        state.legacy_until.remove(address);
        let tally = state.legacy_tally.remove(address).unwrap_or_default();
        tracing::info!(
            "bus relay: the first replay of {address} after binding ended: {} posts already landed, {} stale posts not posted again",
            tally.matched,
            tally.skipped
        );
    }
}

/// Inside a legacy window, a post that would already have expired here
/// under normal delivery: a purged stub (Cloud kept only its metadata) or
/// one received longer ago than the local scratchpad TTL (from config).
///
/// Trade-off: an unbound state file's bus is replayed whole once, and
/// posting its history again would bring back, as fresh 12 h-or-so posts,
/// messages that expired here long ago. Skipping them costs only in an edge
/// case: a vault recreated before an old, unbound state file was first
/// bound, whose new posts older than the TTL are then not shown; posts
/// that old count as stale anyway. Within the TTL a post is matched to its
/// legacy post or posted (see [`legacy_key`]).
fn stale_for_legacy_window(message: &Value, ttl_hours: u64) -> bool {
    if message["purged"].as_bool() == Some(true) {
        return true;
    }
    let Some(received_ms) = message["received_at"].as_u64() else {
        return false;
    };
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    let ttl_ms = ttl_hours.saturating_mul(3_600_000);
    now_ms.saturating_sub(received_ms) > ttl_ms
}

fn deliver_one(
    registry: &mut AgentRegistry,
    map: &VaultMap,
    state: &mut StateV2,
    message: &Value,
    local_workers: &BTreeSet<String>,
    worker_of: &dyn Fn(&AgentEntry) -> Option<String>,
) -> Result<Option<(String, u64)>, String> {
    let address = message["to"]
        .as_str()
        .ok_or("deliver without to")?
        .to_string();
    let seq = message["seq"].as_u64().ok_or("deliver without seq")?;
    if state.cursors.get(&address).is_some_and(|last| seq <= *last) {
        return Ok(None);
    }
    // The legacy window of a first binding ends at the old cursor
    // ([`end_legacy_window`]); a stale post in it only moves the cursor.
    let legacy = state
        .legacy_until
        .get(&address)
        .is_some_and(|until| seq <= *until);
    if legacy
        && stale_for_legacy_window(
            message,
            lean_ctx::core::config::Config::load()
                .agents
                .scratchpad_default_ttl_hours,
        )
    {
        state
            .legacy_tally
            .entry(address.clone())
            .or_default()
            .skipped += 1;
        state.cursors.insert(address.clone(), seq);
        return Ok(Some((address, seq)));
    }
    let Ok(Address::Vault { workspace, vault }) = Address::parse(Some(&address)) else {
        state.cursors.insert(address.clone(), seq);
        return Ok(Some((address, seq)));
    };
    if super::v2::sender_of(message).is_err() {
        state.cursors.insert(address.clone(), seq);
        return Ok(Some((address, seq)));
    }
    let record = map
        .vaults
        .values()
        .find(|r| r.ns == workspace && r.name == vault);
    // QN2 `orchestrator.network`: an older sending host does not enforce
    // it, so an Orchestrator kind for a vault whose team policy turns the
    // network off is dropped here too (only the cursor moves). A floor that
    // cannot be trusted drops it as well, as the local bus refuses it then.
    if let Some(record) = record
        && super::queen_kinds::delivered_fields(message).0.is_some()
        && !matches!(
            lean_ctx::core::policy::cloud_orchestrator::vault_network_refusal(&record.vault_id),
            Ok(None)
        )
    {
        state.cursors.insert(address.clone(), seq);
        return Ok(Some((address, seq)));
    }
    let root = record.and_then(|r| r.roots.iter().find(|root| Path::new(root).is_dir()));
    let from = sender_label(message);
    let spoofed = registry.agents.iter().any(|a| a.agent_id == from);
    if let Some(root) = root.filter(|_| !spoofed && !local_workers.contains(&from)) {
        let kind = message["type"].as_str().unwrap_or("progress");
        let key = inbound_key(state, &address, seq);
        let text = delivered_text(message);
        let (queen_kind, task_id, thread) = super::queen_kinds::delivered_fields(message);
        let to_worker = message["body"]["to_worker"].as_str();
        let addressed: Vec<String> = to_worker.map_or_else(Vec::new, |w| {
            registry
                .list_active(None)
                .into_iter()
                .filter(|agent| worker_of(agent).as_deref() == Some(w))
                .map(|agent| agent.agent_id.clone())
                .collect()
        });
        let targets: Vec<Option<&str>> = match to_worker {
            None => vec![None],
            Some(_) if !addressed.is_empty() => {
                addressed.iter().map(|a| Some(a.as_str())).collect()
            }
            Some(w) if local_workers.contains(w) => vec![None],
            Some(_) => Vec::new(),
        };
        // A copy per session is keyed by that session, not by its place in
        // the list: a replay with another set of live sessions must not
        // take a later session's copy for one already posted (#28 review).
        // The suffix holds no ':', so `cloud_inbound_address` (split at the
        // last ':') still reads `cloud:<address>:<tag>.<seq>@<agent>` right.
        let old_key = legacy.then(|| legacy_key(&address, seq));
        let mut matched = false;
        for to in targets {
            let per_target = |key: &str| {
                to.map_or_else(
                    || key.to_string(),
                    |agent| format!("{key}@{}", agent.replace(':', "_")),
                )
            };
            let key = per_target(&key);
            // In the legacy window a post landed under the legacy key with
            // the same request is this one (its id, nothing written); one
            // with another request is of a recreated vault: posted anew.
            let landed = old_key
                .as_deref()
                .map(per_target)
                .filter(|old| registry.holds_request_id(Some(root), &from, old));
            let mut post = |key: &str| {
                registry.post_message_keyed(
                    Some(root),
                    &from,
                    to,
                    kind,
                    &text,
                    PrivacyLevel::Team,
                    MessagePriority::Normal,
                    None,
                    Some(key),
                )
            };
            let id = match landed.map(|old| post(&old)) {
                Some(Ok(id)) => {
                    matched = true;
                    id
                }
                Some(Err(e)) if e == lean_ctx::core::agents::request_id_conflict() => post(&key)?,
                Some(Err(e)) => return Err(e),
                None => post(&key)?,
            };
            // QN0: the kind, task and thread travel end to end.
            if let Some(entry) = registry.scratchpad.iter_mut().find(|e| e.id == id) {
                if let Some(queen_kind) = queen_kind {
                    entry.metadata.insert(
                        super::queen_kinds::KIND_KEY.into(),
                        queen_kind.as_str().into(),
                    );
                }
                if let Some(thread) = &thread {
                    entry
                        .metadata
                        .insert(super::queen_kinds::THREAD_KEY.into(), thread.clone());
                }
                if task_id.is_some() {
                    entry.task_id.clone_from(&task_id);
                }
            }
        }
        if matched {
            state
                .legacy_tally
                .entry(address.clone())
                .or_default()
                .matched += 1;
        }
    }
    state.cursors.insert(address.clone(), seq);
    Ok(Some((address, seq)))
}

#[cfg(test)]
#[path = "relay_v2_tests.rs"]
mod tests;
