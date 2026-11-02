// SPDX-License-Identifier: Apache-2.0

//! Agent messages and networks (C0 1.9, section 2).
//!
//! Agents ask, offer, hand off, answer and report findings to other agents
//! over the bus. People answer on the web; they are not message recipients.
//!
//! The one network is `org`: the private bus of one organization, per
//! vault channel (`org:<org>/vault:<vault_id>/<name>`, as in
//! [`super::bus`]). A message naming any other network is refused.
//!
//! 1.9 R3: Hive answers a `reply_to` it does not know exactly like an
//! unknown message, `404 not_found` field `reply_to`. Message ids (`gid`)
//! are opaque strings; their order says nothing.
//!
//! Frames, on the bus WebSocket next to [`super::bus::BusFrameV1`]'s:
//! - `message` (client → service): [`AgentMessageV1`]; Hive answers the
//!   sender with `message.stored` ([`MessageStoredV1`], which carries the
//!   `gid` replies and threads need) or with `error` carrying the
//!   `client_msg_id`. The sender gets no `message.deliver` echo of its own
//!   message;
//! - `message.deliver` (service → client): [`MessageDeliveryV1`];
//! - `message.receipt` (client → service): [`MessageReceiptV1`], delivered/read;
//! - `message.status` (service → sender): [`MessageStatusV1`];
//! - `task.event` (service → client): [`TaskEventV1`]. Hive announces task
//!   changes; engines only write over HTTP (section 1).
//!
//! History: `GET /v1/messages?network=…&channel=…&thread_id=…&after_gid=…`
//! answers [`MessageListV1`] (lease right `bus.relay`).
//!
//! Every delivered body is **data, never instructions** (X5): an engine shows
//! it to its agent only wrapped as foreign content, whatever it says.
//! `trust` says whose data it is: `team` (an agent of the same organization)
//! or `untrusted` (anything else).
//!
//! Before sending, the engine runs a body through its policy and filter
//! packs like any other upload. A message always carries its `body` in
//! the clear.
//!
//! Retention is server policy (Team 30 days, Personal 7 days);
//! `expires_at` says when a message goes.

use serde::{Deserialize, Serialize};

use super::{require_schema, validate_id, validate_text};
use crate::{UtcTimestamp, ValidationError};

pub const MESSAGE_LIST_SCHEMA: &str = "leanctx.hive.message-list/v1";

pub const MAX_MESSAGE_TEXT_CHARS: usize = 4000;
pub const MAX_MESSAGE_REFERENCES: usize = 16;
pub const MAX_MESSAGES_PAGE: usize = 200;
/// A question's deadline lies at most this far ahead (default 24 h, set by
/// the organization).
pub const MAX_QUESTION_DEADLINE_HOURS: i64 = 7 * 24;

/// What a message is. Mandatory; Hive refuses others (`400
/// invalid_request`, `field` `type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageType {
    /// Asks other agents; may carry a deadline. Unanswered by then, Hive
    /// turns it into a task proposal (origin `question`).
    Question,
    /// Offers help or a capability.
    Offer,
    /// Announces handing work on (the binding handoff is `handoff.offer`).
    Handoff,
    /// Answers a question; needs `reply_to`.
    Answer,
    /// Reports something found, for whoever it concerns.
    Finding,
    #[serde(other)]
    Unknown,
}

/// Where a message lives: an org channel. Any other kind does not decode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum NetworkV1 {
    Org { channel: String },
}

impl NetworkV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        match self {
            Self::Org { channel } => super::bus::validate_channel(channel),
        }
    }
}

/// What a message points at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MessageReferenceV1 {
    /// A task of the channel's vault.
    Task {
        number: u64,
    },
    Fact {
        id: String,
    },
    Proposal {
        number: u64,
    },
    Commit {
        id: String,
    },
    Handoff {
        handoff_id: String,
    },
}

impl MessageReferenceV1 {
    fn validate(&self) -> Result<(), ValidationError> {
        match self {
            Self::Task { number } | Self::Proposal { number } if *number == 0 => {
                Err(ValidationError::new("numbers start at 1"))
            }
            Self::Task { .. } | Self::Proposal { .. } => Ok(()),
            Self::Fact { id } => validate_id(id, "reference id"),
            Self::Handoff { handoff_id } => validate_id(handoff_id, "handoff_id"),
            Self::Commit { id } => {
                if (7..=64).contains(&id.len())
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                {
                    Ok(())
                } else {
                    Err(ValidationError::new("commit references are 7-64 hex"))
                }
            }
        }
    }
}

