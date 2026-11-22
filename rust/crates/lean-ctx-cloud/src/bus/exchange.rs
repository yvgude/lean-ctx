// SPDX-License-Identifier: Apache-2.0
//! Agent exchange through LeanCTX Cloud (Welle 4, contract §40): the rules
//! every knowledge item (`finding`) and task hand-over (`handoff`, with its
//! `question`, `offer` and `answer` conversation) passes when it leaves this
//! machine for another machine's agent or person, or arrives from one, and
//! the audit record each exchange leaves.
//!
//! Outbound ([`super::messages_v2::send`]), in this order:
//! 1. account: a device enrolled in a personal account exchanges nothing
//!    (team knowledge needs a team, as for `cloud decisions push`);
//! 2. policy: an expired or damaged team policy refuses; the vault's
//!    effective policy decides with `sharing.allowed_vaults` (when set, the
//!    vault must be listed) and `sharing.agent_exchange` (the exchange kinds
//!    that may leave; absent: all, malformed: none);
//! 3. content: the installation screen over every field that leaves (text,
//!    handoff resource, every ref target);
//! 4. the always-enforced person rules and the C0 message validation.
//!
//! Inbound (relay `message.deliver` from another machine): person rules,
//! the content screen over every body string and ref, and the same team
//! policy. A refused delivery moves the cursor only; it never reaches the
//! local bus.
//!
//! The audit ledger (`<cloud data>/bus/exchange-audit.jsonl`, 0600: a header
//! line, then one appended JSON line per record; read as its newest
//! [`AUDIT_MAX`] entries) keeps for each exchange its direction, outcome,
//! kind, address, ids, the refusing rule and a digest of the body — never
//! its content.

use std::collections::BTreeSet;
use std::path::Path;

use lean_ctx_protocol::cloud::effective_policy::EffectivePolicyV1;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::v2::MessageType;
use crate::CloudError;
use crate::effective_floor;
use crate::knowledge::screening::Screen;
use crate::state::{lock_state, write_private};

/// The `sharing` member naming the exchange kinds that may leave a machine.
pub const KINDS_MEMBER: &str = "agent_exchange";
/// The audit ledger file under `<cloud data>/bus/`.
pub const AUDIT_FILE: &str = "exchange-audit.jsonl";
/// Schema of the audit ledger.
pub const AUDIT_SCHEMA: &str = "leanctx.exchange-audit/v1";
/// Entries kept (the newest).
pub const AUDIT_MAX: usize = 2000;

/// Whether `kind` is an exchange (`progress` is a status line, not one).
pub fn is_exchange(kind: MessageType) -> bool {
    kind != MessageType::Progress
}

/// The wire name of `kind` (`finding`, `handoff`, …).
pub fn kind_name(kind: MessageType) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// Why an exchange is refused: the rule and a reason that never quotes the
/// content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub rule: &'static str,
    pub why: String,
}

impl Refusal {
    pub fn new(rule: &'static str, why: impl Into<String>) -> Self {
        Self {
            rule,
            why: why.into(),
        }
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (rule: {})", self.why, self.rule)
    }
}

/// Rule 1: a personal account keeps its own memory.
pub fn account_refusal(org_kind: Option<&str>) -> Option<Refusal> {
    (org_kind == Some("personal")).then(|| {
        Refusal::new(
            "account",
            "knowledge and tasks are exchanged with a team: this device is connected to a personal account, which keeps its own memory",
        )
    })
}

/// The exchange kinds `policy` lets leave: `Ok(None)` when it does not set
/// [`KINDS_MEMBER`]; `Err` when the value is not a sorted list of exchange
/// kinds. Read through JSON so it works whether the protocol types name the
/// member or keep it among the unknown members.
pub(crate) fn allowed_kinds(
    policy: &EffectivePolicyV1,
) -> Result<Option<BTreeSet<MessageType>>, ()> {
    let sharing = serde_json::to_value(&policy.sharing).map_err(|_| ())?;
    let Some(value) = sharing.get(KINDS_MEMBER) else {
        return Ok(None);
    };
    let names = value.as_array().ok_or(())?;
    let mut kinds = BTreeSet::new();
    let mut last: Option<&str> = None;
    for name in names {
        let name = name.as_str().ok_or(())?;
        if last.is_some_and(|prev| prev >= name) {
            return Err(());
        }
        last = Some(name);
        let kind = MessageType::parse(name)
            .filter(|k| is_exchange(*k))
            .ok_or(())?;
        kinds.insert(kind);
    }
    Ok(Some(kinds))
}

