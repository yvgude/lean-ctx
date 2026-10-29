use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};

mod checkpoint;
mod segments;

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
    Ok(read_tail_entry(file)?
        .and_then(|entry| entry.get("entry_hash")?.as_str().map(str::to_string))
        .unwrap_or_else(|| "genesis".to_string()))
}

/// The last entry of `file` as raw JSON; `None` for an empty file. A
/// non-empty file without a readable entry in its tail is an error.
fn read_tail_entry(file: &fs::File) -> Result<Option<serde_json::Value>, String> {
    use std::io::{Read, Seek, SeekFrom};
    const TAIL: i64 = 64 * 1024;

    let mut f = file;
    let len = f
        .seek(SeekFrom::End(0))
        .map_err(|e| format!("audit tail seek: {e}"))?;
    if len == 0 {
        return Ok(None);
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
        let mut last: Option<serde_json::Value> = None;
        for parsed in serde_json::Deserializer::from_str(line).into_iter::<serde_json::Value>() {
            let v = parsed.map_err(|e| format!("audit tail parse: {e}"))?;
            if v.get("entry_hash").and_then(|h| h.as_str()).is_some() {
                last = Some(v);
            }
        }
        if last.is_some() {
            return Ok(last);
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
    lock_trail_with(file, fs2::FileExt::try_lock_exclusive)
}

/// Acquire a shared trail lock for a reader, bounded by [`LOCK_TIMEOUT`].
/// Windows file locks are mandatory: a reader without one that ran into an
/// append's exclusive lock failed mid-file, and the verifier reported that
/// I/O error as a corrupt chain ("audit chain invalid at entry 0"), e.g. a
/// heartbeat racing a resume. The shared lock waits for the append and gives
/// the verifier a consistent snapshot on every platform.
fn lock_trail_shared(file: &fs::File) -> Result<(), String> {
    lock_trail_with(file, fs2::FileExt::try_lock_shared)
}

/// End a reader's shared lock once it has taken the trail's length. Appends
/// hold the exclusive lock and only ever extend the file (a failed append
/// truncates back to its own start), so `file[..len]` stays exactly as it was
/// and the reader goes on without the lock: a pass over a large trail no
/// longer makes every append in every process time out behind it. Windows
/// locks are mandatory, so a reader there keeps its lock to the end.
///
/// This rests on the trail being append-only in place. Anything that
/// rewrites, repairs or rotates `trail.jsonl` must write a new file and
/// rename it over the old one (an open reader keeps its own inode), never
/// truncate or edit it in place; a verifier that finds its file shorter
/// than the length it took reports the trail unavailable
/// ([`still_holds_prefix`]).
fn release_read_lock(file: &fs::File) {
    #[cfg(not(windows))]
    let _ = fs2::FileExt::unlock(file);
    #[cfg(windows)]
    let _ = file;
}

/// Whether `file` still holds the `len` bytes a reader took (see
/// [`release_read_lock`]): false only if something truncated it in place.
fn still_holds_prefix(file: &fs::File, len: u64) -> bool {
    file.metadata().is_ok_and(|meta| meta.len() >= len)
}

fn lock_trail_with(
    file: &fs::File,
    try_lock: fn(&fs::File) -> std::io::Result<()>,
) -> Result<(), String> {
    use std::time::Instant;

    let deadline = Instant::now() + LOCK_TIMEOUT;
    loop {
        match try_lock(file) {
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
    try_record_at_time(path, data, chrono::Utc::now())
}

/// [`try_record_at`] with the clock that decides whether a day has passed.
fn try_record_at_time(
    path: &Path,
    data: AuditEntryData,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<AuditEntry, String> {
    // At most one seal per append: after it the next file is fresh.
    let mut data = data;
    for _ in 0..SNAPSHOT_ATTEMPTS {
        let mut file = open_active_locked(path)?;
        let appended = append_or_seal(path, &mut file, data, now);
        let _ = fs2::FileExt::unlock(&file);
        match appended? {
            Appended::Done(entry) => return Ok(entry),
            Appended::Moved(back) => data = back,
        }
    }
    Err("audit trail: the active file kept moving".to_string())
}

/// What [`append_or_seal`] did with the entry.
enum Appended {
    Done(AuditEntry),
    /// The file was sealed and moved: append the entry to the new one.
    Moved(AuditEntryData),
}

/// Open the active trail and take the exclusive lock. Advisory lock
/// serializes appends ACROSS processes; prev_hash is read from the file
/// under the same lock, so the chain cannot fork. A writer that waited on a
/// file another writer sealed and renamed meanwhile opens the new one.
fn open_active_locked(path: &Path) -> Result<fs::File, String> {
    for _ in 0..SNAPSHOT_ATTEMPTS {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(path)
            .map_err(|e| format!("audit trail open: {e}"))?;
        lock_trail(&file)?;
        if is_current(path, &file) {
            return Ok(file);
        }
        let _ = fs2::FileExt::unlock(&file);
    }
    Err("audit trail: the active file kept moving".to_string())
}

/// Whether `file` is still the file at `path`.
fn is_current(path: &Path, file: &fs::File) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match (fs::metadata(path), file.metadata()) {
            (Ok(named), Ok(open)) => (named.dev(), named.ino()) == (open.dev(), open.ino()),
            _ => false,
        }
    }
    // lean-ctx: std has no stable file id on Windows; a renamed file is
    // caught there by its tail (a seal) on the next append instead.
    #[cfg(not(unix))]
    {
        let _ = (path, file);
        true
    }
}

/// Append `data` to the locked active trail, sealing it first when it is
/// due.
fn append_or_seal(
    path: &Path,
    file: &mut fs::File,
    data: AuditEntryData,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Appended, String> {
    let Some(last) = read_tail_entry(file)? else {
        // A fresh active file continues the chain of the newest segment.
        let prev_hash = match segments::sealed(path).last() {
            Some((_, segment)) => fs::File::open(segment)
                .map_err(|e| format!("audit segment open: {e}"))
                .and_then(|segment| read_last_hash_tail(&segment))?,
            None => "genesis".to_string(),
        };
        return append_locked(file, data, prev_hash).map(Appended::Done);
    };
    let last_hash = last
        .get("entry_hash")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    let next_seq = segments::sealed(path).last().map_or(0, |(seq, _)| seq + 1);
    let sealed_seq = if last.get("event_type").and_then(serde_json::Value::as_str)
        == Some(segments::SEAL_EVENT)
    {
        segments::sealed_seq(last.get("action").and_then(serde_json::Value::as_str))
    } else {
        None
    };
    let len = file
        .metadata()
        .map_err(|e| format!("audit stat: {e}"))?
        .len();
    let last_at = last.get("timestamp").and_then(serde_json::Value::as_str);
    let seq = match sealed_seq {
        // Sealed before, but the rename did not happen.
        Some(seq) if seq == next_seq => seq,
        // Already moved to its segment (a handle that outlived the rename).
        Some(seq) if seq < next_seq => return Ok(Appended::Moved(data)),
        _ if segments::due(len, last_at, now)
            && !segments::backing_off()
            && verified_once(path, file) =>
        {
            append_locked(file, seal_data(next_seq), last_hash.clone())?;
            next_seq
        }
        _ => return append_locked(file, data, last_hash).map(Appended::Done),
    };
    match segments::retire(path, seq) {
        Ok(()) => Ok(Appended::Moved(data)),
        Err(error) => {
            // The seal stays in the active file and the chain runs on; this
            // process tries again after a back-off.
            tracing::warn!("audit trail not sealed: {error}");
            segments::back_off();
            let prev_hash = read_last_hash_tail(file)?;
            append_locked(file, data, prev_hash).map(Appended::Done)
        }
    }
}

/// Never seal a trail this build has not verified: only a trail with a
/// valid checkpoint (from a successful pass) is sealed. Trails outside the
/// data dir have no checkpoint and are never sealed.
fn verified_once(path: &Path, file: &fs::File) -> bool {
    let Some(store) = checkpoint::Store::for_trail(path) else {
        return false;
    };
    checkpoint::FileStamp::of(file).is_some_and(|stamp| store.holds_prefix_of(file, &stamp))
}

fn seal_data(seq: u64) -> AuditEntryData {
    AuditEntryData {
        agent_id: "lean-ctx".to_string(),
        tool: "audit_trail".to_string(),
        action: Some(segments::seal_action(seq)),
        input_hash: String::new(),
        output_tokens: 0,
        role: "system".to_string(),
        event_type: AuditEventType::Unknown(segments::SEAL_EVENT.to_string()),
    }
}

/// Whether an entry is the seal that closes a trail segment: chain
/// bookkeeping, not an agent action.
pub fn is_seal(event_type: &AuditEventType) -> bool {
    matches!(event_type, AuditEventType::Unknown(label) if label == segments::SEAL_EVENT)
}

/// Build and append one entry after `prev_hash`. Caller holds the
/// exclusive lock.
fn append_locked(
    file: &mut fs::File,
    data: AuditEntryData,
    prev_hash: String,
) -> Result<AuditEntry, String> {
    use std::io::{Seek, SeekFrom};

    let authenticated_drift = matches!(
        &data.event_type,
        AuditEventType::AgentDriftDetected | AuditEventType::AgentDriftAcknowledged
    ) || is_seal(&data.event_type);

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

/// The last `limit` entries of the default trail.
pub fn load_recent(limit: usize) -> Vec<AuditEntry> {
    let Some(path) = trail_path() else {
        return Vec::new();
    };
    load_recent_at(&path, limit)
}

/// Bytes read per step when [`load_recent_at`] walks back from the end.
const TAIL_BLOCK: u64 = 256 * 1024;

/// The last `limit` entries of the trail at `path`, read back from its end
/// in blocks until enough whole lines are in, never the whole trail; a
/// short active trail is topped up from the newest sealed segments.
pub fn load_recent_at(path: &Path, limit: usize) -> Vec<AuditEntry> {
    let mut entries: Vec<AuditEntry> = Vec::new();
    for file in segments::chain(path).iter().rev() {
        if entries.len() >= limit {
            break;
        }
        let mut older = load_recent_in(file, limit - entries.len());
        older.append(&mut entries);
        entries = older;
    }
    entries
}

fn load_recent_in(path: &Path, limit: usize) -> Vec<AuditEntry> {
    use std::io::{Seek, SeekFrom};

    if limit == 0 {
        return Vec::new();
    }
    let Some((file, len)) = open_for_listing(path) else {
        return Vec::new();
    };
    let mut start = len;
    let mut blocks: Vec<Vec<u8>> = Vec::new();
    let mut newlines = 0usize;
    while start > 0 && newlines <= limit {
        let from = start.saturating_sub(TAIL_BLOCK);
        let mut block = Vec::new();
        let read = (&file)
            .seek(SeekFrom::Start(from))
            .and_then(|_| (&file).take(start - from).read_to_end(&mut block));
        if read.is_err() {
            break;
        }
        newlines += bytecount::count(&block, b'\n');
        blocks.push(block);
        start = from;
    }
    let mut tail: Vec<u8> = blocks.into_iter().rev().flatten().collect();
    // Unless the walk reached the start, the first line may be cut.
    if start > 0 {
        let cut = tail
            .iter()
            .position(|&byte| byte == b'\n')
            .map_or(tail.len(), |at| at + 1);
        tail.drain(..cut);
    }
    let entries = parse_entries(std::io::BufReader::new(tail.as_slice()));
    let skip = entries.len().saturating_sub(limit);
    entries.into_iter().skip(skip).collect()
}

/// Every parseable entry of the trail at `path`, its sealed segments first.
pub fn load_all_at(path: &Path) -> Vec<AuditEntry> {
    let mut entries = Vec::new();
    for file in segments::chain(path) {
        if let Some((file, len)) = open_for_listing(&file) {
            entries.extend(parse_entries(std::io::BufReader::new(file.take(len))));
        }
    }
    entries
}

/// The files of the trail at `trail` in chain order: its sealed segments,
/// oldest first, then the active trail if it exists. With `since`, a sealed
/// segment whose last entry (its seal) is older than `since` is left out;
/// that takes one tail read per segment. Lines keep the trail's format and
/// run in time order across the files.
pub fn trail_files_since(
    trail: &Path,
    since: Option<chrono::DateTime<chrono::Utc>>,
) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = segments::sealed(trail)
        .into_iter()
        .map(|(_, path)| path)
        .filter(|path| {
            since.is_none_or(|since| {
                fs::File::open(path)
                    .ok()
                    .and_then(|file| read_tail_entry(&file).ok().flatten())
                    .and_then(|last| entry_time(&last))
                    .is_none_or(|sealed_at| sealed_at >= since)
            })
        })
        .collect();
    if trail.exists() {
        files.push(trail.to_path_buf());
    }
    files
}

/// The text of [`trail_files_since`] joined in order; `None` when the trail
/// has no file at all. A seal that lands while the files are read moves the
/// active trail away, so the read is repeated until the segment list held
/// still across it.
pub fn read_trail_since(
    trail: &Path,
    since: Option<chrono::DateTime<chrono::Utc>>,
) -> std::io::Result<Option<String>> {
    for _ in 0..SNAPSHOT_ATTEMPTS {
        let before = segments::sealed(trail);
        let files = trail_files_since(trail, since);
        if files.is_empty() {
            return Ok(None);
        }
        let mut text = String::new();
        for file in files {
            match fs::read_to_string(&file) {
                Ok(part) => {
                    text.push_str(&part);
                    if !text.is_empty() && !text.ends_with('\n') {
                        text.push('\n');
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        if segments::sealed(trail) == before {
            return Ok(Some(text));
        }
    }
    Err(std::io::Error::other(
        "the audit trail kept being sealed while it was read",
    ))
}

/// How often a reader retries when a seal moved the active trail under it.
const SNAPSHOT_ATTEMPTS: usize = 3;

fn entry_time(entry: &serde_json::Value) -> Option<chrono::DateTime<chrono::Utc>> {
    let stamp = entry.get("timestamp")?.as_str()?;
    Some(chrono::DateTime::parse_from_rfc3339(stamp).ok()?.to_utc())
}

/// Open the trail at `path` for a listing, with its length taken under the
/// shared lock (see [`release_read_lock`]).
fn open_for_listing(path: &Path) -> Option<(fs::File, u64)> {
    let file = fs::File::open(path).ok()?;
    // Best effort: without the shared lock a Windows read can stop at an
    // in-progress append (see `lock_trail_shared`); the listing then ends early.
    let locked = lock_trail_shared(&file);
    if let Err(error) = &locked {
        tracing::debug!("audit listing reads unlocked and may end early: {error}");
    }
    let len = file.metadata().ok()?.len();
    if locked.is_ok() {
        release_read_lock(&file);
    }
    Some((file, len))
}

fn parse_entries(reader: impl BufRead) -> Vec<AuditEntry> {
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
    verify_chain_mode(path, agent_id, false)
}

/// Chain state after a verified prefix: everything a verifier needs to
/// continue with the next entry exactly as a full pass would.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct ChainState {
    prev_hash: String,
    total: usize,
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
    fn genesis() -> Self {
        Self {
            prev_hash: "genesis".to_string(),
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
    fn advance(&mut self, value: serde_json::Value) -> Result<(), ()> {
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
        let entry = serde_json::from_value::<AuditEntry>(value).map_err(|_| ())?;
        let seal = is_seal(&entry.event_type);
        if matches!(&entry.event_type, AuditEventType::Unknown(_)) && !seal {
            self.unknown += 1;
        }
        self.sealed = if seal {
            Some(segments::sealed_seq(entry.action.as_deref()).ok_or(())?)
        } else {
            None
        };
        if seal
            || matches!(
                &entry.event_type,
                AuditEventType::AgentDriftDetected | AuditEventType::AgentDriftAcknowledged
            )
        {
            let signature = entry.signature.as_deref().ok_or(())?;
            let signature = crate::core::agent_identity::hex_decode(signature).map_err(|_| ())?;
            let public_key =
                crate::core::agent_identity::load_public_key("lean-ctx").map_err(|_| ())?;
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
        self.prev_hash = entry_hash;
        self.total += 1;
        Ok(())
    }

    /// Verify every line of `reader` (the same line rules as
    /// `BufRead::lines`). Returns the bytes consumed and whether the input
    /// ended on a line boundary (empty input counts as one).
    fn verify_stream(&mut self, reader: &mut impl BufRead) -> Result<(u64, bool), ()> {
        let mut consumed = 0u64;
        let mut ends_on_newline = true;
        let mut buf = Vec::new();
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
                self.advance(parsed.map_err(|_| ())?)?;
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
    verify_chain_mode_at(path, agent_id, force_full, chrono::Utc::now().timestamp())
}

/// [`verify_chain_mode`] at an explicit wall-clock `now` (Unix seconds).
fn verify_chain_mode_at(
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
    match verify_state_at(path, false, chrono::Utc::now().timestamp()) {
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
    match verify_state_at(path, false, chrono::Utc::now().timestamp()) {
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

pub fn hash_input(args: &serde_json::Map<String, serde_json::Value>) -> String {
    let serialized = serde_json::to_string(args).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(serialized.as_bytes());
    crate::core::agent_identity::hex_encode(&hasher.finalize())
}

#[cfg(test)]
#[path = "audit_trail_tests.rs"]
mod tests;
