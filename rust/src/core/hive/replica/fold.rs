// SPDX-License-Identifier: Apache-2.0
//! Materialisation of a place's log into its graph (SPEC-ENGINE §3.4.5).
//!
//! The fold is pure: it orders events by the server's `seq` (the single
//! source of truth) and applies them, so any arrival order of the same
//! events gives the same nodes and edges. Events whose signature or body
//! check failed (`sig_invalid`) never fold.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};

/// One event as the fold sees it (already verified).
#[derive(Debug, Clone, PartialEq)]
pub struct FoldEvent {
    pub seq: u64,
    pub event_id: String,
    pub op: String,
    pub target: Option<String>,
    pub node_type: Option<String>,
    /// `None` for a purged event (key shredded) or a withheld stub.
    pub body: Option<Value>,
    pub sig_invalid: bool,
    /// Server fields: provenance the server stamped (`own`, `workspace`,
    /// `foreign`, …) and review state (`confirmed`, `unconfirmed`, …).
    pub provenance: Option<String>,
    pub review_state: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Node {
    pub node_type: String,
    /// The body fields without `body_nonce`; empty once purged.
    pub fields: Map<String, Value>,
    pub retracted: bool,
    pub purged: bool,
    pub last_seq: u64,
    /// From the event that added the node.
    pub provenance: Option<String>,
    pub review_state: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Edge {
    pub kind: String,
    pub from: String,
    pub to: String,
    pub retracted: bool,
    pub last_seq: u64,
}

/// A purge tombstone as applied (reason and the event ids it shredded).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Purge {
    pub purge_id: String,
    pub reason: String,
}

/// The folded graph of a place.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Graph {
    pub nodes: BTreeMap<String, Node>,
    pub edges: BTreeMap<String, Edge>,
    /// Event ids whose body a purge shredded.
    pub purged_events: BTreeSet<String>,
    /// Node id → the purge that removed its text.
    pub purged_nodes: BTreeMap<String, Purge>,
    /// Every purge tombstone folded: `(purge_id, seq of the tombstone)`;
    /// each one is acknowledged to the place once (E9).
    pub tombstones: BTreeSet<(String, u64)>,
    /// Event id of every `node.add` → its node, so a later purge of that
    /// event finds the node.
    pub added_by: BTreeMap<String, String>,
    /// Highest `seq` folded.
    pub cursor: u64,
}

/// Why folding a place stopped (fail closed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FoldError {
    /// A purge reason outside the four owner-approved values.
    PurgeReasonInvalid(String),
    /// Two different events with the same `seq`.
    SeqConflict(u64),
}

impl std::fmt::Display for FoldError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PurgeReasonInvalid(r) => write!(f, "purge_reason_invalid: {r}"),
            Self::SeqConflict(seq) => write!(f, "two events with seq {seq}"),
        }
    }
}

fn fields_of(body: &Value) -> Map<String, Value> {
    let mut fields = body.as_object().cloned().unwrap_or_default();
    fields.remove("body_nonce");
    fields
}

impl Graph {
    /// Fold `events` (any order) onto this graph. Events at or below the
    /// cursor were folded before and are skipped.
    pub fn fold(&mut self, events: &[FoldEvent]) -> Result<(), FoldError> {
        let mut ordered: Vec<&FoldEvent> = events.iter().filter(|e| e.seq > self.cursor).collect();
        ordered.sort_by_key(|e| e.seq);
        for pair in ordered.windows(2) {
            if pair[0].seq == pair[1].seq {
                if pair[0] == pair[1] {
                    continue;
                }
                return Err(FoldError::SeqConflict(pair[0].seq));
            }
        }
        ordered.dedup_by_key(|e| e.seq);
        for e in ordered {
            self.cursor = e.seq;
            if e.sig_invalid {
                continue;
            }
            self.apply(e)?;
        }
        Ok(())
    }

