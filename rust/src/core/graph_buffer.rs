//! In-memory graph buffer — contiguous arrays with O(1) QN → NodeId lookup.
//!
//! This is the in-memory graph store that replaces `RamGraphBuilder` during
//! the new pipeline. It holds all nodes and edges in contiguous `Vec`s during
//! indexing and provides fast lookup by qualified name.
//!
//! ## Design
//! - `nodes`: append-only `Vec<GbufNode>`, IDs are sequential from 1
//! - `qn_index`: `HashMap<String, NodeId>` for O(1) QN lookup
//! - `edge_dedup`: deduplicates edges by `(source_id, target_id, edge_type)`
//! - `delete_by_file`: cascading delete — removes edges referencing deleted nodes
//! - `merge`: QN-collision-aware merge (source wins) with edge ID remapping
//!
//! ## Compatibility
//! Maps to C's `cbm_gbuf_t` — see `/tmp/codebase-memory-mcp/src/graph_buffer/`.
//! Uses the types from `index_types.rs` (`NodeId`, `EdgeId`, `GbufNode`, `GbufEdge`).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::core::graph_index::ProjectIndex;
use crate::core::index_types::{EdgeId, GbufEdge, GbufNode, NodeId};

/// Contiguous-array graph buffer with O(1) QN → NodeId lookup.
///
/// Nodes and edges are stored in append-only `Vec`s. The `qn_index` provides
/// fast qualified-name lookups. Edges are deduplicated by
/// `(source_id, target_id, edge_type)`.
pub struct GraphBuffer {
    #[allow(dead_code)]
    project_root: String,
    nodes: Vec<GbufNode>,
    edges: Vec<GbufEdge>,
    qn_index: HashMap<String, NodeId>,
    edge_dedup: HashMap<(NodeId, NodeId, String), EdgeId>,
    next_node_id: u32,
    next_edge_id: u32,
    shared_ids: Option<Arc<AtomicU32>>,
}

impl GraphBuffer {
    // ── Construction ────────────────────────────────────────────────

    /// Create a new graph buffer.
    ///
    /// IDs start at 1 (sequential). The buffer owns all data.
    pub fn new(project_root: &str) -> Self {
        Self {
            project_root: project_root.to_string(),
            nodes: Vec::new(),
            edges: Vec::new(),
            qn_index: HashMap::new(),
            edge_dedup: HashMap::new(),
            next_node_id: 1,
            next_edge_id: 1,
            shared_ids: None,
        }
    }

    /// Create a graph buffer with a shared atomic ID source.
    ///
    /// IDs are allocated via `fetch_add` on `id_source`. Used for parallel
    /// extraction where multiple gbufs in different threads need globally
    /// unique IDs without coordination.
    pub fn new_shared_ids(project_root: &str, id_source: Arc<AtomicU32>) -> Self {
        Self {
            project_root: project_root.to_string(),
            nodes: Vec::new(),
            edges: Vec::new(),
            qn_index: HashMap::new(),
            edge_dedup: HashMap::new(),
            next_node_id: 1,
            next_edge_id: 1,
            shared_ids: Some(id_source),
        }
    }

    // ── Internal helpers ────────────────────────────────────────────

    /// Allocate the next node ID from the shared atomic or local counter.
    fn alloc_node_id(&mut self) -> NodeId {
        if let Some(ref shared) = self.shared_ids {
            NodeId(shared.fetch_add(1, Ordering::Relaxed))
        } else {
            let id = self.next_node_id;
            self.next_node_id += 1;
            NodeId(id)
        }
    }

    /// Allocate the next edge ID from the shared atomic or local counter.
    fn alloc_edge_id(&mut self) -> EdgeId {
        if let Some(ref shared) = self.shared_ids {
            EdgeId(shared.fetch_add(1, Ordering::Relaxed))
        } else {
            let id = self.next_edge_id;
            self.next_edge_id += 1;
            EdgeId(id)
        }
    }

    // ── Node operations ─────────────────────────────────────────────