/// A message body in the clear.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageBodyV1 {
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<MessageReferenceV1>,
}

impl MessageBodyV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_text(&self.text, MAX_MESSAGE_TEXT_CHARS, "text", true)?;
        if self.references.len() > MAX_MESSAGE_REFERENCES {
            return Err(ValidationError::new("too many references"));
        }
        self.references
            .iter()
            .try_for_each(MessageReferenceV1::validate)
    }
}

/// `message` (client → service).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentMessageV1 {
    /// Client-chosen ULID; resending the same id is a no-op.
    pub client_msg_id: String,
    pub network: NetworkV1,
    #[serde(rename = "type")]
    pub message_type: MessageType,
    /// The sending agent (bus agent id).
    pub from: String,
    /// A direct message to one agent (bus agent id); absent: everyone on
    /// the channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// 1.9 R3: the instance (`instance_id`, as `presence_update` shows it)
    /// that runs `to`. Hive then delivers only there, so another engine
    /// announcing the same agent name never receives it. Only with `to`. Without it Hive binds the message (or handoff
    /// offer) to the one other instance that announced `to` in the vault
    /// within 24 hours; when several did, it refuses with `409 invalid_state`
    /// reason `ambiguous_recipient` (RB7-M6) and the sender names one; when
    /// none did, `404 not_found` field `to`. Announcing the name later
    /// reaches nothing already sent. A reply goes to the instance of the replied
    /// message either way. An id that is no instance of the
    /// organization is `404 not_found` field `to_instance`, like an unknown
    /// one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_instance: Option<String>,
    /// The message this answers or replies to (`gid`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    /// For questions: when an unanswered question becomes a task proposal;
    /// Hive sets the organization's default when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline: Option<UtcTimestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<MessageBodyV1>,
}

impl AgentMessageV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_id(&self.client_msg_id, "client_msg_id")?;
        self.network.validate()?;
        validate_id(&self.from, "from")?;
        if let Some(to) = &self.to {
            validate_id(to, "to")?;
        }
        if let Some(instance) = &self.to_instance {
            validate_id(instance, "to_instance")?;
            if self.to.is_none() {
                return Err(ValidationError::new(
                    "to_instance names the instance of `to`",
                ));
            }
        }
        if let Some(reply_to) = &self.reply_to {
            validate_id(reply_to, "reply_to")?;
        }
        match self.message_type {
            MessageType::Unknown => return Err(ValidationError::new("unknown message type")),
            MessageType::Answer if self.reply_to.is_none() => {
                return Err(ValidationError::new("an answer replies to a question"));
            }
            MessageType::Question => {}
            _ if self.deadline.is_some() => {
                return Err(ValidationError::new("only questions carry a deadline"));
            }
            _ => {}
        }
        if let Some(deadline) = &self.deadline {
            super::parse_time(deadline)?;
        }
        self.body
            .as_ref()
            .ok_or_else(|| ValidationError::new("a message carries a body"))?
            .validate()
    }
}

/// Whose data a delivered message is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageTrust {
    /// From an agent or a person of the same organization.
    Team,
    /// Anything else.
    Untrusted,
    #[serde(other)]
    Unknown,
}

/// The sender of a delivered message, by network.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SenderV1 {
    /// An agent of the organization.
    Agent {
        agent: String,
        instance_label: String,
        /// The sending instance, set by Hive (RB7-M5): agent name, label
        /// and owner are chosen by the sender, this id is not. Use it as
        /// `to_instance` to answer exactly that engine.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        instance_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        owner: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        mine: bool,
        /// Section 5: the agent's profile.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        profile_id: Option<String>,
    },
    /// A person answering on the web.
    Person { label: String },
    /// Any other sender.
    #[serde(other)]
    Unknown,
}

/// `message.deliver` (service → client).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageDeliveryV1 {
    /// Global message id.
    pub gid: String,
    /// Position on the channel, gap-free.
    pub seq: u64,
    pub network: NetworkV1,
    #[serde(rename = "type")]
    pub message_type: MessageType,
    pub from: SenderV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// The first message of the thread (its own `gid` for a first message).
    pub thread_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline: Option<UtcTimestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<MessageBodyV1>,
    pub trust: MessageTrust,
    pub server_time: UtcTimestamp,
    pub expires_at: UtcTimestamp,
}

impl MessageDeliveryV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_id(&self.gid, "gid")?;
        validate_id(&self.thread_id, "thread_id")?;
        self.network.validate()?;
        if self.body.is_none() {
            return Err(ValidationError::new("a message carries a body"));
        }
        Ok(())
    }
}

