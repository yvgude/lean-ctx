// SPDX-License-Identifier: Apache-2.0
//! Bus v2 wire shapes the engine sends and reads (C0 2.0 section 3,
//! `bus.schema.json`; SPEC-ENGINE §3.7): typed messages on vault and swarm
//! buses, Hive-signed controls and presence.
//!
//! Network-free: the relay builds and checks these, the socket moves them.

use std::collections::BTreeMap;

use ed25519_dalek::{Signature, Verifier as _, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::address::{Address, is_worker_id};

pub const BUS_SCHEMA: &str = "leanctx.hive.bus/v2";
pub const CONTROL_DOMAIN: &str = "leanctx.hive.control.v1\n";
const TEXT_MAX: usize = 4000;
const REFS_MAX: usize = 10;

/// The nine message types (SPEC-ENGINE §3.7.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageType {
    Question,
    Offer,
    Handoff,
    Finding,
    Answer,
    Need,
    Claim,
    Progress,
    Result,
}

impl MessageType {
    pub fn parse(s: &str) -> Option<Self> {
        serde_json::from_value(Value::String(s.to_string())).ok()
    }

    /// Phase A on vault buses (V6.2a); `need`, `claim` and `result` follow
    /// with swarms (V6.2c/d).
    pub fn in_phase_a(self) -> bool {
        matches!(
            self,
            Self::Question
                | Self::Answer
                | Self::Finding
                | Self::Offer
                | Self::Handoff
                | Self::Progress
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ref {
    pub kind: RefKind,
    #[serde(rename = "ref")]
    pub target: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefKind {
    Node,
    Msg,
    File,
    Commit,
    Url,
    Test,
}

/// E17 body and `place.publish` frame body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Publish {
    pub schema: String,
    pub client_msg_id: String,
    pub to: String,
    #[serde(rename = "type")]
    pub kind: MessageType,
    pub worker_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refs: Vec<Ref>,
    pub body: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ck_wrapped: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub epoch: Option<u32>,
    /// `collaboration` marks a friction finding (C0 d072c29): it carries
    /// plaintext `counts` and a `window` and may have no refs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub about: Option<About>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counts: Option<CollabCounts>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<Window>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum About {
    Collaboration,
}

/// The friction signals of a collaboration finding (`enums/friction-signals.json`);
/// every key optional, unknown keys refused.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollabCounts {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registration_renewed: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_timeout: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_conflict: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim_refused: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim_expired: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate_reruns: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_seconds: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_seconds: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Window {
    pub from_ms: u64,
    pub to_ms: u64,
}

/// Why a message may not be published (local refusal, before the wire).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invalid(pub String);

impl std::fmt::Display for Invalid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid_message: {}", self.0)
    }
}

/// The C0 `sender` of a stored message. The web-only `place_role` (W36)
/// never reaches an engine, so a sender carrying it is refused like any
/// other unknown member.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Sender {
    Worker {
        worker_id: String,
        name: String,
        owner_handle: String,
        host_app: String,
    },
    Person {
        handle: String,
    },
    System,
}

/// The sender of a stored message, refused when it is not the C0 shape.
pub fn sender_of(message: &Value) -> Result<Sender, Invalid> {
    let sender = message.get("sender").ok_or(Invalid("sender".into()))?;
    let parsed: Sender =
        serde_json::from_value(sender.clone()).map_err(|e| Invalid(format!("sender: {e}")))?;
    if let Sender::Worker { worker_id, .. } = &parsed
        && !is_worker_id(worker_id)
    {
        return Err(Invalid("sender.worker_id".into()));
    }
    Ok(parsed)
}

fn bad(why: impl Into<String>) -> Invalid {
    Invalid(why.into())
}

fn chars(value: &Value, field: &str, min: usize, max: usize) -> Result<(), Invalid> {
    let n = value[field]
        .as_str()
        .ok_or_else(|| bad(format!("{field} is required")))?
        .chars()
        .count();
    if (min..=max).contains(&n) {
        Ok(())
    } else {
        Err(bad(format!("{field} must be {min} to {max} characters")))
    }
}

