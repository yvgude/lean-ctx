// SPDX-License-Identifier: Apache-2.0
//! Per-session enforcement evidence (3.12.3): which Hive policy (version,
//! digest, signature state) was in force for which app session, with the
//! receipts made under it.
//!
//! The app names its session with `LEAN_CTX_SESSION_ID` in the environment
//! of every session it starts. On every persisted Instance receipt the
//! session's process records the policy installed at that moment (the
//! floor the Hive policy pull verified and installed) in
//! `<data_dir>/hive/policy/sessions/<session>.json`. A process without a
//! session id records nothing. The session evidence stays local; Hive
//! receives only the `policy.applied` bus frame ([`applied_frame`]): the
//! policy version and digest this Instance applied and how many live
//! sessions enforce it, once per version and digest.
//!
//! Kinds `policy.sessions` (all sessions) and `policy.session` (one).

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::effective_floor;
use crate::state::{HiveStore, write_private};

/// Environment variable naming the app's session (the same name and rule
/// as `lean_ctx_activity::identity::SESSION_ENV`).
pub const SESSION_ENV: &str = "LEAN_CTX_SESSION_ID";
const MAX_SESSION_ID_BYTES: usize = 128;
/// Receipt ids kept per row (the newest).
pub const MAX_RECEIPT_IDS: usize = 32;
/// Policy rows kept per session (the newest).
pub const MAX_ROWS: usize = 16;
/// Session files kept (the most recently written).
pub const MAX_SESSIONS: usize = 256;

const DIR: &str = "sessions";

/// `raw` as a session id: trimmed, 1–128 bytes of `[A-Za-z0-9._:-]`.
pub fn session_id(raw: &str) -> Option<String> {
    let id = raw.trim();
    let ok = !id.is_empty()
        && id.len() <= MAX_SESSION_ID_BYTES
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'));
    ok.then(|| id.to_string())
}

/// The session of this process, from [`SESSION_ENV`].
pub fn current_session() -> Option<String> {
    session_id(&std::env::var(SESSION_ENV).ok()?)
}

/// The policy installed on this machine right now.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Installed {
    pub version: Option<u64>,
    pub digest: Option<String>,
    pub key_id: Option<String>,
    /// `verified` (installed after its signature verified), `invalid` (the
    /// installed files are damaged: fails closed), `none` (no Hive policy).
    pub signature: String,
    /// `in_force`, `expired` (past `not_after`: sensitive operations
    /// refused) or `none`.
    pub state: String,
    pub acked: bool,
    pub acked_at: Option<String>,
}

/// Read what is installed in `hive_data` at `now`.
pub fn installed(hive_data: &Path, now: DateTime<Utc>) -> Installed {
    let checked = effective_floor::read(&effective_floor::floor_dir_in(hive_data)).and_then(
        |(floor, required)| effective_floor::check(floor.as_deref(), required.as_deref()),
    );
    let floor = match checked {
        Ok(Some(floor)) => floor,
        Ok(None) => {
            return Installed {
                signature: "none".into(),
                state: "none".into(),
                ..Installed::default()
            };
        }
        Err(_) => {
            return Installed {
                signature: "invalid".into(),
                state: "none".into(),
                ..Installed::default()
            };
        }
    };
    let effective = crate::policy::load_state(hive_data)
        .effective
        .filter(|e| e.version == floor.version && e.digest == floor.digest);
    let ack = effective
        .as_ref()
        .and_then(|e| e.ack.as_ref())
        .filter(|a| a.version == floor.version && a.digest == floor.digest && a.state == "acked");
    let expired = DateTime::parse_from_rfc3339(&floor.not_after)
        .map_or(true, |t| t.with_timezone(&Utc) <= now);
    Installed {
        version: Some(floor.version),
        digest: Some(floor.digest.clone()),
        key_id: effective.as_ref().map(|e| e.key_id.clone()),
        signature: "verified".into(),
        state: if expired { "expired" } else { "in_force" }.into(),
        acked: ack.is_some(),
        acked_at: ack.map(|a| a.acked_at.clone()),
    }
}

