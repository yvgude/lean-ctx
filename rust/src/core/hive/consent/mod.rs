// SPDX-License-Identifier: Apache-2.0
//! Out-of-band consent (SPEC-ENGINE §3.8; C0 2.0 `consent.schema.json`,
//! `enums/consent-kinds.json`, `vectors/consent_*.vectors.json`).
//!
//! The engine creates every consent (RESOLUTIONS R16); a person answers it
//! in a terminal, the Cockpit or the browser. No MCP action can approve:
//! approval lives only in [`broker::Broker::approve`], which takes a
//! [`Proof`] that only an approval channel produces.
//!
//! - [`Kind`]: the nine kinds of 3.12.0, equal to the embedded fixture.
//! - [`Record`]: one request with its decision and its proof, stored
//!   `<config>/hive/consent/<id>.json` (0600); [`broker`] verifies the
//!   proof on every read.

pub mod broker;
pub mod channel;
#[cfg(unix)]
pub mod helper_mac;
pub mod proof;
pub mod web;

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

pub use broker::{Broker, Restart, Trust};

/// The C0 kind list, embedded (`crates/` is not packaged); a test keeps it
/// byte-equal to `tests/fixtures/hive-v2/enums/consent-kinds.json`.
pub(crate) const KINDS_JSON: &str = include_str!("consent-kinds.json");

/// Domain of a presence-key signature (§3.8.1, form (b) of §3.8.3).
pub const PROOF_DOMAIN: &str = "leanctx.hive.consent_proof.v1\n";
/// Domain and schema of a Hive-signed web answer (§3.8.2 step 5).
pub const STATEMENT_DOMAIN: &str = "leanctx.hive.consent.v1\n";
pub const STATEMENT_SCHEMA: &str = "leanctx.hive.consent.v1";

/// A request may be answered for 24 h.
pub const REQUEST_TTL_MS: u64 = 24 * 60 * 60 * 1000;

/// The nine consent kinds of 3.12.0 (RESOLUTIONS R107).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    LinkVault,
    PublicPresence,
    SwarmJoin,
    PledgeChange,
    UnattendedStart,
    ProcedureConfirm,
    ReviewerEnable,
    ApiKeySet,
    SealedGrant,
}

impl Kind {
    pub const ALL: [Kind; 9] = [
        Kind::LinkVault,
        Kind::PublicPresence,
        Kind::SwarmJoin,
        Kind::PledgeChange,
        Kind::UnattendedStart,
        Kind::ProcedureConfirm,
        Kind::ReviewerEnable,
        Kind::ApiKeySet,
        Kind::SealedGrant,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::LinkVault => "link_vault",
            Kind::PublicPresence => "public_presence",
            Kind::SwarmJoin => "swarm_join",
            Kind::PledgeChange => "pledge_change",
            Kind::UnattendedStart => "unattended_start",
            Kind::ProcedureConfirm => "procedure_confirm",
            Kind::ReviewerEnable => "reviewer_enable",
            Kind::ApiKeySet => "api_key_set",
            Kind::SealedGrant => "sealed_grant",
        }
    }

    pub fn parse(value: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.as_str() == value)
    }

    fn spec(self) -> &'static KindSpec {
        kind_specs()
            .iter()
            .find(|s| s.value == self.as_str())
            .expect("every kind is in the embedded fixture (tested)")
    }

    /// The subject fields, sorted (the fixture lists them so).
    pub fn subject_fields(self) -> &'static [String] {
        &self.spec().subject
    }

    /// Whether the person may answer it on `channel` (`cli`, `cockpit`, `web`).
    pub fn answerable_on(self, channel: &str) -> bool {
        self.spec().channels.iter().any(|c| c == channel)
    }

    /// A server route checks its own proof (§3.8.3).
    pub fn server_gated(self) -> bool {
        self.spec().server_route.is_some()
    }

    /// The subject value the person types on the TTY to approve (§3.8.2):
    /// the place, swarm, vendor or account it names.
    pub fn typed_field(self) -> &'static str {
        match self {
            Kind::LinkVault | Kind::PublicPresence | Kind::SealedGrant => "vault",
            Kind::SwarmJoin | Kind::PledgeChange | Kind::UnattendedStart => "swarm",
            Kind::ProcedureConfirm => "item_id",
            Kind::ReviewerEnable => "place",
            Kind::ApiKeySet => "vendor",
        }
    }
}