/// Whether `vault_id` (UUID or 32 lower hex) is among `allowed`.
fn vault_listed(allowed: &[String], vault_id: &str) -> bool {
    let compact: String = vault_id
        .chars()
        .filter(|c| *c != '-')
        .collect::<String>()
        .to_ascii_lowercase();
    allowed.iter().any(|v| v == vault_id || *v == compact)
}

/// Rule 2 on one effective policy: the vault and the kind.
pub fn policy_refusal(
    policy: &EffectivePolicyV1,
    vault_id: &str,
    kind: MessageType,
) -> Option<Refusal> {
    if let Some(allowed) = &policy.sharing.allowed_vaults
        && !vault_listed(allowed, vault_id)
    {
        return Some(Refusal::new(
            "policy",
            "the team policy does not allow sharing with this vault (sharing.allowed_vaults)",
        ));
    }
    match allowed_kinds(policy) {
        Ok(None) => None,
        Ok(Some(kinds)) if kinds.contains(&kind) => None,
        Ok(Some(_)) => Some(Refusal::new(
            "policy",
            format!(
                "the team policy does not allow exchanging a {} (sharing.{KINDS_MEMBER})",
                kind_name(kind)
            ),
        )),
        Err(()) => Some(Refusal::new(
            "policy",
            format!(
                "the team policy's sharing.{KINDS_MEMBER} is not a sorted list of message types; nothing is exchanged until it is repaired"
            ),
        )),
    }
}

/// Rule 2 against the floor installed under `cloud_data`: an expired or
/// damaged policy refuses; no installed policy leaves the decision to Cloud.
pub fn floor_refusal(
    cloud_data: &Path,
    vault_id: &str,
    kind: MessageType,
    now: lean_ctx_time::UtcDateTime,
) -> Option<Refusal> {
    if let Err(why) = crate::effective_policy::sensitive_gate(cloud_data, now) {
        return Some(Refusal::new("policy", why));
    }
    let floor = effective_floor::read(&effective_floor::floor_dir_in(cloud_data)).and_then(
        |(floor, required)| effective_floor::check(floor.as_deref(), required.as_deref()),
    );
    match floor {
        Ok(None) => None,
        Ok(Some(floor)) => policy_refusal(floor.policy.for_vault(Some(vault_id)), vault_id, kind),
        Err(why) => Some(Refusal::new(
            "policy",
            format!("{why}; nothing is exchanged until the team policy is repaired"),
        )),
    }
}

/// Rule 3: every field that leaves passes the screen.
pub fn screen_fields(screen: &mut Screen, fields: &[&str]) -> Result<(), Refusal> {
    for field in fields.iter().filter(|f| !f.is_empty()) {
        screen
            .check(field)
            .map_err(|blocked| Refusal::new("content", format!("it contains a {blocked}")))?;
    }
    Ok(())
}

/// Every string in `value`, depth first.
fn strings_of<'a>(value: &'a Value, out: &mut Vec<&'a str>) {
    match value {
        Value::String(s) => out.push(s),
        Value::Array(items) => items.iter().for_each(|v| strings_of(v, out)),
        Value::Object(map) => map.values().for_each(|v| strings_of(v, out)),
        _ => {}
    }
}