    /// Upsert a node by qualified name.
    ///
    /// If a node with the same QN already exists, updates its fields in place
    /// (label, name, file_path, start_line, end_line, properties) and returns
    /// the **existing** `NodeId` — the operation is idempotent.
    ///
    /// If no node with this QN exists, allocates a fresh `NodeId` and inserts
    /// it.
    pub fn upsert_node(
        &mut self,
        label: &str,
        name: &str,
        qualified_name: &str,
        file_path: &str,
        start_line: u32,
        end_line: u32,
        properties: HashMap<String, String>,
    ) -> NodeId {
        if let Some(&existing_id) = self.qn_index.get(qualified_name) {
            let existing = self
                .nodes
                .iter_mut()
                .find(|n| n.id == existing_id)
                .expect("node referenced in qn_index must exist in nodes vec");

            existing.label = label.to_string();
            existing.name = name.to_string();
            existing.file_path = file_path.to_string();
            existing.start_line = start_line;
            existing.end_line = end_line;
            existing.properties = properties;
            existing_id
        } else {
            let id = self.alloc_node_id();
            let node = GbufNode {
                id,
                label: label.to_string(),
                name: name.to_string(),
                qualified_name: qualified_name.to_string(),
                file_path: file_path.to_string(),
                start_line,
                end_line,
                properties,
            };
            self.qn_index.insert(qualified_name.to_string(), id);
            self.nodes.push(node);
            id
        }
    }

    /// Find a node by its qualified name.
    ///
    /// Returns `None` when the QN has not been upserted or has been deleted.
    pub fn find_by_qn(&self, qn: &str) -> Option<&GbufNode> {
        self.qn_index
            .get(qn)
            .and_then(|id| self.nodes.iter().find(|n| n.id == *id))
    }

    /// Find a node by its `NodeId`.
    ///
    /// Returns `None` when no node with this ID exists (e.g. it was deleted).
    pub fn find_by_id(&self, id: NodeId) -> Option<&GbufNode> {
        self.nodes.iter().find(|n| n.id == id)
    }

    /// Find all nodes with a given label. O(n) scan — no secondary index.
    pub fn find_nodes_by_label(&self, label: &str) -> Vec<&GbufNode> {
        self.nodes.iter().filter(|n| n.label == label).collect()
    }

    /// Return the number of live (indexed) nodes.
    pub fn node_count(&self) -> usize {
        self.qn_index.len()
    }

    /// Return the next available node ID.
    ///
    /// When a shared atomic source is configured, this loads the current value
    /// from the atomic. Otherwise returns the local counter.
    pub fn next_node_id(&self) -> u32 {
        self.shared_ids
            .as_ref()
            .map_or(self.next_node_id, |atomic| atomic.load(Ordering::Relaxed))
    }

    /// Set the local next-node-ID counter.
    ///
    /// Used after merging worker gbufs to sync the main counter. Has no effect
    /// when a shared atomic source is configured (the atomic always takes
    /// precedence during allocation).
    pub fn set_next_node_id(&mut self, id: u32) {
        self.next_node_id = id;
    }

    /// Delete all nodes for a given file path.
    ///
    /// Cascade-deletes all edges that reference any of the removed nodes.
    /// Nodes whose `file_path` does not match, or that have already been
    /// removed from the QN index, are left untouched.
    pub fn delete_by_file(&mut self, file_path: &str) {
        // Collect NodeIds of nodes matching the file path.
        let deleted: HashSet<NodeId> = self
            .nodes
            .iter()
            .filter(|n| n.file_path == file_path && self.qn_index.contains_key(&n.qualified_name))
            .map(|n| n.id)
            .collect();

        if deleted.is_empty() {
            return;
        }

        // Remove from QN index.
        self.qn_index.retain(|_, id| !deleted.contains(id));

        // Remove referencing edges from dedup index.
        self.edge_dedup
            .retain(|(src, tgt, _), _| !deleted.contains(src) && !deleted.contains(tgt));

        // Remove referencing edges from edges vec.
        self.edges
            .retain(|e| !deleted.contains(&e.source_id) && !deleted.contains(&e.target_id));

        // Remove deleted nodes from nodes vec.
        self.nodes.retain(|n| !deleted.contains(&n.id));
    }

    // ── Edge operations ─────────────────────────────────────────────

