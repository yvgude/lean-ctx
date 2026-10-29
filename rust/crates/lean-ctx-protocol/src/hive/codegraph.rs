// SPDX-License-Identifier: Apache-2.0

//! The published code graph (C0 1.9, section 8; plan N1).
//!
//! By default Hive knows code only through fact anchors (`code_graph`
//! `anchors`, section 3 `GraphV1` derives code nodes from them). The vault
//! owner may publish the full graph (`VaultV1.code_graph` `full`, set on
//! the web). Then an engine sends it on an explicit publish, or after a pull
//! that shows the setting changed:
//! `PUT /v1/vaults/{vault_id}/code-graph` [`CodeGraphPutV1`] →
//! [`CodeGraphAckV1`] (right `knowledge.draft`). With the setting at
//! `anchors` that is `409 invalid_state` reason `code_graph_disabled`.
//! Every upload replaces the vault's graph; Hive records it in the audit
//! log with the publishing instance, and shows who published the current
//! graph.
//!
//! The graph names files and symbols and how they relate, nothing else: no
//! source text, no bodies, no digests (a hash of short code can be guessed).
//! Paths follow the anchor rules and pass the same admission as anchors on
//! the engine (restricted or secret files are dropped from the graph).
//!
//! A sealed vault sends the same [`CodeGraphV1`] only as `sealed`
//! (content `leanctx.hive.code-graph/v1`, AAD
//! `vault_id|0|epoch|content_schema`). Hive stores it opaquely; its
//! `GraphV1` stays `409 sealed` and engines merge the graph locally.

use serde::{Deserialize, Serialize};

use super::anchor::validate_anchor_path;
use super::sealed::SealedBlobV1;
use super::{require_schema, validate_text};
use crate::ValidationError;

pub const CODE_GRAPH_SCHEMA: &str = "leanctx.hive.code-graph/v1";
pub const CODE_GRAPH_PUT_SCHEMA: &str = "leanctx.hive.code-graph-put/v1";
pub const CODE_GRAPH_ACK_SCHEMA: &str = "leanctx.hive.code-graph-ack/v1";
pub const MAX_CODE_GRAPH_NODES: usize = super::graph::MAX_GRAPH_NODES;
pub const MAX_CODE_GRAPH_EDGES: usize = super::graph::MAX_GRAPH_EDGES;
/// Characters of a code node id, as for `GraphV1` node ids.
pub const MAX_CODE_NODE_ID_CHARS: usize = 700;

/// `VaultV1.code_graph`; absent means `anchors`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeGraphMode {
    Anchors,
    Full,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeNodeKind {
    File,
    Symbol,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeEdgeKind {
    /// file → symbol, symbol → nested symbol.
    Contains,
    /// file → file.
    Imports,
    /// symbol → symbol.
    Calls,
    #[serde(other)]
    Unknown,
}

/// `code:<path>` (a file) or `code:<path>#<symbol>`, as in `GraphV1`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodeNodeV1 {
    pub id: String,
    pub kind: CodeNodeKind,
    /// Tree-sitter language id, a hint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
    /// `fn`, `type`, `trait`, `const`, … for symbols, a hint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol_kind: Option<String>,
}

impl CodeNodeV1 {
    fn validate(&self) -> Result<(), ValidationError> {
        if self.id.chars().count() > MAX_CODE_NODE_ID_CHARS {
            return Err(ValidationError::new(format!(
                "a code node id has at most {MAX_CODE_NODE_ID_CHARS} characters"
            )));
        }
        let rest = self
            .id
            .strip_prefix("code:")
            .ok_or_else(|| ValidationError::new("code node ids start with code:"))?;
        let (path, symbol) = match rest.split_once('#') {
            Some((path, symbol)) => (path, Some(symbol)),
            None => (rest, None),
        };
        validate_anchor_path(path)?;
        match (self.kind, symbol) {
            (CodeNodeKind::File, None) => {}
            (CodeNodeKind::Symbol, Some(symbol)) => {
                validate_text(
                    symbol,
                    super::anchor::MAX_ANCHOR_SYMBOL_CHARS,
                    "symbol",
                    false,
                )?;
            }
            _ => {
                return Err(ValidationError::new(
                    "a file node has no #symbol and a symbol node has one",
                ));
            }
        }
        for (hint, field) in [(&self.lang, "lang"), (&self.symbol_kind, "symbol_kind")] {
            if let Some(hint) = hint
                && (hint.is_empty()
                    || hint.len() > 32
                    || !hint
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'))
            {
                return Err(ValidationError::new(format!(
                    "{field} is 1-32 characters of [a-z0-9_]"
                )));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodeEdgeV1 {
    pub from: String,
    pub to: String,
    #[serde(rename = "type")]
    pub kind: CodeEdgeKind,
}

/// The engine's code graph of one repo (also the sealed content).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodeGraphV1 {
    pub schema: String,
    /// The git commit the graph was built from (40 or 64 lowercase hex).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision_hint: Option<String>,
    /// Sorted by `id`, without repeats.
    pub nodes: Vec<CodeNodeV1>,
    /// Sorted by `(from, to, type)`, without repeats; both ends are nodes.
    pub edges: Vec<CodeEdgeV1>,
}

impl CodeGraphV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, CODE_GRAPH_SCHEMA)?;
        if let Some(hint) = &self.revision_hint
            && !(matches!(hint.len(), 40 | 64)
                && hint
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
        {
            return Err(ValidationError::new("revision_hint is a git commit id"));
        }
        if self.nodes.len() > MAX_CODE_GRAPH_NODES || self.edges.len() > MAX_CODE_GRAPH_EDGES {
            return Err(ValidationError::new(format!(
                "a code graph has at most {MAX_CODE_GRAPH_NODES} nodes and {MAX_CODE_GRAPH_EDGES} edges"
            )));
        }
        for (i, node) in self.nodes.iter().enumerate() {
            node.validate()?;
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
            if edge.kind == CodeEdgeKind::Unknown {
                return Err(ValidationError::new("unknown edge type"));
            }
            if !known(&edge.from) || !known(&edge.to) {
                return Err(ValidationError::new("an edge names a missing node"));
            }
            if i > 0 {
                let prev = &self.edges[i - 1];
                if (&prev.from, &prev.to, prev.kind) >= (&edge.from, &edge.to, edge.kind) {
                    return Err(ValidationError::new("edges must be sorted without repeats"));
                }
            }
        }
        Ok(())
    }
}

/// `PUT …/code-graph`: exactly one of `graph` (clear) and `sealed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodeGraphPutV1 {
    pub schema: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph: Option<CodeGraphV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sealed: Option<SealedBlobV1>,
}

impl CodeGraphPutV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, CODE_GRAPH_PUT_SCHEMA)?;
        match (&self.graph, &self.sealed) {
            (Some(graph), None) => graph.validate(),
            (None, Some(sealed)) => sealed.validate(),
            _ => Err(ValidationError::new(
                "a code graph upload carries exactly one of graph and sealed",
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeGraphAckV1 {
    pub schema: String,
    /// Stored counts; 0 for a sealed upload (Hive cannot count it).
    pub nodes: u64,
    pub edges: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision_hint: Option<String>,
}