/// The inbound rules for a delivered `message` of an exchange kind (other
/// messages pass): person rules, the content screen over body and refs,
/// and the team policy of `vault_id` (when the vault is linked here).
pub fn inbound_refusal(
    cloud_data: &Path,
    screen: &mut Screen,
    message: &Value,
    vault_id: Option<&str>,
    now: lean_ctx_time::UtcDateTime,
) -> Option<Refusal> {
    let kind = MessageType::parse(message["type"].as_str()?)?;
    if !is_exchange(kind) {
        return None;
    }
    let body = &message["body"];
    if message["sender"]["kind"].as_str() == Some("worker")
        && let Err(invalid) = super::person_rules::check_agent_body(kind, body)
    {
        return Some(Refusal::new("person_rules", invalid.to_string()));
    }
    let mut fields = Vec::new();
    strings_of(body, &mut fields);
    strings_of(&message["refs"], &mut fields);
    if let Err(refusal) = screen_fields(screen, &fields) {
        return Some(refusal);
    }
    vault_id.and_then(|vault| floor_refusal(cloud_data, vault, kind, now))
}

/// What the exchange rules make of one delivered message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    /// Not an exchange from another machine (a replay, a status line, this
    /// machine's own message coming back): the relay decides as before.
    Pass,
    /// An exchange the rules admit, with its audit record.
    Allowed(Box<Entry>),
    /// An exchange the rules refuse: only the cursor moves to `seq`.
    Refused {
        address: String,
        seq: u64,
        entry: Box<Entry>,
    },
}

/// The inbound decision for a `message.deliver` (`local_workers`: the
/// workers of this machine, whose own messages come back and pass).
#[cfg_attr(not(feature = "http-server"), allow(dead_code))]
pub(crate) fn delivered(
    cloud_data: &Path,
    map: &crate::vault::VaultMap,
    state: &super::relay_v2::StateV2,
    message: &Value,
    local_workers: &BTreeSet<String>,
    now: lean_ctx_time::UtcDateTime,
) -> Delivery {
    let Some((address, seq, record)) = super::relay_v2::new_delivery(map, state, message) else {
        return Delivery::Pass;
    };
    let Some(kind) = message["type"]
        .as_str()
        .and_then(MessageType::parse)
        .filter(|k| is_exchange(*k))
    else {
        return Delivery::Pass;
    };
    let sender = &message["sender"];
    let peer = match sender["kind"].as_str() {
        Some("worker") => sender["worker_id"].as_str().map(str::to_owned),
        Some("person") => sender["handle"].as_str().map(|h| format!("person:{h}")),
        _ => None,
    };
    if peer.as_ref().is_some_and(|p| local_workers.contains(p)) {
        return Delivery::Pass;
    }
    let mut entry = Entry::now(now, "in", "delivered", kind, &address);
    entry.msg_id = message["id"].as_str().map(str::to_owned);
    entry.peer = peer;
    entry.digest = Some(digest(&message["body"]));
    let refusal = inbound_refusal(
        cloud_data,
        &mut Screen::for_installation(),
        message,
        record.map(|r| r.vault_id.as_str()),
        now,
    );
    match refusal {
        None => Delivery::Allowed(Box::new(entry)),
        Some(refusal) => {
            entry.outcome = "dropped".into();
            Delivery::Refused {
                address,
                seq,
                entry: Box::new(entry.refused_by(&refusal)),
            }
        }
    }
}

/// `sha256:<hex>` of `body` as canonical JSON: what was exchanged, without
/// the content.
pub fn digest(body: &Value) -> String {
    let canonical = lean_ctx_protocol::cloud::canonical_json(body)
        .unwrap_or_else(|_| body.to_string().into_bytes());
    format!("sha256:{}", hex::encode(Sha256::digest(&canonical)))
}

/// One audit record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// RFC 3339, seconds, UTC.
    pub at: String,
    /// `out` | `in`.
    pub direction: String,
    /// `queued` | `refused` (out), `delivered` | `dropped` (in).
    pub outcome: String,
    /// `finding`, `handoff`, `question`, `offer`, `answer`.
    pub kind: String,
    /// `#<workspace>/<vault>`.
    pub address: String,
    /// `client_msg_id` (out) or Cloud `msg_…` id (in).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub msg_id: Option<String>,
    /// The local agent that sent it (out).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// The other side: addressed worker (out) or sender (in).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
}

/// Longest refusal reason kept in a record (characters): a record stays
/// small, so the ledger's size follows from its entry count.
pub const REASON_MAX: usize = 300;

