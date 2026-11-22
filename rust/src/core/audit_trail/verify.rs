// SPDX-License-Identifier: Apache-2.0
//! Hash-chain verification of the audit trail: the full and incremental
//! (checkpointed) passes over the sealed segments and the active file, and
//! the security evidence aggregated along the way.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{BufRead, Read};
use std::path::{Path, PathBuf};

use super::{
    AuditEntry, AuditEventType, ChainVerifyResult, SNAPSHOT_ATTEMPTS, checkpoint,
    compute_entry_hash, is_seal, lock_trail_shared, release_read_lock, segments,
    still_holds_prefix, trail_path, v2,
};

/// The hashed field set of an entry, kept as raw JSON values.
///
/// The chain hash covers exactly these fields in exactly this order, so a
/// reader can re-derive it WITHOUT understanding `event_type` — that is what
/// makes an additive event type verifiable by an older reader (OCP Part 4).
/// A missing field is a structural defect and yields `None`.
fn hashed_fields(value: &serde_json::Value) -> Option<serde_json::Value> {
    let get = |key: &str| value.get(key).cloned();
    Some(serde_json::json!({
        "agent_id": get("agent_id")?,
        "tool": get("tool")?,
        "action": value.get("action").cloned().unwrap_or(serde_json::Value::Null),
        "input_hash": get("input_hash")?,
        "output_tokens": get("output_tokens")?,
        "role": get("role")?,
        "event_type": get("event_type")?,
    }))
}

fn invalid_at(total: usize, unknown: usize) -> ChainVerifyResult {
    ChainVerifyResult {
        total_entries: total,
        valid: false,
        first_invalid_at: Some(total),
        source_present: true,
        unknown_event_types: unknown,
    }
}

fn unavailable() -> ChainVerifyResult {
    ChainVerifyResult {
        total_entries: 0,
        valid: false,
        first_invalid_at: Some(0),
        source_present: false,
        unknown_event_types: 0,
    }
}

/// Verify the hash chain over the whole trail.
///
/// A namespaced extension entry whose `event_type` this build does not know is counted in
/// `unknown_event_types` and its links are still verified from the raw
/// fields — a reader older than the writer must not report a forward-
/// compatible `x-` event as tampering. Anything else (unparseable line,
/// missing hashed field, broken `prev_hash` link, wrong `entry_hash`) is
/// still reported as invalid at its index; corruption is never hidden.
pub(super) fn verify_chain_with_drift(
    agent_id: Option<&str>,
) -> (ChainVerifyResult, Option<(String, String)>) {
    let Some(path) = trail_path() else {
        return (unavailable(), None);
    };
    verify_chain_with_drift_at(&path, agent_id)
}

fn verify_chain_with_drift_at(
    path: &Path,
    agent_id: Option<&str>,
) -> (ChainVerifyResult, Option<(String, String)>) {
    verify_chain_mode(path, agent_id, false)
}

/// Chain state after a verified prefix: everything a verifier needs to
/// continue with the next entry exactly as a full pass would.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct ChainState {
    pub(super) prev_hash: String,
    /// The v2 chain head when it differs from `prev_hash` (the last entry
    /// was a v2 entry).
    pub(super) head: Option<String>,
    pub(super) total: usize,
    unknown: usize,
    /// Latest unacknowledged drift per agent id, with the same transition
    /// rule a single-agent pass applies (an agent's state depends only on
    /// its own entries, so tracking all agents is exactly equivalent).
    pending: std::collections::BTreeMap<String, (String, String)>,
    /// Signed-tally security evidence of the verified entries.
    tally: SignedTallyEvidence,
    /// Security evidence of every kind (the lifetime value proof), so the
    /// proof needs no second read of the trail.
    security: SignedTallyEvidence,
    /// The segment the last entry sealed, if it was a seal.
    sealed: Option<u64>,
}