impl std::fmt::Display for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Deserialize)]
struct KindSpec {
    value: String,
    channels: Vec<String>,
    subject: Vec<String>,
    server_route: Option<String>,
}

#[derive(Debug, Deserialize)]
struct KindList {
    values: Vec<KindSpec>,
}

fn kind_specs() -> &'static [KindSpec] {
    static SPECS: OnceLock<Vec<KindSpec>> = OnceLock::new();
    SPECS.get_or_init(|| {
        serde_json::from_str::<KindList>(KINDS_JSON)
            .expect("embedded consent-kinds.json parses (tested)")
            .values
    })
}

/// How a decision was made (`enums/consent-channels.json`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    OsPresence,
    TtyOnly,
    Cockpit,
    Web,
}

impl Channel {
    pub fn as_str(self) -> &'static str {
        match self {
            Channel::OsPresence => "os_presence",
            Channel::TtyOnly => "tty_only",
            Channel::Cockpit => "cockpit",
            Channel::Web => "web",
        }
    }

    /// The kind-list column this channel answers in.
    fn surface(self) -> &'static str {
        match self {
            Channel::OsPresence | Channel::TtyOnly => "cli",
            Channel::Cockpit => "cockpit",
            Channel::Web => "web",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Pending,
    Approved,
    Denied,
    Expired,
}

/// The presence-key signature of an approval (§3.8.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresenceSignature {
    pub key_id: String,
    pub alg: String,
    /// base64url without padding.
    pub signature: String,
}

/// Exactly one proof per approved record (§3.8.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Proof {
    Presence(PresenceSignature),
    Statement(Value),
    /// hex `blake3_keyed(K_file, …)`, tty-only hosts only.
    Mac(String),
}

/// One consent request and its decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub consent_id: String,
    pub kind: Kind,
    pub subject: Value,
    pub subject_digest: String,
    pub created_at: u64,
    pub expires_at: u64,
    pub state: State,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<Channel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proof: Option<Proof>,
}

impl Record {
    /// What the presence key signs (§3.8.1).
    pub fn proof_input(&self, approved_at: u64) -> Result<Vec<u8>, ConsentError> {
        lean_ctx_protocol::hive::signing_input(
            PROOF_DOMAIN,
            &json!({
                "consent_id": self.consent_id,
                "kind": self.kind,
                "subject_digest": self.subject_digest,
                "approved_at": approved_at,
            }),
        )
        .map_err(|e| ConsentError::Local(e.to_string()))
    }

    /// The "Request fingerprint" the terminal and the web show (R82).
    pub fn fingerprint(&self) -> &str {
        &self.subject_digest[..16]
    }

    /// The value the person types to approve on a TTY.
    pub fn typed_value(&self) -> String {
        let value = self.subject[self.kind.typed_field()]
            .as_str()
            .unwrap_or_default();
        // A UUID is typed by its first block.
        if is_uuid(value) {
            value[..8].to_string()
        } else {
            value.to_string()
        }
    }
}

/// Why a consent-gated step did not run. `code()` is the C0/engine error
/// code; `Display` is the person-facing text of SPEC-ENGINE §3.17.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsentError {
    /// A request waits for the person (`consent_required`).
    Required {
        consent_id: String,
        kind: Kind,
    },
    /// The person declined (`consent_denied`).
    Denied,
    /// Older than 24 h (`consent_expired`).
    Expired,
    /// A record failed its proof check (`consent_record_invalid`).
    RecordInvalid,
    /// `procedure_confirm` outside the web (`consent_web_only`).
    WebOnly {
        consent_id: String,
    },
    /// `api_key_set` outside the terminal (`consent_terminal_only`).
    TerminalOnly,
    /// Tty-only approval refused by workspace policy.
    PresenceRequiredByPolicy {
        consent_id: String,
    },
    /// The approval channel refused (no TTY, agent ancestry, wrong typed
    /// value); nothing was stored.
    ChannelRefused(String),
    /// Not a request of this engine.
    NotFound,
    /// A subject of the wrong shape for its kind (a caller bug).
    InvalidSubject(String),
    /// Already answered.
    InvalidState,
    /// No browser answer within 10 minutes (`consent_web_timeout`).
    WebTimeout,
    /// The person cancelled the system dialog (`presence_cancelled`).
    PresenceCancelled,
    /// `LeanCTX Consent.app` is not installed (`helper_missing`).
    HelperMissing,
    /// The helper is not ours (`helper_signature`).
    HelperSignature,
    /// A sensitive operation under an expired, missing or damaged Hive
    /// policy (`policy_authorisation_expired`); fails closed.
    PolicyExpired(String),
    Local(String),
}