impl Entry {
    /// An entry stamped at `now`.
    pub fn now(
        now: lean_ctx_time::UtcDateTime,
        direction: &str,
        outcome: &str,
        kind: MessageType,
        address: &str,
    ) -> Self {
        Self {
            at: now.to_rfc3339_opts(lean_ctx_time::SecondsFormat::Secs, true),
            direction: direction.into(),
            outcome: outcome.into(),
            kind: kind_name(kind),
            address: address.into(),
            msg_id: None,
            agent: None,
            peer: None,
            rule: None,
            reason: None,
            digest: None,
        }
    }

    /// The same entry carrying `refusal` (its reason cut to [`REASON_MAX`]).
    pub fn refused_by(mut self, refusal: &Refusal) -> Self {
        self.rule = Some(refusal.rule.into());
        self.reason = Some(refusal.why.chars().take(REASON_MAX).collect());
        self
    }
}

/// The first line of the ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Header {
    schema: String,
}

/// A ledger file this size or larger is compacted to its newest
/// [`AUDIT_MAX`] entries on the next append.
pub const COMPACT_BYTES: u64 = 4 << 20;

fn local(path: &Path, e: impl std::fmt::Display) -> CloudError {
    CloudError::Local(format!("{}: {e}", path.display()))
}

/// The ledger's header line followed by `entries`, one JSON line each.
fn ledger_bytes<'a>(entries: impl IntoIterator<Item = &'a Entry>) -> Result<Vec<u8>, CloudError> {
    let encode = |e: serde_json::Error| CloudError::Local(e.to_string());
    let mut bytes = serde_json::to_vec(&Header {
        schema: AUDIT_SCHEMA.into(),
    })
    .map_err(encode)?;
    bytes.push(b'\n');
    for entry in entries {
        bytes.extend(serde_json::to_vec(entry).map_err(encode)?);
        bytes.push(b'\n');
    }
    Ok(bytes)
}

/// Whether the regular file at `path` is an intact ledger: our header
/// first, and the last append complete (ends with a newline). Reads the
/// first line and the last byte only.
fn intact(path: &Path) -> Result<bool, CloudError> {
    use std::io::{BufRead, Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).map_err(|e| local(path, e))?;
    let mut last = [0u8; 1];
    if file.seek(SeekFrom::End(-1)).is_err() || file.read_exact(&mut last).is_err() {
        return Ok(false);
    }
    file.seek(SeekFrom::Start(0)).map_err(|e| local(path, e))?;
    let mut first = String::new();
    std::io::BufReader::new(file.take(4096))
        .read_line(&mut first)
        .map_err(|e| local(path, e))?;
    Ok(last[0] == b'\n'
        && serde_json::from_str::<Header>(first.trim_end()).is_ok_and(|h| h.schema == AUDIT_SCHEMA))
}

/// Append one line to the existing ledger at `path`; never creates it (a
/// new ledger is written private by [`write_private`]).
fn append_line(path: &Path, line: &[u8]) -> Result<u64, CloudError> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.append(true);
    let mut file = options.open(path).map_err(|e| local(path, e))?;
    file.write_all(line).map_err(|e| local(path, e))?;
    file.sync_data().map_err(|e| local(path, e))?;
    Ok(file.metadata().map_err(|e| local(path, e))?.len())
}

/// Append `entry` to the ledger under `data_dir`, under the state lock: one
/// line is written, the ledger is never rewritten for it. A damaged ledger
/// is moved aside first and a new one started; a ledger past
/// [`COMPACT_BYTES`] is compacted to its newest [`AUDIT_MAX`] entries.
pub fn record(data_dir: &Path, entry: &Entry) -> Result<(), CloudError> {
    record_bounded(data_dir, entry, COMPACT_BYTES)
}

