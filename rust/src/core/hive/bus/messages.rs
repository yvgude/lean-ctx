// SPDX-License-Identifier: Apache-2.0
//! Typed agent messages over Hive (C0 1.9 §2, I1).
//!
//! Sender: `ctx_agent send|reply` checks the bus controls, screens the text
//! like every upload, seals it in a sealed project with a key this machine
//! holds, and queues an [`AgentMessageV1`] in the mailbox. No network in the
//! tool call: the relay sends queued messages on its tick and settles them on
//! `published` or `error`.
//!
//! Receiver: the relay files `message.deliver` and `task.event` frames in the
//! mailbox and acknowledges each delivery. `ctx_agent inbox` shows an agent
//! its unread messages, every body wrapped as foreign data (X5), and queues
//! the `read` receipts the relay sends on its next tick. Task events show
//! only the task number; the task itself is `ctx_tasks action=show number=<n>`.
//!
//! Networks are never bridged: a reply always takes the network of the
//! message it answers, whatever the caller asked for.
//!
//! State in `<data>/hive/bus/messages.json`, guarded by `messages.lock`
//! because the tool process and the relay both write it.

// The relay half is used by the socket loop, built with `http-server`.
#![cfg_attr(not(feature = "http-server"), allow(dead_code))]

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::OpenOptions;
use std::path::Path;

use fs2::FileExt as _;
use serde::{Deserialize, Serialize};

use super::relay::{GENERAL_CHANNEL, channel};
use crate::core::hive::HiveError;
use crate::core::hive::knowledge::screening::Screen;
use crate::core::hive::state::{HiveStore, load_or_quarantine, write_private};
use crate::core::hive::untrusted::{UntrustedKind, wrap};
use crate::core::hive::vault::VaultMap;
use lean_ctx_protocol::UtcTimestamp;
use lean_ctx_protocol::hive::bus::BusFrameV1;
use lean_ctx_protocol::hive::messages::{
    AgentMessageV1, MAX_QUESTION_DEADLINE_HOURS, MessageBodyV1, MessageDeliveryV1,
    MessageReceiptState, MessageReceiptV1, MessageReferenceV1, MessageStatusV1, MessageStoredV1,
    MessageTrust, MessageType, NetworkV1, SEALED_MESSAGE_SCHEMA, SenderV1, TaskEventV1,
};

const MAILBOX_FILE: &str = "messages.json";
const LOCK_FILE: &str = "messages.lock";
/// Received messages kept (oldest drop first), in all and per network;
/// besides that, a message goes at its `expires_at` (server retention).
const MAX_INBOX: usize = 500;
const MAX_PER_NETWORK: usize = 200;
const MAX_EVENTS: usize = 200;
const MAX_SENT: usize = 200;
/// Messages queued for Hive at most; a send beyond it is refused instead
/// of growing the file without end while Hive is unreachable (RB8 M14).
const MAX_OUTBOX: usize = 1000;
/// Messages one `inbox` call shows at most; the rest stay unread.
pub const INBOX_PAGE: usize = 20;

/// A received message and the local agents that have read it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Inbound {
    pub delivery: MessageDeliveryV1,
    #[serde(default)]
    pub read_by: BTreeSet<String>,
}

/// A task event and the local agents that have seen it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub event: TaskEventV1,
    #[serde(default)]
    pub seen_by: BTreeSet<String>,
}

/// Where a sent message stands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum SentState {
    Queued,
    /// `message.stored`: replies and threads use the `gid`.
    Stored {
        gid: String,
        thread_id: String,
        seq: u64,
    },
    Refused {
        reason: String,
    },
}

/// A message this machine sent, with the receipts that came back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sent {
    pub client_msg_id: String,
    pub from: String,
    #[serde(rename = "type")]
    pub message_type: MessageType,
    pub network: NetworkV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(flatten)]
    pub state: SentState,
    #[serde(default)]
    pub receipts: BTreeMap<String, MessageReceiptState>,
    /// Whether `inbox` has shown the sender this state and these receipts.
    #[serde(default)]
    pub reported: bool,
}

/// `messages.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Mailbox {
    #[serde(default)]
    pub inbox: VecDeque<Inbound>,
    #[serde(default)]
    pub events: VecDeque<Event>,
    /// Messages waiting for the relay.
    #[serde(default)]
    pub outbox: Vec<AgentMessageV1>,
    #[serde(default)]
    pub sent: VecDeque<Sent>,
    /// Receipts waiting for the relay.
    #[serde(default)]
    pub receipts: Vec<MessageReceiptV1>,
}

