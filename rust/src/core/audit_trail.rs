use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub timestamp: String,
    pub agent_id: String,
    pub tool: String,
    pub action: Option<String>,
    pub input_hash: String,
    pub output_tokens: u32,
    pub role: String,
    pub event_type: AuditEventType,
    pub prev_hash: String,
    pub entry_hash: String,
    /// Ed25519 signature over `entry_hash`, proving provenance from the local
    /// lean-ctx identity. `None` only when the keypair is unavailable (early boot).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub signature: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuditEventType {
    ToolCall,
    ToolDenied,
    PathJailViolation,
    BudgetExceeded,
    CrossProjectAccess,
    RateLimited,
    SecurityViolation,
    RoleChanged,
    SecretDetected,
    // Agent identity lifecycle (GL #433) — additive OCP Part 4 evolution.
    AgentRegistered,
    AgentSuspended,
    AgentResumed,
    AgentDecommissioned,
    /// Attestation drift observed on heartbeat: the running binary and/or
    /// the active role file no longer match the attestation the registry
    /// holds for this identity (GL #433 / Phase 18 S3).
    AgentDriftDetected,
    /// An operator explicitly acknowledged one specific observed drift,
    /// naming its evidence. Only this event clears the sticky drift mark —
    /// resuming a suspended identity does not (GL #433 / Phase 18 S3).
    AgentDriftAcknowledged,
    /// Forward-compatible additive event emitted by a newer writer. Keep the
    /// exact wire label so reports never silently discard security evidence.
    Unknown(String),
}

impl AuditEventType {
    fn wire_label(&self) -> &str {
        match self {
            Self::ToolCall => "tool_call",
            Self::ToolDenied => "tool_denied",
            Self::PathJailViolation => "path_jail_violation",
            Self::BudgetExceeded => "budget_exceeded",
            Self::CrossProjectAccess => "cross_project_access",
            Self::RateLimited => "rate_limited",
            Self::SecurityViolation => "security_violation",
            Self::RoleChanged => "role_changed",
            Self::SecretDetected => "secret_detected",
            Self::AgentRegistered => "agent_registered",
            Self::AgentSuspended => "agent_suspended",
            Self::AgentResumed => "agent_resumed",
            Self::AgentDecommissioned => "agent_decommissioned",
            Self::AgentDriftDetected => "agent_drift_detected",
            Self::AgentDriftAcknowledged => "agent_drift_acknowledged",
            Self::Unknown(label) => label,
        }
    }
}

impl Serialize for AuditEventType {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        if let Self::Unknown(label) = self {
            if !valid_unknown_event_label_for_write(label) {
                return Err(serde::ser::Error::custom(format!(
                    "unknown event_type is not a safe additive label: {label}"
                )));
            }
        }
        serializer.serialize_str(self.wire_label())
    }
}

impl<'de> Deserialize<'de> for AuditEventType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let label = String::deserialize(deserializer)?;
        let event_type = match label.as_str() {
            "tool_call" => Self::ToolCall,
            "tool_denied" => Self::ToolDenied,
            "path_jail_violation" => Self::PathJailViolation,
            "budget_exceeded" => Self::BudgetExceeded,
            "cross_project_access" => Self::CrossProjectAccess,
            "rate_limited" => Self::RateLimited,
            "security_violation" => Self::SecurityViolation,
            "role_changed" => Self::RoleChanged,
            "secret_detected" => Self::SecretDetected,
            "agent_registered" => Self::AgentRegistered,
            "agent_suspended" => Self::AgentSuspended,
            "agent_resumed" => Self::AgentResumed,
            "agent_decommissioned" => Self::AgentDecommissioned,
            "agent_drift_detected" => Self::AgentDriftDetected,
            "agent_drift_acknowledged" => Self::AgentDriftAcknowledged,
            _ if valid_extension_event_label(&label) => Self::Unknown(label),
            _ => {
                return Err(serde::de::Error::custom(
                    "unknown event_type must use the x- extension namespace",
                ));
            }
        };
        Ok(event_type)
    }
}

