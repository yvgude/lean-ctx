use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, Write};
use std::path::PathBuf;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
fn read_last_hash_tail(file: &fs::File) -> String {
    use std::io::{Read, Seek, SeekFrom};
    const TAIL: i64 = 64 * 1024;

    let mut f = file;
    let Ok(len) = f.seek(SeekFrom::End(0)) else {
        return "genesis".to_string();
    };
    if len == 0 {
        return "genesis".to_string();
    }
    let start = if (len as i64) > TAIL {
        -TAIL
    } else {
        -(len as i64)
    };
    if f.seek(SeekFrom::End(start)).is_err() {
        return "genesis".to_string();
    }
    let mut buf = String::new();
    if f.read_to_string(&mut buf).is_err() {
        return "genesis".to_string();
    }
    // Concurrent-append history may hold multiple objects per line; a
    // stream parse of the last non-empty line yields the true tail entry.
    for line in buf.lines().rev() {
        if line.trim().is_empty() {
            continue;
        }
        let mut last: Option<String> = None;
        for v in serde_json::Deserializer::from_str(line)
            .into_iter::<serde_json::Value>()
            .flatten()
        {
            if let Some(h) = v.get("entry_hash").and_then(|h| h.as_str()) {
                last = Some(h.to_string());
            }
        }
        if let Some(h) = last {
            return h;
        }
    }
    "genesis".to_string()
}

fn compute_entry_hash(prev_hash: &str, data_json: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(prev_hash.as_bytes());
    hasher.update(data_json.as_bytes());
    crate::core::agent_identity::hex_encode(&hasher.finalize())
}

pub fn record(data: AuditEntryData) {
    use fs2::FileExt;

    let Some(path) = trail_path() else { return };
    let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .open(&path)
    else {
        return;
    };
    // Advisory lock serializes appends ACROSS processes; prev_hash is read
    // from the file under the same lock, so the chain cannot fork.
    if file.lock_exclusive().is_err() {
        return;
    }
    let prev_hash = read_last_hash_tail(&file);

    let partial = serde_json::json!({
        "agent_id": data.agent_id,
        "tool": data.tool,
        "action": data.action,
        "input_hash": data.input_hash,
        "output_tokens": data.output_tokens,
        "role": data.role,
        "event_type": data.event_type,
    });
    let data_json = serde_json::to_string(&partial).unwrap_or_default();
    let entry_hash = compute_entry_hash(&prev_hash, &data_json);

    let signature = crate::core::agent_identity::sign_bytes("lean-ctx", entry_hash.as_bytes())
        .map(|sig| crate::core::agent_identity::hex_encode(&sig))
        .ok();

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

    if let Ok(line) = serde_json::to_string(&entry) {
        let _ = writeln!(file, "{line}");
    }
    let _ = FileExt::unlock(&file);
}

pub fn load_recent(limit: usize) -> Vec<AuditEntry> {
    let Some(path) = trail_path() else {
        return Vec::new();
    };
    let Ok(file) = fs::File::open(&path) else {
        return Vec::new();
    };
    let reader = std::io::BufReader::new(file);
    let entries: Vec<AuditEntry> = reader
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str(&line).ok())
        .collect();
    let skip = entries.len().saturating_sub(limit);
    entries.into_iter().skip(skip).collect()
}

pub fn verify_chain() -> ChainVerifyResult {
    let Some(path) = trail_path() else {
        return ChainVerifyResult {
            total_entries: 0,
            valid: true,
            first_invalid_at: None,
        };
    };
    let Ok(file) = fs::File::open(&path) else {
        return ChainVerifyResult {
            total_entries: 0,
            valid: true,
            first_invalid_at: None,
        };
    };
    let reader = std::io::BufReader::new(file);
    let mut prev_hash = "genesis".to_string();
    let mut total = 0usize;

    for line in reader.lines().map_while(Result::ok) {
        let entry: AuditEntry = match serde_json::from_str(&line) {
            Ok(e) => e,
            Err(_) => {
                return ChainVerifyResult {
                    total_entries: total,
                    valid: false,
                    first_invalid_at: Some(total),
                };
            }
        };

        if entry.prev_hash != prev_hash {
            return ChainVerifyResult {
                total_entries: total,
                valid: false,
                first_invalid_at: Some(total),
            };
        }

        let partial = serde_json::json!({
            "agent_id": entry.agent_id,
            "tool": entry.tool,
            "action": entry.action,
            "input_hash": entry.input_hash,
            "output_tokens": entry.output_tokens,
            "role": entry.role,
            "event_type": entry.event_type,
        });
        let data_json = serde_json::to_string(&partial).unwrap_or_default();
        let expected = compute_entry_hash(&prev_hash, &data_json);

        if entry.entry_hash != expected {
            return ChainVerifyResult {
                total_entries: total,
                valid: false,
                first_invalid_at: Some(total),
            };
        }

        prev_hash = entry.entry_hash;
        total += 1;
    }

    ChainVerifyResult {
        total_entries: total,
        valid: true,
        first_invalid_at: None,
    }
}

#[must_use]
pub fn hash_input(args: &serde_json::Map<String, serde_json::Value>) -> String {
    let serialized = serde_json::to_string(args).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(serialized.as_bytes());
    crate::core::agent_identity::hex_encode(&hasher.finalize())
}