pub(crate) fn record_bounded(
    data_dir: &Path,
    entry: &Entry,
    compact_bytes: u64,
) -> Result<(), CloudError> {
    let dir = data_dir.join("bus");
    let _lock = lock_state(&dir, AUDIT_FILE)?;
    let path = dir.join(AUDIT_FILE);
    let existing = match std::fs::symlink_metadata(&path) {
        Ok(meta) if meta.file_type().is_file() => true,
        Ok(_) => return Err(local(&path, "not a regular file; nothing is recorded")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => return Err(local(&path, e)),
    };
    if existing && !intact(&path)? {
        set_aside(&path)?;
    } else if existing {
        let mut line = serde_json::to_vec(entry).map_err(|e| CloudError::Local(e.to_string()))?;
        line.push(b'\n');
        if append_line(&path, &line)? < compact_bytes {
            return Ok(());
        }
        // Compaction reads every line: a line damaged anywhere (not only
        // the header or the last append) sets the ledger aside too, so a
        // damaged ledger never stops recording or grows past the bound.
        let bytes = std::fs::read(&path).map_err(|e| local(&path, e))?;
        match parse_entries(&bytes) {
            Some(kept) => return write_private(&dir, AUDIT_FILE, &ledger_bytes(newest(&kept))?),
            None => set_aside(&path)?,
        }
    }
    write_private(&dir, AUDIT_FILE, &ledger_bytes([entry])?)
}

/// Move the damaged ledger at `path` aside; the caller starts a new one.
fn set_aside(path: &Path) -> Result<(), CloudError> {
    let moved = crate::state::quarantine(path)?;
    tracing::warn!(
        "cloud exchange audit: {} is damaged; moved to {}",
        path.display(),
        moved.display()
    );
    Ok(())
}

/// The newest [`AUDIT_MAX`] of `entries`.
fn newest(entries: &[Entry]) -> &[Entry] {
    &entries[entries.len().saturating_sub(AUDIT_MAX)..]
}

/// Every entry of the ledger at `path`, oldest first.
fn read_entries(path: &Path) -> Result<Vec<Entry>, CloudError> {
    let bytes = std::fs::read(path).map_err(|e| local(path, e))?;
    parse_entries(&bytes).ok_or_else(|| CloudError::Local(format!("{} is damaged", path.display())))
}

/// The entries of ledger `bytes`, oldest first; `None` when any line (the
/// header included) is damaged.
fn parse_entries(bytes: &[u8]) -> Option<Vec<Entry>> {
    let body = std::str::from_utf8(bytes).ok()?.strip_suffix('\n')?;
    let mut lines = body.split('\n');
    let header: Header = serde_json::from_str(lines.next()?).ok()?;
    if header.schema != AUDIT_SCHEMA {
        return None;
    }
    lines
        .map(|l| serde_json::from_str::<Entry>(l).ok())
        .collect()
}

/// The ledger's newest [`AUDIT_MAX`] entries under `data_dir`, oldest first
/// (none yet: empty).
pub fn entries(data_dir: &Path) -> Result<Vec<Entry>, CloudError> {
    let path = data_dir.join("bus").join(AUDIT_FILE);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let mut all = read_entries(&path)?;
    let excess = all.len().saturating_sub(AUDIT_MAX);
    all.drain(..excess);
    Ok(all)
}

/// The exchange rules' verdict on one delivered `message`, recorded: `Ok(true)`
/// when it goes on to the local bus, `Ok(false)` when it was refused and
/// only the cursor moved. When its audit record cannot be written the
/// delivery is neither applied nor skipped (`Err`): the cursor stays, so it
/// comes again.
#[cfg_attr(not(feature = "http-server"), allow(dead_code))]
pub(crate) fn admit(
    cloud_data: &Path,
    map: &crate::vault::VaultMap,
    state: &mut super::relay_v2::StateV2,
    message: &Value,
    local_workers: &BTreeSet<String>,
    now: lean_ctx_time::UtcDateTime,
) -> Result<bool, CloudError> {
    match delivered(cloud_data, map, state, message, local_workers, now) {
        Delivery::Pass => Ok(true),
        Delivery::Allowed(entry) => record(cloud_data, &entry).map(|()| true),
        Delivery::Refused {
            address,
            seq,
            entry,
        } => {
            record(cloud_data, &entry)?;
            super::relay_v2::skip_delivery(state, &address, seq);
            Ok(false)
        }
    }
}

#[cfg(test)]
#[path = "exchange_tests.rs"]
mod tests;