fn valid_extension_event_label(label: &str) -> bool {
    const MAX_EVENT_LABEL_BYTES: usize = 64;
    if label.len() > MAX_EVENT_LABEL_BYTES {
        return false;
    }
    let Some(rest) = label.strip_prefix("x-") else {
        return false;
    };
    let mut chars = rest.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn valid_unknown_event_label_for_write(label: &str) -> bool {
    valid_extension_event_label(label)
}

pub struct AuditEntryData {
    pub agent_id: String,
    pub tool: String,
    pub action: Option<String>,
    pub input_hash: String,
    pub output_tokens: u32,
    pub role: String,
    pub event_type: AuditEventType,
}

pub struct ChainVerifyResult {
    pub total_entries: usize,
    pub valid: bool,
    pub first_invalid_at: Option<usize>,
    /// Whether the audit trail file existed and was readable. A missing trail
    /// is a valid empty history for a fresh install, but callers that already
    /// hold audited state must treat `false` as evidence loss.
    pub source_present: bool,
    /// Entries whose `event_type` this build does not know, but whose hash
    /// links verify. Forward compatibility, NOT corruption: a reader older
    /// than the writer must report those separately instead of raising a
    /// false tamper alarm (OCP Part 4 additive-event rule). Actual hash or
    /// structure damage still sets `valid = false`.
    pub unknown_event_types: usize,
}

fn trail_path() -> Option<PathBuf> {
    let dir = crate::core::data_dir::lean_ctx_data_dir().ok()?;
    let audit_dir = dir.join("audit");
    fs::create_dir_all(&audit_dir).ok()?;
    Some(audit_dir.join("trail.jsonl"))
}

/// Read the chain tail from the file itself. Called under the exclusive
/// file lock — the file is the ONLY source of truth for `prev_hash`. (A
/// per-process cache forked the chain whenever two processes appended
/// concurrently — found by `leanctx-verify` on a real trail, GL #425.)
///
/// Every I/O failure is propagated instead of being folded into `"genesis"`:
/// a seek/read error on a NON-empty trail is indistinguishable from an empty
/// one at that point, so the old fallback silently forked the chain and still
/// returned `Ok` to a caller (attestation drift) that commits state on it.
fn read_last_hash_tail(file: &fs::File) -> Result<String, String> {
    use std::io::{Read, Seek, SeekFrom};
    const TAIL: i64 = 64 * 1024;

    let mut f = file;
    let len = f
        .seek(SeekFrom::End(0))
        .map_err(|e| format!("audit tail seek: {e}"))?;
    if len == 0 {
        return Ok("genesis".to_string());
    }
    let start = if (len as i64) > TAIL {
        -TAIL
    } else {
        -(len as i64)
    };
    f.seek(SeekFrom::End(start))
        .map_err(|e| format!("audit tail seek: {e}"))?;
    let mut buf = String::new();
    f.read_to_string(&mut buf)
        .map_err(|e| format!("audit tail read: {e}"))?;
    // Concurrent-append history may hold multiple objects per line; a
    // stream parse of the last non-empty line yields the true tail entry.
    for line in buf.lines().rev() {
        if line.trim().is_empty() {
            continue;
        }
        let mut last: Option<String> = None;
        for parsed in serde_json::Deserializer::from_str(line).into_iter::<serde_json::Value>() {
            let v = parsed.map_err(|e| format!("audit tail parse: {e}"))?;
            if let Some(h) = v.get("entry_hash").and_then(|h| h.as_str()) {
                last = Some(h.to_string());
            }
        }
        if let Some(h) = last {
            return Ok(h);
        }
    }
    // Non-empty file with no parseable entry_hash in the tail window: the
    // trail exists but has no usable head, so a fresh chain would fork it.
    Err("audit tail: no readable chain head".to_string())
}

fn compute_entry_hash(prev_hash: &str, data_json: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(prev_hash.as_bytes());
    hasher.update(data_json.as_bytes());
    crate::core::agent_identity::hex_encode(&hasher.finalize())
}

/// Fire-and-forget append. The hot tool path must never fail a tool call
/// because the audit sink is momentarily unavailable, so every error is
/// swallowed here. Callers whose own state update is only sound *with* the
/// evidence on disk (agent attestation drift) use [`try_record`] instead.
pub fn record(data: AuditEntryData) {
    let _ = try_record(data);
}

/// How long an append waits for the cross-process trail lock before giving
/// up. Bounded on purpose: the drift path takes this lock while it holds the
/// (also deadline-bounded) identity-registry lock, so an unbounded blocking
/// `flock` would let one stalled process freeze every registry operation.
const LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(750);
const LOCK_RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_millis(25);

/// Acquire the exclusive trail lock, bounded by [`LOCK_TIMEOUT`].
fn lock_trail(file: &fs::File) -> Result<(), String> {
    use fs2::FileExt;
    use std::time::Instant;

    let deadline = Instant::now() + LOCK_TIMEOUT;
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(()),
            Err(error) if super::file_lock::is_contended(&error) => {
                if Instant::now() >= deadline {
                    return Err(format!(
                        "audit trail lock timed out after {}ms; another append is still active",
                        LOCK_TIMEOUT.as_millis()
                    ));
                }
                std::thread::sleep(LOCK_RETRY_INTERVAL);
            }
            Err(error) => return Err(format!("audit trail lock: {error}")),
        }
    }
}

