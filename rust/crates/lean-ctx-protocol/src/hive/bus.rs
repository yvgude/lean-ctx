// SPDX-License-Identifier: Apache-2.0

//! Cross-instance agent bus.
//!
//! - `wss://<host>/v1/bus` (bearer token): JSON text frames [`BusFrameV1`],
//!   tagged by `"t"`. The client sends `hello` first. 1.9 R3: it may send
//!   `hello` again on the open socket whenever its agents change; a later
//!   `hello` replaces the announced agents (its `instance_id` must be the
//!   socket's, otherwise `error` and close), and its `cursors` replay newer
//!   events only for channels the socket did not follow yet. A frame for an
//!   agent that no `hello` on this socket announced is refused.
//! - Fallback `GET /v1/bus/poll?cursor=` → [`BusPollV1`].
//! - Catch-up `GET /v1/bus/channels/{channel}/events?after_seq=&limit=` (≤500)
//!   → [`BusEventsV1`].
//! - Leases `POST /v1/bus/leases` [`BusLeaseRequestV1`] → [`BusLeaseV1`];
//!   `POST /v1/bus/leases/{lease_id}/renew` [`BusLeaseRenewRequestV1`] →
//!   [`BusLeaseV1`]; `DELETE /v1/bus/leases/{lease_id}?fencing_token=` → 204.
//!
//! Channels are `org:<org_id>/vault:<vault_id>/<name>`. The service
//! assigns `seq` per channel, strictly increasing, and stamps `server_time`.
//! A handoff completes only on `handoff.ack`; until its deadline the service
//! redelivers the offer. Lease expiry is decided by the service clock.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

use super::error::HiveErrorDetailV1;
use super::{require_schema, validate_id, validate_text};
use crate::{UtcTimestamp, ValidationError};

pub const BUS_POLL_SCHEMA: &str = "leanctx.hive.bus-poll/v1";
pub const BUS_EVENTS_SCHEMA: &str = "leanctx.hive.bus-events/v1";
pub const BUS_LEASE_REQUEST_SCHEMA: &str = "leanctx.hive.bus-lease-request/v1";
pub const BUS_LEASE_RENEW_REQUEST_SCHEMA: &str = "leanctx.hive.bus-lease-renew-request/v1";
pub const BUS_LEASE_SCHEMA: &str = "leanctx.hive.bus-lease/v1";

/// Largest serialized `body` of one message.
pub const MAX_BUS_BODY_BYTES: usize = 64 * 1024;
/// Events per catch-up page.
pub const MAX_BUS_EVENTS_PAGE: usize = 500;
/// Longest bus lease.
pub const MAX_BUS_LEASE_MS: u64 = 15 * 60 * 1000;

/// Validate `org:<org>/vault:<vault_id>/<name>`.
pub fn validate_channel(channel: &str) -> Result<(), ValidationError> {
    let mut parts = channel.splitn(3, '/');
    let org = parts.next().and_then(|part| part.strip_prefix("org:"));
    let vault = parts.next().and_then(|part| part.strip_prefix("vault:"));
    let name = parts.next();
    match (org, vault, name) {
        (Some(org), Some(vault), Some(name)) => {
            validate_id(org, "channel org")?;
            super::validate_vault_id(vault)?;
            validate_id(name, "channel name")
        }
        _ => Err(ValidationError::new(
            "channel must be org:<org>/vault:<vault>/<name>",
        )),
    }
}

/// Agent presence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresenceState {
    Active,
    Idle,
    Closed,
}

/// Message priority (same values as the local bus).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BusPriority {
    Low,
    Normal,
    High,
    Critical,
}

/// Message privacy (same values as the local bus). `private` bodies are
/// redacted by the engine before publish and delivered only to `to`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BusPrivacy {
    Public,
    Team,
    Private,
}

/// Control actions an org admin (or the lead agent with `bus.control`) issues.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlAction {
    /// Refuse mutating tools until `resume`.
    Hold,
    /// Refuse the lease or handoff named by `target_ref`.
    Veto,
    /// Refuse mutating tools; the agent should end its task.
    Stop,
    Resume,
}

