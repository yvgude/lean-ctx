// SPDX-License-Identifier: Apache-2.0

//! The vault graph view (C0 1.9, section 3).
//!
//! `GET /v1/vaults/{vault_id}/graph?revision=` → [`GraphV1`] (right
//! `knowledge.read`): one released revision's knowledge as typed nodes and
//! edges, for the web view and for engines that link it to their code graph.
//! Hive derives it from the document, the anchors, the tasks and the
//! contribution credits; nobody writes it. A sealed vault answers
//! `409 invalid_state` reason `sealed`: its engines derive the same view
//! locally after opening the revision.
//!
//! With a published code graph (section 8, `code_graph` `full`, clear
//! vaults) the code nodes and their `contains|imports|calls` edges come
//! from it as well.
//!
//! Node ids are typed so they never collide: `fact:<fact id>`,
//! `page:<page id>`, `code:<path>` or `code:<path>#<symbol>`,
//! `task:<number>`, `proposal:<number>`, `actor:<label>`.

use serde::{Deserialize, Serialize};

use super::{require_schema, validate_text};
use crate::ValidationError;

pub const GRAPH_SCHEMA: &str = "leanctx.hive.graph/v1";
pub const MAX_GRAPH_NODES: usize = 20_000;
pub const MAX_GRAPH_EDGES: usize = 60_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphNodeKind {
    /// A path, or a symbol in it (from anchors).
    Code,
    /// A fact or a page.
    Knowledge,
    /// A task or a proposal.
    Work,
    /// A person or an agent instance (by its public label).
    Actor,
    #[serde(other)]
    Unknown,
}

/// Edge types: the document's relation types between facts
/// (`knowledge::RELATION_TYPES`), plus the derived ones.
pub const GRAPH_EDGE_TYPES: [&str; 8] = [
    // fact → code: an anchor of the fact.
    "applies_to",
    // 1.9 section 8, from a published code graph: file → symbol, file →
    // file, symbol → symbol.
    "contains",
    "imports",
    "calls",
    // page → fact: the page shows the fact.
    "shows",
    // task/proposal → fact: the work added or changed the fact.
    "changed",
    // actor → task/proposal: who did the work.
    "worked_on",
    // fact → fact, task → fact: an open doubt or question about it.
    "questions",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphNodeV1 {
    pub id: String,
    pub kind: GraphNodeKind,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphEdgeV1 {
    pub from: String,
    pub to: String,
    #[serde(rename = "type")]
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphV1 {
    pub schema: String,
    pub vault_id: String,
    pub revision: u64,
    /// Sorted by `id`.
    pub nodes: Vec<GraphNodeV1>,
    /// Sorted by `(from, to, type)`; both ends are nodes.
    pub edges: Vec<GraphEdgeV1>,
}

impl GraphV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, GRAPH_SCHEMA)?;
        super::validate_vault_id(&self.vault_id)?;
        if self.nodes.len() > MAX_GRAPH_NODES || self.edges.len() > MAX_GRAPH_EDGES {
            return Err(ValidationError::new(format!(
                "a graph has at most {MAX_GRAPH_NODES} nodes and {MAX_GRAPH_EDGES} edges"
            )));
        }
        for (i, node) in self.nodes.iter().enumerate() {
            validate_text(&node.id, 700, "node id", false)?;
            validate_text(&node.label, 300, "node label", false)?;
            if i > 0 && self.nodes[i - 1].id >= node.id {
                return Err(ValidationError::new(
                    "nodes must be sorted by id without repeats",
                ));
            }
        }
        let known = |id: &str| {
            self.nodes
                .binary_search_by(|node| node.id.as_str().cmp(id))
                .is_ok()
        };
        for (i, edge) in self.edges.iter().enumerate() {
            if !known(&edge.from) || !known(&edge.to) {
                return Err(ValidationError::new("an edge names a missing node"));
            }
            if !GRAPH_EDGE_TYPES.contains(&edge.kind.as_str())
                && !super::knowledge::RELATION_TYPES.contains(&edge.kind.as_str())
            {
                return Err(ValidationError::new("unknown edge type"));
            }
            if i > 0 {
                let prev = &self.edges[i - 1];
                if (&prev.from, &prev.to, &prev.kind) >= (&edge.from, &edge.to, &edge.kind) {
                    return Err(ValidationError::new("edges must be sorted without repeats"));
                }
            }
        }
        Ok(())
    }
}
