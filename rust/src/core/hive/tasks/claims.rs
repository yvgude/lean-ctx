// SPDX-License-Identifier: Apache-2.0
//! The task claims this machine holds: `<data>/hive/task-claims.json`,
//! 0600. Only the holder ever learns a claim's lease id and fencing token,
//! so they are kept here and never printed.
//!
//! Each claim names the process that holds it (`pid`): that process renews
//! it and gives it back on stop. A new session of the same agent adopts its
//! claims (an MCP restart within the claim's minutes).

use std::path::Path;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::core::hive::HiveError;
use crate::core::hive::metrics::hive_use;
use crate::core::hive::state::write_private;
use lean_ctx_protocol::hive::tasks::ClaimRefV1;

const CLAIMS_FILE: &str = "task-claims.json";

/// One claim held on this machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldClaim {
    /// On disk as `project_id` (pre-VA name) for downgrades and mixed-version teams.
    #[serde(rename = "project_id", alias = "vault_id")]
    pub vault_id: String,
    pub number: u64,
    /// The local checkout the claim was made in.
    pub root: String,
    /// The bus agent id the claim is for.
    pub agent: String,
    pub claim: ClaimRefV1,
    /// RFC 3339, service time.
    pub until: String,
    pub minutes: u32,
    pub pid: u32,
}

impl HeldClaim {
    #[must_use]
    pub fn until(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        chrono::DateTime::parse_from_rfc3339(&self.until)
            .ok()
            .map(|t| t.with_timezone(&chrono::Utc))
    }

    /// Whether half of the claim's minutes have run out at `now`.
    #[must_use]
    pub fn renew_due(&self, now: chrono::DateTime<chrono::Utc>) -> bool {
        self.until().is_none_or(|until| {
            until - now <= chrono::Duration::seconds(i64::from(self.minutes) * 30)
        })
    }