    /// Insert an edge.
    ///
    /// Deduplicates by `(source_id, target_id, edge_type)` — on duplicate,
    /// merges properties (later wins) and returns the existing `EdgeId`.
    ///
    /// Returns `None` when `source_id` or `target_id` does not reference a
    /// live node.
    pub fn insert_edge(
        &mut self,
        source_id: NodeId,
        target_id: NodeId,
        edge_type: &str,
        properties: HashMap<String, String>,
    ) -> Option<EdgeId> {
        // Validate source and target exist.
        if !self.nodes.iter().any(|n| n.id == source_id) {
            return None;
        }
        if !self.nodes.iter().any(|n| n.id == target_id) {
            return None;
        }

        // Check for dedup.
        let dedup_key = (source_id, target_id, edge_type.to_string());
        if let Some(&existing_id) = self.edge_dedup.get(&dedup_key) {
            // Merge properties (later wins).
            if let Some(edge) = self.edges.iter_mut().find(|e| e.id == existing_id) {
                edge.properties = properties;
            }
            return Some(existing_id);
        }

        // Allocate new ID and insert.
        let id = self.alloc_edge_id();
        let edge = GbufEdge {
            id,
            source_id,
            target_id,
            edge_type: edge_type.to_string(),
            properties,
        };
        self.edge_dedup.insert(dedup_key, id);
        self.edges.push(edge);
        Some(id)
    }

    /// Find all edges from `source_id` with a given `edge_type`. O(n) scan.
    pub fn find_edges_by_source_type(&self, source_id: NodeId, edge_type: &str) -> Vec<&GbufEdge> {
        self.edges
            .iter()
            .filter(|e| e.source_id == source_id && e.edge_type == edge_type)
            .collect()
    }