/// One policy as applied to a session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppliedRow {
    pub version: Option<u64>,
    pub digest: Option<String>,
    pub key_id: Option<String>,
    pub signature: String,
    pub state: String,
    pub acked: bool,
    pub first_at: String,
    pub last_at: String,
    pub receipts: u64,
    /// The newest [`MAX_RECEIPT_IDS`] local receipt ids, oldest first.
    pub receipt_ids: Vec<String>,
}

/// The evidence file of one session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionEvidence {
    pub session_id: String,
    /// Oldest first; the last row is the policy of the newest receipt.
    pub applied: Vec<AppliedRow>,
}

impl SessionEvidence {
    /// Add one receipt made under `policy` at `at`.
    pub fn note(&mut self, policy: &Installed, receipt_id: &str, at: &str) {
        let same = |row: &AppliedRow| {
            row.version == policy.version
                && row.digest == policy.digest
                && row.signature == policy.signature
                && row.state == policy.state
        };
        if !self.applied.last().is_some_and(same) {
            self.applied.push(AppliedRow {
                version: policy.version,
                digest: policy.digest.clone(),
                key_id: policy.key_id.clone(),
                signature: policy.signature.clone(),
                state: policy.state.clone(),
                first_at: at.to_string(),
                ..AppliedRow::default()
            });
            if self.applied.len() > MAX_ROWS {
                let extra = self.applied.len() - MAX_ROWS;
                self.applied.drain(..extra);
            }
        }
        let Some(row) = self.applied.last_mut() else {
            return;
        };
        row.acked = policy.acked;
        row.last_at = at.to_string();
        row.receipts = row.receipts.saturating_add(1);
        row.receipt_ids.push(receipt_id.to_string());
        if row.receipt_ids.len() > MAX_RECEIPT_IDS {
            let extra = row.receipt_ids.len() - MAX_RECEIPT_IDS;
            row.receipt_ids.drain(..extra);
        }
    }
}

fn sessions_dir(hive_data: &Path) -> PathBuf {
    hive_data.join("policy").join(DIR)
}

/// The evidence of `session` (`None` when it has none).
pub fn load(hive_data: &Path, session: &str) -> Option<SessionEvidence> {
    let id = session_id(session)?;
    std::fs::read(sessions_dir(hive_data).join(format!("{id}.json")))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
}

/// Every session's evidence, sorted by session id.
pub fn list(hive_data: &Path) -> Vec<SessionEvidence> {
    let Ok(entries) = std::fs::read_dir(sessions_dir(hive_data)) else {
        return Vec::new();
    };
    let mut out: Vec<SessionEvidence> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            load(hive_data, name.strip_suffix(".json")?)
        })
        .collect();
    out.sort_by(|a, b| a.session_id.cmp(&b.session_id));
    out
}

/// Record receipt `receipt_id` of `session` at `now`.
pub fn record(hive_data: &Path, session: &str, receipt_id: &str, now: DateTime<Utc>) {
    let Some(id) = session_id(session) else {
        return;
    };
    let policy = installed(hive_data, now);
    let fresh = load(hive_data, &id).is_none();
    let mut evidence = load(hive_data, &id).unwrap_or_else(|| SessionEvidence {
        session_id: id.clone(),
        applied: Vec::new(),
    });
    evidence.note(
        &policy,
        receipt_id,
        &now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    );
    let Ok(bytes) = serde_json::to_vec_pretty(&evidence) else {
        return;
    };
    let dir = sessions_dir(hive_data);
    if write_private(&dir, &format!("{id}.json"), &bytes).is_ok() && fresh {
        prune(&dir, MAX_SESSIONS);
    }
}