/// An agent announced in `hello`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BusAgentV1 {
    pub agent_id: String,
    pub role: String,
    pub vault_id: String,
}

/// Current lease holder, when a lease was not granted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BusLeaseHolderV1 {
    pub agent_id: String,
    pub instance_id: String,
    pub expires_at: UtcTimestamp,
}

/// One WebSocket frame (also the items of [`BusPollV1`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t")]
pub enum BusFrameV1 {
    /// client → service, first frame.
    #[serde(rename = "hello")]
    Hello {
        instance_id: String,
        agents: Vec<BusAgentV1>,
        /// Last `seq` seen per channel; the service replays newer events.
        #[serde(default)]
        cursors: BTreeMap<String, u64>,
    },
    /// client → service.
    #[serde(rename = "presence")]
    Presence {
        agent_id: String,
        state: PresenceState,
        /// 1.9 (section 5): the agent's self-reported profile.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        profile: Option<super::profiles::PresenceProfileV1>,
    },
    /// client → service.
    #[serde(rename = "publish")]
    Publish {
        /// Client-chosen ULID; republishing the same id is a no-op.
        client_msg_id: String,
        channel: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to: Option<String>,
        from: String,
        kind: String,
        priority: BusPriority,
        privacy: BusPrivacy,
        body: Value,
    },
    /// client → service: everything up to `seq` on `channel` is processed.
    #[serde(rename = "ack")]
    Ack { channel: String, seq: u64 },
    /// service → client: a publish was stored.
    #[serde(rename = "published")]
    Published {
        client_msg_id: String,
        channel: String,
        seq: u64,
        server_time: UtcTimestamp,
    },
    /// service → client.
    #[serde(rename = "deliver")]
    Deliver {
        seq: u64,
        channel: String,
        from: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to: Option<String>,
        kind: String,
        priority: BusPriority,
        privacy: BusPrivacy,
        body: Value,
        server_time: UtcTimestamp,
    },
    /// service → client.
    #[serde(rename = "presence_update")]
    PresenceUpdate {
        agent_id: String,
        instance_id: String,
        role: String,
        vault_id: String,
        state: PresenceState,
        server_time: UtcTimestamp,
        /// 1.9 (section 5): the agent's profile id.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        profile_id: Option<String>,
        /// 1.9 (section 5): what the agent announced.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        profile: Option<super::profiles::PresenceProfileV1>,
    },
    /// service → client.
    #[serde(rename = "control")]
    Control {
        control_id: String,
        /// Absent: every agent of the instance.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_agent: Option<String>,
        /// Lease or handoff id for `veto`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_ref: Option<String>,
        action: ControlAction,
        reason: String,
        issued_by: String,
        seq: u64,
        server_time: UtcTimestamp,
    },
    /// either direction.
    #[serde(rename = "handoff.offer")]
    HandoffOffer {
        handoff_id: String,
        channel: String,
        from: String,
        to: String,
        /// Reference to a context bundle the receiver fetches; never the content.
        bundle_ref: String,
        deadline: UtcTimestamp,
    },
    /// receiver → service → offerer.
    #[serde(rename = "handoff.ack")]
    HandoffAck { handoff_id: String, by: String },
    /// receiver → service → offerer.
    #[serde(rename = "handoff.reject")]
    HandoffReject {
        handoff_id: String,
        by: String,
        reason: String,
    },
    /// 1.9 (section 2), client → service: an agent message.
    #[serde(rename = "message")]
    Message(super::messages::AgentMessageV1),
    /// 1.9, service → sender: the message was stored (its `gid`).
    #[serde(rename = "message.stored")]
    MessageStored(super::messages::MessageStoredV1),
    /// 1.9, service → client.
    #[serde(rename = "message.deliver")]
    MessageDeliver(super::messages::MessageDeliveryV1),
    /// 1.9, client → service: delivered or read.
    #[serde(rename = "message.receipt")]
    MessageReceipt(super::messages::MessageReceiptV1),
    /// 1.9, service → sender.
    #[serde(rename = "message.status")]
    MessageStatus(super::messages::MessageStatusV1),
    /// 1.9, service → client: a task changed (section 1).
    #[serde(rename = "task.event")]
    TaskEvent(super::messages::TaskEventV1),
    /// 1.9, service → client: a knowledge revision was released,
    /// quarantined or rolled back (section 3).
    #[serde(rename = "knowledge.event")]
    KnowledgeEvent(super::knowledge::KnowledgeEventV1),
    #[serde(rename = "ping")]
    Ping { nonce: u64 },
    #[serde(rename = "pong")]
    Pong { nonce: u64 },
    /// service → client: a frame was refused; the socket stays open.
    #[serde(rename = "error")]
    Error {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_msg_id: Option<String>,
        error: HiveErrorDetailV1,
    },
    /// A frame type this engine does not know; ignored.
    #[serde(other)]
    Unknown,
}