    fn same_task(&self, vault_id: &str, number: u64) -> bool {
        self.vault_id == vault_id && self.number == number
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ClaimsFile {
    #[serde(default)]
    claims: Vec<HeldClaim>,
}

/// One writer per process at a time (the renew thread and tool calls); the
/// file lock below serializes the processes (every MCP session, the CLI,
/// `hive worker stop`).
static LOCK: Mutex<()> = Mutex::new(());
const LOCK_FILE: &str = "task-claims.lock";

fn load(data_dir: &Path) -> ClaimsFile {
    std::fs::read(data_dir.join(CLAIMS_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save(data_dir: &Path, file: &ClaimsFile) -> Result<(), HiveError> {
    let bytes = serde_json::to_vec_pretty(file).map_err(|e| HiveError::Local(e.to_string()))?;
    write_private(data_dir, CLAIMS_FILE, &bytes)
}

fn change<T>(data_dir: &Path, edit: impl FnOnce(&mut Vec<HeldClaim>) -> T) -> Result<T, HiveError> {
    let _guard = LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    std::fs::create_dir_all(data_dir).map_err(|e| HiveError::Local(format!("claims: {e}")))?;
    let _file_lock = crate::core::agents::FileLock::acquire(&data_dir.join(LOCK_FILE))
        .map_err(|e| HiveError::Local(format!("claims lock: {e}")))?;
    let mut file = load(data_dir);
    let out = edit(&mut file.claims);
    save(data_dir, &file)?;
    Ok(out)
}

/// Every claim on this machine.
#[must_use]
pub fn all(data_dir: &Path) -> Vec<HeldClaim> {
    load(data_dir).claims
}

/// The claims this process holds.
#[must_use]
pub fn own(data_dir: &Path) -> Vec<HeldClaim> {
    let pid = std::process::id();
    all(data_dir)
        .into_iter()
        .filter(|claim| claim.pid == pid)
        .collect()
}

#[must_use]
pub fn find(data_dir: &Path, vault_id: &str, number: u64) -> Option<HeldClaim> {
    all(data_dir)
        .into_iter()
        .find(|claim| claim.same_task(vault_id, number))
}

/// Record `claim`, replacing an earlier one on the same task (a renewal
/// keeps the X4 claim window open; a new claim opens one).
pub fn put(data_dir: &Path, claim: HeldClaim) -> Result<(), HiveError> {
    let (vault_id, number) = (claim.vault_id.clone(), claim.number);
    change(data_dir, |claims| {
        claims.retain(|held| !held.same_task(&claim.vault_id, claim.number));
        claims.push(claim);
    })?;
    hive_use::claim_started(data_dir, &vault_id, number, chrono::Utc::now());
    Ok(())
}

/// Store the renewal of `held`: only while that very claim (same lease) is
/// still recorded here, so a renewal that crossed a release, a result or
/// a stop never brings the claim back. Returns whether it was stored.
pub fn renewed(data_dir: &Path, held: &HeldClaim) -> Result<bool, HiveError> {
    change(data_dir, |claims| {
        let Some(slot) = claims.iter_mut().find(|known| {
            known.same_task(&held.vault_id, held.number)
                && known.agent == held.agent
                && known.claim.lease_id == held.claim.lease_id
        }) else {
            return false;
        };
        slot.claim = held.claim.clone();
        slot.until.clone_from(&held.until);
        true
    })
}

/// Forget the claim on a task (released, submitted or lost).
pub fn remove(data_dir: &Path, vault_id: &str, number: u64) -> Result<(), HiveError> {
    change(data_dir, |claims| {
        claims.retain(|held| !held.same_task(vault_id, number));
    })?;
    hive_use::claim_ended(data_dir, vault_id, number, chrono::Utc::now());
    Ok(())
}

/// Forget every claim on this machine (worker mode stopped: Hive ended
/// them all).
pub fn clear(data_dir: &Path) -> Result<(), HiveError> {
    let ended = change(data_dir, std::mem::take)?;
    let now = chrono::Utc::now();
    for held in ended {
        hive_use::claim_ended(data_dir, &held.vault_id, held.number, now);
    }
    Ok(())
}

/// Move the unexpired claims of `agent` to this process and drop expired
/// claims; returns the claims this process now holds for `agent`.
pub fn adopt(
    data_dir: &Path,
    agent: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Vec<HeldClaim>, HiveError> {
    let pid = std::process::id();
    let mut expired = Vec::new();
    let adopted = change(data_dir, |claims| {
        let (live, gone): (Vec<_>, Vec<_>) = std::mem::take(claims)
            .into_iter()
            .partition(|held| held.until().is_some_and(|until| until > now));
        *claims = live;
        expired = gone;
        claims
            .iter_mut()
            .filter(|held| held.agent == agent)
            .map(|held| {
                held.pid = pid;
                held.clone()
            })
            .collect()
    })?;
    // An expired claim ended at its expiry.
    for held in expired {
        let until = held.until().unwrap_or(now);
        hive_use::claim_ended(data_dir, &held.vault_id, held.number, until);
    }
    Ok(adopted)
}

/// The unexpired claims whose holding process has ended (lean-ctx #3: a
/// claim made by the CLI, whose process ends right away). They stay bound
/// to that ended process: whoever keeps them alive (the relay, in the
/// daemon) never becomes their holder, so the agent's next session adopts
/// them and no command is refused for a daemon's pid.
#[must_use]
pub fn orphaned(data_dir: &Path, now: chrono::DateTime<chrono::Utc>) -> Vec<HeldClaim> {
    let pid = std::process::id();
    all(data_dir)
        .into_iter()
        .filter(|held| {
            held.pid != pid
                && held.until().is_some_and(|until| until > now)
                && !crate::ipc::process::is_alive(held.pid)
        })
        .collect()
}

#[cfg(test)]
mod vault_rename_tests {
    use super::*;

    /// `HeldClaim` of the engine before the VA rename.
    #[derive(Deserialize)]
    #[allow(dead_code)]
    struct PreVaHeldClaim {
        project_id: String,
        number: u64,
        root: String,
        agent: String,
        claim: ClaimRefV1,
        until: String,
        minutes: u32,
        pid: u32,
    }

    #[test]
    fn held_claims_keep_pre_va_names() {
        let old = r#"{"project_id":"v1","number":7,"root":"/w","agent":"a","claim":{"lease_id":"lse_1","fencing_token":3},"until":"2026-10-22T10:00:00Z","minutes":30,"pid":1}"#;
        let read = crate::core::hive::vault::assert_pre_va_round_trip::<HeldClaim, PreVaHeldClaim>(
            old,
            "project_id",
            "vault_id",
        );
        assert_eq!(
            (read.vault_id.as_str(), read.claim.fencing_token),
            ("v1", 3)
        );
    }
}
