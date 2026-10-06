//! First-class agent identities: registry + lifecycle (GL #433, H3 Epic D).
//!
//! An agent stops being an anonymous process with a role config and
//! becomes a registered identity: stable `agent_id`, mandatory human
//! `owner` (accountability principle — orphaned agents are the security
//! hole of the agent era), lifecycle state, best-effort attestation and a
//! SPIFFE-compatible identity string for workload-IAM integration.
//!
//! Storage: `<data_dir>/agents/identity-registry.json`, advisory-file-locked like
//! the audit trail (multiple concurrent agent processes are LeanCTX's
//! normal operating mode). Every lifecycle transition writes a
//! tamper-evident audit entry (OCP Part 4, additive event types).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

use super::audit_trail::{self, AuditEntryData, AuditEventType};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AgentStatus {
    Active,
    Suspended,
    Decommissioned,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Attestation {
    /// SHA-256 of the running binary at registration/heartbeat time.
    pub binary_sha256: String,
    /// SHA-256 of the active role file (empty when the role is built-in).
    pub config_sha256: String,
    pub attested_at: String,
    /// Set when this attestation *replaced* a different one, i.e. drift was
    /// observed and audited at `attested_at`. Sticky: heartbeat persists the
    /// freshly observed state, so every later heartbeat is clean by
    /// construction and auto-clearing would make `block_on_drift` expire
    /// after a single beat. Cleared ONLY by
    /// `lean-ctx agent ack-drift <agent-id> --evidence <evidence>`, which
    /// binds the acknowledgement to the exact evidence the operator
    /// reviewed. Resuming a suspended identity deliberately does not clear
    /// it: un-suspending an identity after an unrelated incident must never
    /// silently sign off attestation drift nobody looked at.
    ///
    /// `default` on purpose: a pre-Phase-18 registry has no `drift` key and
    /// must keep round-tripping as "no unacknowledged drift".
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub drift: Option<DriftMark>,
}

/// Bounded, non-secret evidence of one observed drift, persisted next to
/// the attestation and mirrored verbatim into the `AgentDriftDetected`
/// audit entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DriftMark {
    pub detected_at: String,
    /// The running binary changed.
    pub binary: bool,
    /// The active role file changed (independent of `binary`).
    pub config: bool,
    pub evidence: String,
}

/// Result of one heartbeat.
///
/// `drift` is the identity's *state* after the beat (sticky until
/// acknowledged), `newly_observed` is the *transition*. Keeping them apart is
/// the whole point: the monitoring signal must stay red on every beat while
/// the drift is unacknowledged, but only the beat that first observed it may
/// append an `AgentDriftDetected` entry — otherwise a one-minute heartbeat
/// would write one audit entry per minute forever.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HeartbeatOutcome {
    pub drift: Option<DriftMark>,
    pub newly_observed: bool,
}

#[cfg(test)]
impl HeartbeatOutcome {
    /// Evidence of the unacknowledged drift, if any.
    pub(crate) fn evidence(&self) -> Option<&str> {
        self.drift.as_ref().map(|mark| mark.evidence.as_str())
    }

    /// Compatibility view for existing callers: `None` means this beat did
    /// not observe a new transition, while [`Self::drift`] remains sticky.
    pub(crate) fn is_none(&self) -> bool {
        !self.newly_observed
    }

    /// Compatibility view for existing tests that extracted the newly
    /// observed evidence from the old `Result<Option<String>, String>` shape.
    pub(crate) fn expect(self, message: &str) -> String {
        self.drift
            .map(|mark| mark.evidence)
            .unwrap_or_else(|| panic!("{message}"))
    }
}

/// A completed, audited drift acknowledgement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DriftAcknowledgement {
    pub agent_id: String,
    /// The evidence that was cleared — verbatim, as the operator saw it.
    pub evidence: String,
    pub detected_at: String,
    pub acknowledged_at: String,
}

/// Which attestation dimensions changed. Two independent booleans on
/// purpose: the previous `if / else if` chain reported binary drift and
/// silently dropped a simultaneous role-config change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct DriftDimensions {
    pub binary: bool,
    pub config: bool,
}

impl DriftDimensions {
    fn any(self) -> bool {
        self.binary || self.config
    }

    /// Stable, bounded label — the only place the two dimensions are joined.
    fn label(self) -> &'static str {
        match (self.binary, self.config) {
            (true, true) => "binary+config",
            (true, false) => "binary",
            (false, true) => "config",
            (false, false) => "none",
        }
    }
}

