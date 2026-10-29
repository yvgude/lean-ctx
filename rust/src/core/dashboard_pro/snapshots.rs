// SPDX-License-Identifier: Apache-2.0
//! Knowledge snapshots of one project, kept in its Pro partition:
//! `<data>/dashboard-pro/<partition>/snapshots/<id>.json` (every record of
//! the store) and `<id>.meta` (`{created_at, facts, reason}`). Ids are
//! `YYYYMMDDTHHMMSSZ-<6hex>`; only names of that form are ever read.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::core::knowledge::{KnowledgeFact, ProjectKnowledge};

use super::wire::SnapshotMeta;
use super::{ProError, ProResult, ProjectScope, read_bounded, read_json, write_json};

const SNAPSHOT_LIMIT: u64 = 64 << 20;
pub(crate) const LIST_LIMIT: usize = 30;

#[derive(Debug, Serialize, Deserialize)]
struct SnapshotFile {
    schema: String,
    project_hash: String,
    facts: Vec<KnowledgeFact>,
}

pub(crate) fn is_snapshot_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    bytes.len() == 23
        && bytes[..8].iter().all(u8::is_ascii_digit)
        && bytes[8] == b'T'
        && bytes[9..15].iter().all(u8::is_ascii_digit)
        && bytes[15] == b'Z'
        && bytes[16] == b'-'
        && bytes[17..]
            .iter()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
}

fn dir(scope: &ProjectScope) -> PathBuf {
    scope.partition_dir().join("snapshots")
}

pub(crate) fn new_id(now: DateTime<Utc>) -> String {
    let mut random = [0u8; 3];
    if getrandom::fill(&mut random).is_err() {
        random = now
            .timestamp_subsec_nanos()
            .to_le_bytes()
            .split_at(3)
            .0
            .try_into()
            .unwrap_or([0; 3]);
    }
    format!("{}-{}", now.format("%Y%m%dT%H%M%SZ"), hex::encode(random))
}

/// Saves every record of the project's knowledge store.
pub(crate) fn create(
    scope: &ProjectScope,
    knowledge: &ProjectKnowledge,
    reason: &str,
    now: DateTime<Utc>,
) -> ProResult<SnapshotMeta> {
    let id = new_id(now);
    let file = SnapshotFile {
        schema: "leanctx.knowledge-snapshot/1".into(),
        project_hash: knowledge.project_hash.clone(),
        facts: knowledge.facts.clone(),
    };
    let current = knowledge.facts.iter().filter(|f| f.is_current()).count();
    write_json(&dir(scope).join(format!("{id}.json")), &file)?;
    let meta = json!({
        "created_at": now.to_rfc3339(),
        "facts": current,
        "reason": reason.chars().take(120).collect::<String>(),
    });
    write_json(&dir(scope).join(format!("{id}.meta")), &meta)?;
    Ok(SnapshotMeta {
        id,
        created_at: meta.get("created_at").cloned(),
        facts: meta.get("facts").cloned(),
        reason: meta.get("reason").cloned(),
    })
}

/// The newest snapshots of this project's partition.
pub(crate) fn list(scope: &ProjectScope, limit: usize) -> Vec<SnapshotMeta> {
    let Ok(entries) = std::fs::read_dir(dir(scope)) else {
        return Vec::new();
    };
    let mut ids: Vec<String> = entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            e.file_name()
                .to_str()?
                .strip_suffix(".json")
                .filter(|id| is_snapshot_id(id))
                .map(str::to_string)
        })
        .collect();
    ids.sort_unstable_by(|a, b| b.cmp(a));
    ids.truncate(limit);
    ids.into_iter()
        .map(|id| {
            let meta: Value = read_json(&dir(scope).join(format!("{id}.meta"))).unwrap_or_default();
            SnapshotMeta {
                created_at: meta.get("created_at").cloned(),
                facts: meta.get("facts").cloned(),
                reason: meta.get("reason").cloned(),
                id,
            }
        })
        .collect()
}

/// Every record a snapshot of this project holds.
pub(crate) fn read_facts(scope: &ProjectScope, id: &str) -> ProResult<Vec<KnowledgeFact>> {
    if !is_snapshot_id(id) {
        return Err(ProError::bad("Unknown snapshot"));
    }
    let bytes = read_bounded(&dir(scope).join(format!("{id}.json")), SNAPSHOT_LIMIT)
        .ok_or_else(|| ProError::not_found("Unknown snapshot"))?;
    let file: SnapshotFile = serde_json::from_slice(&bytes)
        .map_err(|_| ProError::conflict("The snapshot is not readable"))?;
    if file.project_hash != ProjectKnowledge::new(&scope.root).project_hash {
        return Err(ProError::conflict(
            "The snapshot belongs to another project",
        ));
    }
    Ok(file.facts)
}