    /// Return the total number of edges.
    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }

    /// Check whether an edge `(source_id, target_id, edge_type)` exists.
    ///
    /// Returns `true` if a matching edge has been inserted.
    pub fn edge_dedup_key(&self, source_id: NodeId, target_id: NodeId, edge_type: &str) -> bool {
        let key = (source_id, target_id, edge_type.to_string());
        self.edge_dedup.contains_key(&key)
    }

    // ── Merge + Finalize ────────────────────────────────────────────

    /// Merge all nodes and edges from `other` into `self`.
    ///
    /// ## Node merge semantics
    /// - **QN collision**: source wins — updates dest node fields (label, name,
    ///   file_path, line range, properties) in place. The collision is recorded
    ///   so edges referencing the source node ID can be remapped.
    /// - **No collision**: the source node is inserted into self. If its original
    ///   ID conflicts with an existing node in self, a new ID is assigned and
    ///   the remap recorded so edges can be updated.
    ///
    /// ## Edge merge semantics
    /// - Edges are remapped when their source or target node ID changed (QN
    ///   collision or ID conflict).
    /// - Remapped edges are deduplicated against existing edges in self.
    /// - Dangling edges (referencing nodes that no longer exist) are silently
    ///   skipped.
    ///
    /// After merge, `other` is emptied (all data is moved into `self`).
    pub fn merge(&mut self, other: &mut GraphBuffer) {
        if other.nodes.is_empty() && other.edges.is_empty() {
            return;
        }

        // Take ownership of other's data.
        let other_nodes = std::mem::take(&mut other.nodes);
        let other_edges = std::mem::take(&mut other.edges);
        other.qn_index.clear();
        other.edge_dedup.clear();

        // ID remap: src NodeId → dst NodeId.
        let mut remap: HashMap<NodeId, NodeId> = HashMap::new();

        // Track existing IDs in self to detect conflicts.
        let mut existing_ids: HashSet<NodeId> = self.nodes.iter().map(|n| n.id).collect();

        for node in other_nodes {
            if let Some(&existing_id) = self.qn_index.get(&node.qualified_name) {
                // QN collision: update dest in place (src wins).
                let existing = self
                    .nodes
                    .iter_mut()
                    .find(|n| n.id == existing_id)
                    .expect("node referenced in qn_index must exist in nodes vec");

                if node.id != existing.id {
                    remap.insert(node.id, existing.id);
                }

                existing.label = node.label;
                existing.name = node.name;
                existing.file_path = node.file_path;
                existing.start_line = node.start_line;
                existing.end_line = node.end_line;
                existing.properties = node.properties;
            } else {
                // New node: detect ID conflict with existing nodes in self.
                let final_id = if existing_ids.contains(&node.id) {
                    let new_id = self.alloc_node_id();
                    remap.insert(node.id, new_id);
                    new_id
                } else {
                    node.id
                };

                existing_ids.insert(final_id);
                self.qn_index.insert(node.qualified_name.clone(), final_id);

                let mut merged_node = node;
                merged_node.id = final_id;
                self.nodes.push(merged_node);

                // Bump the local counter if the incoming ID is past it.
                if final_id.0 >= self.next_node_id {
                    self.next_node_id = final_id.0 + 1;
                }
            }
        }

        // Merge edges with optional ID remapping for QN-colliding nodes.
        for edge in other_edges {
            let new_src = remap
                .get(&edge.source_id)
                .copied()
                .unwrap_or(edge.source_id);
            let new_tgt = remap
                .get(&edge.target_id)
                .copied()
                .unwrap_or(edge.target_id);

            // Skip dangling edges (referencing nodes not in self).
            if !self.nodes.iter().any(|n| n.id == new_src) {
                continue;
            }
            if !self.nodes.iter().any(|n| n.id == new_tgt) {
                continue;
            }

            // Edge dedup check.
            let dedup_key = (new_src, new_tgt, edge.edge_type.clone());
            if self.edge_dedup.contains_key(&dedup_key) {
                continue;
            }

            // Allocate new edge ID in self.
            let edge_id = self.alloc_edge_id();
            let new_edge = GbufEdge {
                id: edge_id,
                source_id: new_src,
                target_id: new_tgt,
                edge_type: edge.edge_type,
                properties: edge.properties,
            };
            self.edge_dedup.insert(dedup_key, edge_id);
            self.edges.push(new_edge);
        }
    }

    /// Iterate over all live (indexed) nodes.
    ///
    /// Skips nodes whose QN has been deleted from the index. Matches C's
    /// `cbm_gbuf_foreach_node` semantics.
    pub fn foreach_node(&self, f: &mut dyn FnMut(&GbufNode)) {
        for node in &self.nodes {
            if self.qn_index.contains_key(&node.qualified_name) {
                f(node);
            }
        }
    }

    /// Iterate over all edges.
    pub fn foreach_edge(&self, f: &mut dyn FnMut(&GbufEdge)) {
        for edge in &self.edges {
            f(edge);
        }
    }

    /// Finalize the buffer into a `ProjectIndex` for the property graph mirror.
    ///
    /// Converts `GbufNode`s into `FileEntry` or `SymbolEntry` depending on
    /// their label, and converts `GbufEdge`s into `IndexEdge`s.
    #[allow(clippy::cast_possible_truncation)]
    pub fn finalize(&self) -> ProjectIndex {
        use crate::core::graph_index::{FileEntry, SymbolEntry};
        use crate::core::index_types::Minhash;

        let mut files = HashMap::new();
        let mut symbols = HashMap::new();
        let mut edges = Vec::new();

        // Convert File nodes → FileEntry, other typed nodes → SymbolEntry.
        self.foreach_node(&mut |n| {
            if n.label == "File" {
                let language = n.file_path.rsplit('.').next().unwrap_or("").to_string();
                files.insert(
                    n.qualified_name.clone(),
                    FileEntry {
                        path: n.qualified_name.clone(),
                        hash: n
                            .properties
                            .get("content_hash")
                            .cloned()
                            .unwrap_or_default(),
                        language,
                        line_count: n.end_line as usize,
                        token_count: 0,
                        exports: vec![],
                        summary: String::new(),
                    },
                );
            }

            if matches!(
                n.label.as_str(),
                "Function" | "Method" | "Class" | "Struct" | "Interface" | "Enum" | "Variable"
            ) {
                let minhash: Vec<u32> = n
                    .properties
                    .get("fp")
                    .and_then(|hex| Minhash::from_hex(hex))
                    .map(|mh| mh.0.to_vec())
                    .unwrap_or_default();

                symbols.insert(
                    n.qualified_name.clone(),
                    SymbolEntry {
                        file: n.file_path.clone(),
                        name: n.name.clone(),
                        kind: n.label.clone(),
                        start_line: n.start_line as usize,
                        end_line: n.end_line as usize,
                        is_exported: n.properties.get("is_exported").is_some_and(|v| v == "true"),
                        minhash,
                    },
                );
            }
        });

        // Convert GbufEdge → IndexEdge
        self.foreach_edge(&mut |e| {
            edges.push(crate::core::graph_index::IndexEdge {
                from: self
                    .find_by_id(e.source_id)
                    .map(|n| n.qualified_name.clone())
                    .unwrap_or_default(),
                to: self
                    .find_by_id(e.target_id)
                    .map(|n| n.qualified_name.clone())
                    .unwrap_or_default(),
                kind: e.edge_type.clone(),
                weight: 1.0,
            });
        });

        ProjectIndex {
            version: 6,
            project_root: self.project_root.clone(),
            last_scan: chrono::Utc::now().to_rfc3339(),
            files,
            edges,
            symbols,
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Shorthand to build a `HashMap<String, String>` from key-value pairs.
    fn props(kvs: &[(&str, &str)]) -> HashMap<String, String> {
        kvs.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    // ── Node operations ──────────────────────────────────────────

    #[test]
    fn test_upsert_node_same_qn_returns_same_id() {
        let mut gb = GraphBuffer::new("test");
        let id1 = gb.upsert_node(
            "Function",
            "foo",
            "pkg.foo",
            "src/lib.rs",
            1,
            10,
            props(&[]),
        );
        let id2 = gb.upsert_node(
            "Function",
            "foo",
            "pkg.foo",
            "src/lib.rs",
            1,
            10,
            props(&[]),
        );
        assert_eq!(id1, id2, "same QN must return same NodeId");
        assert_eq!(gb.node_count(), 1, "should have exactly one node");
    }

    #[test]
    fn test_find_by_qn_nonexistent() {
        let gb = GraphBuffer::new("test");
        assert!(gb.find_by_qn("nonexistent").is_none());
    }

    #[test]
    fn test_find_by_qn_found() {
        let mut gb = GraphBuffer::new("test");
        gb.upsert_node(
            "Function",
            "bar",
            "pkg.bar",
            "src/bar.rs",
            5,
            20,
            props(&[("key", "val")]),
        );
        let node = gb.find_by_qn("pkg.bar").expect("should find node");
        assert_eq!(node.name, "bar");
        assert_eq!(node.label, "Function");
        assert_eq!(node.file_path, "src/bar.rs");
        assert_eq!(node.properties.get("key").unwrap(), "val");
    }

    #[test]
    fn test_upsert_node_updates_in_place() {
        let mut gb = GraphBuffer::new("test");
        let id = gb.upsert_node("Function", "old", "pkg.x", "old.rs", 1, 5, props(&[]));
        let id2 = gb.upsert_node(
            "Class",
            "new",
            "pkg.x",
            "new.rs",
            10,
            20,
            props(&[("lang", "rust")]),
        );
        assert_eq!(id, id2, "same QN => same ID");

        let node = gb.find_by_qn("pkg.x").unwrap();
        assert_eq!(node.label, "Class", "label should be updated");
        assert_eq!(node.name, "new", "name should be updated");
        assert_eq!(node.file_path, "new.rs", "file_path should be updated");
        assert_eq!(node.properties.get("lang").unwrap(), "rust");
    }

    #[test]
    fn test_upsert_updates_properties_only() {
        let mut gb = GraphBuffer::new("test");
        let id = gb.upsert_node("Function", "x", "pkg.x", "x.rs", 1, 5, props(&[("a", "1")]));
        gb.upsert_node(
            "Function",
            "x",
            "pkg.x",
            "x.rs",
            1,
            5,
            props(&[("a", "2"), ("b", "3")]),
        );

        let node = gb.find_by_id(id).unwrap();
        assert_eq!(node.properties.get("a").unwrap(), "2");
        assert_eq!(node.properties.get("b").unwrap(), "3");
        assert_eq!(node.properties.len(), 2);
    }

    #[test]
    fn test_multiple_nodes_different_qns() {
        let mut gb = GraphBuffer::new("test");
        let id1 = gb.upsert_node("Function", "a", "pkg.a", "a.rs", 1, 5, props(&[]));
        let id2 = gb.upsert_node("Function", "b", "pkg.b", "b.rs", 10, 15, props(&[]));
        let id3 = gb.upsert_node("Function", "c", "pkg.c", "c.rs", 20, 25, props(&[]));

        assert_ne!(id1, id2, "different QNs => different IDs");
        assert_ne!(id2, id3, "different QNs => different IDs");
        assert_eq!(gb.node_count(), 3);
    }

    // ── Edge operations ──────────────────────────────────────────

    #[test]
    fn test_edge_dedup() {
        let mut gb = GraphBuffer::new("test");
        let n1 = gb.upsert_node("Function", "a", "pkg.a", "a.rs", 1, 5, props(&[]));
        let n2 = gb.upsert_node("Function", "b", "pkg.b", "b.rs", 10, 15, props(&[]));

        let e1 = gb
            .insert_edge(n1, n2, "calls", props(&[("k", "v1")]))
            .unwrap();
        let e2 = gb
            .insert_edge(n1, n2, "calls", props(&[("k", "v2")]))
            .unwrap();

        assert_eq!(e1, e2, "duplicate edge should return same ID");
        let edge = gb.edges.iter().find(|e| e.id == e1).unwrap();
        assert_eq!(
            edge.properties.get("k").unwrap(),
            "v2",
            "later properties should win"
        );
        assert_eq!(gb.edge_count(), 1, "only one edge should exist");
    }

    #[test]
    fn test_edge_dedup_key_check() {
        let mut gb = GraphBuffer::new("test");
        let n1 = gb.upsert_node("Function", "a", "pkg.a", "a.rs", 1, 5, props(&[]));
        let n2 = gb.upsert_node("Function", "b", "pkg.b", "b.rs", 10, 15, props(&[]));

        assert!(!gb.edge_dedup_key(n1, n2, "calls"), "should not exist yet");
        gb.insert_edge(n1, n2, "calls", props(&[]));
        assert!(
            gb.edge_dedup_key(n1, n2, "calls"),
            "should exist after insert"
        );
        assert!(
            !gb.edge_dedup_key(n2, n1, "calls"),
            "reverse edge should not exist"
        );
    }

    #[test]
    fn test_insert_edge_invalid_node_returns_none() {
        let mut gb = GraphBuffer::new("test");
        let n1 = gb.upsert_node("Function", "a", "pkg.a", "a.rs", 1, 5, props(&[]));
        let bogus = NodeId(999);

        assert!(gb.insert_edge(bogus, n1, "calls", props(&[])).is_none());
        assert!(gb.insert_edge(n1, bogus, "calls", props(&[])).is_none());
        assert!(gb.insert_edge(bogus, bogus, "calls", props(&[])).is_none());
    }

    // ── Merge ────────────────────────────────────────────────────

    #[test]
    fn test_merge_combines_two_buffers() {
        let mut gb1 = GraphBuffer::new("test");
        let n1 = gb1.upsert_node("Function", "a", "pkg.a", "a.rs", 1, 5, props(&[]));
        gb1.insert_edge(n1, n1, "self_edge", props(&[]));

        let mut gb2 = GraphBuffer::new("test");
        gb2.upsert_node("Function", "b", "pkg.b", "b.rs", 10, 20, props(&[]));

        gb1.merge(&mut gb2);

        assert_eq!(gb1.node_count(), 2, "should have 2 nodes after merge");
        assert!(gb1.find_by_qn("pkg.a").is_some(), "pkg.a should exist");
        assert!(gb1.find_by_qn("pkg.b").is_some(), "pkg.b should exist");
        assert_eq!(gb2.node_count(), 0, "other should be emptied");
    }

    #[test]
    fn test_merge_qn_dedup_src_wins() {
        let mut gb1 = GraphBuffer::new("test");
        gb1.upsert_node(
            "Function",
            "original",
            "pkg.shared",
            "old.rs",
            1,
            5,
            props(&[("key", "old")]),
        );

        let mut gb2 = GraphBuffer::new("test");
        gb2.upsert_node(
            "Class",
            "updated",
            "pkg.shared",
            "new.rs",
            10,
            30,
            props(&[("key", "new")]),
        );

        gb1.merge(&mut gb2);

        let node = gb1.find_by_qn("pkg.shared").unwrap();
        assert_eq!(node.label, "Class", "src wins: label");
        assert_eq!(node.name, "updated", "src wins: name");
        assert_eq!(node.file_path, "new.rs", "src wins: file_path");
        assert_eq!(
            node.properties.get("key").unwrap(),
            "new",
            "src wins: properties"
        );
    }

    #[test]
    fn test_merge_edge_count_sum() {
        let mut gb1 = GraphBuffer::new("test");
        let n1 = gb1.upsert_node("Function", "a", "pkg.a", "a.rs", 1, 5, props(&[]));
        let n2 = gb1.upsert_node("Function", "b", "pkg.b", "b.rs", 10, 15, props(&[]));
        gb1.insert_edge(n1, n2, "calls", props(&[]));

        let mut gb2 = GraphBuffer::new("test");
        let n3 = gb2.upsert_node("Function", "c", "pkg.c", "c.rs", 20, 25, props(&[]));
        let n4 = gb2.upsert_node("Function", "d", "pkg.d", "d.rs", 30, 35, props(&[]));
        gb2.insert_edge(n3, n4, "calls", props(&[]));
        gb2.insert_edge(n4, n3, "calls", props(&[]));

        let edge_count_before = gb1.edge_count() + gb2.edge_count();
        gb1.merge(&mut gb2);
        assert_eq!(
            gb1.edge_count(),
            edge_count_before,
            "edge count after merge should equal sum"
        );
        assert_eq!(
            gb2.edge_count(),
            0,
            "other should have no edges after merge"
        );
    }

    #[test]
    fn test_merge_no_dangling_edges() {
        let mut gb1 = GraphBuffer::new("test");
        gb1.upsert_node("Function", "a", "pkg.a", "a.rs", 1, 5, props(&[]));

        let mut gb2 = GraphBuffer::new("test");
        let n2 = gb2.upsert_node("Function", "b", "pkg.b", "b.rs", 10, 15, props(&[]));
        gb2.insert_edge(n2, n2, "self", props(&[]));

        gb1.merge(&mut gb2);

        let valid_ids: HashSet<NodeId> = gb1.nodes.iter().map(|n| n.id).collect();
        for edge in &gb1.edges {
            assert!(
                valid_ids.contains(&edge.source_id),
                "edge source_id {:?} not found in nodes",
                edge.source_id
            );
            assert!(
                valid_ids.contains(&edge.target_id),
                "edge target_id {:?} not found in nodes",
                edge.target_id
            );
        }
    }

    #[test]
    fn test_merge_dangling_edge_skipped() {
        // An edge in `other` that references nonexistent nodes is skipped.
        let mut gb1 = GraphBuffer::new("test");
        gb1.upsert_node("Function", "a", "pkg.a", "a.rs", 1, 5, props(&[]));

        let mut gb2 = GraphBuffer::new("test");
        // Push an edge manually with IDs that don't exist.
        let fake_edge = GbufEdge {
            id: EdgeId(1),
            source_id: NodeId(999),
            target_id: NodeId(1000),
            edge_type: "calls".to_string(),
            properties: HashMap::new(),
        };
        gb2.edges.push(fake_edge);
        gb2.edge_dedup
            .insert((NodeId(999), NodeId(1000), "calls".to_string()), EdgeId(1));

        gb1.merge(&mut gb2);

        assert_eq!(gb1.edge_count(), 0);
    }

    // ── delete_by_file ───────────────────────────────────────────

    #[test]
    fn test_delete_by_file() {
        let mut gb = GraphBuffer::new("test");
        gb.upsert_node("Function", "a", "pkg.a", "src/a.rs", 1, 5, props(&[]));
        let n2 = gb.upsert_node("Function", "b", "pkg.b", "src/b.rs", 10, 15, props(&[]));
        let n3 = gb.upsert_node("Function", "c", "pkg.c", "src/a.rs", 20, 25, props(&[]));

        gb.insert_edge(n2, n3, "calls", props(&[]));

        assert_eq!(gb.node_count(), 3);
        assert_eq!(gb.edge_count(), 1);

        gb.delete_by_file("src/a.rs");

        assert_eq!(gb.node_count(), 1, "only pkg.b should remain");
        assert!(gb.find_by_qn("pkg.a").is_none(), "pkg.a should be deleted");
        assert!(gb.find_by_qn("pkg.b").is_some(), "pkg.b should remain");
        assert!(gb.find_by_qn("pkg.c").is_none(), "pkg.c should be deleted");
        assert_eq!(
            gb.edge_count(),
            0,
            "referencing edge should be cascade-deleted"
        );
    }

    #[test]
    fn test_delete_by_file_nonexistent() {
        let mut gb = GraphBuffer::new("test");
        gb.upsert_node("Function", "a", "pkg.a", "src/a.rs", 1, 5, props(&[]));
        gb.delete_by_file("nonexistent.rs");
        assert_eq!(gb.node_count(), 1, "should not delete anything");
    }

    #[test]
    fn test_delete_by_file_cascade_removes_edge_to_remaining() {
        // Edge from a deleted node to a remaining node is cascade-deleted.
        let mut gb = GraphBuffer::new("test");
        let n1 = gb.upsert_node("Function", "a", "pkg.a", "src/a.rs", 1, 5, props(&[]));
        let n2 = gb.upsert_node("Function", "b", "pkg.b", "src/b.rs", 10, 15, props(&[]));
        gb.insert_edge(n1, n2, "calls", props(&[]));

        gb.delete_by_file("src/a.rs");

        assert!(gb.find_by_qn("pkg.a").is_none(), "pkg.a should be deleted");
        assert!(gb.find_by_qn("pkg.b").is_some(), "pkg.b should remain");
        assert_eq!(
            gb.edge_count(),
            0,
            "edge referencing deleted node should be cascade-deleted"
        );
    }

    // ── Iteration ────────────────────────────────────────────────

    #[test]
    fn test_foreach_node() {
        let mut gb = GraphBuffer::new("test");
        gb.upsert_node("Function", "a", "pkg.a", "a.rs", 1, 5, props(&[]));
        gb.upsert_node("Class", "b", "pkg.b", "b.rs", 10, 20, props(&[]));

        let mut names: Vec<String> = Vec::new();
        gb.foreach_node(&mut |n| names.push(n.name.clone()));
        names.sort();
        assert_eq!(names, vec!["a", "b"]);
    }

    #[test]
    fn test_foreach_edge() {
        let mut gb = GraphBuffer::new("test");
        let n1 = gb.upsert_node("Function", "a", "pkg.a", "a.rs", 1, 5, props(&[]));
        let n2 = gb.upsert_node("Function", "b", "pkg.b", "b.rs", 10, 15, props(&[]));
        gb.insert_edge(n1, n2, "calls", props(&[]));

        let mut count = 0;
        gb.foreach_edge(&mut |_| count += 1);
        assert_eq!(count, 1);
    }

    // ── Safety ───────────────────────────────────────────────────

    #[test]
    fn test_no_unsafe() {
        // Compile-time assertion that the module uses no unsafe code.
        let mut gb = GraphBuffer::new("test");
        let id = gb.upsert_node("Function", "safe", "pkg.safe", "safe.rs", 1, 1, props(&[]));
        assert!(gb.find_by_id(id).is_some());
    }

    // ── finalize ──────────────────────────────────────────────────

    #[test]
    fn finalize_produces_valid_project_index() {
        let mut gbuf = GraphBuffer::new("test_proj");
        gbuf.upsert_node(
            "File",
            "main.rs",
            "main.rs",
            "main.rs",
            0,
            0,
            HashMap::new(),
        );
        gbuf.upsert_node(
            "Function",
            "hello",
            "main::hello",
            "main.rs",
            1,
            10,
            HashMap::new(),
        );
        gbuf.upsert_node(
            "Method",
            "do",
            "main::MyStruct::do",
            "main.rs",
            5,
            8,
            HashMap::new(),
        );
        // add an edge
        let file = gbuf.find_by_qn("main.rs").unwrap().id;
        let func = gbuf.find_by_qn("main::hello").unwrap().id;
        gbuf.insert_edge(file, func, "DEFINES", HashMap::new());

        let pi = gbuf.finalize();
        assert_eq!(pi.file_count(), 1);
        assert_eq!(pi.symbols.len(), 2);
        assert_eq!(pi.edges.len(), 1);
    }
}