/// Whether unacknowledged attestation drift denies a `check`.
///
/// COMPATIBILITY DEFAULT: `block_on_drift = false`. Before this slice drift
/// was advisory only (the heartbeat printed it, nothing acted on it), so the
/// default keeps every existing caller's allow/deny bit identical and only
/// enriches `detail`. Operators opt in with
/// `LEAN_CTX_AGENT_BLOCK_ON_DRIFT=1|true|yes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct DriftPolicy {
    pub block_on_drift: bool,
}

impl DriftPolicy {
    pub(crate) fn from_env() -> Self {
        Self {
            block_on_drift: std::env::var("LEAN_CTX_AGENT_BLOCK_ON_DRIFT")
                .is_ok_and(|value| matches!(value.trim(), "1" | "true" | "yes")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AgentRecord {
    /// Stable identity (key of the registry).
    pub agent_id: String,
    /// Role name under `roles/*.toml` / built-ins.
    pub role: String,
    /// Human accountable for this agent — mandatory, never empty.
    pub owner: String,
    pub status: AgentStatus,
    pub created_at: String,
    /// Ed25519 public key (hex) bound to this identity.
    pub public_key: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub attestation: Option<Attestation>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub last_heartbeat: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub suspended_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub decommissioned_at: Option<String>,
}

/// Outcome of an identity check on a call path (team server middleware,
/// enforce mode).
#[derive(Debug, Clone, Serialize)]
pub(crate) struct IdentityCheck {
    pub agent_id: String,
    pub registered: bool,
    /// Active = may act. Suspended/decommissioned/unregistered = may not.
    pub allowed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<AgentStatus>,
    /// Unacknowledged attestation drift is recorded on the identity. Whether
    /// it also flips `allowed` is the caller's [`DriftPolicy`] decision.
    pub drifted: bool,
    pub detail: String,
}

fn registry_path() -> Result<PathBuf, String> {
    let dir = crate::core::data_dir::lean_ctx_data_dir()
        .map_err(|e| format!("data dir: {e}"))?
        .join("agents");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join("identity-registry.json"))
}

fn legacy_registry_path() -> Result<PathBuf, String> {
    registry_path().map(|path| path.with_file_name("registry.json"))
}

fn load_unlocked(path: &PathBuf) -> Result<BTreeMap<String, AgentRecord>, String> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(BTreeMap::new());
        }
        Err(error) => return Err(format!("read registry {}: {error}", path.display())),
    };
    serde_json::from_str(&contents)
        .map_err(|error| format!("parse registry {}: {error}", path.display()))
}

fn load_registry(path: &PathBuf) -> Result<BTreeMap<String, AgentRecord>, String> {
    if path.exists() {
        return load_unlocked(path);
    }
    let legacy = legacy_registry_path()?;
    match load_unlocked(&legacy) {
        Ok(registry) => Ok(registry),
        Err(identity_error) => {
            // `agents/registry.json` is also the MCP presence registry. Its
            // versioned top-level `agents` array was never an identity map and
            // must be left untouched rather than treated as corrupt legacy
            // identity state.
            let is_presence_registry = std::fs::read_to_string(&legacy)
                .ok()
                .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
                .and_then(|value| value.get("agents").cloned())
                .is_some_and(|agents| agents.is_array());
            if is_presence_registry {
                Ok(BTreeMap::new())
            } else {
                Err(identity_error)
            }
        }
    }
}

/// Read durable identities without interpreting malformed state as an empty
/// registry. The legacy path is migration input only, never presence state.
fn load_registry_strict() -> Result<BTreeMap<String, AgentRecord>, String> {
    let path = registry_path()?;
    let path = if path.exists() {
        path
    } else {
        legacy_registry_path()?
    };
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let contents = std::fs::read_to_string(&path)
        .map_err(|error| format!("read durable identity registry: {error}"))?;
    serde_json::from_str(&contents)
        .map_err(|error| format!("durable identity registry is corrupt: {error}"))
}