fn opt_chars(value: &Value, field: &str, min: usize, max: usize) -> Result<(), Invalid> {
    if value.get(field).is_some() {
        chars(value, field, min, max)
    } else {
        Ok(())
    }
}

fn int(value: &Value, field: &str, min: u64, max: u64, required: bool) -> Result<(), Invalid> {
    match value.get(field) {
        None if !required => Ok(()),
        None => Err(bad(format!("{field} is required"))),
        Some(v) => v
            .as_u64()
            .filter(|n| (min..=max).contains(n))
            .map(|_| ())
            .ok_or_else(|| bad(format!("{field} must be an integer {min} to {max}"))),
    }
}

fn worker(value: &Value, field: &str, required: bool) -> Result<(), Invalid> {
    match value.get(field) {
        None if !required => Ok(()),
        Some(Value::String(id)) if is_worker_id(id) => Ok(()),
        _ => Err(bad(format!("{field} must be a worker id"))),
    }
}

fn only(body: &Map<String, Value>, allowed: &[&str]) -> Result<(), Invalid> {
    match body.keys().find(|k| !allowed.contains(&k.as_str())) {
        Some(extra) => Err(bad(format!("unknown body field {extra}"))),
        None => Ok(()),
    }
}

/// The engine-sent body of each type (server-set fields are refused).
fn check_body(kind: MessageType, body: &Value) -> Result<(), Invalid> {
    let map = body
        .as_object()
        .ok_or_else(|| bad("body must be an object"))?;
    match kind {
        MessageType::Question => {
            only(map, &["text", "to_worker", "deadline"])?;
            chars(body, "text", 1, TEXT_MAX)?;
            worker(body, "to_worker", false)?;
            int(body, "deadline", 0, u64::MAX, false)
        }
        MessageType::Offer => {
            only(map, &["what", "capability", "eta_minutes", "withdraw"])?;
            chars(body, "what", 1, 1000)?;
            chars(body, "capability", 1, 64)?;
            int(body, "eta_minutes", 1, 10_080, true)?;
            match body.get("withdraw") {
                None | Some(Value::Bool(_)) => Ok(()),
                _ => Err(bad("withdraw must be true or false")),
            }
        }
        MessageType::Handoff => {
            only(map, &["to_worker", "resource", "note", "lease_id"])?;
            worker(body, "to_worker", true)?;
            chars(body, "resource", 1, 200)?;
            opt_chars(body, "note", 1, 1000)?;
            lease(body, false)
        }
        MessageType::Finding => {
            only(map, &["statement", "evidence"])?;
            chars(body, "statement", 24, 300)?;
            match body["evidence"].as_str() {
                Some("observed" | "stated" | "imported") => Ok(()),
                _ => Err(bad("evidence must be observed, stated or imported")),
            }
        }
        MessageType::Answer => {
            only(map, &["text", "accept"])?;
            chars(body, "text", 1, TEXT_MAX)?;
            match body.get("accept") {
                None | Some(Value::Bool(_)) => Ok(()),
                _ => Err(bad("accept must be true or false")),
            }
        }
        MessageType::Need => {
            only(map, &["text", "capabilities", "budget_hint", "expires_at"])?;
            chars(body, "text", 1, 1000)?;
            let caps = body["capabilities"]
                .as_array()
                .ok_or_else(|| bad("capabilities is required"))?;
            if !(1..=5).contains(&caps.len())
                || caps.iter().any(|c| {
                    c.as_str()
                        .is_none_or(|c| c.is_empty() || c.chars().count() > 64)
                })
            {
                return Err(bad("capabilities must name 1 to 5 tools or hosts"));
            }
            if let Some(hint) = body.get("budget_hint") {
                let hint_map = hint
                    .as_object()
                    .ok_or_else(|| bad("budget_hint must be an object"))?;
                only(hint_map, &["tasks", "usd_cents"])?;
                int(hint, "tasks", 1, 50, false)?;
                int(hint, "usd_cents", 0, 50_000, false)?;
            }
            int(body, "expires_at", 0, u64::MAX, false)
        }
        MessageType::Claim => {
            only(map, &["resource", "lease_id"])?;
            chars(body, "resource", 1, 200)?;
            lease(body, true)
        }
        MessageType::Progress => {
            only(map, &["percentage", "step", "usage", "goal"])?;
            if map.get("percentage").is_none() && map.get("step").is_none() {
                return Err(bad("progress needs percentage or step"));
            }
            int(body, "percentage", 0, 100, false)?;
            opt_chars(body, "step", 1, 300)
        }
        MessageType::Result => {
            only(map, &["summary", "artefacts", "goal"])?;
            chars(body, "summary", 1, 1000)?;
            match body["artefacts"].as_array() {
                Some(a) if a.len() <= 20 => Ok(()),
                _ => Err(bad("artefacts must be a list of at most 20")),
            }
        }
    }
}

