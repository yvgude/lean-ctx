// SPDX-License-Identifier: Apache-2.0
//! Engine upgrade duties of the first 3.12 start (SPEC-ENGINE §3.16, V13.E1),
//! from a 3.11.5 or a 3.12.0-rc data directory.
//!
//! Two phases, each run once and recorded in `<data>/hive/upgrade-3.12.json`:
//!
//! - **local** (first start, offline): rc-era replicas without
//!   `user_version = 1` are deleted (the next sync clones them again);
//!   `drafts/<stable>.watermark` moves to `capture/<place>.watermark`;
//!   queued session drafts are recaptured through the gate (the old ones are
//!   dropped, what passes is queued again).
//! - **C0 2.0** (first start against a Hive that speaks 2.0): the v1 relay
//!   cursors are reset (channels became places), the agent-keyed presence
//!   map is discarded (presence is keyed by worker id), and the facts that
//!   1.x pulls copied in from a vault that now has a replica are removed,
//!   their local relations rewritten to the replica node ([`retire_pulled`]).
//!
//! The `auto_draft` → `auto_capture` rename is a config alias
//! (`core::config::renamed`), applied on load and rewritten on write.
//! `hive status` prints the counts once ([`status_lines_once`]).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

use chrono::Utc;
use serde::{Deserialize, Serialize};

use super::HiveError;
use super::capture::dedupe;
use super::config::HiveConfig;
use super::knowledge::draft;
use super::knowledge::learn;
use super::knowledge::screening::Screen;
use super::outbox::{CHANNEL_KNOWLEDGE, Outbox};
use super::replica::Replica;
use super::replica::fold::Graph;
use super::state::{HiveStore, lock_state, write_private};
use super::vault::{VaultMap, VaultRecord, project_identity};
use crate::core::knowledge::{NodeType, ProjectKnowledge};
use crate::core::knowledge_relations::{KnowledgeNodeRef, KnowledgeRelationGraph};

const STATE_FILE: &str = "upgrade-3.12.json";
/// Category of a local relation endpoint that is a replica node:
/// `vault:<vault_id>` / `<node_id>`.
pub const REPLICA_NODE_CATEGORY: &str = "vault:";

/// The relation endpoint of node `node_id` in the replica of `vault_id`.
pub fn replica_node_ref(vault_id: &str, node_id: &str) -> KnowledgeNodeRef {
    KnowledgeNodeRef::new(&format!("{REPLICA_NODE_CATEGORY}{vault_id}"), node_id)
}

/// What the upgrade did, summed over both phases.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpgradeReport {
    /// Unsent 3.11.5/rc session drafts removed.
    pub drafts_dropped: usize,
    /// Drafts the recapture queued instead (only gate-passing claims).
    pub drafts_requeued: usize,
    pub watermarks_moved: usize,
    /// rc-era replicas deleted for a fresh clone.
    pub replicas_reset: usize,
    pub relay_cursors_reset: usize,
    pub presence_discarded: bool,
    /// Facts pulled by 1.x that a replica now serves.
    pub facts_removed: usize,
    pub relations_rewritten: usize,
    /// Relations whose target has no replica node.
    pub relations_dropped: usize,
}