/// Run `f` over the registry under an exclusive cross-process lock and
/// persist the result.
fn with_registry<T>(
    f: impl FnOnce(&mut BTreeMap<String, AgentRecord>) -> Result<T, String>,
) -> Result<T, String> {
    use fs2::FileExt;
    use std::time::{Duration, Instant};

    const LOCK_TIMEOUT: Duration = Duration::from_millis(750);
    const LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(25);
    let path = registry_path()?;
    let lock_path = path.with_extension("lock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .map_err(|e| format!("registry lock: {e}"))?;
    let deadline = Instant::now() + LOCK_TIMEOUT;
    loop {
        match lock.try_lock_exclusive() {
            Ok(()) => break,
            Err(error) if super::file_lock::is_contended(&error) && Instant::now() < deadline => {
                std::thread::sleep(LOCK_RETRY_INTERVAL);
            }
            Err(error) if super::file_lock::is_contended(&error) => {
                return Err(format!(
                    "registry lock timed out after {}ms; another agent operation is still active",
                    LOCK_TIMEOUT.as_millis()
                ));
            }
            Err(error) => return Err(format!("registry lock: {error}")),
        }
    }

    let mut registry = load_registry(&path)?;
    let result = f(&mut registry);
    let persisted = if result.is_ok() {
        persist_registry(&path, &registry)
    } else {
        Ok(())
    };
    let _ = FileExt::unlock(&lock);
    persisted?;
    result
}

/// Atomic replace: write a sibling temp file, fsync it, rename it over the
/// registry. An interrupted in-place write left a truncated
/// `identity-registry.json` behind, and a truncated registry parses as an
/// *empty* one — silently resurrecting decommissioned identities. A failed
/// write now leaves the previous snapshot byte-identical and drops the temp.
fn persist_registry(
    path: &PathBuf,
    registry: &BTreeMap<String, AgentRecord>,
) -> Result<(), String> {
    use std::io::Write;

    let json = serde_json::to_string_pretty(registry).map_err(|e| format!("serialize: {e}"))?;
    let tmp = path.with_extension("json.tmp");
    let written = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(json.as_bytes())?;
        file.sync_all()
    })();
    if let Err(error) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("persist registry: {error}"));
    }
    std::fs::rename(&tmp, path).map_err(|error| {
        let _ = std::fs::remove_file(&tmp);
        format!("persist registry: {error}")
    })
}

/// Read-only registry snapshot.
#[cfg(test)]
pub(crate) fn list() -> Vec<AgentRecord> {
    try_list().unwrap_or_default()
}

pub(crate) fn try_list() -> Result<Vec<AgentRecord>, String> {
    let path = registry_path()?;
    Ok(load_registry(&path)?.into_values().collect())
}

/// Active identities are the peer view. Lifecycle history remains available
/// through `list()` for audit and operator use.
#[cfg(test)]
pub(crate) fn list_active() -> Vec<AgentRecord> {
    list()
        .into_iter()
        .filter(|record| record.status == AgentStatus::Active)
        .collect()
}

pub(crate) fn try_list_active() -> Result<Vec<AgentRecord>, String> {
    Ok(try_list()?
        .into_iter()
        .filter(|record| record.status == AgentStatus::Active)
        .collect())
}

#[cfg(test)]
pub(crate) fn get(agent_id: &str) -> Option<AgentRecord> {
    try_get(agent_id).ok().flatten()
}

pub(crate) fn try_get(agent_id: &str) -> Result<Option<AgentRecord>, String> {
    let path = registry_path()?;
    Ok(load_registry(&path)?.remove(agent_id))
}

/// Fail closed unless the explicitly bound durable identity currently exists
/// and remains active.
pub(crate) fn require_active(agent_id: &str) -> Result<(), String> {
    if agent_id.trim().is_empty() {
        return Err("durable identity id must not be empty".to_string());
    }
    let registry = load_registry_strict()?;
    let Some(record) = registry.get(agent_id) else {
        return Err(format!("durable identity '{agent_id}' is not registered"));
    };
    match record.status {
        AgentStatus::Active => Ok(()),
        AgentStatus::Suspended => Err(format!("durable identity '{agent_id}' is suspended")),
        AgentStatus::Decommissioned => {
            Err(format!("durable identity '{agent_id}' is decommissioned"))
        }
    }
}

fn audit(event_type: AuditEventType, agent_id: &str, role: &str, detail: Option<String>) {
    audit_trail::record(AuditEntryData {
        agent_id: agent_id.to_string(),
        tool: "agent_registry".to_string(),
        action: detail,
        input_hash: audit_trail::hash_input(&serde_json::Map::new()),
        output_tokens: 0,
        role: role.to_string(),
        event_type,
    });
}