/// Fallible append. Returns the persisted entry, so a caller can commit its
/// own state only once the evidence is durable. On `Err` nothing was
/// appended — a short write is rolled back to the pre-append length — which
/// makes a retry safe and leaves the hash chain intact.
///
/// Durability is AT-LEAST-ONCE, not exactly-once: the line is `fsync`ed, but
/// the caller's own state file is a separate object with no cross-file
/// atomicity, so a crash between the two can leave the entry on disk without
/// the state update. The direction is deliberate — a duplicate observation on
/// the next attempt, never a lost one.
pub fn try_record(data: AuditEntryData) -> Result<AuditEntry, String> {
    let path = trail_path().ok_or_else(|| "audit trail unavailable".to_string())?;
    try_record_at(&path, data)
}

/// Append one entry to the trail at an explicit `path` (used by callers that
/// keep their own chained trail, e.g. value proofs). Errors are swallowed like
/// [`record`].
pub fn record_at(path: &Path, data: AuditEntryData) {
    let _ = try_record_at(path, data);
}

fn try_record_at(path: &Path, data: AuditEntryData) -> Result<AuditEntry, String> {
    use fs2::FileExt;

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .open(path)
        .map_err(|e| format!("audit trail open: {e}"))?;
    // Advisory lock serializes appends ACROSS processes; prev_hash is read
    // from the file under the same lock, so the chain cannot fork.
    lock_trail(&file)?;
    let appended = append_locked(&mut file, data);
    let _ = FileExt::unlock(&file);
    appended
}

/// Build and append one entry. Caller holds the exclusive lock.
fn append_locked(file: &mut fs::File, data: AuditEntryData) -> Result<AuditEntry, String> {
    use std::io::{Seek, SeekFrom};

    let prev_hash = read_last_hash_tail(file)?;
    let authenticated_drift = matches!(
        &data.event_type,
        AuditEventType::AgentDriftDetected | AuditEventType::AgentDriftAcknowledged
    );

    let partial = serde_json::json!({
        "agent_id": data.agent_id,
        "tool": data.tool,
        "action": data.action,
        "input_hash": data.input_hash,
        "output_tokens": data.output_tokens,
        "role": data.role,
        "event_type": data.event_type,
    });
    let data_json = serde_json::to_string(&partial).map_err(|e| format!("audit serialize: {e}"))?;
    let entry_hash = compute_entry_hash(&prev_hash, &data_json);

    let signature_result =
        crate::core::agent_identity::sign_bytes("lean-ctx", entry_hash.as_bytes())
            .map(|sig| crate::core::agent_identity::hex_encode(&sig));
    let signature = if authenticated_drift {
        Some(signature_result.map_err(|e| format!("audit signature: {e}"))?)
    } else {
        signature_result.ok()
    };

    let entry = AuditEntry {
        timestamp: chrono::Utc::now().to_rfc3339(),
        agent_id: data.agent_id,
        tool: data.tool,
        action: data.action,
        input_hash: data.input_hash,
        output_tokens: data.output_tokens,
        role: data.role,
        event_type: data.event_type,
        prev_hash,
        entry_hash,
        signature,
    };

    let mut line = serde_json::to_string(&entry).map_err(|e| format!("audit serialize: {e}"))?;
    line.push('\n');
    // A short write would leave a truncated JSON line and invalidate the
    // chain from that point onwards; roll the file back to its pre-append
    // length so a failed append is a no-op instead of corruption.
    let rollback_len = file
        .seek(SeekFrom::End(0))
        .map_err(|e| format!("audit seek: {e}"))?;
    if let Err(e) = file
        .write_all(line.as_bytes())
        .and_then(|()| file.flush())
        .and_then(|()| file.sync_data())
    {
        let _ = file.set_len(rollback_len);
        return Err(format!("audit append: {e}"));
    }
    Ok(entry)
}

