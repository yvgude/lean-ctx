// SPDX-License-Identifier: Apache-2.0
//! Audit entry v2 (ent3-1): session binding, hashed time and decision facts.
//!
//! Every entry this build writes is a v2 entry: a v1 entry plus `v: 2`,
//! its `timestamp` under the hash, and, when it records a call of a session,
//! the facts of [`SessionFacts`]: `session_id`, `seq` (per session, from 1),
//! `instance_id`, `policy_digest`, `decision` and `output_digest` (`null`
//! outside a session). The v1 `entry_hash` stays exactly as it was: it
//! covers the v1 field set and links through `prev_hash`, so every reader
//! that predates v2 (an older engine, `leanctx-verify`, the evidence bundle)
//! still verifies a mixed trail and never reports a v2 entry as tampering.
//!
//! The v2 facts are bound by a second chained hash, `v2_hash`:
//!
//! ```text
//! v2_hash = sha256("leanctx.audit.v2\n" ‖ head ‖ canonical_json(v2 fields))
//! ```
//!
//! where `head` is the chain head before the entry (the `v2_hash` of the
//! previous entry when it is a v2 entry, else its `entry_hash`; `genesis` at
//! the start) and the v2 fields are the facts above plus `timestamp`, `v`
//! and the entry's own `entry_hash`, in sorted key order with absent facts
//! as `null`. Editing a timestamp, seq or any other fact breaks that entry,
//! and rewriting its `v2_hash` breaks the next v2 entry: the property the v1
//! chain gives the v1 fields. A v1 entry appended by an older writer
//! restarts the head at its own `entry_hash`; the v1 chain still links
//! through it. Like the v1 chain, the chain alone cannot protect the last
//! entry (it can be cut, or stripped back to v1): the signed per-session
//! proof over the head (ent3-2) closes that.
//!
//! Input and output digests of v2 entries are HMAC-SHA256 under the
//! engine-local key `<data_dir>/hive/receipts.hmac-key` (the key receipts v2
//! already use, created on first use, never uploaded), so a digest that
//! leaves the machine cannot be tested against a guessed input.

use std::path::Path;

use hmac::{Hmac, KeyInit as _, Mac as _};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The entry version this module writes.
pub const ENTRY_VERSION: u64 = 2;
/// Domain of the `v2_hash` chain.
const V2_DOMAIN: &str = "leanctx.audit.v2\n";
/// Prefix of a keyed digest, the one receipts v2 carry.
pub use lean_ctx_protocol::hive::receipts_v2::HMAC_PREFIX;
/// The engine-local HMAC key file inside `<data_dir>/hive`.
const KEY_FILE: &str = "receipts.hmac-key";

/// What the gateway decided for the call an entry records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuditDecision {
    #[default]
    Allow,
    Deny,
    Redact,
    Error,
}

impl AuditDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
            Self::Redact => "redact",
            Self::Error => "error",
        }
    }

    fn known(label: &str) -> bool {
        matches!(label, "allow" | "deny" | "redact" | "error")
    }
}

/// The session facts a v2 entry carries. `seq` is not here: the writer
/// assigns it per session, under the trail lock, in chain order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionFacts {
    pub session_id: String,
    /// The Hive instance id when this installation is attached.
    pub instance_id: Option<String>,
    /// Digest of the policy that decided the call (ent2).
    pub policy_digest: Option<String>,
    pub decision: AuditDecision,
    /// Keyed digest of the output ([`keyed_digest`]).
    pub output_digest: Option<String>,
}

/// The v2 hashed field set of a raw v2 entry, or `None` when a fact is
/// missing or malformed (a structural defect, reported as invalid).
pub(super) fn v2_fields(value: &serde_json::Value) -> Option<serde_json::Value> {
    use serde_json::Value;
    let text = |key: &str| value.get(key).and_then(Value::as_str);
    let optional = |key: &str, valid: &dyn Fn(&Value) -> bool| match value.get(key) {
        None | Some(Value::Null) => Some(Value::Null),
        Some(v) if valid(v) => Some(v.clone()),
        Some(_) => None,
    };
    let is_text = |v: &Value| v.is_string();
    let session_id = optional("session_id", &|v| v.as_str().is_some_and(|s| !s.is_empty()))?;
    let seq = optional("seq", &|v| v.as_u64().is_some_and(|n| n >= 1))?;
    // A session entry has both, any other entry neither.
    if session_id.is_null() != seq.is_null() {
        return None;
    }
    let decision = optional("decision", &|v| {
        v.as_str().is_some_and(AuditDecision::known)
    })?;
    // Sorted keys, built in sorted order: the same bytes with or without
    // serde_json's `preserve_order`.
    Some(serde_json::json!({
        "decision": decision,
        "entry_hash": text("entry_hash")?,
        "instance_id": optional("instance_id", &is_text)?,
        "output_digest": optional("output_digest", &is_text)?,
        "policy_digest": optional("policy_digest", &is_text)?,
        "seq": seq,
        "session_id": session_id,
        "timestamp": text("timestamp")?,
        "v": ENTRY_VERSION,
    }))
}