/// SHA-256 of the running binary, cached by (len, mtime) — heartbeats may
/// fire every minute and the binary is large; re-hashing is only needed
/// when the file on disk actually changed (which is exactly the drift
/// signal we care about, and it changes the mtime).
///
/// KNOWN LIMIT: the cache key is metadata, not content. Inside a long-lived
/// process (the MCP server), a swap that preserves both length and mtime
/// (`touch -r`) is not re-hashed and evades the binary dimension until the
/// process restarts. Attestation is a drift detector, not a defence against
/// an attacker who already controls the host.
fn binary_sha256() -> String {
    use std::sync::{Mutex, OnceLock};
    /// (binary len, binary mtime secs) → hex digest.
    type HashCache = Mutex<Option<((u64, u64), String)>>;
    static CACHE: OnceLock<HashCache> = OnceLock::new();

    let Ok(exe) = std::env::current_exe() else {
        return String::new();
    };
    let Ok(meta) = std::fs::metadata(&exe) else {
        return String::new();
    };
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    let key = (meta.len(), mtime);

    let cache = CACHE.get_or_init(|| Mutex::new(None));
    let mut slot = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((cached_key, hash)) = slot.as_ref()
        && *cached_key == key
    {
        return hash.clone();
    }
    let hash = std::fs::read(&exe)
        .map(|bytes| sha256_hex(&bytes))
        .unwrap_or_default();
    *slot = Some((key, hash.clone()));
    hash
}

/// Best-effort attestation: hash the running binary and the role file.
/// Detects drift; does NOT stop a determined attacker who controls the
/// host (documented in docs/enterprise/agent-identity.md).
pub(crate) fn attest(role: &str) -> Attestation {
    let binary_sha256 = binary_sha256();
    let config_sha256 = role_file_path(role)
        .and_then(|p| std::fs::read(p).ok())
        .map(|bytes| sha256_hex(&bytes))
        .unwrap_or_default();
    Attestation {
        binary_sha256,
        config_sha256,
        attested_at: chrono::Utc::now().to_rfc3339(),
        drift: None,
    }
}

/// Compare two attestations per dimension. Both dimensions are evaluated
/// unconditionally, so simultaneous binary+config drift reports *both*.
fn diff_attestation(previous: &Attestation, fresh: &Attestation) -> DriftDimensions {
    DriftDimensions {
        binary: previous.binary_sha256 != fresh.binary_sha256,
        config: previous.config_sha256 != fresh.config_sha256,
    }
}

/// Hex characters of each digest kept in the evidence. 48 bits is more than
/// enough to tell two attestations apart in an audit review and keeps the
/// evidence string bounded; the digests are public hashes, not secrets.
const EVIDENCE_DIGEST_PREFIX: usize = 12;

/// Bounded, deterministic, injection-proof rendering of one digest.
/// Non-hex bytes from a hand-edited registry become `?`, so evidence can
/// never smuggle a newline or a quote into the JSONL audit line.
fn digest_prefix(digest: &str) -> String {
    if digest.is_empty() {
        return "none".to_string();
    }
    digest
        .chars()
        .take(EVIDENCE_DIGEST_PREFIX)
        .map(|c| if c.is_ascii_hexdigit() { c } else { '?' })
        .collect()
}

/// Deterministic drift evidence for the audit entry: the changed-dimension
/// label plus, per changed dimension, the previous and freshly observed
/// digest prefixes. No paths, no file contents, no owner data — and at most
/// 110 characters regardless of what the registry holds.
fn drift_evidence(
    previous: &Attestation,
    fresh: &Attestation,
    dimensions: DriftDimensions,
) -> String {
    use std::fmt::Write as _;

    let mut evidence = format!("attestation drift dimensions={}", dimensions.label());
    if dimensions.binary {
        let _ = write!(
            evidence,
            " binary={}->{}",
            digest_prefix(&previous.binary_sha256),
            digest_prefix(&fresh.binary_sha256)
        );
    }
    if dimensions.config {
        let _ = write!(
            evidence,
            " config={}->{}",
            digest_prefix(&previous.config_sha256),
            digest_prefix(&fresh.config_sha256)
        );
    }
    evidence
}

/// Upper bound on any evidence string that reaches an audit line. Matches
/// what [`drift_evidence`] can produce; applied again on the way *out* of the
/// registry because that file is operator-writable.
const EVIDENCE_MAX_CHARS: usize = 256;

/// Re-clamp evidence read back from the registry before it enters a JSONL
/// audit line: control characters and quoting characters are dropped and the
/// length is bounded, exactly as for freshly generated evidence.
fn bounded_evidence(evidence: &str) -> String {
    evidence
        .chars()
        .filter(|c| !c.is_control() && *c != '"' && *c != '\\')
        .take(EVIDENCE_MAX_CHARS)
        .collect()
}