/// Path of the local audit trail (`<data_dir>/audit/trail.jsonl`).
pub fn default_trail_path() -> Option<PathBuf> {
    trail_path()
}

pub fn load_recent(limit: usize) -> Vec<AuditEntry> {
    let Some(path) = trail_path() else {
        return Vec::new();
    };
    let entries = load_all_at(&path);
    let skip = entries.len().saturating_sub(limit);
    entries.into_iter().skip(skip).collect()
}

/// Every parseable entry of the trail at `path`.
pub fn load_all_at(path: &Path) -> Vec<AuditEntry> {
    let Ok(file) = fs::File::open(path) else {
        return Vec::new();
    };
    let reader = std::io::BufReader::new(file);
    let mut entries = Vec::new();
    for line in reader.lines().map_while(Result::ok) {
        entries.extend(
            serde_json::Deserializer::from_str(&line)
                .into_iter::<AuditEntry>()
                .map_while(Result::ok),
        );
    }
    entries
}

/// Reconstruct the latest unacknowledged drift from the append-only trail.
/// This is the downgrade-safe source of truth: an older binary may rewrite an
/// `Attestation` without its newer `drift` field, but it cannot synthesize the
/// explicit acknowledgement event required to clear this history.
pub fn latest_unacknowledged_drift(agent_id: &str) -> Result<Option<(String, String)>, String> {
    let (chain, pending) = verify_chain_with_drift(Some(agent_id));
    if !chain.source_present {
        return Err("audit trail unavailable".to_string());
    }
    if !chain.valid {
        return Err(format!(
            "audit chain invalid at entry {}",
            chain.first_invalid_at.unwrap_or(chain.total_entries)
        ));
    }
    if chain.total_entries == 0 {
        // An existing zero-byte trail is evidence loss for any identity that
        // has already reached an attested state. Keep the generic verifier's
        // fresh-install view (`valid=true`, `source_present=true`) compatible,
        // but do not let a caller that reconstructs durable identity state
        // interpret an emptied trail as a clean history.
        return Err("audit trail unavailable".to_string());
    }

    Ok(pending)
}

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
fn verify_chain_with_drift(
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
    let empty = ChainVerifyResult {
        total_entries: 0,
        valid: true,
        first_invalid_at: None,
        source_present: false,
        unknown_event_types: 0,
    };
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return (empty, None),
        Err(_) => return (unavailable(), None),
    };
    let reader = std::io::BufReader::new(file);
    let mut prev_hash = "genesis".to_string();
    let mut total = 0usize;
    let mut unknown = 0usize;
    let mut pending = None;

    for line_result in reader.lines() {
        let Ok(line) = line_result else {
            return (invalid_at(total, unknown), None);
        };
        let mut saw_value = false;
        for parsed in serde_json::Deserializer::from_str(&line).into_iter::<serde_json::Value>() {
            saw_value = true;
            let Ok(value) = parsed else {
                return (invalid_at(total, unknown), None);
            };
            let (Some(entry_prev), Some(entry_hash)) = (
                value.get("prev_hash").and_then(serde_json::Value::as_str),
                value.get("entry_hash").and_then(serde_json::Value::as_str),
            ) else {
                return (invalid_at(total, unknown), None);
            };
            let Some(partial) = hashed_fields(&value) else {
                return (invalid_at(total, unknown), None);
            };
            if entry_prev != prev_hash {
                return (invalid_at(total, unknown), None);
            }
            let data_json = serde_json::to_string(&partial).unwrap_or_default();
            if entry_hash != compute_entry_hash(&prev_hash, &data_json) {
                return (invalid_at(total, unknown), None);
            }
            let entry_hash = entry_hash.to_string();
            let Ok(entry) = serde_json::from_value::<AuditEntry>(value) else {
                return (invalid_at(total, unknown), None);
            };
            if matches!(&entry.event_type, AuditEventType::Unknown(_)) {
                unknown += 1;
            }
            if matches!(
                &entry.event_type,
                AuditEventType::AgentDriftDetected | AuditEventType::AgentDriftAcknowledged
            ) {
                let Some(signature) = entry.signature.as_deref() else {
                    return (invalid_at(total, unknown), None);
                };
                let Ok(signature) = crate::core::agent_identity::hex_decode(signature) else {
                    return (invalid_at(total, unknown), None);
                };
                let Ok(public_key) = crate::core::agent_identity::load_public_key("lean-ctx")
                else {
                    return (invalid_at(total, unknown), None);
                };
                if !crate::core::agent_identity::verify_signature(
                    public_key.as_bytes(),
                    entry.entry_hash.as_bytes(),
                    &signature,
                ) {
                    return (invalid_at(total, unknown), None);
                }
            }
            if agent_id == Some(entry.agent_id.as_str()) {
                match entry.event_type {
                    AuditEventType::AgentDriftDetected => {
                        if let Some(evidence) = entry.action {
                            pending = Some((
                                entry.timestamp,
                                format!("{evidence} observation={}", entry.entry_hash),
                            ));
                        }
                    }
                    AuditEventType::AgentDriftAcknowledged
                        if pending.as_ref().is_some_and(|(_, evidence)| {
                            entry.action.as_ref() == Some(evidence)
                        }) =>
                    {
                        pending = None;
                    }
                    _ => {}
                }
            }
            prev_hash = entry_hash;
            total += 1;
        }
        if !saw_value {
            return (invalid_at(total, unknown), None);
        }
    }

    (
        ChainVerifyResult {
            total_entries: total,
            valid: true,
            first_invalid_at: None,
            source_present: true,
            unknown_event_types: unknown,
        },
        pending,
    )
}