impl ChainState {
    pub(super) fn genesis() -> Self {
        Self {
            prev_hash: "genesis".to_string(),
            head: None,
            total: 0,
            unknown: 0,
            pending: std::collections::BTreeMap::new(),
            tally: SignedTallyEvidence::default(),
            security: SignedTallyEvidence::default(),
            sealed: None,
        }
    }

    fn invalid(&self) -> (ChainVerifyResult, Option<(String, String)>) {
        (invalid_at(self.total, self.unknown), None)
    }

    fn valid(&self, agent_id: Option<&str>) -> (ChainVerifyResult, Option<(String, String)>) {
        (
            ChainVerifyResult {
                total_entries: self.total,
                valid: true,
                first_invalid_at: None,
                source_present: true,
                unknown_event_types: self.unknown,
            },
            agent_id.and_then(|id| self.pending.get(id).cloned()),
        )
    }

    /// Verify one parsed entry and advance. `Err` leaves `total`/`unknown`
    /// at the values a report of the failure must carry.
    ///
    /// Every signature present is verified against the local `lean-ctx`
    /// key, not only the ones drift and seal entries must carry: a forged
    /// or foreign signature on an ordinary entry is tampering too. The key
    /// is loaded once per pass into `key`.
    fn advance(
        &mut self,
        value: serde_json::Value,
        key: &mut Option<Result<ed25519_dalek::VerifyingKey, ()>>,
    ) -> Result<(), ()> {
        let (Some(entry_prev), Some(entry_hash)) = (
            value.get("prev_hash").and_then(serde_json::Value::as_str),
            value.get("entry_hash").and_then(serde_json::Value::as_str),
        ) else {
            return Err(());
        };
        let partial = hashed_fields(&value).ok_or(())?;
        if entry_prev != self.prev_hash {
            return Err(());
        }
        let data_json = serde_json::to_string(&partial).unwrap_or_default();
        if entry_hash != compute_entry_hash(&self.prev_hash, &data_json) {
            return Err(());
        }
        let entry_hash = entry_hash.to_string();
        // v2 binds the timestamp and the session facts in `v2_hash`. A later
        // version is forward-compatible: its v1 link is verified above, the
        // rest is counted like an unknown event, never reported as tampering.
        let version = v2::version_of(&value).ok_or(())?;
        let newer_version = match version {
            0 => return Err(()),
            1 => false,
            v2::ENTRY_VERSION => {
                let fields = v2::v2_fields(&value).ok_or(())?;
                let claimed = value.get("v2_hash").and_then(serde_json::Value::as_str);
                if claimed != Some(v2::compute_v2_hash(self.head(), &fields).as_str()) {
                    return Err(());
                }
                false
            }
            _ => true,
        };
        let head = v2::head_of(&value, version).ok_or(())?;
        let entry = serde_json::from_value::<AuditEntry>(value).map_err(|_| ())?;
        let seal = is_seal(&entry.event_type);
        if (matches!(&entry.event_type, AuditEventType::Unknown(_)) && !seal) || newer_version {
            self.unknown += 1;
        }
        self.sealed = if seal {
            Some(segments::sealed_seq(entry.action.as_deref()).ok_or(())?)
        } else {
            None
        };
        let signature_required = seal
            || matches!(
                &entry.event_type,
                AuditEventType::AgentDriftDetected | AuditEventType::AgentDriftAcknowledged
            );
        if signature_required && entry.signature.is_none() {
            return Err(());
        }
        if let Some(signature) = entry.signature.as_deref() {
            let signature = crate::core::agent_identity::hex_decode(signature).map_err(|_| ())?;
            let Ok(public_key) = key
                .get_or_insert_with(|| {
                    crate::core::agent_identity::load_public_key("lean-ctx").map_err(|_| ())
                })
                .as_ref()
            else {
                return Err(());
            };
            if !crate::core::agent_identity::verify_signature(
                public_key.as_bytes(),
                entry.entry_hash.as_bytes(),
                &signature,
            ) {
                return Err(());
            }
        }
        if let Some((kind, n, _)) = entry
            .action
            .as_deref()
            .and_then(crate::core::security_events::parse_action)
        {
            self.security.record(kind, n, &entry.entry_hash);
            if kind.in_signed_tally() {
                self.tally.record(kind, n, &entry.entry_hash);
            }
        }
        match entry.event_type {
            AuditEventType::AgentDriftDetected => {
                if let Some(evidence) = entry.action {
                    self.pending.insert(
                        entry.agent_id,
                        (
                            entry.timestamp,
                            format!("{evidence} observation={}", entry.entry_hash),
                        ),
                    );
                }
            }
            AuditEventType::AgentDriftAcknowledged
                if self
                    .pending
                    .get(&entry.agent_id)
                    .is_some_and(|(_, evidence)| entry.action.as_ref() == Some(evidence)) =>
            {
                self.pending.remove(&entry.agent_id);
            }
            _ => {}
        }
        self.head = (head != entry_hash).then_some(head);
        self.prev_hash = entry_hash;
        self.total += 1;
        Ok(())
    }