impl Mailbox {
    /// The mailbox without messages past their `expires_at`.
    pub fn load(data_dir: &Path) -> Self {
        let mut mailbox = Self::read(data_dir);
        mailbox.prune(&now_stamp());
        mailbox
    }

    /// Read without changing anything (no lock): a damaged or unknown file
    /// reads as empty but stays on disk; only [`Self::mutate`] moves it aside.
    fn read(data_dir: &Path) -> Self {
        std::fs::read(data_dir.join("bus").join(MAILBOX_FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Read for a change, under the lock: a damaged file (or one of a newer
    /// engine) is moved aside to `messages.json.damaged-<n>` first and the
    /// mailbox starts empty, so the damaged copy is never overwritten
    /// (RB8 M12). An unreadable file is an error.
    fn read_for_change(data_dir: &Path) -> Result<Self, HiveError> {
        let path = data_dir.join("bus").join(MAILBOX_FILE);
        match load_or_quarantine::<Self>(&path, |_| true) {
            Ok(mailbox) => Ok(mailbox.unwrap_or_default()),
            Err(error) if !path.exists() => {
                tracing::warn!("hive mailbox: {error}");
                Ok(Self::default())
            }
            Err(error) => Err(error),
        }
    }

    /// Server retention (D5) holds locally too: expired messages go, and
    /// each network keeps at most [`MAX_PER_NETWORK`] (newest stay).
    /// `now` is `YYYY-MM-DDTHH:MM:SSZ`, which orders like the stamps it
    /// is compared with.
    fn prune(&mut self, now: &str) {
        self.inbox
            .retain(|inbound| inbound.delivery.expires_at.as_str() > now);
        let mut per_network: BTreeMap<String, usize> = BTreeMap::new();
        let mut keep = vec![true; self.inbox.len()];
        for (index, inbound) in self.inbox.iter().enumerate().rev() {
            let count = per_network
                .entry(network_key(&inbound.delivery.network))
                .or_default();
            *count += 1;
            keep[index] = *count <= MAX_PER_NETWORK;
        }
        let mut keep = keep.into_iter();
        self.inbox.retain(|_| keep.next().unwrap_or(false));
        bounded(&mut self.inbox, MAX_INBOX);
    }

    fn save(&self, data_dir: &Path) -> Result<(), HiveError> {
        let bytes = serde_json::to_vec(self).map_err(|e| HiveError::Local(e.to_string()))?;
        write_private(&data_dir.join("bus"), MAILBOX_FILE, &bytes)
    }

    /// Load, change and save under the mailbox lock.
    pub fn mutate<T>(data_dir: &Path, change: impl FnOnce(&mut Self) -> T) -> Result<T, HiveError> {
        let dir = data_dir.join("bus");
        std::fs::create_dir_all(&dir).map_err(|e| HiveError::Local(e.to_string()))?;
        let mut options = OpenOptions::new();
        options.create(true).truncate(false).write(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let lock = options
            .open(dir.join(LOCK_FILE))
            .map_err(|e| HiveError::Local(e.to_string()))?;
        lock.lock_exclusive()
            .map_err(|e| HiveError::Local(format!("message lock: {e}")))?;
        // Compared against what is on disk, so pruning alone also rewrites it.
        let mut mailbox = Self::read_for_change(data_dir)?;
        let before = mailbox.clone();
        mailbox.prune(&now_stamp());
        let out = change(&mut mailbox);
        mailbox.prune(&now_stamp());
        if mailbox != before {
            mailbox.save(data_dir)?;
        }
        Ok(out)
    }

    fn sent_mut(&mut self, client_msg_id: &str) -> Option<&mut Sent> {
        self.sent
            .iter_mut()
            .find(|sent| sent.client_msg_id == client_msg_id)
    }
}

fn now_stamp() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn network_key(network: &NetworkV1) -> String {
    match network {
        NetworkV1::Org { channel } => format!("org {channel}"),
        NetworkV1::Swarm { swarm_id } => format!("swarm {swarm_id}"),
    }
}

fn bounded<T>(queue: &mut VecDeque<T>, max: usize) {
    while queue.len() > max {
        queue.pop_front();
    }
}

/// The project id in an org channel (`org:<org>/vault:<vault_id>/<name>`).
fn channel_project(channel: &str) -> Option<&str> {
    channel
        .split('/')
        .nth(1)
        .and_then(|part| part.strip_prefix("vault:"))
}

/// The project a message concerns; `None` for a swarm (every local agent).
fn network_project(network: &NetworkV1) -> Option<&str> {
    match network {
        NetworkV1::Org { channel } => channel_project(channel),
        NetworkV1::Swarm { .. } => None,
    }
}

// ---------------------------------------------------------------- relay side

/// File a `message.deliver`; returns the `delivered` receipt for a direct
/// message. A redelivery (same `gid`) or an invalid frame changes nothing.
pub(super) fn receive(data_dir: &Path, delivery: &MessageDeliveryV1) -> Option<BusFrameV1> {
    delivery.validate().ok()?;
    Mailbox::mutate(data_dir, |mailbox| {
        if mailbox
            .inbox
            .iter()
            .any(|known| known.delivery.gid == delivery.gid)
        {
            return None;
        }
        mailbox.inbox.push_back(Inbound {
            delivery: delivery.clone(),
            read_by: BTreeSet::new(),
        });
        bounded(&mut mailbox.inbox, MAX_INBOX);
        delivery.to.as_ref().map(|to| {
            BusFrameV1::MessageReceipt(MessageReceiptV1 {
                gid: delivery.gid.clone(),
                by: to.clone(),
                state: MessageReceiptState::Delivered,
            })
        })
    })
    .ok()
    .flatten()
}

/// File a `task.event` (once per channel and seq).
pub(super) fn task_event(data_dir: &Path, event: &TaskEventV1) -> Result<(), HiveError> {
    Mailbox::mutate(data_dir, |mailbox| {
        if mailbox
            .events
            .iter()
            .any(|known| known.event.channel == event.channel && known.event.seq == event.seq)
        {
            return;
        }
        mailbox.events.push_back(Event {
            event: event.clone(),
            seen_by: BTreeSet::new(),
        });
        bounded(&mut mailbox.events, MAX_EVENTS);
    })
}

/// A recipient's receipt for a message this machine sent.
pub(super) fn status(data_dir: &Path, status: &MessageStatusV1) -> Result<(), HiveError> {
    Mailbox::mutate(data_dir, |mailbox| {
        if let Some(sent) = mailbox.sent_mut(&status.client_msg_id) {
            // `read` never goes back to `delivered`.
            let before = sent.receipts.get(&status.by).copied();
            if before != Some(MessageReceiptState::Read) && before != Some(status.state) {
                sent.receipts.insert(status.by.clone(), status.state);
                sent.reported = false;
            }
        }
    })
}

/// `message.stored`: the queued message has its `gid`. False when the id is
/// not one of this mailbox's messages.
pub(super) fn stored(data_dir: &Path, stored: &MessageStoredV1) -> bool {
    settle(
        data_dir,
        &stored.client_msg_id,
        SentState::Stored {
            gid: stored.gid.clone(),
            thread_id: stored.thread_id.clone(),
            seq: stored.seq,
        },
    )
}

/// `error` for a queued message: refused for good, never resent.
pub(super) fn refused(data_dir: &Path, client_msg_id: &str, reason: &str) -> bool {
    settle(
        data_dir,
        client_msg_id,
        SentState::Refused {
            reason: reason.chars().take(200).collect(),
        },
    )
}

fn settle(data_dir: &Path, client_msg_id: &str, state: SentState) -> bool {
    Mailbox::mutate(data_dir, |mailbox| {
        let before = mailbox.outbox.len();
        mailbox
            .outbox
            .retain(|message| message.client_msg_id != client_msg_id);
        if let Some(sent) = mailbox.sent_mut(client_msg_id)
            && sent.state == SentState::Queued
        {
            sent.state = state;
            sent.reported = false;
        }
        before != mailbox.outbox.len()
    })
    .unwrap_or(false)
}

/// The tick's half: queued messages not yet in flight, and queued receipts
/// (taken out of the mailbox; a lost receipt is only a stale read mark).
pub(super) fn due(data_dir: &Path, in_flight: &BTreeSet<String>) -> Vec<BusFrameV1> {
    Mailbox::mutate(data_dir, |mailbox| {
        let mut frames: Vec<BusFrameV1> = mailbox
            .outbox
            .iter()
            .filter(|message| !in_flight.contains(&message.client_msg_id))
            .cloned()
            .map(BusFrameV1::Message)
            .collect();
        frames.extend(mailbox.receipts.drain(..).map(BusFrameV1::MessageReceipt));
        frames
    })
    .unwrap_or_default()
}

// ----------------------------------------------------------------- tool side

/// What `ctx_agent send|reply` asks for.
#[derive(Debug, Clone, Default)]
pub struct SendRequest<'a> {
    pub message_type: Option<&'a str>,
    pub text: &'a str,
    pub to: Option<&'a str>,
    /// The instance that runs `to` (org only); absent on a fresh direct
    /// message, the presence map decides (`bind_instance`).
    pub to_instance: Option<&'a str>,
    pub reply_to: Option<&'a str>,
    /// Questions only: hours until an unanswered question becomes a task.
    pub deadline_hours: Option<u64>,
    /// A swarm network to send in.
    pub swarm_id: Option<&'a str>,
    /// `task:<n>`, `fact:<id>`, `proposal:<n>`, `commit:<sha>`, `handoff:<id>`.
    pub references: &'a [String],
}

pub const TYPES: &str = "question|offer|handoff|answer|finding";

fn parse_type(raw: Option<&str>) -> Result<MessageType, String> {
    let raw = raw.ok_or_else(|| format!("type is required ({TYPES})"))?;
    match serde_json::from_value(serde_json::Value::String(raw.trim().to_ascii_lowercase())) {
        Ok(MessageType::Unknown) | Err(_) => Err(format!("unknown type {raw:?}; use {TYPES}")),
        Ok(message_type) => Ok(message_type),
    }
}

fn parse_reference(raw: &str) -> Result<MessageReferenceV1, String> {
    let (kind, value) = raw
        .split_once(':')
        .ok_or_else(|| format!("reference {raw:?} must be kind:value"))?;
    let number = || {
        value
            .trim_start_matches('#')
            .parse::<u64>()
            .map_err(|_| format!("reference {raw:?} needs a number"))
    };
    Ok(match kind {
        "task" => MessageReferenceV1::Task { number: number()? },
        "proposal" => MessageReferenceV1::Proposal { number: number()? },
        "fact" => MessageReferenceV1::Fact { id: value.into() },
        "commit" => MessageReferenceV1::Commit { id: value.into() },
        "handoff" => MessageReferenceV1::Handoff {
            handoff_id: value.into(),
        },
        _ => {
            return Err(format!(
                "reference kind {kind:?}: task|fact|proposal|commit|handoff"
            ));
        }
    })
}

/// Everything [`send`] needs from the local Hive state.
pub(crate) struct SendContext<'a> {
    pub store: &'a HiveStore,
    pub org_id: &'a str,
    pub vault_id: &'a str,
    pub sealed: bool,
    /// This machine's instance id (never a recipient instance).
    pub instance_id: &'a str,
}

/// Queue a message from `from`; returns its `client_msg_id`.
pub(crate) fn send(
    ctx: &SendContext<'_>,
    from: &str,
    request: &SendRequest<'_>,
    screen: &mut Screen,
) -> Result<String, String> {
    if let Some(refusal) = super::control::admission(ctx.store, Some(from)) {
        return Err(refusal.message());
    }
    let message_type = parse_type(request.message_type)?;
    let data = ctx.store.data_dir();
    let mailbox = Mailbox::load(data);
    // A reply stays in the network (and thread) of what it answers.
    let network = match request.reply_to {
        Some(gid) => {
            // A received message, or one this machine sent (a follow-up).
            let network = mailbox
                .inbox
                .iter()
                .find(|known| known.delivery.gid == gid)
                .map(|known| known.delivery.network.clone())
                .or_else(|| {
                    mailbox.sent.iter().find_map(|sent| match &sent.state {
                        SentState::Stored { gid: stored, .. } if stored == gid => {
                            Some(sent.network.clone())
                        }
                        _ => None,
                    })
                })
                .ok_or_else(|| format!("no known message {gid:?} to reply to"))?;
            if request.swarm_id.is_some_and(
                |q| !matches!(&network, NetworkV1::Swarm { swarm_id } if swarm_id == q),
            ) {
                return Err(
                    "a reply stays in its network (org and swarms are never bridged)".into(),
                );
            }
            if let Some(project) = network_project(&network)
                && project != ctx.vault_id
            {
                return Err("that message belongs to another vault".into());
            }
            network
        }
        None => match request.swarm_id {
            Some(swarm_id) => NetworkV1::Swarm {
                swarm_id: swarm_id.into(),
            },
            None => NetworkV1::Org {
                channel: channel(ctx.org_id, ctx.vault_id, GENERAL_CHANNEL),
            },
        },
    };
    let to_instance = bind_instance(ctx, &network, request)?;
    if request.deadline_hours.is_some() && message_type != MessageType::Question {
        return Err("only questions carry a deadline".into());
    }
    let deadline = request
        .deadline_hours
        .map(|hours| {
            let hours = i64::try_from(hours)
                .ok()
                .filter(|h| (1..=MAX_QUESTION_DEADLINE_HOURS).contains(h))
                .ok_or_else(|| format!("deadline is 1-{MAX_QUESTION_DEADLINE_HOURS} hours"))?;
            let at = (chrono::Utc::now() + chrono::Duration::hours(hours))
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
            UtcTimestamp::new(&at).map_err(|e| e.to_string())
        })
        .transpose()?;
    let references = request
        .references
        .iter()
        .map(|raw| parse_reference(raw))
        .collect::<Result<Vec<_>, _>>()?;
    screen
        .check(request.text)
        .map_err(|blocked| format!("not sent: the text contains a {blocked}"))?;
    let swarm = matches!(network, NetworkV1::Swarm { .. });
    // A swarm message carries no text of this engine's repositories
    // (R3-3e-2). The queue does not know the swarm's scope, so a team
    // swarm is checked too.
    if swarm {
        crate::core::hive::own_text::check(ctx.store, &[("message", request.text)])
            .map_err(|e| e.to_string())?;
    }
    let body = MessageBodyV1 {
        text: request.text.to_string(),
        references,
    };
    let (body, sealed) = if ctx.sealed && !swarm {
        (None, Some(seal(ctx.store, ctx.vault_id, &body)?))
    } else {
        (Some(body), None)
    };
    let message = AgentMessageV1 {
        client_msg_id: format!("lcm-{}", uuid::Uuid::new_v4().simple()),
        network,
        message_type,
        from: from.into(),
        to: request.to.map(str::to_string),
        to_instance,
        reply_to: request.reply_to.map(str::to_string),
        deadline,
        body,
        sealed,
    };
    // A sealed body was validated in the clear by `seal`.
    message.validate().map_err(|e| format!("not sent: {e}"))?;
    let id = message.client_msg_id.clone();
    let queued = Mailbox::mutate(data, |mailbox| {
        if mailbox.outbox.len() >= MAX_OUTBOX {
            return false;
        }
        mailbox.sent.push_back(Sent {
            client_msg_id: id.clone(),
            from: from.into(),
            message_type,
            network: message.network.clone(),
            to: message.to.clone(),
            state: SentState::Queued,
            receipts: BTreeMap::new(),
            // Queued is what `send` already told the caller.
            reported: true,
        });
        bounded(&mut mailbox.sent, MAX_SENT);
        mailbox.outbox.push(message);
        true
    })
    .map_err(|e| e.to_string())?;
    if !queued {
        return Err(format!(
            "not sent: {MAX_OUTBOX} messages are still waiting for Hive; \
             try again once this machine is connected"
        ));
    }
    Ok(id)
}

/// `to_instance` of a message (C0 1.9 R3): a direct message on the org
/// network is bound to the instance that runs its recipient, so another
/// engine announcing the same agent name never receives it. An explicit
/// `to_instance` is sent as given (Hive refuses a foreign one). A fresh
/// direct message binds to the one remote instance that announced `to`
/// within the presence window; several such instances make the name
/// ambiguous and none leaves it unbound, so both are refused here and the
/// sender names the instance; it is never picked silently. A
/// reply carries none (Hive answers to the instance of the replied
/// message), nor does a message to everyone or in a swarm.
fn bind_instance(
    ctx: &SendContext<'_>,
    network: &NetworkV1,
    request: &SendRequest<'_>,
) -> Result<Option<String>, String> {
    let org = matches!(network, NetworkV1::Org { .. });
    match (request.to, request.to_instance) {
        (None, Some(_)) => Err("to_instance needs to_agent (a direct message)".into()),
        (Some(_), Some(_)) if !org => {
            Err("to_instance is for org messages; a swarm has no instances".into())
        }
        (Some(_), Some(instance)) => Ok(Some(instance.to_string())),
        (Some(to), None) if org && request.reply_to.is_none() => {
            let instances = super::presence::PresenceMap::load(ctx.store.data_dir()).instances_of(
                to,
                ctx.instance_id,
                &now_stamp(),
            );
            match instances.as_slice() {
                // Unbound, the message would reach whichever instance
                // announces the name first (6e's review, M15).
                [] => Err(format!(
                    "not sent: no other instance announced {to} in the last {} hours; \
                     address it with to_instance (or ctx_agent action=post for an agent on this machine)",
                    super::presence::SEEN_WINDOW_HOURS
                )),
                [one] => Ok(Some(one.clone())),
                several => Err(format!(
                    "not sent: {to} is announced by several instances ({}); address it with to_instance",
                    several.join(", ")
                )),
            }
        }
        _ => Ok(None),
    }
}

/// Seal a body with the newest project key this machine holds (no network).
fn seal(
    store: &HiveStore,
    vault_id: &str,
    body: &MessageBodyV1,
) -> Result<lean_ctx_protocol::hive::sealed::SealedBlobV1, String> {
    use crate::core::hive::sealed;
    body.validate(lean_ctx_protocol::hive::messages::NetworkKind::Org)
        .map_err(|e| format!("not sent: {e}"))?;
    let epoch = sealed::newest_held_epoch(store, vault_id)
        .map_err(|e| format!("not sent: {e}"))?
        .ok_or(
            "not sent: this machine holds no key for the sealed vault yet (lean-ctx hive sync)",
        )?;
    let key = sealed::held_key(store, vault_id, epoch)
        .ok_or("not sent: the sealed vault's key is unreadable")?;
    let plain = zeroize::Zeroizing::new(serde_json::to_vec(body).map_err(|e| e.to_string())?);
    sealed::seal(&key, vault_id, 0, epoch, SEALED_MESSAGE_SCHEMA, &plain).map_err(|e| e.to_string())
}

fn open_sealed(
    store: &HiveStore,
    vault_id: &str,
    blob: &lean_ctx_protocol::hive::sealed::SealedBlobV1,
) -> Option<MessageBodyV1> {
    use crate::core::hive::sealed;
    if blob.content_schema != SEALED_MESSAGE_SCHEMA {
        return None;
    }
    let key = sealed::held_key(store, vault_id, blob.epoch)?;
    let plain = sealed::open(&key, vault_id, 0, blob).ok()?;
    serde_json::from_slice(&plain).ok()
}

fn wire<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".into())
}

/// The sender as the wrapper's `source`: never an org or instance for a swarm.
fn sender_label(sender: &SenderV1) -> String {
    match sender {
        SenderV1::Agent {
            agent,
            instance_label,
            ..
        } => format!("{agent}@{instance_label}"),
        SenderV1::Person { label } => format!("person {label}"),
        SenderV1::Swarm { handle, reputation } => format!("{handle} (reputation {reputation})"),
        SenderV1::Unknown => "unknown".into(),
    }
}

fn reference_label(reference: &MessageReferenceV1) -> String {
    match reference {
        MessageReferenceV1::Task { number } => {
            format!("task #{number} (ctx_tasks action=show number={number})")
        }
        MessageReferenceV1::Proposal { number } => format!("proposal #{number}"),
        MessageReferenceV1::Fact { id } => format!("fact {id}"),
        MessageReferenceV1::Commit { id } => format!("commit {id}"),
        MessageReferenceV1::Handoff { handoff_id } => format!("handoff {handoff_id}"),
    }
}

/// One message as the agent sees it: a metadata line, then the body wrapped.
fn render(store: &HiveStore, inbound: &Inbound, reader_project: Option<&str>) -> String {
    let d = &inbound.delivery;
    // Outside the fence only the engine's words and the two enums: every id
    // and the sender came from Hive or another party and stand inside it
    // (6e's review, M6). `reply_to=<gid>` takes the gid from there.
    let meta = format!(
        "[{}] trust={}",
        wire(&d.message_type),
        if d.trust == MessageTrust::Team {
            "team"
        } else {
            "untrusted"
        }
    );
    let mut head = format!("gid: {}\nfrom: {}", d.gid, sender_label(&d.from));
    // The sending instance, so a direct reply can name it (`to_instance`)
    // when an agent id is ambiguous (C0 1.9 `ambiguous_recipient`).
    if let SenderV1::Agent {
        instance_id: Some(instance),
        ..
    } = &d.from
    {
        head.push_str(&format!("\nfrom instance: {instance}"));
    }
    if d.thread_id != d.gid {
        head.push_str(&format!("\nthread: {}", d.thread_id));
    }
    if let Some(reply_to) = &d.reply_to {
        head.push_str(&format!("\nreply to: {reply_to}"));
    }
    if let Some(deadline) = &d.deadline {
        head.push_str(&format!("\ndeadline: {}", deadline.as_str()));
    }
    if let NetworkV1::Swarm { swarm_id } = &d.network {
        head.push_str(&format!("\nswarm: {swarm_id}"));
    }
    let body = match (&d.body, &d.sealed) {
        (Some(body), _) => Some(body.clone()),
        (None, Some(blob)) => network_project(&d.network).and_then(|p| open_sealed(store, p, blob)),
        (None, None) => None,
    };
    let text = match body {
        Some(body) => {
            let mut text = admitted(&body.text, reader_project);
            if !body.references.is_empty() {
                let refs: Vec<String> = body.references.iter().map(reference_label).collect();
                text.push_str(&format!("\nreferences: {}", refs.join(", ")));
            }
            text
        }
        None => "(sealed; this machine holds no key for it yet)".into(),
    };
    let kind = match d.network {
        NetworkV1::Swarm { .. } => UntrustedKind::Swarm,
        NetworkV1::Org { .. } => UntrustedKind::BusMessage,
    };
    format!(
        "{meta}\n{}",
        wrap(kind, "hive message", &format!("{head}\n\n{text}"))
    )
}

/// The received text under the current context policy, like a stored bus
/// message (redacted or withheld as policy says).
fn admitted(text: &str, reader_project: Option<&str>) -> String {
    use crate::core::context_admission::stored;
    match stored::serve_stored(text, None, reader_project) {
        Ok(served) => {
            stored::publish_served(None);
            served
        }
        Err(withheld) => withheld.notice(),
    }
}

/// What `ctx_agent inbox` filters by.
#[derive(Debug, Clone, Copy, Default)]
pub struct InboxFilter<'a> {
    pub message_type: Option<&'a str>,
}

/// The unread messages and task events of `agent` in `vault_id`, marked
/// read; `read` receipts are queued for direct messages.
pub(crate) fn inbox(
    store: &HiveStore,
    vault_id: &str,
    reader_root: &str,
    agent: &str,
    filter: InboxFilter<'_>,
) -> Result<String, String> {
    let wanted = filter
        .message_type
        .map(|t| parse_type(Some(t)))
        .transpose()?;
    let concerns = |network: &NetworkV1| network_project(network).is_none_or(|p| p == vault_id);
    let (messages, events, more, own) = Mailbox::mutate(store.data_dir(), |mailbox| {
        let mut picked = Vec::new();
        let mut more = 0usize;
        for inbound in &mut mailbox.inbox {
            let d = &inbound.delivery;
            if !concerns(&d.network)
                || d.to.as_deref().is_some_and(|to| to != agent)
                || inbound.read_by.contains(agent)
                || wanted.is_some_and(|t| t != d.message_type)
            {
                continue;
            }
            if picked.len() == INBOX_PAGE {
                more += 1;
                continue;
            }
            inbound.read_by.insert(agent.to_string());
            picked.push(inbound.clone());
        }
        mailbox.receipts.extend(
            picked
                .iter()
                .filter(|inbound| inbound.delivery.to.as_deref() == Some(agent))
                .map(|inbound| MessageReceiptV1 {
                    gid: inbound.delivery.gid.clone(),
                    by: agent.to_string(),
                    state: MessageReceiptState::Read,
                }),
        );
        let mut events = Vec::new();
        if wanted.is_none() {
            for event in &mut mailbox.events {
                if channel_project(&event.event.channel) == Some(vault_id)
                    && event.seen_by.insert(agent.to_string())
                {
                    events.push(event.event.clone());
                }
            }
        }
        let mut own = Vec::new();
        for sent in &mut mailbox.sent {
            if sent.from == agent && !sent.reported && concerns(&sent.network) {
                sent.reported = true;
                own.push(sent.clone());
            }
        }
        (picked, events, more, own)
    })
    .map_err(|e| e.to_string())?;
    if messages.is_empty() && events.is_empty() && own.is_empty() {
        return Ok("No new Hive messages.".into());
    }
    let mut out = String::new();
    if !own.is_empty() {
        // Gids, refusal texts and readers' ids come from Hive: the whole
        // status stands in one fence (M6).
        let mut status = String::new();
        for sent in &own {
            status.push_str(&format!(
                "{} [{}] ",
                sent.client_msg_id,
                wire(&sent.message_type)
            ));
            match &sent.state {
                SentState::Queued => status.push_str("queued"),
                SentState::Stored { gid, thread_id, .. } => {
                    status.push_str(&format!("stored as {gid}"));
                    if thread_id != gid {
                        status.push_str(&format!(" (thread {thread_id})"));
                    }
                }
                SentState::Refused { reason } => status.push_str(&format!("refused: {reason}")),
            }
            let read: Vec<&str> = sent
                .receipts
                .iter()
                .filter(|(_, state)| **state == MessageReceiptState::Read)
                .map(|(by, _)| by.as_str())
                .collect();
            if !read.is_empty() {
                status.push_str(&format!("; read by {}", read.join(", ")));
            }
            status.push('\n');
        }
        out.push_str("Your messages:\n");
        out.push_str(&wrap(
            UntrustedKind::BusMessage,
            "hive status",
            status.trim_end(),
        ));
        out.push('\n');
    }
    if !messages.is_empty() {
        out.push_str(&format!(
            "Hive messages ({}){}:\n",
            messages.len(),
            if more > 0 {
                format!(", {more} more unread")
            } else {
                String::new()
            }
        ));
        for inbound in &messages {
            out.push_str(&render(store, inbound, Some(reader_root)));
            out.push('\n');
        }
    }
    if !events.is_empty() {
        out.push_str("Task events:\n");
        for event in &events {
            out.push_str(&format!(
                "  task #{} {} (now {}) → ctx_tasks action=show number={}\n",
                event.number,
                wire(&event.event),
                wire(&event.state),
                event.number
            ));
        }
    }
    Ok(out)
}

/// Unread message count for `agent` (session start, status lines).
pub(crate) fn unread(data_dir: &Path, vault_id: &str, agent: &str) -> usize {
    Mailbox::load(data_dir)
        .inbox
        .iter()
        .filter(|inbound| {
            let d = &inbound.delivery;
            network_project(&d.network).is_none_or(|p| p == vault_id)
                && d.to.as_deref().is_none_or(|to| to == agent)
                && !inbound.read_by.contains(agent)
        })
        .count()
}

/// `ctx_agent send|reply` in `project_root`: needs a connected machine, a
/// linked project and a lease with the bus.
pub fn send_from_root(
    project_root: &str,
    from: &str,
    request: &SendRequest<'_>,
) -> Result<String, String> {
    let connected = super::leases::connected(Path::new(project_root))?;
    let sealed =
        project_of(connected.store.data_dir(), project_root).is_some_and(|(_, sealed)| sealed);
    let ctx = SendContext {
        store: &connected.store,
        org_id: &connected.session.lease.lease.org_id,
        vault_id: &connected.vault_id,
        sealed,
        instance_id: &connected.session.instance.instance_id,
    };
    send(&ctx, from, request, &mut Screen::for_installation())
}

/// `ctx_agent inbox` in `project_root`; works offline from the mailbox.
pub fn inbox_for_root(
    project_root: &str,
    agent: &str,
    filter: InboxFilter<'_>,
) -> Result<String, String> {
    let store = HiveStore::open().map_err(|e| e.to_string())?;
    let (vault_id, _) = project_of(store.data_dir(), project_root)
        .ok_or("this repository is not linked to a vault")?;
    inbox(&store, &vault_id, project_root, agent, filter)
}

/// `"N Hive messages waiting → ctx_agent action=inbox"` when there are any
/// for `agent` in `project_root` (no network; never the content).
pub fn waiting_hint(project_root: &str, agent: &str) -> Option<String> {
    let store = HiveStore::open().ok()?;
    let (vault_id, _) = project_of(store.data_dir(), project_root)?;
    match unread(store.data_dir(), &vault_id, agent) {
        0 => None,
        n => Some(format!(
            "{n} Hive message(s) waiting → ctx_agent action=inbox"
        )),
    }
}

/// Project id and sealing of a linked project, without a network call.
pub(crate) fn project_of(data_dir: &Path, project_root: &str) -> Option<(String, bool)> {
    VaultMap::load(data_dir)
        .for_root(project_root)
        .map(|record| (record.vault_id.clone(), record.sealed))
}

#[cfg(test)]
#[path = "messages_tests.rs"]
mod tests;