    fn apply(&mut self, e: &FoldEvent) -> Result<(), FoldError> {
        let target = e.target.clone().unwrap_or_default();
        match e.op.as_str() {
            "node.add" => {
                self.added_by
                    .entry(e.event_id.clone())
                    .or_insert_with(|| target.clone());
                let purged = e.body.is_none() || self.purged_events.contains(&e.event_id);
                let node = self.nodes.entry(target.clone()).or_insert_with(|| Node {
                    node_type: e.node_type.clone().unwrap_or_default(),
                    fields: if purged {
                        Map::new()
                    } else {
                        e.body.as_ref().map(fields_of).unwrap_or_default()
                    },
                    purged,
                    provenance: e.provenance.clone(),
                    review_state: e.review_state.clone(),
                    ..Node::default()
                });
                // The same claim added again (another engine) keeps the
                // first text; only the sequence moves.
                node.last_seq = e.seq;
            }
            "field.set" => {
                if let (Some(node), Some(body)) = (self.nodes.get_mut(&target), &e.body)
                    && !node.purged
                    && let Some(field) = body.get("field").and_then(Value::as_str)
                {
                    node.fields.insert(
                        field.to_string(),
                        body.get("value").cloned().unwrap_or(Value::Null),
                    );
                    node.last_seq = e.seq;
                }
            }
            "edge.add" => {
                if let Some(body) = &e.body {
                    let s = |k: &str| {
                        body.get(k)
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string()
                    };
                    let edge = self.edges.entry(target).or_insert_with(|| Edge {
                        kind: s("kind"),
                        from: s("from"),
                        to: s("to"),
                        ..Edge::default()
                    });
                    edge.last_seq = e.seq;
                }
            }
            "retract" | "restore" => {
                let retracted = e.op == "retract";
                if let Some(node) = self.nodes.get_mut(&target) {
                    node.retracted = retracted;
                    node.last_seq = e.seq;
                } else if let Some(edge) = self.edges.get_mut(&target) {
                    edge.retracted = retracted;
                    edge.last_seq = e.seq;
                }
            }
            "purge" => self.apply_purge(e)?,
            _ => {}
        }
        Ok(())
    }

    fn apply_purge(&mut self, e: &FoldEvent) -> Result<(), FoldError> {
        let Some(body) = &e.body else {
            return Ok(());
        };
        let reason = body
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if !super::store::PURGE_REASONS.contains(&reason.as_str()) {
            return Err(FoldError::PurgeReasonInvalid(reason));
        }
        let purge = Purge {
            purge_id: body
                .get("purge_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            reason,
        };
        self.tombstones.insert((purge.purge_id.clone(), e.seq));
        let ids = ["targets", "cascade"]
            .iter()
            .filter_map(|k| body.get(*k).and_then(Value::as_array))
            .flatten()
            .filter_map(Value::as_str);
        for id in ids {
            self.purged_events.insert(id.to_string());
            if let Some(node_id) = self.added_by.get(id)
                && let Some(node) = self.nodes.get_mut(node_id)
            {
                node.fields.clear();
                node.purged = true;
                self.purged_nodes.insert(node_id.clone(), purge.clone());
            }
        }
        Ok(())
    }

    /// Nodes an agent may be given: not retracted, not purged.
    pub fn live_nodes(&self) -> impl Iterator<Item = (&String, &Node)> {
        self.nodes.iter().filter(|(_, n)| !n.retracted && !n.purged)
    }

    /// A digest of the graph two engines must agree on once they folded the
    /// same log: every node (type, fields, state) and edge in id order, as
    /// canonical JSON. Local server-field views (provenance, review state)
    /// are left out; they differ per reader.
    pub fn digest(&self) -> String {
        let mut h = blake3::Hasher::new();
        for (id, n) in &self.nodes {
            let line = serde_json::json!([id, n.node_type, n.fields, n.retracted, n.purged]);
            if let Ok(bytes) = lean_ctx_protocol::hive::canonical_json(&line) {
                h.update(&bytes);
                h.update(b"\n");
            }
        }
        for (id, e) in &self.edges {
            let line = serde_json::json!([id, e.kind, e.from, e.to, e.retracted]);
            if let Ok(bytes) = lean_ctx_protocol::hive::canonical_json(&line) {
                h.update(&bytes);
                h.update(b"\n");
            }
        }
        h.finalize().to_hex()[..16].to_string()
    }
}

#[cfg(test)]
#[path = "fold_tests.rs"]
mod tests;