    /// The v2 chain head the next entry hashes after.
    fn head(&self) -> &str {
        self.head.as_deref().unwrap_or(&self.prev_hash)
    }

    /// Verify every line of `reader` (the same line rules as
    /// `BufRead::lines`). Returns the bytes consumed and whether the input
    /// ended on a line boundary (empty input counts as one).
    pub(super) fn verify_stream(&mut self, reader: &mut impl BufRead) -> Result<(u64, bool), ()> {
        let mut consumed = 0u64;
        let mut ends_on_newline = true;
        let mut buf = Vec::new();
        let mut key = None;
        loop {
            buf.clear();
            let read = reader.read_until(b'\n', &mut buf).map_err(|_| ())?;
            if read == 0 {
                return Ok((consumed, ends_on_newline));
            }
            consumed += read as u64;
            ends_on_newline = buf.last() == Some(&b'\n');
            if ends_on_newline {
                buf.pop();
                if buf.last() == Some(&b'\r') {
                    buf.pop();
                }
            }
            let line = std::str::from_utf8(&buf).map_err(|_| ())?;
            let mut saw_value = false;
            for parsed in serde_json::Deserializer::from_str(line).into_iter::<serde_json::Value>()
            {
                saw_value = true;
                self.advance(parsed.map_err(|_| ())?, &mut key)?;
            }
            if !saw_value {
                return Err(());
            }
        }
    }
}

/// Verify the trail at `path`. With `force_full == false` a signed,
/// identity-bound checkpoint of the last verified prefix (see
/// [`checkpoint`]) lets the verifier read only the appended suffix; any
/// doubt about the prefix falls back to a full pass. A full pass refreshes
/// the checkpoint.
fn verify_chain_mode(
    path: &Path,
    agent_id: Option<&str>,
    force_full: bool,
) -> (ChainVerifyResult, Option<(String, String)>) {
    verify_chain_mode_at(
        path,
        agent_id,
        force_full,
        lean_ctx_time::UtcDateTime::now().timestamp(),
    )
}

/// [`verify_chain_mode`] at an explicit wall-clock `now` (Unix seconds).
pub(super) fn verify_chain_mode_at(
    path: &Path,
    agent_id: Option<&str>,
    force_full: bool,
    now: i64,
) -> (ChainVerifyResult, Option<(String, String)>) {
    match verify_state_at(path, force_full, now) {
        Verified::Missing => (
            ChainVerifyResult {
                total_entries: 0,
                valid: true,
                first_invalid_at: None,
                source_present: false,
                unknown_event_types: 0,
            },
            None,
        ),
        Verified::Unavailable => (unavailable(), None),
        Verified::Invalid(state) => state.invalid(),
        Verified::Valid(state) => state.valid(agent_id),
    }
}

enum Verified {
    /// No trail file: a valid empty history.
    Missing,
    /// Unreadable or lock contention: fail closed, but not corruption.
    Unavailable,
    /// Broken at `state.total`.
    Invalid(ChainState),
    Valid(ChainState),
}