impl ConsentError {
    pub fn code(&self) -> &'static str {
        match self {
            ConsentError::Required { .. } => "consent_required",
            ConsentError::Denied => "consent_denied",
            ConsentError::Expired => "consent_expired",
            ConsentError::RecordInvalid => "consent_record_invalid",
            ConsentError::WebOnly { .. } => "consent_web_only",
            ConsentError::TerminalOnly => "consent_terminal_only",
            ConsentError::PresenceRequiredByPolicy { .. } => "consent_presence_required_by_policy",
            ConsentError::ChannelRefused(_) => "consent_channel_refused",
            ConsentError::NotFound => "not_found",
            ConsentError::InvalidSubject(_) => "invalid_subject",
            ConsentError::InvalidState => "invalid_state",
            ConsentError::WebTimeout => "consent_web_timeout",
            ConsentError::PresenceCancelled => "presence_cancelled",
            ConsentError::HelperMissing => "helper_missing",
            ConsentError::HelperSignature => "helper_signature",
            ConsentError::PolicyExpired(_) => "policy_authorisation_expired",
            ConsentError::Local(_) => "local",
        }
    }
}

impl std::fmt::Display for ConsentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // Never the id alone as something to pass back (§3.8.1).
            ConsentError::Required { .. } => {
                f.write_str("Approve in a terminal: lean-ctx hive consent")
            }
            ConsentError::Denied => f.write_str("The person declined this."),
            ConsentError::Expired => f.write_str("This request expired; run the command again."),
            ConsentError::RecordInvalid => {
                f.write_str("A consent record was altered; approve again.")
            }
            ConsentError::WebOnly { consent_id } => write!(
                f,
                "Answer this one in the browser: lean-ctx hive consent --web {consent_id}"
            ),
            ConsentError::TerminalOnly => {
                f.write_str("This one is answered in the terminal only: lean-ctx hive consent")
            }
            ConsentError::PresenceRequiredByPolicy { consent_id } => write!(
                f,
                "Your workspace requires system confirmation or the browser. Run: lean-ctx hive consent --web {consent_id}"
            ),
            ConsentError::ChannelRefused(why) => write!(f, "Not approved: {why}"),
            ConsentError::NotFound => f.write_str("No such consent request on this computer."),
            ConsentError::InvalidSubject(why) => write!(f, "invalid consent subject: {why}"),
            ConsentError::InvalidState => f.write_str("This request was already answered."),
            ConsentError::WebTimeout => {
                f.write_str("No answer in the browser within 10 minutes; run the command again.")
            }
            ConsentError::PresenceCancelled => {
                f.write_str("System confirmation was cancelled. Nothing was approved.")
            }
            ConsentError::HelperMissing => f.write_str(
                "LeanCTX Consent.app is not installed; run lean-ctx hive consent-helper install.",
            ),
            ConsentError::HelperSignature => f.write_str(
                "LeanCTX Consent.app signature invalid; run lean-ctx hive consent-helper install.",
            ),
            ConsentError::PolicyExpired(why) => write!(f, "Refused: {why}"),
            ConsentError::Local(why) => write!(f, "consent store: {why}"),
        }
    }
}

impl std::error::Error for ConsentError {}

impl From<ConsentError> for crate::core::hive::HiveError {
    fn from(error: ConsentError) -> Self {
        crate::core::hive::HiveError::Refused(format!("{}: {error}", error.code()))
    }
}

/// `cns_` + 16 lowercase base32 (80 random bits).
pub fn new_consent_id() -> Result<String, ConsentError> {
    let mut bytes = [0u8; 10];
    getrandom::fill(&mut bytes).map_err(|e| ConsentError::Local(e.to_string()))?;
    Ok(format!("cns_{}", base32_lower(&bytes)))
}