pub fn verify_chain() -> ChainVerifyResult {
    verify_chain_with_drift(None).0
}

/// Verify the hash chain of the trail at an explicit `path`.
pub fn verify_chain_at(path: &Path) -> ChainVerifyResult {
    verify_chain_with_drift_at(path, None).0
}

pub fn hash_input(args: &serde_json::Map<String, serde_json::Value>) -> String {
    let serialized = serde_json::to_string(args).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(serialized.as_bytes());
    crate::core::agent_identity::hex_encode(&hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drift_data() -> AuditEntryData {
        AuditEntryData {
            agent_id: "drift-agent".to_string(),
            tool: "agent_registry".to_string(),
            action: Some("attestation drift dimensions=binary+config".to_string()),
            input_hash: hash_input(&serde_json::Map::new()),
            output_tokens: 0,
            role: "coder".to_string(),
            event_type: AuditEventType::AgentDriftDetected,
        }
    }

    /// The drift event must be a first-class, on-disk-stable event type:
    /// the OCP Part 4 contract enumerates the snake_case label, so a rename
    /// would silently invalidate every consumer's schema check.
    #[test]
    fn drift_event_type_has_a_stable_wire_label() {
        let json = serde_json::to_string(&AuditEventType::AgentDriftDetected).expect("serialize");
        assert_eq!(json, "\"agent_drift_detected\"");
        let parsed: AuditEventType =
            serde_json::from_str("\"agent_drift_detected\"").expect("deserialize");
        assert!(matches!(parsed, AuditEventType::AgentDriftDetected));
    }

    #[test]
    fn try_record_appends_a_chained_drift_entry() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let first = try_record(drift_data()).expect("first append");
        assert_eq!(first.prev_hash, "genesis");
        let second = try_record(drift_data()).expect("second append");
        assert_eq!(second.prev_hash, first.entry_hash);

        let entries = load_recent(10);
        assert_eq!(entries.len(), 2);
        assert!(
            entries
                .iter()
                .all(|e| matches!(e.event_type, AuditEventType::AgentDriftDetected))
        );
        let chain = verify_chain();
        assert!(
            chain.valid,
            "chain must verify: {:?}",
            chain.first_invalid_at
        );
        assert_eq!(chain.total_entries, 2);
    }

    #[test]
    fn drift_entries_require_a_valid_local_signature() {
        let iso = crate::core::data_dir::isolated_data_dir();
        let entry = try_record(drift_data()).expect("append signed drift");
        assert!(
            entry.signature.is_some(),
            "drift writes must fail without a signature"
        );

        let path = iso.path().join("audit").join("trail.jsonl");
        let mut value: serde_json::Value =
            serde_json::from_str(std::fs::read_to_string(&path).expect("trail").trim_end())
                .expect("entry JSON");
        value["signature"] = serde_json::Value::String("00".repeat(64));
        std::fs::write(
            &path,
            format!("{}\n", serde_json::to_string(&value).expect("JSON")),
        )
        .expect("forge signature");

        let chain = verify_chain();
        assert!(
            !chain.valid,
            "a hash-linked forged drift entry must fail authentication"
        );
        assert_eq!(chain.first_invalid_at, Some(0));
    }

    /// A blocked sink must surface as an error rather than being swallowed —
    /// that is the whole reason `try_record` exists. `record` keeps the
    /// historical fire-and-forget contract on the same failure.
    #[test]
    fn try_record_surfaces_sink_failure_that_record_swallows() {
        let iso = crate::core::data_dir::isolated_data_dir();
        // A regular file where the audit directory belongs makes
        // `create_dir_all` fail, so no trail path can be resolved.
        std::fs::write(iso.path().join("audit"), b"not a directory").expect("block audit dir");

        let error = try_record(drift_data()).expect_err("blocked sink must fail");
        assert!(error.contains("audit trail unavailable"), "{error}");
        record(drift_data());
        assert!(load_recent(10).is_empty(), "nothing may be persisted");
    }

    #[test]
    fn unknown_additive_event_verifies_as_forward_compatible_not_tampered() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let path = trail_path().expect("trail path");
        let event_type = "x-agent_future_observed";
        let partial = serde_json::json!({
            "agent_id": "forward-agent",
            "tool": "agent_registry",
            "action": null,
            "input_hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "output_tokens": 0,
            "role": "coder",
            "event_type": event_type,
        });
        let data_json = serde_json::to_string(&partial).expect("partial serializes");
        let entry_hash = compute_entry_hash("genesis", &data_json);
        let mut entry = serde_json::json!({
            "timestamp": "2026-09-06T20:00:00Z",
            "agent_id": "forward-agent",
            "tool": "agent_registry",
            "action": null,
            "input_hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "output_tokens": 0,
            "role": "coder",
            "event_type": event_type,
            "prev_hash": "genesis",
            "entry_hash": entry_hash,
        });
        std::fs::write(
            &path,
            format!(
                "{}\n",
                serde_json::to_string(&entry).expect("entry serializes")
            ),
        )
        .expect("write forward entry");

        let forward = verify_chain();
        assert!(forward.valid, "unknown additive event is not tampering");
        assert_eq!(forward.total_entries, 1);
        assert_eq!(forward.unknown_event_types, 1);
        assert_eq!(forward.first_invalid_at, None);
        let recent = load_recent(1);
        assert!(matches!(
            recent.as_slice(),
            [AuditEntry {
                event_type: AuditEventType::Unknown(label),
                ..
            }] if label == event_type
        ));

        try_record(drift_data()).expect("append after unknown event");
        let continued = verify_chain();
        assert!(continued.valid);
        assert_eq!(continued.total_entries, 2);
        assert_eq!(continued.unknown_event_types, 1);

        entry["entry_hash"] = serde_json::json!("0".repeat(64));
        std::fs::write(
            &path,
            format!(
                "{}\n",
                serde_json::to_string(&entry).expect("tampered serializes")
            ),
        )
        .expect("write tampered entry");
        let tampered = verify_chain();
        assert!(
            !tampered.valid,
            "actual hash corruption must remain invalid"
        );
        assert_eq!(tampered.first_invalid_at, Some(0));
    }

    #[test]
    fn unnamespaced_unknown_event_is_rejected_even_when_hash_linked() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let path = trail_path().expect("trail path");
        let event_type = "agent_future_observed";
        let partial = serde_json::json!({
            "agent_id": "forward-agent",
            "tool": "agent_registry",
            "action": null,
            "input_hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "output_tokens": 0,
            "role": "coder",
            "event_type": event_type,
        });
        let data_json = serde_json::to_string(&partial).expect("partial serializes");
        let entry_hash = compute_entry_hash("genesis", &data_json);
        let entry = serde_json::json!({
            "timestamp": "2026-09-06T20:00:00Z",
            "agent_id": "forward-agent",
            "tool": "agent_registry",
            "action": null,
            "input_hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "output_tokens": 0,
            "role": "coder",
            "event_type": event_type,
            "prev_hash": "genesis",
            "entry_hash": entry_hash,
        });
        std::fs::write(
            &path,
            format!("{}\n", serde_json::to_string(&entry).unwrap()),
        )
        .expect("write future entry");

        let result = verify_chain();
        assert!(!result.valid);
        assert_eq!(result.first_invalid_at, Some(0));
        assert!(load_recent(1).is_empty());
    }

    #[test]
    fn writers_reject_unsafe_unknown_event_labels() {
        let unsafe_event = AuditEventType::Unknown("future\nlabel".to_string());
        assert!(serde_json::to_string(&unsafe_event).is_err());
        assert!(
            serde_json::to_string(&AuditEventType::Unknown(
                "agent_future_observed".to_string()
            ))
            .is_err()
        );
        assert!(
            serde_json::to_string(&AuditEventType::Unknown(
                "x-agent_future_observed".to_string()
            ))
            .is_ok()
        );
        assert!(
            serde_json::to_string(&AuditEventType::Unknown(format!("x-a{}", "b".repeat(64))))
                .is_err(),
            "extension labels must stay report-bounded"
        );
    }

    #[test]
    fn verifier_accepts_legacy_multiple_objects_on_one_line() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        try_record(drift_data()).expect("first append");
        try_record(drift_data()).expect("second append");
        let path = trail_path().expect("trail path");
        let joined = std::fs::read_to_string(&path)
            .expect("audit trail")
            .lines()
            .collect::<String>();
        std::fs::write(&path, format!("{joined}\n")).expect("legacy joined line");

        let chain = verify_chain();
        assert!(chain.valid, "legacy stream must remain verifiable");
        assert_eq!(chain.total_entries, 2);
        assert_eq!(load_recent(2).len(), 2);
    }

    #[test]
    fn audited_state_fails_closed_when_trail_disappears() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        try_record(drift_data()).expect("append drift");
        let path = trail_path().expect("trail path");
        std::fs::remove_file(path).expect("remove audit trail");

        let chain = verify_chain();
        assert!(chain.valid, "missing file is an empty history generically");
        assert!(!chain.source_present);
        let error = latest_unacknowledged_drift("drift-agent").expect_err("evidence loss");
        assert!(error.contains("unavailable"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn audit_lock_contention_is_bounded() {
        use fs2::FileExt;
        use std::time::{Duration, Instant};

        let _iso = crate::core::data_dir::isolated_data_dir();
        let path = trail_path().expect("trail path");
        let held = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .expect("trail lock file");
        held.lock_exclusive().expect("hold trail lock");

        let started = Instant::now();
        let error = try_record(drift_data()).expect_err("contention must fail closed");
        assert!(error.contains("timed out"), "unexpected error: {error}");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "bounded audit lock exceeded two seconds"
        );
    }
}