impl BusFrameV1 {
    /// Checks the service applies to client frames (and the engine before sending).
    pub fn validate(&self) -> Result<(), ValidationError> {
        match self {
            Self::Hello {
                instance_id,
                agents,
                cursors,
            } => {
                validate_id(instance_id, "instance_id")?;
                if agents.len() > 64 || cursors.len() > 256 {
                    return Err(ValidationError::new("hello lists are too long"));
                }
                for agent in agents {
                    validate_id(&agent.agent_id, "agent_id")?;
                    validate_text(&agent.role, 64, "role", false)?;
                    super::validate_vault_id(&agent.vault_id)?;
                }
                cursors
                    .keys()
                    .try_for_each(|channel| validate_channel(channel))
            }
            Self::Presence {
                agent_id, profile, ..
            } => {
                validate_id(agent_id, "agent_id")?;
                profile
                    .as_ref()
                    .map_or(Ok(()), super::profiles::PresenceProfileV1::validate)
            }
            Self::Publish {
                client_msg_id,
                channel,
                to,
                from,
                kind,
                body,
                ..
            } => {
                validate_id(client_msg_id, "client_msg_id")?;
                validate_channel(channel)?;
                validate_id(from, "from")?;
                if let Some(to) = to {
                    validate_id(to, "to")?;
                }
                validate_id(kind, "kind")?;
                validate_body(body)
            }
            Self::Ack { channel, .. } => validate_channel(channel),
            Self::HandoffOffer {
                handoff_id,
                channel,
                from,
                to,
                bundle_ref,
                ..
            } => {
                validate_id(handoff_id, "handoff_id")?;
                validate_channel(channel)?;
                validate_id(from, "from")?;
                validate_id(to, "to")?;
                validate_text(bundle_ref, 512, "bundle_ref", false)
            }
            Self::HandoffAck { handoff_id, by } => {
                validate_id(handoff_id, "handoff_id")?;
                validate_id(by, "by")
            }
            Self::HandoffReject {
                handoff_id,
                by,
                reason,
            } => {
                validate_id(handoff_id, "handoff_id")?;
                validate_id(by, "by")?;
                validate_text(reason, 500, "reason", false)
            }
            Self::Message(message) => message.validate(),
            Self::MessageReceipt(receipt) => receipt.validate(),
            Self::Ping { .. } | Self::Pong { .. } => Ok(()),
            Self::Published { .. }
            | Self::Deliver { .. }
            | Self::PresenceUpdate { .. }
            | Self::Control { .. }
            | Self::MessageStored(_)
            | Self::MessageDeliver(_)
            | Self::MessageStatus(_)
            | Self::TaskEvent(_)
            | Self::KnowledgeEvent(_)
            | Self::Error { .. }
            | Self::Unknown => Err(ValidationError::new("not a client frame")),
        }
    }
}

fn validate_body(body: &Value) -> Result<(), ValidationError> {
    let size = serde_json::to_vec(body)
        .map_err(|error| ValidationError::new(format!("body: {error}")))?
        .len();
    if size > MAX_BUS_BODY_BYTES {
        return Err(ValidationError::new("bus body exceeds 64 KiB"));
    }
    Ok(())
}

/// Response of `GET /v1/bus/poll`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BusPollV1 {
    pub schema: String,
    pub frames: Vec<BusFrameV1>,
    /// Opaque; pass back as `cursor`.
    pub cursor: String,
}