/// Verify the trail at `path` through its sealed segments and the active
/// file, in chain order. The segment list is read before and after the
/// active file is opened, so a seal that lands in between is retried
/// instead of read as a gap.
fn verify_state_at(path: &Path, force_full: bool, now: i64) -> Verified {
    for _ in 0..SNAPSHOT_ATTEMPTS {
        let sealed = segments::sealed(path);
        let active = match fs::File::open(path) {
            Ok(file) => Some(file),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => return Verified::Unavailable,
        };
        if segments::sealed(path) == sealed {
            return verify_snapshot(path, &sealed, active, force_full, now);
        }
    }
    Verified::Unavailable
}

fn verify_snapshot(
    path: &Path,
    sealed: &[(u64, PathBuf)],
    active: Option<fs::File>,
    force_full: bool,
    now: i64,
) -> Verified {
    if sealed.is_empty() && active.is_none() {
        return Verified::Missing;
    }
    // A sealed segment is never written again, so its checkpoint is trusted
    // past the 24 h full pass, except for one segment per day in rotation.
    let day = usize::try_from(now.div_euclid(24 * 60 * 60)).unwrap_or(0);
    let sample = day.checked_rem(sealed.len());
    let mut state = ChainState::genesis();
    for (index, (seq, segment)) in sealed.iter().enumerate() {
        let Ok(file) = fs::File::open(segment) else {
            return Verified::Unavailable;
        };
        let max_age = (Some(index) == sample).then_some(checkpoint::FULL_REVERIFY_SECS);
        state = match verify_file(segment, &file, &state, force_full, max_age, now) {
            // Each segment ends with the seal that names it.
            Verified::Valid(state) if state.sealed == Some(*seq) => state,
            Verified::Valid(state) => return Verified::Invalid(state),
            other => return other,
        };
    }
    match active {
        Some(file) => verify_file(
            path,
            &file,
            &state,
            force_full,
            Some(checkpoint::FULL_REVERIFY_SECS),
            now,
        ),
        None => Verified::Valid(state),
    }
}

/// Verify one file of the trail, continuing from `start`: from its
/// checkpoint when one resumes (see [`checkpoint::Store::resume`]), else in
/// full.
fn verify_file(
    path: &Path,
    file: &fs::File,
    start: &ChainState,
    force_full: bool,
    max_age: Option<i64>,
    now: i64,
) -> Verified {
    use std::io::{Seek, SeekFrom};

    let mut cursor = file;
    // Contention is not corruption: a reader that cannot get the lock in time
    // reports the trail unavailable (fail-closed), never invalid. Appends need
    // the exclusive lock, so the stamp taken under it describes exactly the
    // prefix `file[..len]` read below, which no append changes afterwards (see
    // `release_read_lock`); the pass itself runs without the lock.
    if lock_trail_shared(file).is_err() {
        return Verified::Unavailable;
    }
    let store = checkpoint::Store::for_trail(path);
    let stamp = checkpoint::FileStamp::of(file);
    let Some(len) = stamp
        .as_ref()
        .map(|stamp| stamp.size)
        .or_else(|| file.metadata().ok().map(|meta| meta.len()))
    else {
        return Verified::Unavailable;
    };
    release_read_lock(file);

    if !force_full {
        if let (Some(store), Some(stamp)) = (&store, &stamp) {
            if let Some(resumed) = store.resume(file, stamp, start, max_age, now) {
                let mut state = resumed.state;
                if resumed.offset == stamp.size {
                    return Verified::Valid(state);
                }
                if cursor.seek(SeekFrom::Start(resumed.offset)).is_ok() {
                    let suffix = state.verify_stream(&mut std::io::BufReader::new(
                        file.take(len - resumed.offset),
                    ));
                    if !still_holds_prefix(file, len) {
                        return Verified::Unavailable;
                    }
                    if let Ok((read, clean)) = suffix {
                        if clean && resumed.offset + read == stamp.size {
                            store.save(file, stamp, start, &state, resumed.full_verified_at);
                        }
                        return Verified::Valid(state);
                    }
                }
                // A broken suffix is reported by a full pass, so the index
                // is the true first invalid entry even if the prefix changed.
            }
        }
    }

    if cursor.seek(SeekFrom::Start(0)).is_err() {
        return Verified::Unavailable;
    }
    let mut state = start.clone();
    let pass = state.verify_stream(&mut std::io::BufReader::new(file.take(len)));
    // Cut in place while it was read: what the pass saw is no verdict.
    if !still_holds_prefix(file, len) {
        return Verified::Unavailable;
    }
    let Ok((read, clean)) = pass else {
        if let Some(store) = &store {
            store.discard();
        }
        return Verified::Invalid(state);
    };
    if let (Some(store), Some(stamp)) = (&store, &stamp) {
        if clean && read == stamp.size {
            store.save(file, stamp, start, &state, now);
        }
    }
    Verified::Valid(state)
}