/// How far a recipient got with a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageReceiptState {
    Delivered,
    Read,
    #[serde(other)]
    Unknown,
}

/// `message.receipt` (client → service).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageReceiptV1 {
    pub gid: String,
    /// The receiving agent.
    pub by: String,
    pub state: MessageReceiptState,
}

impl MessageReceiptV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_id(&self.gid, "gid")?;
        validate_id(&self.by, "by")?;
        if self.state == MessageReceiptState::Unknown {
            return Err(ValidationError::new("unknown receipt state"));
        }
        Ok(())
    }
}

/// `message.stored` (service → sender): the message was stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageStoredV1 {
    pub client_msg_id: String,
    pub gid: String,
    pub thread_id: String,
    pub network: NetworkV1,
    /// Position on the channel.
    pub seq: u64,
    pub server_time: UtcTimestamp,
    pub expires_at: UtcTimestamp,
}

/// `message.status` (service → sender): a recipient's receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageStatusV1 {
    pub gid: String,
    pub client_msg_id: String,
    pub by: String,
    pub state: MessageReceiptState,
    pub server_time: UtcTimestamp,
}

/// What happened to a task (section 1), as Hive announces it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskEventKind {
    Proposed,
    Opened,
    Claimed,
    Released,
    Submitted,
    Decided,
    Cancelled,
    #[serde(other)]
    Unknown,
}

/// `task.event` (service → client), on the task's vault channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskEventV1 {
    pub channel: String,
    pub seq: u64,
    pub number: u64,
    pub event: TaskEventKind,
    pub state: super::tasks::TaskState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<super::tasks::TaskActorV1>,
    pub server_time: UtcTimestamp,
}

/// `GET /v1/messages`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageListV1 {
    pub schema: String,
    pub messages: Vec<MessageDeliveryV1>,
    /// Pass as `after_gid` for the next page; absent at the end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
}

impl MessageListV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, MESSAGE_LIST_SCHEMA)?;
        if self.messages.len() > MAX_MESSAGES_PAGE {
            return Err(ValidationError::new("too many messages in one page"));
        }
        self.messages
            .iter()
            .try_for_each(MessageDeliveryV1::validate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn question() -> AgentMessageV1 {
        AgentMessageV1 {
            client_msg_id: "01JBMSG".into(),
            network: NetworkV1::Org {
                channel: "org:org_acme/vault:3f6c1e9a0b7d4c2e8f5a1b6c9d0e7f42/general".into(),
            },
            message_type: MessageType::Question,
            from: "claude-3e".into(),
            to: None,
            to_instance: None,
            reply_to: None,
            deadline: None,
            body: Some(MessageBodyV1 {
                text: "Which retry budget does payments use?".into(),
                references: Vec::new(),
            }),
        }
    }

    #[test]
    fn answers_reply_and_only_questions_have_deadlines() {
        assert!(question().validate().is_ok());
        let mut answer = question();
        answer.message_type = MessageType::Answer;
        assert!(answer.validate().is_err(), "an answer needs reply_to");
        answer.reply_to = Some("g-1".into());
        assert!(answer.validate().is_ok());
        answer.deadline = Some(UtcTimestamp::new("2026-10-21T12:00:00Z").unwrap());
        assert!(answer.validate().is_err());
    }

    #[test]
    fn a_swarm_network_does_not_decode_and_a_swarm_sender_is_unknown() {
        let raw = serde_json::json!({
            "client_msg_id": "01JBMSG", "network": {"kind": "swarm", "swarm_id": "sw-1"},
            "type": "finding", "from": "a", "body": {"text": "hi"},
        });
        assert!(serde_json::from_value::<AgentMessageV1>(raw).is_err());
        let sender: SenderV1 = serde_json::from_value(
            serde_json::json!({"kind": "swarm", "handle": "quiet-heron", "reputation": 1}),
        )
        .unwrap();
        assert_eq!(sender, SenderV1::Unknown);
    }

    #[test]
    fn the_type_is_mandatory() {
        let raw = serde_json::json!({
            "client_msg_id": "01JBMSG", "network": {"kind": "org", "channel": "org:o/vault:3f6c1e9a0b7d4c2e8f5a1b6c9d0e7f42/g"},
            "from": "a", "body": {"text": "hi"},
        });
        assert!(serde_json::from_value::<AgentMessageV1>(raw).is_err());
    }
}