fn validate_drift_mark(mark: DriftMark) -> Result<DriftMark, String> {
    let validated = bounded_evidence(&mark.evidence);
    if validated.is_empty() || validated != mark.evidence {
        return Err("stored drift evidence is invalid or oversized".to_string());
    }
    Ok(mark)
}

/// Append the typed drift event. Fallible on purpose: the caller must not
/// commit the fresh attestation when the evidence could not be persisted.
fn audit_drift(agent_id: &str, role: &str, evidence: &str) -> Result<String, String> {
    audit_trail::try_record(AuditEntryData {
        agent_id: agent_id.to_string(),
        tool: "agent_registry".to_string(),
        action: Some(evidence.to_string()),
        input_hash: audit_trail::hash_input(&serde_json::Map::new()),
        output_tokens: 0,
        role: role.to_string(),
        event_type: AuditEventType::AgentDriftDetected,
    })
    .map(|entry| entry.entry_hash)
    .map_err(|error| format!("drift audit failed, attestation not updated: {error}"))
}

/// Append the typed acknowledgement event, naming the exact evidence being
/// cleared. Fallible for the same reason as [`audit_drift`], in the opposite
/// direction: an acknowledgement that cannot be recorded must not clear the
/// mark, or the identity would be allowed again with nothing in the trail
/// saying anyone signed it off.
fn audit_drift_ack(agent_id: &str, role: &str, mark: &DriftMark) -> Result<(), String> {
    let chain = audit_trail::verify_chain();
    if !chain.source_present {
        return Err("acknowledgement audit failed, audit trail unavailable".to_string());
    }
    if !chain.valid {
        return Err(format!(
            "acknowledgement audit failed, audit chain invalid at entry {}",
            chain.first_invalid_at.unwrap_or(chain.total_entries)
        ));
    }
    let evidence = bounded_evidence(&mark.evidence);
    if evidence.is_empty() || evidence != mark.evidence {
        return Err("acknowledgement evidence on record is invalid or oversized".to_string());
    }
    audit_trail::try_record(AuditEntryData {
        agent_id: agent_id.to_string(),
        tool: "agent_registry".to_string(),
        action: Some(evidence),
        input_hash: audit_trail::hash_input(&serde_json::Map::new()),
        output_tokens: 0,
        role: role.to_string(),
        event_type: AuditEventType::AgentDriftAcknowledged,
    })
    .map(|_| ())
    .map_err(|error| format!("acknowledgement audit failed, drift not cleared: {error}"))
}

fn drift_from_audit(agent_id: &str) -> Result<Option<DriftMark>, String> {
    let Some((detected_at, evidence)) = audit_trail::latest_unacknowledged_drift(agent_id)? else {
        return Ok(None);
    };
    let validated = bounded_evidence(&evidence);
    if validated.is_empty() || validated != evidence {
        return Err("audit drift evidence is invalid or oversized".to_string());
    }
    let dimensions = evidence
        .split_whitespace()
        .find_map(|part| part.strip_prefix("dimensions="))
        .unwrap_or("none");
    Ok(Some(DriftMark {
        detected_at,
        binary: matches!(dimensions, "binary" | "binary+config"),
        config: matches!(dimensions, "config" | "binary+config"),
        evidence,
    }))
}

fn role_file_path(role: &str) -> Option<PathBuf> {
    let dir = crate::core::data_dir::lean_ctx_data_dir()
        .ok()?
        .join("roles");
    let path = dir.join(format!("{role}.toml"));
    path.exists().then_some(path)
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    crate::core::agent_identity::hex_encode(&hasher.finalize())
}

/// Register a new agent identity. The role must exist; the owner is
/// mandatory (accountability). Creating an identity also provisions its
/// Ed25519 keypair.
pub(crate) fn register(agent_id: &str, role: &str, owner: &str) -> Result<AgentRecord, String> {
    if agent_id.trim().is_empty()
        || !agent_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("agent_id must be non-empty [A-Za-z0-9_-]".to_string());
    }
    if owner.trim().is_empty() {
        return Err(
            "owner is mandatory — every agent identity has a human accountable for it".to_string(),
        );
    }
    if crate::core::roles::load_role(role).is_none() {
        return Err(format!(
            "role '{role}' does not exist (see `lean-ctx roles list`)"
        ));
    }

    let public_key = crate::core::agent_identity::get_public_key(agent_id)
        .map(|k| crate::core::agent_identity::hex_encode(k.as_bytes()))
        .map_err(|e| format!("keypair: {e}"))?;

    let record = AgentRecord {
        agent_id: agent_id.to_string(),
        role: role.to_string(),
        owner: owner.trim().to_string(),
        status: AgentStatus::Active,
        created_at: chrono::Utc::now().to_rfc3339(),
        public_key,
        attestation: Some(attest(role)),
        last_heartbeat: None,
        suspended_reason: None,
        decommissioned_at: None,
    };

    with_registry(|reg| {
        if reg.contains_key(agent_id) {
            return Err(format!("agent '{agent_id}' is already registered"));
        }
        reg.insert(agent_id.to_string(), record.clone());
        Ok(())
    })?;
    audit(
        AuditEventType::AgentRegistered,
        agent_id,
        role,
        Some(format!("owner={}", record.owner)),
    );
    Ok(record)
}