/// Security evidence a signed savings-batch tally rests on, aggregated while
/// the chain is verified: the counts of the kinds the tally carries and the
/// entry hashes of the entries they come from.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedTallyEvidence {
    pub counts: crate::core::security_events::SecurityCounts,
    pub entries: u64,
    pub first_hash: Option<String>,
    pub last_hash: Option<String>,
}

impl SignedTallyEvidence {
    fn record(&mut self, kind: crate::core::security_events::SecurityKind, n: u64, hash: &str) {
        self.counts.add(kind, n);
        self.entries += 1;
        self.first_hash.get_or_insert_with(|| hash.to_string());
        self.last_hash = Some(hash.to_string());
    }
}

/// The signed-tally evidence of the trail at `path`, from the same
/// (incremental) pass that verifies the chain. `None` when the trail fails
/// verification or is unavailable; a missing trail is an empty tally.
pub fn verified_signed_tally_at(path: &Path) -> Option<SignedTallyEvidence> {
    match verify_state_at(path, false, lean_ctx_time::UtcDateTime::now().timestamp()) {
        Verified::Missing => Some(SignedTallyEvidence::default()),
        Verified::Valid(state) => Some(state.tally),
        Verified::Unavailable | Verified::Invalid(_) => None,
    }
}

/// The chain check of the trail at `path` and the security evidence of
/// every kind it holds, from one (incremental) verification: what the
/// lifetime value proof shows, without reading the trail a second time.
/// The evidence is `None` unless the chain verifies; a missing trail is an
/// empty, intact chain.
pub fn verified_security_at(path: &Path) -> (ChainVerifyResult, Option<SignedTallyEvidence>) {
    match verify_state_at(path, false, lean_ctx_time::UtcDateTime::now().timestamp()) {
        Verified::Missing => (
            ChainVerifyResult {
                total_entries: 0,
                valid: true,
                first_invalid_at: None,
                source_present: false,
                unknown_event_types: 0,
            },
            Some(SignedTallyEvidence::default()),
        ),
        Verified::Unavailable => (unavailable(), None),
        Verified::Invalid(state) => (state.invalid().0, None),
        Verified::Valid(state) => (state.valid(None).0, Some(state.security)),
    }
}

/// Verify the default trail, resuming from the verified checkpoint when it
/// is still trustworthy (hot path: billing, dashboard, drift reconstruction).
pub fn verify_chain() -> ChainVerifyResult {
    verify_chain_with_drift(None).0
}

/// Verify the default trail with a full pass from genesis, ignoring any
/// checkpoint. For explicit audit commands and signed compliance reports.
pub fn verify_chain_full() -> ChainVerifyResult {
    let Some(path) = trail_path() else {
        return unavailable();
    };
    verify_chain_mode(&path, None, true).0
}

/// Verify the hash chain of the trail at an explicit `path` (incremental,
/// like [`verify_chain`]).
pub fn verify_chain_at(path: &Path) -> ChainVerifyResult {
    verify_chain_with_drift_at(path, None).0
}