pub fn is_consent_id(value: &str) -> bool {
    value.strip_prefix("cns_").is_some_and(|rest| {
        rest.len() == 16
            && rest
                .bytes()
                .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b))
    })
}

fn base32_lower(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut out = String::with_capacity(bytes.len().div_ceil(5) * 8);
    let (mut buffer, mut bits) = (0u32, 0u32);
    for &byte in bytes {
        buffer = (buffer << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((buffer >> bits) & 31) as usize] as char);
        }
    }
    out
}

/// `hex(blake3(canonical_json(subject)))`.
pub fn subject_digest(subject: &Value) -> Result<String, ConsentError> {
    let bytes = lean_ctx_protocol::hive::canonical_json(subject)
        .map_err(|e| ConsentError::InvalidSubject(e.to_string()))?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

/// The subject carries exactly the kind's fields (no `isolation`, R107),
/// each of the type the schema gives it.
pub fn check_subject(kind: Kind, subject: &Value) -> Result<(), ConsentError> {
    let bad = |why: String| Err(ConsentError::InvalidSubject(format!("{kind}: {why}")));
    let Some(fields) = subject.as_object() else {
        return bad("not an object".into());
    };
    let expected = kind.subject_fields();
    if fields.len() != expected.len() || !expected.iter().all(|f| fields.contains_key(f)) {
        let got: Vec<&String> = fields.keys().collect();
        return bad(format!("fields {got:?}, expected {expected:?}"));
    }
    for (name, value) in fields {
        if !field_ok(kind, name, value) {
            return bad(format!("field {name}"));
        }
    }
    Ok(())
}

fn field_ok(kind: Kind, name: &str, value: &Value) -> bool {
    let text = value.as_str();
    match name {
        "link_digest" | "pledge_digest" | "before_digest" | "after_digest" | "item_digest" => {
            text.is_some_and(is_hex64)
        }
        "command_digests" => value.as_array().is_some_and(|items| {
            (1..=10).contains(&items.len())
                && items.iter().all(|d| d.as_str().is_some_and(is_hex64))
        }),
        "vault" if kind == Kind::LinkVault => {
            text.is_some_and(|v| crate::core::hive::vault_address::VaultAddress::parse(v).is_ok())
        }
        "swarm" | "vault" => text.is_some_and(is_uuid),
        "remote_seen" | "bound_repo" => {
            value.is_null() || text.is_some_and(|t| (1..=500).contains(&t.chars().count()))
        }
        "target_vault" => value.is_null(),
        "worker_id" => text.is_some_and(crate::core::hive::bus::address::is_worker_id),
        "cli" => text.is_some_and(|t| !t.is_empty() && t.len() <= 64),
        "usd_cents_per_day" => value.as_u64().is_some(),
        "reviewer_usd_cents_per_day" => value.as_u64().is_some_and(|c| (100..=50_000).contains(&c)),
        "source" => matches!(text, Some("owner" | "own")),
        "place" => text.is_some_and(|t| {
            t.split_once(':')
                .is_some_and(|(k, id)| !k.is_empty() && !id.is_empty())
        }),
        "item_id" => text.is_some_and(|t| t.starts_with("rv_")),
        "vendor" => text.is_some_and(|t| handle_like(t, 32)),
        "requester" => text.is_some_and(|t| handle_like(t, 39)),
        "engine_fingerprint" => text.is_some_and(|t| t.len() == 16 && is_lower_hex(t)),
        _ => false,
    }
}

/// `^[a-z0-9][a-z0-9-]{0,max-1}$`.
fn handle_like(value: &str, max: usize) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= max
        && bytes[0] != b'-'
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

fn is_lower_hex(value: &str) -> bool {
    value
        .bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn is_hex64(value: &str) -> bool {
    value.len() == 64 && is_lower_hex(value)
}

fn is_uuid(value: &str) -> bool {
    let parts: Vec<&str> = value.split('-').collect();
    parts.len() == 5
        && parts.iter().map(|p| p.len()).eq([8, 4, 4, 4, 12])
        && parts.iter().all(|p| is_lower_hex(p))
}

/// Build a subject from its fields (callers name each one).
pub fn subject(fields: &[(&str, Value)]) -> Value {
    Value::Object(
        fields
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect::<Map<_, _>>(),
    )
}

#[cfg(test)]
mod tests;