/// Heartbeat: liveness + re-attestation.
///
/// Returns the identity's unacknowledged-drift STATE plus whether this beat
/// is the one that observed it. Every beat reports sticky drift, so a monitor
/// wired to this call cannot flip back to green one minute after the incident
/// while `check` still denies; only the observing beat appends evidence.
///
/// The freshly observed attestation *replaces* the stored one, so the next
/// heartbeat compares against reality instead of against registration time
/// (drift used to be re-reported forever, and a rolled-back binary was
/// indistinguishable from a fixed one).
///
/// Ordering is deliberate and is the fail-safe: the `AgentDriftDetected`
/// entry is appended BEFORE the registry write, and `with_registry` persists
/// only when this closure returns `Ok`. A failing audit sink therefore
/// leaves the stored attestation and `last_heartbeat` untouched and the very
/// same drift is re-detected on the next beat — never a silent state update
/// that loses the evidence. The opposite failure (evidence durable, registry
/// write fails) is safe by the same argument: the registry keeps its
/// previous snapshot atomically and the next beat re-emits the same
/// deterministic evidence.
///
/// Evidence and registry state are two separate files with no cross-file
/// atomicity, so the guarantee is AT-LEAST-ONCE, never exactly-once: a crash
/// between the fsynced append and the registry rename can duplicate an
/// observation on the next beat. Losing one is what the ordering rules out.
pub(crate) fn heartbeat(agent_id: &str) -> Result<HeartbeatOutcome, String> {
    with_registry(|reg| {
        let record = reg
            .get_mut(agent_id)
            .ok_or_else(|| format!("agent '{agent_id}' is not registered"))?;
        if record.status == AgentStatus::Decommissioned {
            return Err(format!("agent '{agent_id}' is decommissioned"));
        }
        let stored_drift = record
            .attestation
            .as_ref()
            .and_then(|attestation| attestation.drift.clone())
            .map(validate_drift_mark)
            .transpose()?;
        // The append-only audit trail is canonical and is verified on every
        // heartbeat for an attested identity, even when the registry already
        // carries a sticky mark. A genuinely pre-attestation legacy record has
        // no drift state to recover or mask, so its first heartbeat may create
        // the initial attestation before an audit trail exists.
        let recovered_drift = match drift_from_audit(agent_id) {
            Ok(drift) => drift,
            Err(error)
                if record.attestation.is_none()
                    && record.last_heartbeat.is_none()
                    && error == "audit trail unavailable" =>
            {
                None
            }
            Err(error) => return Err(error),
        };
        let mut fresh = attest(&record.role);
        let observed = record.attestation.as_ref().and_then(|previous| {
            let dimensions = diff_attestation(previous, &fresh);
            dimensions
                .any()
                .then(|| (dimensions, drift_evidence(previous, &fresh, dimensions)))
        });

        match &observed {
            Some((dimensions, evidence)) => {
                let observation_id = audit_drift(agent_id, &record.role, evidence)?;
                fresh.drift = Some(DriftMark {
                    detected_at: fresh.attested_at.clone(),
                    binary: dimensions.binary,
                    config: dimensions.config,
                    evidence: format!("{evidence} observation={observation_id}"),
                });
            }
            // Carry an unacknowledged mark forward — see `Attestation::drift`.
            None => {
                fresh.drift = recovered_drift.or(stored_drift);
            }
        }

        record.last_heartbeat = Some(fresh.attested_at.clone());
        let outcome = HeartbeatOutcome {
            drift: fresh.drift.clone(),
            newly_observed: observed.is_some(),
        };
        record.attestation = Some(fresh);
        Ok(outcome)
    })
}

pub(crate) fn suspend(agent_id: &str, reason: &str) -> Result<(), String> {
    let role = transition(agent_id, AgentStatus::Suspended, Some(reason.to_string()))?;
    audit(
        AuditEventType::AgentSuspended,
        agent_id,
        &role,
        Some(reason.to_string()),
    );
    Ok(())
}

