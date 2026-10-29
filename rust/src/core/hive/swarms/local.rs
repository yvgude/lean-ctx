// SPDX-License-Identifier: Apache-2.0
//! What this engine keeps per joined swarm, under
//! `<data>/hive/swarm/<swarm_id>/` (0700): the participant record and the
//! claims its host sessions hold. `swarm leave` deletes the directory.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{ClaimGrant, is_swarm_id};
use crate::core::hive::HiveError;
use crate::core::hive::state::{lock_state, write_private};

const PARTICIPANT_FILE: &str = "participant.json";
const CLAIMS_FILE: &str = "claims.json";

/// This engine's participation in one swarm.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Participant {
    pub swarm: String,
    /// The participant worker (swarm alias, §3.6).
    pub worker_id: String,
    /// The host CLI the join named (`swarm_join` subject `cli`).
    pub cli: String,
    pub joined_at: u64,
    /// The approved `swarm_join` consent.
    pub consent_id: String,
}

/// A claim a host session of this engine holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldClaim {
    pub goal: String,
    pub worker_id: String,
    #[serde(flatten)]
    pub grant: ClaimGrant,
}

pub fn dir(data_dir: &Path, swarm: &str) -> Result<PathBuf, HiveError> {
    if !is_swarm_id(swarm) {
        return Err(HiveError::Config(format!("not a swarm id: {swarm}")));
    }
    Ok(data_dir.join("swarm").join(swarm))
}

fn read<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>, HiveError> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| HiveError::Local(format!("{}: {e}", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(HiveError::Local(format!("{}: {e}", path.display()))),
    }
}

fn write<T: Serialize>(dir: &Path, name: &str, value: &T) -> Result<(), HiveError> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| HiveError::Local(e.to_string()))?;
    write_private(dir, name, &bytes)
}

pub fn participant(data_dir: &Path, swarm: &str) -> Result<Option<Participant>, HiveError> {
    read(&dir(data_dir, swarm)?.join(PARTICIPANT_FILE))
}

pub fn joined(data_dir: &Path) -> Vec<Participant> {
    let Ok(entries) = std::fs::read_dir(data_dir.join("swarm")) else {
        return Vec::new();
    };
    let mut out: Vec<Participant> = entries
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| is_swarm_id(name))
        .filter_map(|name| participant(data_dir, &name).ok().flatten())
        .collect();
    out.sort_by(|a, b| a.swarm.cmp(&b.swarm));
    out
}

pub fn save_participant(data_dir: &Path, p: &Participant) -> Result<(), HiveError> {
    write(&dir(data_dir, &p.swarm)?, PARTICIPANT_FILE, p)
}

/// Claims by claim id.
pub fn claims(data_dir: &Path, swarm: &str) -> Result<BTreeMap<String, HeldClaim>, HiveError> {
    Ok(read(&dir(data_dir, swarm)?.join(CLAIMS_FILE))?.unwrap_or_default())
}

/// Read-modify-write of the claims under the swarm's state lock.
pub fn with_claims<T>(
    data_dir: &Path,
    swarm: &str,
    change: impl FnOnce(&mut BTreeMap<String, HeldClaim>) -> T,
) -> Result<T, HiveError> {
    let at = dir(data_dir, swarm)?;
    let _lock = lock_state(&at, "claims")?;
    let mut held: BTreeMap<String, HeldClaim> = read(&at.join(CLAIMS_FILE))?.unwrap_or_default();
    let out = change(&mut held);
    write(&at, CLAIMS_FILE, &held)?;
    Ok(out)
}

/// `swarm leave`: everything of the swarm under `<data>/hive/swarm/<id>/`.
pub fn forget(data_dir: &Path, swarm: &str) -> Result<(), HiveError> {
    let at = dir(data_dir, swarm)?;
    match std::fs::remove_dir_all(&at) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(HiveError::Local(format!("{}: {e}", at.display()))),
    }
}