/// Keep the `keep` most recently written session files.
fn prune(dir: &Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().ends_with(".json"))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    if files.len() <= keep {
        return;
    }
    files.sort();
    for (_, path) in files.iter().take(files.len() - keep) {
        let _ = std::fs::remove_file(path);
    }
}

/// The receipt sink's evidence step: this process's session, if the app
/// named one, and only on a machine that is or was connected to Hive.
pub fn note_receipt(receipt_id: &str, now: DateTime<Utc>) {
    let Some(session) = current_session() else {
        return;
    };
    let Ok(store) = HiveStore::open() else {
        return;
    };
    if !matches!(store.instance(), Ok(Some(_))) {
        return;
    }
    record(store.data_dir(), &session, receipt_id, now);
}

/// A session counts as live while its newest receipt is this recent.
pub const LIVE_WINDOW_MINUTES: i64 = 15;
const REPORTED_FILE: &str = "applied.json";

/// The live sessions (newest receipt within [`LIVE_WINDOW_MINUTES`] of
/// `now`) whose newest receipt was made under `policy`, a verified policy
/// in force; sorted. Empty for any other policy.
pub fn enforcing(hive_data: &Path, policy: &Installed, now: DateTime<Utc>) -> Vec<String> {
    if policy.signature != "verified" || policy.state != "in_force" || policy.version.is_none() {
        return Vec::new();
    }
    let since = now - chrono::Duration::minutes(LIVE_WINDOW_MINUTES);
    list(hive_data)
        .into_iter()
        .filter(|evidence| {
            evidence.applied.last().is_some_and(|row| {
                row.version == policy.version
                    && row.digest == policy.digest
                    && row.signature == "verified"
                    && row.state == "in_force"
                    && DateTime::parse_from_rfc3339(&row.last_at)
                        .is_ok_and(|at| at.with_timezone(&Utc) >= since)
            })
        })
        .map(|evidence| evidence.session_id)
        .collect()
}

/// What this Instance last reported to Hive as applied
/// (`<data_dir>/hive/policy/applied.json`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Reported {
    pub version: u64,
    pub digest: String,
    pub sessions: u64,
    pub reported_at: String,
}

/// The last [`Reported`] record, if any.
pub fn load_reported(hive_data: &Path) -> Option<Reported> {
    std::fs::read(hive_data.join("policy").join(REPORTED_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
}

/// Record that `reported` went out on the bus.
pub fn note_reported(hive_data: &Path, reported: &Reported) {
    if let Ok(bytes) = serde_json::to_vec_pretty(reported) {
        let _ = write_private(&hive_data.join("policy"), REPORTED_FILE, &bytes);
    }
}

/// The `policy.applied` frame this Instance owes Hive at `now`, with the
/// record to keep once it is sent: only for a policy whose signature
/// verified, that is in force and that Hive acknowledged for this version
/// and digest, and only once per version and digest. A stale (expired,
/// outdated) or unverified policy is never reported as applied.
pub fn applied_frame(
    hive_data: &Path,
    now: DateTime<Utc>,
) -> Option<(serde_json::Value, Reported)> {
    let policy = installed(hive_data, now);
    if policy.signature != "verified" || policy.state != "in_force" || !policy.acked {
        return None;
    }
    let (version, digest) = (policy.version?, policy.digest.clone()?);
    if load_reported(hive_data).is_some_and(|r| r.version == version && r.digest == digest) {
        return None;
    }
    let sessions = enforcing(hive_data, &policy, now).len() as u64;
    let frame = serde_json::json!({
        "t": "policy.applied",
        "version": version,
        "digest": digest,
        "key_id": policy.key_id,
        "applied_at": policy.acked_at,
        "sessions": sessions,
    });
    let reported = Reported {
        version,
        digest,
        sessions,
        reported_at: now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    };
    Some((frame, reported))
}

#[cfg(test)]
#[path = "policy_sessions_tests.rs"]
mod tests;