/// Response of `GET /v1/bus/channels/{channel}/events`; `events` are `deliver` frames.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BusEventsV1 {
    pub schema: String,
    pub channel: String,
    pub events: Vec<BusFrameV1>,
    /// Pass as `after_seq` for the next page; absent when caught up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_after_seq: Option<u64>,
}

impl BusEventsV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, BUS_EVENTS_SCHEMA)?;
        validate_channel(&self.channel)?;
        if self.events.len() > MAX_BUS_EVENTS_PAGE {
            return Err(ValidationError::new("event page is too long"));
        }
        let mut previous = None;
        for event in &self.events {
            let BusFrameV1::Deliver { seq, channel, .. } = event else {
                return Err(ValidationError::new("catch-up events are deliver frames"));
            };
            if channel != &self.channel || previous.is_some_and(|previous| *seq <= previous) {
                return Err(ValidationError::new(
                    "events must belong to the channel in ascending seq",
                ));
            }
            previous = Some(*seq);
        }
        Ok(())
    }
}

/// `POST /v1/bus/leases`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BusLeaseRequestV1 {
    pub schema: String,
    /// e.g. `file`, `task`, `branch`.
    pub resource_kind: String,
    /// Lowercase hex SHA-256 of the repo-relative resource; never a path.
    pub resource_ref: String,
    pub vault_id: String,
    pub owner_agent_id: String,
    pub duration_ms: u64,
}

impl BusLeaseRequestV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, BUS_LEASE_REQUEST_SCHEMA)?;
        validate_id(&self.resource_kind, "resource_kind")?;
        super::validate_hex64(&self.resource_ref, "resource_ref")?;
        super::validate_vault_id(&self.vault_id)?;
        validate_id(&self.owner_agent_id, "owner_agent_id")?;
        validate_duration(self.duration_ms)
    }
}

fn validate_duration(duration_ms: u64) -> Result<(), ValidationError> {
    if (1..=MAX_BUS_LEASE_MS).contains(&duration_ms) {
        Ok(())
    } else {
        Err(ValidationError::new("duration_ms must be 1-900000"))
    }
}

/// `POST /v1/bus/leases/{lease_id}/renew`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BusLeaseRenewRequestV1 {
    pub schema: String,
    pub fencing_token: u64,
    pub duration_ms: u64,
}

impl BusLeaseRenewRequestV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, BUS_LEASE_RENEW_REQUEST_SCHEMA)?;
        validate_duration(self.duration_ms)
    }
}

/// Lease decision. Granted: `lease_id`, `fencing_token` (strictly increasing
/// per resource) and `expires_at`. Refused: `holder`. 1.9 erratum: a renewal
/// keeps `lease_id` and `fencing_token` and only moves `expires_at`; a new
/// token comes only with a new grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BusLeaseV1 {
    pub schema: String,
    pub granted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fencing_token: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<UtcTimestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub holder: Option<BusLeaseHolderV1>,
    pub server_time: UtcTimestamp,
}

impl BusLeaseV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, BUS_LEASE_SCHEMA)?;
        let granted_fields =
            self.lease_id.is_some() && self.fencing_token.is_some() && self.expires_at.is_some();
        if self.granted != granted_fields || (self.granted && self.holder.is_some()) {
            return Err(ValidationError::new(
                "a granted lease has id, token and expiry; a refused one has none",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channels_have_three_parts() {
        assert!(
            validate_channel("org:org_1/vault:3f6c1e9a0b7d4c2e8f5a1b6c9d0e7f42/general").is_ok()
        );
        assert!(validate_channel("org:org_1/vault:prj_1/general").is_err());
        assert!(validate_channel("org:org_1/general").is_err());
        assert!(validate_channel("vault:prj_1/org:org_1/general").is_err());
    }

    #[test]
    fn unknown_frames_are_tolerated() {
        let frame: BusFrameV1 = serde_json::from_str(r#"{"t":"future","x":1}"#).unwrap();
        assert_eq!(frame, BusFrameV1::Unknown);
    }
}