/// Resume is a lifecycle transition ONLY: suspended → active. It deliberately
/// does not touch the sticky [`DriftMark`] — un-suspending an identity after
/// an unrelated incident must not silently sign off attestation drift the
/// operator never saw, and a beat landing between review and resume must not
/// get acknowledged along the way. Drift is cleared exclusively by
/// [`acknowledge_drift`], which names the evidence.
pub(crate) fn resume(agent_id: &str) -> Result<(), String> {
    let role = transition(agent_id, AgentStatus::Active, None)?;
    audit(AuditEventType::AgentResumed, agent_id, &role, None);
    Ok(())
}

/// Explicit operator acknowledgement of ONE observed attestation drift.
///
/// `observed_evidence` must equal the evidence currently on the identity, so
/// an acknowledgement can only ever clear the drift the operator actually
/// reviewed: if a heartbeat observed a newer change in the meantime, the
/// stored evidence differs and this fails with both strings, leaving the mark
/// intact. The typed `AgentDriftAcknowledged` entry is appended BEFORE the
/// registry mutation and inside the same registry lock, so a failing audit
/// sink aborts the whole acknowledgement (`with_registry` persists only on
/// `Ok`) rather than clearing the mark with nothing in the trail.
///
/// The `AgentDriftDetected` evidence stays in the trail — acknowledging never
/// erases history.
///
/// The audit trail and registry snapshot are separate files: durability is
/// AT-LEAST-ONCE, not exactly-once. A crash after the audit fsync but before
/// the registry rename may require the operator to retry the same evidence;
/// it can never turn an unrecorded acknowledgement into a cleared mark.
pub(crate) fn acknowledge_drift(
    agent_id: &str,
    observed_evidence: &str,
) -> Result<DriftAcknowledgement, String> {
    let observed = observed_evidence.to_string();
    with_registry(|reg| {
        let record = reg
            .get_mut(agent_id)
            .ok_or_else(|| format!("agent '{agent_id}' is not registered"))?;
        if record.status == AgentStatus::Decommissioned {
            return Err(format!(
                "agent '{agent_id}' is decommissioned — identities are never reactivated"
            ));
        }
        let role = record.role.clone();
        let mark = drift_from_audit(agent_id)?.ok_or_else(|| {
            format!("agent '{agent_id}' has no audited unacknowledged attestation drift")
        })?;
        if mark.evidence != observed {
            return Err(format!(
                "acknowledgement does not match the drift on record — \
                 you acknowledged: {observed} — current: {} (detected at {}). \
                 Re-review and acknowledge the current evidence.",
                mark.evidence, mark.detected_at
            ));
        }

        audit_drift_ack(agent_id, &role, &mark)?;
        if let Some(attestation) = record.attestation.as_mut() {
            attestation.drift = None;
        }
        Ok(DriftAcknowledgement {
            agent_id: agent_id.to_string(),
            evidence: mark.evidence,
            detected_at: mark.detected_at,
            acknowledged_at: chrono::Utc::now().to_rfc3339(),
        })
    })
}

/// Decommission closes the identity with a final audit entry; the record
/// stays in the registry (auditability) but can never act again.
pub(crate) fn decommission(agent_id: &str) -> Result<(), String> {
    let role = with_registry(|reg| {
        let record = reg
            .get_mut(agent_id)
            .ok_or_else(|| format!("agent '{agent_id}' is not registered"))?;
        record.status = AgentStatus::Decommissioned;
        record.decommissioned_at = Some(chrono::Utc::now().to_rfc3339());
        Ok(record.role.clone())
    })?;
    audit(
        AuditEventType::AgentDecommissioned,
        agent_id,
        &role,
        Some("audit-closing entry".to_string()),
    );
    Ok(())
}

fn transition(agent_id: &str, to: AgentStatus, reason: Option<String>) -> Result<String, String> {
    with_registry(|reg| {
        let record = reg
            .get_mut(agent_id)
            .ok_or_else(|| format!("agent '{agent_id}' is not registered"))?;
        if record.status == AgentStatus::Decommissioned {
            return Err(format!(
                "agent '{agent_id}' is decommissioned — identities are never reactivated"
            ));
        }
        record.status = to;
        record.suspended_reason = reason;
        Ok(record.role.clone())
    })
}