/// `v2_hash` of an entry whose v2 fields are `fields`, after chain head
/// `head`.
pub(super) fn compute_v2_hash(head: &str, fields: &serde_json::Value) -> String {
    let mut hasher = Sha256::new();
    hasher.update(V2_DOMAIN.as_bytes());
    hasher.update(head.as_bytes());
    hasher.update(serde_json::to_string(fields).unwrap_or_default().as_bytes());
    crate::core::agent_identity::hex_encode(&hasher.finalize())
}

/// The entry version of a raw entry: 1 when the field is absent.
pub(super) fn version_of(value: &serde_json::Value) -> Option<u64> {
    match value.get("v") {
        None => Some(1),
        Some(v) => v.as_u64(),
    }
}

/// The chain head an entry of `version` leaves behind: the `v2_hash` of a
/// v2 entry (or of a later version that kept the field), else the
/// `entry_hash`.
pub(super) fn head_of(value: &serde_json::Value, version: u64) -> Option<String> {
    let field = |key: &str| value.get(key).and_then(serde_json::Value::as_str);
    let head = if version >= ENTRY_VERSION {
        field("v2_hash").or_else(|| field("entry_hash"))
    } else {
        field("entry_hash")
    };
    head.map(str::to_string)
}

/// Next `seq` of `session_id` in this process. Called under the exclusive
/// trail lock, so the numbers follow chain order.
pub(super) fn next_seq(session_id: &str) -> u64 {
    static SESSION_SEQ: std::sync::Mutex<Option<std::collections::HashMap<String, u64>>> =
        std::sync::Mutex::new(None);
    let mut guard = SESSION_SEQ
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let seq = guard
        .get_or_insert_with(std::collections::HashMap::new)
        .entry(session_id.to_string())
        .or_insert(0);
    *seq += 1;
    *seq
}

/// The engine-local HMAC key in `dir` (32 random bytes, hex), created on
/// first use. Shared with Hive receipts v2.
pub fn load_or_create_hmac_key(dir: &Path) -> Option<[u8; 32]> {
    if let Some(key) = load_hmac_key(dir) {
        return Some(key);
    }
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).ok()?;
    crate::core::secure_fs::write_private(dir, KEY_FILE, hex::encode(bytes).as_bytes()).ok()?;
    // Another process may have written first: the file wins.
    load_hmac_key(dir)
}

fn load_hmac_key(dir: &Path) -> Option<[u8; 32]> {
    let text = std::fs::read_to_string(dir.join(KEY_FILE)).ok()?;
    hex::decode(text.trim()).ok()?.try_into().ok()
}

/// HMAC-SHA256(key, `label` ‖ `\n` ‖ `data`), lowercase hex.
pub fn hmac_hex(key: &[u8; 32], label: &str, data: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC takes any key length");
    mac.update(label.as_bytes());
    mac.update(b"\n");
    mac.update(data);
    hex::encode(mac.finalize().into_bytes())
}

/// `hmac-sha256:<hex>` of `data` under this installation's local key, or
/// `None` when the key cannot be read or created (the entry then carries no
/// digest rather than a guessable one).
pub fn keyed_digest(label: &str, data: &[u8]) -> Option<String> {
    let dir = crate::core::data_dir::lean_ctx_data_dir()
        .ok()?
        .join("hive");
    let key = load_or_create_hmac_key(&dir)?;
    Some(format!("{HMAC_PREFIX}{}", hmac_hex(&key, label, data)))
}

/// Keyed digest of tool arguments (the v2 `input_hash`).
pub fn keyed_input_digest(args: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
    keyed_digest("input", serde_json::to_string(args).ok()?.as_bytes())
}