fn lease(body: &Value, required: bool) -> Result<(), Invalid> {
    match body.get("lease_id") {
        None if !required => Ok(()),
        Some(Value::String(id))
            if id.strip_prefix("bl_").is_some_and(|r| {
                (1..=40).contains(&r.len())
                    && r.bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
            }) =>
        {
            Ok(())
        }
        _ => Err(bad("lease_id must be bl_<id>")),
    }
}

fn is_msg_id(s: &str) -> bool {
    s.strip_prefix("msg_").is_some_and(|r| {
        r.len() == 16
            && r.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

impl Publish {
    /// The C0 2.0 rules for an engine publish. A sealed body is opaque
    /// here: only its key material is checked.
    pub fn validate(&self) -> Result<(), Invalid> {
        if self.schema != BUS_SCHEMA {
            return Err(bad("schema"));
        }
        if !is_ulid(&self.client_msg_id) {
            return Err(bad("client_msg_id must be a ULID"));
        }
        match Address::parse(Some(&self.to)) {
            Ok(a) if a.is_remote() => {}
            _ => return Err(bad("to must be #<ws>/<vault>, ~<swarm> or @wkr_<id>")),
        }
        if !is_worker_id(&self.worker_id) {
            return Err(bad("worker_id"));
        }
        for id in [&self.thread_id, &self.reply_to].into_iter().flatten() {
            if !is_msg_id(id) {
                return Err(bad("thread_id and reply_to must be msg_ ids"));
            }
        }
        if self.refs.len() > REFS_MAX
            || self
                .refs
                .iter()
                .any(|r| r.target.is_empty() || r.target.chars().count() > 500)
        {
            return Err(bad("refs: at most 10, each 1 to 500 characters"));
        }
        let sealed = self.body.get("sealed").is_some();
        let collaboration = self.about.is_some();
        if collaboration {
            if self.kind != MessageType::Finding {
                return Err(bad("about: collaboration is for findings only"));
            }
            if sealed {
                return Err(bad("collaboration counts never go to a sealed place"));
            }
            if self.counts.is_none() || self.window.is_none() {
                return Err(bad("a collaboration finding needs counts and window"));
            }
        } else if self.counts.is_some() || self.window.is_some() {
            return Err(bad("counts and window need about: collaboration"));
        }
        if self.kind == MessageType::Finding && self.refs.is_empty() && !collaboration {
            return Err(bad("a finding needs at least one ref"));
        }
        if self.kind == MessageType::Answer && self.reply_to.is_none() {
            return Err(bad("an answer needs reply_to"));
        }
        match (sealed, &self.ck_wrapped, self.epoch) {
            (true, Some(ck), Some(_)) => {
                let len_ok = ck.len() == 96
                    && ck
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
                if !len_ok {
                    return Err(bad("ck_wrapped"));
                }
                Ok(())
            }
            (true, _, _) => Err(bad("a sealed body needs ck_wrapped and epoch")),
            (false, None, None) => check_body(self.kind, &self.body),
            (false, _, _) => Err(bad("key material on a plain place")),
        }
    }
}

/// Crockford base32 upper, 26 characters (ULID layout).
pub fn is_ulid(s: &str) -> bool {
    s.len() == 26
        && s.bytes().all(|b| {
            b.is_ascii_digit()
                || (b.is_ascii_uppercase() && !matches!(b, b'I' | b'L' | b'O' | b'U'))
        })
}

/// A new ULID: 48-bit ms time, 80 random bits.
pub fn new_ulid(now_ms: u64) -> String {
    let mut random = [0u8; 10];
    let _ = getrandom::fill(&mut random);
    ulid_of(now_ms, random)
}

/// The ULID of one local message: its time and 80 bits of
/// `blake3(seed)`, so a resend after a reconnect keeps its `client_msg_id`
/// and Hive deduplicates it.
pub fn ulid_for(at_ms: u64, seed: &str) -> String {
    let digest = blake3::hash(seed.as_bytes());
    let mut bits = [0u8; 10];
    bits.copy_from_slice(&digest.as_bytes()[..10]);
    ulid_of(at_ms, bits)
}

fn ulid_of(ms: u64, low: [u8; 10]) -> String {
    const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let mut value: u128 = u128::from(ms & 0xFFFF_FFFF_FFFF) << 80;
    for (i, byte) in low.iter().enumerate() {
        value |= u128::from(*byte) << (8 * (9 - i));
    }
    (0..26)
        .rev()
        .map(|i| ALPHABET[((value >> (5 * i)) & 31) as usize] as char)
        .collect()
}

/// Hive-signed Control (R8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Control {
    pub id: String,
    pub place: String,
    pub target: ControlTarget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_id: Option<String>,
    #[serde(default, rename = "ref", skip_serializing_if = "Option::is_none")]
    pub target_ref: Option<ControlRef>,
    pub action: ControlAction,
    pub reason: String,
    pub issued_by: IssuedBy,
    pub created_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifted_at: Option<u64>,
    pub key_id: String,
    pub signature: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlTarget {
    Worker,
    Bus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlAction {
    Hold,
    Stop,
    Resume,
    Veto,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlRef {
    pub kind: String,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IssuedBy {
    pub kind: String,
    pub user_id: String,
    /// Display name of the issuer (≤ 64; C0 b000ffd), covered by the
    /// signature like every other member.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// Why a control frame is ignored (counted `sig_invalid` or `invalid`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlRejection {
    /// Shape breaks the contract (target/worker_id, veto without ref,
    /// issued/lifted mismatch).
    Invalid,
    /// Unknown key, a key without purpose `control`, or a bad signature.
    SigInvalid,
}

/// Hive keys a control may be signed with: `key_id` → public key of
/// purpose `control` (from the E13 key set).
pub type ControlKeys = BTreeMap<String, VerifyingKey>;

impl Control {
    /// Check shape and signature (`domain ‖ canonical_json(control without
    /// signature)`, vector `control.vectors.json`). `lifted` is the frame's
    /// `state == lifted`.
    pub fn verify(&self, lifted: bool, keys: &ControlKeys) -> Result<(), ControlRejection> {
        let shape_ok = (self.target == ControlTarget::Worker) == self.worker_id.is_some()
            && (self.action != ControlAction::Veto || self.target_ref.is_some())
            && lifted == self.lifted_at.is_some()
            && (2..=500).contains(&self.reason.chars().count())
            && self.issued_by.kind == "person";
        if !shape_ok {
            return Err(ControlRejection::Invalid);
        }
        let key = keys.get(&self.key_id).ok_or(ControlRejection::SigInvalid)?;
        let value = serde_json::to_value(self).map_err(|_| ControlRejection::Invalid)?;
        let input =
            lean_ctx_protocol::hive::signing_input_without_signature(CONTROL_DOMAIN, &value)
                .map_err(|_| ControlRejection::Invalid)?;
        let mut sig = [0u8; 64];
        hex::decode_to_slice(&self.signature, &mut sig)
            .map_err(|_| ControlRejection::SigInvalid)?;
        key.verify(&input, &Signature::from_bytes(&sig))
            .map_err(|_| ControlRejection::SigInvalid)
    }
}

/// Presence of a local worker on one place (SPEC-ENGINE §3.7.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresenceState {
    Active,
    Idle,
    Held,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresenceEntry {
    pub worker_id: String,
    pub role: String,
    pub state: PresenceState,
}

/// The frames the engine reads; others are ignored by the relay.
#[derive(Debug, Clone, PartialEq)]
pub enum Inbound {
    HelloAck(Value),
    Stored {
        client_msg_id: String,
        id: String,
        seq: u64,
        thread_id: String,
    },
    Deliver(Value),
    Presence {
        address: String,
        worker_id: String,
        role: String,
        state: PresenceState,
        at: u64,
    },
    Control {
        lifted: bool,
        control: Box<Control>,
    },
    PlaceDenied {
        place: String,
        code: String,
    },
    Resync {
        place: String,
    },
    Error {
        client_msg_id: Option<String>,
        code: String,
        reason: Option<String>,
        /// A `quota_exceeded` frame's optional quota fields (`resource`,
        /// `limit`, `used`, `reset_at`, `retry_after_s`), only those of the
        /// expected type; `None` when the frame carries none.
        quota: Option<Box<Value>>,
    },
    Ping,
    /// The person answered a consent in the browser (§3.8.2 step 4).
    ConsentAnswered {
        consent_id: String,
    },
    Other(String),
}

/// The quota fields of an `error` frame that have their C0 type (strings
/// `resource`/`reset_at`, unsigned `limit`/`used`/`retry_after_s`); a field
/// of another type is left out, so an odd one never hides the others.
fn quota_fields(frame: &Value) -> Option<Box<Value>> {
    let mut out = serde_json::Map::new();
    for (field, string) in [
        ("resource", true),
        ("reset_at", true),
        ("limit", false),
        ("used", false),
        ("retry_after_s", false),
    ] {
        let value = &frame[field];
        if field == "retry_after_s" {
            // Capped here as well: a hostile frame never reaches time math.
            if let Some(s) = value.as_u64() {
                let capped = s.min(crate::core::hive::error::RETRY_AFTER_CAP_S);
                out.insert(field.to_string(), Value::from(capped));
            }
        } else if (string && value.is_string()) || (!string && value.is_u64()) {
            out.insert(field.to_string(), value.clone());
        }
    }
    (!out.is_empty()).then(|| Box::new(Value::Object(out)))
}

/// Parse one text frame (flat `{t, ...body}`).
pub fn parse_frame(text: &str) -> Result<Inbound, Invalid> {
    let value: Value = serde_json::from_str(text).map_err(|e| bad(format!("frame: {e}")))?;
    let t = value["t"]
        .as_str()
        .ok_or_else(|| bad("frame without t"))?
        .to_string();
    let s = |field: &str| -> Result<String, Invalid> {
        value[field]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| bad(format!("{t}: {field}")))
    };
    let n = |field: &str| -> Result<u64, Invalid> {
        value[field]
            .as_u64()
            .ok_or_else(|| bad(format!("{t}: {field}")))
    };
    Ok(match t.as_str() {
        "hello.ack" => Inbound::HelloAck(value),
        "message.stored" => Inbound::Stored {
            client_msg_id: s("client_msg_id")?,
            id: s("id")?,
            seq: n("seq")?,
            thread_id: s("thread_id")?,
        },
        "message.deliver" => Inbound::Deliver(value),
        "presence.update" => Inbound::Presence {
            address: s("address")?,
            worker_id: s("worker_id")?,
            role: s("role")?,
            state: serde_json::from_value(value["state"].clone())
                .map_err(|_| bad("presence.update: state"))?,
            at: n("at")?,
        },
        "control" => {
            let lifted = match value["state"].as_str() {
                Some("issued") => false,
                Some("lifted") => true,
                _ => return Err(bad("control: state")),
            };
            let control = serde_json::from_value(value["control"].clone())
                .map_err(|e| bad(format!("control: {e}")))?;
            Inbound::Control { lifted, control }
        }
        "place.denied" => Inbound::PlaceDenied {
            place: s("place")?,
            code: s("code")?,
        },
        "resync" => Inbound::Resync { place: s("place")? },
        "error" => Inbound::Error {
            client_msg_id: value["client_msg_id"].as_str().map(str::to_string),
            code: s("code")?,
            reason: value["reason"].as_str().map(str::to_string),
            quota: quota_fields(&value),
        },
        "ping" => Inbound::Ping,
        "consent.answered" => Inbound::ConsentAnswered {
            consent_id: s("consent_id")?,
        },
        _ => Inbound::Other(t),
    })
}

#[cfg(test)]
#[path = "v2_tests.rs"]
mod tests;