/// Owner offboarding (SCIM `active=false` hook, GL #399): suspend every
/// active agent owned by `owner`. Returns the suspended agent ids.
pub(crate) fn suspend_agents_for_owner(owner: &str, reason: &str) -> Result<Vec<String>, String> {
    let suspended = with_registry(|reg| {
        let mut hit = Vec::new();
        for record in reg.values_mut() {
            if record.owner == owner && record.status == AgentStatus::Active {
                record.status = AgentStatus::Suspended;
                record.suspended_reason = Some(reason.to_string());
                hit.push((record.agent_id.clone(), record.role.clone()));
            }
        }
        Ok(hit)
    })?;
    for (agent_id, role) in &suspended {
        audit(
            AuditEventType::AgentSuspended,
            agent_id,
            role,
            Some(format!("owner offboarded: {reason}")),
        );
    }
    Ok(suspended.into_iter().map(|(id, _)| id).collect())
}

/// Identity check for enforce paths (team-server middleware): registered
/// AND active. Unregistered agents are reported (monitor mode logs,
/// enforce mode rejects — the caller decides).
///
/// Drift policy comes from the environment; see [`DriftPolicy`] for the
/// documented compatibility default (drift is reported, never blocking).
pub(crate) fn check(agent_id: &str) -> IdentityCheck {
    check_with_policy(agent_id, DriftPolicy::from_env())
}

/// [`check`] with an explicit drift decision — the deterministic entry point
/// for callers that carry their own policy instead of reading the process
/// environment.
pub(crate) fn check_with_policy(agent_id: &str, policy: DriftPolicy) -> IdentityCheck {
    let record = match try_get(agent_id) {
        Ok(record) => record,
        Err(error) => {
            return IdentityCheck {
                agent_id: agent_id.to_string(),
                registered: false,
                allowed: false,
                status: None,
                drifted: false,
                detail: format!("denied, registry unavailable: {error}"),
            };
        }
    };
    match record {
        None => IdentityCheck {
            agent_id: agent_id.to_string(),
            registered: false,
            allowed: false,
            status: None,
            drifted: false,
            detail: "not registered — register with `lean-ctx agent register`".to_string(),
        },
        Some(record) => {
            let stored_drift = record
                .attestation
                .as_ref()
                .and_then(|attestation| attestation.drift.clone())
                .map(validate_drift_mark)
                .transpose();
            let (drift, audit_error) = match drift_from_audit(agent_id) {
                Ok(audit_drift) => match stored_drift {
                    Ok(stored) => (audit_drift.or(stored), None),
                    Err(error) => (audit_drift, Some(error)),
                },
                Err(error) => (stored_drift.unwrap_or(None), Some(error)),
            };
            let blocked_by_drift = policy.block_on_drift && drift.is_some();
            let allowed =
                record.status == AgentStatus::Active && !blocked_by_drift && audit_error.is_none();
            IdentityCheck {
                agent_id: agent_id.to_string(),
                registered: true,
                allowed,
                status: Some(record.status),
                // Evidence loss is an unknown/red state, never a green signal.
                drifted: drift.is_some() || audit_error.is_some(),
                detail: match record.status {
                    AgentStatus::Active => match (drift.as_ref(), audit_error.as_ref()) {
                        (_, Some(error)) => {
                            format!("denied, audit state unavailable: {error}")
                        }
                        (Some(mark), _) if policy.block_on_drift => {
                            format!("denied, unacknowledged {}", mark.evidence)
                        }
                        (Some(mark), _) => format!(
                            "active, owner {} (advisory: unacknowledged {})",
                            record.owner, mark.evidence
                        ),
                        (None, None) => format!("active, owner {}", record.owner),
                    },
                    AgentStatus::Suspended => format!(
                        "suspended: {}",
                        record.suspended_reason.as_deref().unwrap_or("no reason")
                    ),
                    AgentStatus::Decommissioned => "decommissioned".to_string(),
                },
            }
        }
    }
}

/// SPIFFE-compatible workload identity:
/// `spiffe://<trust_domain>/agent/<role>/<agent_id>`.
pub(crate) fn spiffe_id(record: &AgentRecord, trust_domain: &str) -> String {
    format!(
        "spiffe://{}/agent/{}/{}",
        trust_domain.trim_matches('/'),
        record.role,
        record.agent_id
    )
}

#[cfg(test)]
#[path = "agent_registry_tests.rs"]
mod tests;

/// Adversarial attestation-drift coverage (Phase 18 S3), with access to private helpers.
#[cfg(test)]
#[path = "agent_registry_drift_tests.rs"]
mod drift_tests;