impl UpgradeReport {
    fn absorb(&mut self, retired: &Retired) {
        self.facts_removed += retired.facts_removed;
        self.relations_rewritten += retired.relations_rewritten;
        self.relations_dropped += retired.relations_dropped;
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct UpgradeState {
    local_done: bool,
    v2_done: bool,
    report: UpgradeReport,
    /// The report as `hive status` last printed it.
    shown: UpgradeReport,
}

fn local(e: impl std::fmt::Display) -> HiveError {
    HiveError::Local(e.to_string())
}

fn load_state(data: &Path) -> UpgradeState {
    std::fs::read(data.join(STATE_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_state(data: &Path, state: &UpgradeState) -> Result<(), HiveError> {
    write_private(data, STATE_FILE, &serde_json::to_vec(state).map_err(local)?)
}

/// Run the upgrade phases not done yet. Cheap once both are done.
pub fn run(store: &HiveStore, config: &HiveConfig) -> Result<UpgradeReport, HiveError> {
    let data = store.data_dir();
    let mut state = load_state(data);
    let v2 = super::bus::relay_v2::speaks_v2(store);
    if state.local_done && (state.v2_done || !v2) {
        return Ok(state.report);
    }
    let _lock = lock_state(data, STATE_FILE)?;
    state = load_state(data);
    if !state.local_done {
        state.report.replicas_reset += reset_stale_replicas(data)?;
        state.report.watermarks_moved += move_watermarks(data)?;
        let (dropped, requeued) = recapture_drafts(store, config)?;
        state.report.drafts_dropped += dropped;
        state.report.drafts_requeued += requeued;
        state.local_done = true;
        save_state(data, &state)?;
    }
    if !state.v2_done && v2 {
        state.report.relay_cursors_reset += reset_relay_cursors(data)?;
        state.report.presence_discarded |= discard_presence(data)?;
        for retired in retire_all(store)? {
            state.report.absorb(&retired);
        }
        state.v2_done = true;
        save_state(data, &state)?;
    }
    Ok(state.report)
}

/// `hive status` lines about the upgrade, once: empty until a later phase
/// changes the counts.
pub fn status_lines_once(store: &HiveStore) -> Vec<String> {
    let data = store.data_dir();
    let Ok(_lock) = lock_state(data, STATE_FILE) else {
        return Vec::new();
    };
    let mut state = load_state(data);
    if state.report == state.shown {
        return Vec::new();
    }
    let lines = status_lines(&state.report);
    state.shown = state.report.clone();
    if save_state(data, &state).is_err() {
        return Vec::new();
    }
    lines
}

fn status_lines(r: &UpgradeReport) -> Vec<String> {
    let mut lines = Vec::new();
    if r.drafts_dropped > 0 || r.drafts_requeued > 0 {
        lines.push(format!(
            "Upgrade:      {} unsent session draft(s) dropped, {} queued again through the capture gate",
            r.drafts_dropped, r.drafts_requeued
        ));
    }
    if r.facts_removed > 0 || r.relations_rewritten > 0 || r.relations_dropped > 0 {
        lines.push(format!(
            "Upgrade:      {} pulled vault fact(s) now served by the replica; {} relation(s) moved to replica nodes, {} dropped (no replica node)",
            r.facts_removed, r.relations_rewritten, r.relations_dropped
        ));
    }
    if r.replicas_reset > 0 {
        lines.push(format!(
            "Upgrade:      {} pre-release replica(s) removed; the next sync clones them again",
            r.replicas_reset
        ));
    }
    lines
}

// ---- local phase ----------------------------------------------------------

/// Delete every replica directory whose `replica.db` does not carry the
/// schema version this engine writes (rc-era files, §3.16).
fn reset_stale_replicas(data: &Path) -> Result<usize, HiveError> {
    let Ok(entries) = std::fs::read_dir(data.join("replica")) else {
        return Ok(0);
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        if replica_version(&dir.join("replica.db")) != Some(super::replica::store::SCHEMA_VERSION) {
            std::fs::remove_dir_all(&dir).map_err(local)?;
            removed += 1;
        }
    }
    Ok(removed)
}

fn replica_version(db: &Path) -> Option<i64> {
    if !db.is_file() {
        return None;
    }
    let conn =
        rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .ok()?;
    conn.pragma_query_value(None, "user_version", |r| r.get(0))
        .ok()
}

/// `drafts/<stable>.watermark` → `capture/<place>.watermark`. A watermark
/// already at the new place wins.
fn move_watermarks(data: &Path) -> Result<usize, HiveError> {
    let Ok(entries) = std::fs::read_dir(data.join(draft::LEGACY_WATERMARK_DIR)) else {
        return Ok(0);
    };
    let mut moved = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some(draft::WATERMARK_EXT) {
            continue;
        }
        let Some(stable) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let place = draft::watermark_place(data, stable);
        let name = format!("{place}.{}", draft::WATERMARK_EXT);
        let target = data.join(draft::WATERMARK_DIR).join(&name);
        if !target.exists() {
            let bytes = std::fs::read(&path).map_err(local)?;
            write_private(&data.join(draft::WATERMARK_DIR), &name, &bytes)?;
        }
        std::fs::remove_file(&path).map_err(local)?;
        moved += 1;
    }
    Ok(moved)
}

/// Recapture every project with queued session drafts (§3.1.6): its old
/// drafts go, the gate queues what passes. A draft whose checkout is gone
/// is dropped. Returns `(dropped, requeued)`.
fn recapture_drafts(store: &HiveStore, config: &HiveConfig) -> Result<(usize, usize), HiveError> {
    let outbox = Outbox::at(store.data_dir());
    let mut projects: BTreeMap<(String, String), Vec<_>> = BTreeMap::new();
    for item in outbox.list(CHANNEL_KNOWLEDGE) {
        if let Some(q) = draft::queued(&item.payload)
            && q.draft.origin.is_none()
        {
            projects
                .entry((q.stable_identity, q.root))
                .or_default()
                .push(item);
        }
    }
    let (mut dropped, mut requeued) = (0, 0);
    for ((stable, root), items) in projects {
        let root = Path::new(&root);
        let same_project = project_identity(root)
            .is_some_and(|i| lean_ctx_protocol::hive::vault::stable_identity(&i) == stable);
        if same_project {
            let mut screen = Screen::for_installation();
            if let Ok(done) = draft::recapture(store, config, root, &mut screen, Utc::now()) {
                dropped += done.removed;
                requeued += done.queued.len();
                continue;
            }
        }
        for item in &items {
            outbox.remove(item)?;
            dropped += 1;
        }
    }
    Ok((dropped, requeued))
}

// ---- C0 2.0 phase -----------------------------------------------------------

/// The v1 relay's per-channel cursors (`bus/state.json`); channel names
/// became places, so they start over. Only a v1 relay writes this file, and
/// none runs once the Hive speaks 2.0.
fn reset_relay_cursors(data: &Path) -> Result<usize, HiveError> {
    if !data.join("bus").join("state.json").exists() {
        return Ok(0);
    }
    let mut state = super::bus::relay::RelayState::load(data);
    let n = state.cursors.len();
    if n > 0 {
        state.cursors.clear();
        state.save(data)?;
    }
    Ok(n)
}

/// The agent-keyed presence map is discarded; 2.0 presence is per worker.
fn discard_presence(data: &Path) -> Result<bool, HiveError> {
    let dir = data.join("bus");
    let path = dir.join("presence.json");
    if !path.exists() {
        return Ok(false);
    }
    let _lock = lock_state(&dir, "presence.json")?;
    std::fs::remove_file(&path).map_err(local)?;
    Ok(true)
}

/// [`retire_pulled`] for every vault that has a replica, in each of its
/// checkouts.
fn retire_all(store: &HiveStore) -> Result<Vec<Retired>, HiveError> {
    let data = store.data_dir();
    let records: Vec<VaultRecord> = VaultMap::load(data)
        .vaults
        .into_values()
        .filter(|r| {
            super::replica::store::place_dir(data, &r.vault_id)
                .join("replica.db")
                .exists()
        })
        .collect();
    if records.is_empty() {
        return Ok(Vec::new());
    }
    let (mk, _) = super::replica::crypt::master_key(store.config_dir(), false)?;
    let mut out = Vec::new();
    for record in records {
        let (replica, _) = Replica::open(data, &record.vault_id, &mk)?;
        for root in &record.roots {
            if Path::new(root).is_dir() {
                out.push(retire_pulled(&replica, &record, Path::new(root))?);
            }
        }
    }
    Ok(out)
}

/// What [`retire_pulled`] did in one checkout.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Retired {
    pub facts_removed: usize,
    pub relations_rewritten: usize,
    pub relations_dropped: usize,
}

/// [`retire_pulled`] right after a vault's first clone; its counts go to the
/// upgrade report `hive status` prints once. Returns the facts removed.
pub fn retire_on_clone(
    store: &HiveStore,
    replica: &Replica,
    record: &VaultRecord,
    root: &Path,
) -> Result<usize, HiveError> {
    let retired = retire_pulled(replica, record, root)?;
    if retired != Retired::default() {
        let data = store.data_dir();
        let _lock = lock_state(data, STATE_FILE)?;
        let mut state = load_state(data);
        state.report.absorb(&retired);
        save_state(data, &state)?;
    }
    Ok(retired.facts_removed)
}

/// Remove the facts 1.x pulls copied from `record`'s vault into the
/// knowledge of the checkout at `root` (the replica serves them now) and
/// rewrite the local relations that pointed at them to the replica node of
/// the same claim: the keyed exact dedupe key of its node type and
/// normalised statement (§3.1.5), never a recomputed `fact_id`. A relation
/// whose claim has no live replica node is dropped and counted.
pub fn retire_pulled(
    replica: &Replica,
    record: &VaultRecord,
    root: &Path,
) -> Result<Retired, HiveError> {
    let root_s = root.to_string_lossy();
    let Some(knowledge) = ProjectKnowledge::load(&root_s) else {
        return Ok(Retired::default());
    };
    let prefix = learn::import_tag_prefix(&record.ns, &record.name);
    let pulled: Vec<_> = knowledge
        .facts
        .iter()
        .filter(|f| {
            f.imported_from
                .as_deref()
                .and_then(learn::import_tag_source)
                .is_some_and(|source| source.starts_with(&prefix))
        })
        .cloned()
        .collect();
    let facts_removed =
        learn::remove_pulled(&root_s, &record.ns, &record.name).map_err(HiveError::Local)?;
    let mut out = Retired {
        facts_removed,
        ..Retired::default()
    };
    if pulled.is_empty() {
        return Ok(out);
    }
    let Some(mut relations) = KnowledgeRelationGraph::load(&knowledge.project_hash) else {
        return Ok(out);
    };
    // Endpoints that no longer name a fact, and the node each one becomes.
    let left = ProjectKnowledge::load(&root_s).unwrap_or(knowledge);
    let index = NodeIndex::of(replica)?;
    let mut gone: HashMap<KnowledgeNodeRef, Option<KnowledgeNodeRef>> = HashMap::new();
    for fact in &pulled {
        let r = KnowledgeNodeRef::new(&fact.category, &fact.key);
        if left
            .facts
            .iter()
            .any(|f| f.category == r.category && f.key == r.key)
        {
            continue;
        }
        let statement =
            super::untrusted::body_of(&fact.value).unwrap_or_else(|| fact.value.clone());
        let node = index
            .node_of(&statement)
            .map(|node| replica_node_ref(&record.vault_id, &node));
        let slot = gone.entry(r).or_insert(None);
        if slot.is_none() {
            *slot = node;
        }
    }
    let mut kept = Vec::with_capacity(relations.edges.len());
    let mut seen = HashSet::new();
    let mut changed = false;
    for mut edge in std::mem::take(&mut relations.edges) {
        let mut touched = false;
        let mut dangling = false;
        for end in [&mut edge.from, &mut edge.to] {
            if let Some(target) = gone.get(end) {
                touched = true;
                match target {
                    Some(node) => *end = node.clone(),
                    None => dangling = true,
                }
            }
        }
        if dangling || (touched && edge.from == edge.to) {
            out.relations_dropped += 1;
            changed = true;
            continue;
        }
        if !seen.insert((edge.from.clone(), edge.to.clone(), edge.kind.as_str())) {
            // The rewrite merged it with an edge that is already there.
            out.relations_rewritten += usize::from(touched);
            changed = true;
            continue;
        }
        if touched {
            out.relations_rewritten += 1;
            changed = true;
        }
        kept.push(edge);
    }
    relations.edges = kept;
    if changed {
        relations.save().map_err(HiveError::Local)?;
    }
    Ok(out)
}

/// Live replica nodes by their exact dedupe key (`dedupe_keys` holds only
/// the place's own captures, so the folded nodes are keyed as well).
struct NodeIndex<'a> {
    replica: &'a Replica,
    pk_id: Option<[u8; 32]>,
    live: BTreeMap<[u8; 32], String>,
    live_ids: BTreeSet<String>,
}

const NODE_TYPES: [NodeType; 5] = [
    NodeType::Fact,
    NodeType::Decision,
    NodeType::Gotcha,
    NodeType::Procedure,
    NodeType::Finding,
];

impl<'a> NodeIndex<'a> {
    fn of(replica: &'a Replica) -> Result<Self, HiveError> {
        let pk_id = replica.id_salt()?;
        let mut graph = Graph::default();
        graph
            .fold(&replica.fold_events()?)
            .map_err(|e| HiveError::Protocol(e.to_string()))?;
        let mut live = BTreeMap::new();
        let mut live_ids = BTreeSet::new();
        for (id, node) in graph.live_nodes() {
            live_ids.insert(id.clone());
            let (Some(pk), Some(nt), Some(statement)) = (
                pk_id,
                NodeType::parse(&node.node_type),
                node.fields.get("statement").and_then(|v| v.as_str()),
            ) else {
                continue;
            };
            live.insert(dedupe::exact_key(&pk, nt, statement), id.clone());
        }
        Ok(Self {
            replica,
            pk_id,
            live,
            live_ids,
        })
    }

    /// The live node holding `statement` (a 1.x procedure's text is its
    /// title followed by numbered steps, so its first line is tried too).
    fn node_of(&self, statement: &str) -> Option<String> {
        let pk = self.pk_id?;
        let first = statement.lines().next().unwrap_or_default();
        for text in [statement, first] {
            for nt in NODE_TYPES {
                let dk = dedupe::exact_key(&pk, nt, text);
                if let Some(id) = self.live.get(&dk) {
                    return Some(id.clone());
                }
                if let Ok(Some(id)) = self.replica.dedupe_node(&dk)
                    && self.live_ids.contains(&id)
                {
                    return Some(id);
                }
            }
        }
        None
    }
}

#[cfg(test)]
#[path = "migrate_tests.rs"]
mod tests;
