//! Edge preservation for incremental re-indexing.
//!
//! Before purging stale nodes for changed/deleted files in an incremental
//! re-index, edges that cross the changed/unchanged boundary must be
//! snapshotted and restored after re-extraction. Otherwise, edges from
//! unchanged files into changed files would be permanently lost — the purge
//! cascade-deletes them and re-extraction only runs on changed files, so the
//! edges are never regenerated.
//!
//! ## Design
//!
//! - [`snapshot_cross_file_edges`] captures edges whose target is in a changed
//!   file and whose source is NOT (inbound cross-file edges).
//! - [`relink_edges`] resolves QN pairs against the post-re-extraction graph
//!   buffer and re-inserts them using the new `NodeId`s.
//! - QN-based lookup ensures edge stability across re-extraction.
//! - `SIMILAR_TO` and `SEMANTICALLY_RELATED` edges are skipped — they are
//!   rebuilt globally by post-passes and a stale snapshot could produce
//!   incorrect edges.
//! - Dedup is automatic via [`GraphBuffer::insert_edge`]'s internal dedup
//!   map, so relinking an edge the resolver already regenerated is a no-op.

use std::collections::{HashMap, HashSet};

use crate::core::graph_buffer::GraphBuffer;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A cross-file edge preserved as QN pairs, ready for relinking after
/// re-extraction.
///
/// Uses qualified names (stable across re-extraction) rather than `NodeId`
/// (which changes after nodes are deleted and re-created).
#[derive(Debug, Clone)]
pub struct PreservedEdge {
    pub source_qn: String,
    pub target_qn: String,
    pub edge_type: String,
    pub properties: HashMap<String, String>,
}

// ---------------------------------------------------------------------------
// Edge types to skip — rebuilt globally by post-passes
// ---------------------------------------------------------------------------

/// Edge types that are rebuilt globally by post-passes and must not be
/// snapshotted. Restoring a stale snapshot could produce edges that a full
/// reindex would not generate.
const SKIPPED_EDGE_TYPES: &[&str] = &["SIMILAR_TO", "SEMANTICALLY_RELATED"];

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Capture edges that cross the changed/unchanged boundary.
///
/// Before deleting/purging nodes for changed files, this function extracts
/// edges where one endpoint is in the "changed" set and the other is not.
/// Only inbound cross-file edges are captured: target in a changed file,
/// source in an unchanged (never-re-parsed) file. These are the edges that
/// would be permanently orphaned by the cascade delete.
///
/// ## Parameters
///
/// * `gbuf` — The current graph buffer containing nodes and edges.
/// * `changed_files` — Relative file paths of changed (or new) files that
///   will be purged and re-extracted.
///
/// ## Returns
///
/// A list of [`PreservedEdge`]s, empty when there are no cross-file edges
/// into changed files.
pub fn snapshot_cross_file_edges(
    gbuf: &GraphBuffer,
    changed_files: &[String],
) -> Vec<PreservedEdge> {
    // Early return for empty buffer.
    if matches!(gbuf, GraphBuffer::Empty) {
        return Vec::new();
    }

    if changed_files.is_empty() {
        return Vec::new();
    }

    let changed: HashSet<&str> = changed_files.iter().map(|s| s.as_str()).collect();
    let mut preserved = Vec::new();

    gbuf.foreach_edge(&mut |edge| {
        // Skip edge types that are rebuilt globally by post-passes.
        if SKIPPED_EDGE_TYPES.contains(&edge.edge_type.as_str()) {
            return;
        }

        // Look up source and target nodes.
        let source_node = match gbuf.find_by_id(edge.source_id) {
            Some(n) => n,
            None => return,
        };
        let target_node = match gbuf.find_by_id(edge.target_id) {
            Some(n) => n,
            None => return,
        };

        // Only preserve inbound cross-file edges: target in changed file,
        // source NOT in changed file.
        let target_is_changed = changed.contains(target_node.file_path.as_str());
        let source_is_changed = changed.contains(source_node.file_path.as_str());

        if target_is_changed && !source_is_changed {
            preserved.push(PreservedEdge {
                source_qn: source_node.qualified_name.clone(),
                target_qn: target_node.qualified_name.clone(),
                edge_type: edge.edge_type.clone(),
                properties: edge.properties.clone(),
            });
        }
    });

    preserved
}

/// Relink preserved edges into the graph buffer after re-extraction.
///
/// For each preserved edge, resolves `source_qn` and `target_qn` against the
/// current graph buffer to find the new `NodeId`s. Skips edges where either
/// QN is not found (symbol deleted or renamed by the edit — matching full
/// reindex semantics).
///
/// Dedup is automatic: [`GraphBuffer::insert_edge`] detects duplicate
/// `(source_id, target_id, edge_type)` tuples and returns the existing
/// `EdgeId`, so relinking an edge that the resolver already recreated is a
/// harmless no-op.
pub fn relink_edges(gbuf: &mut GraphBuffer, preserved: &[PreservedEdge]) {
    if matches!(gbuf, GraphBuffer::Empty) {
        return;
    }

    for edge in preserved {
        let source_id = match gbuf.find_by_qn(&edge.source_qn) {
            Some(n) => n.id,
            None => continue,
        };
        let target_id = match gbuf.find_by_qn(&edge.target_qn) {
            Some(n) => n.id,
            None => continue,
        };
        gbuf.insert_edge(
            source_id,
            target_id,
            &edge.edge_type,
            edge.properties.clone(),
        );
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::graph_buffer::GraphBuffer;

    /// Shorthand to build a `HashMap<String, String>` from key-value pairs.
    fn props(kvs: &[(&str, &str)]) -> HashMap<String, String> {
        kvs.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    // ── snapshot_cross_file_edges ───────────────────────────────────

    #[test]
    fn test_snapshot_captures_cross_file_edges() {
        let mut gbuf = GraphBuffer::new("test");

        // Node in unchanged file.
        let src = gbuf.upsert_node(
            "Function",
            "caller",
            "pkg.caller",
            "src/unchanged.rs",
            1,
            5,
            props(&[]),
        );
        // Nodes in changed file.
        let tgt1 = gbuf.upsert_node(
            "Function",
            "callee1",
            "pkg.callee1",
            "src/changed.rs",
            10,
            15,
            props(&[]),
        );
        let tgt2 = gbuf.upsert_node(
            "Function",
            "callee2",
            "pkg.callee2",
            "src/changed.rs",
            20,
            25,
            props(&[]),
        );
        // Node in another unchanged file.
        let other = gbuf.upsert_node(
            "Function",
            "other",
            "pkg.other",
            "src/other.rs",
            30,
            35,
            props(&[]),
        );

        // Cross-file edge: unchanged -> changed (should be captured).
        gbuf.insert_edge(src, tgt1, "calls", props(&[]));
        // Edge within changed file (should NOT be captured).
        gbuf.insert_edge(tgt1, tgt2, "calls", props(&[]));
        // Edge from changed -> unchanged (should NOT be captured).
        gbuf.insert_edge(tgt1, other, "calls", props(&[]));
        // Edge within unchanged (should NOT be captured).
        gbuf.insert_edge(other, src, "calls", props(&[]));

        let changed = vec!["src/changed.rs".to_string()];
        let preserved = snapshot_cross_file_edges(&gbuf, &changed);

        assert_eq!(
            preserved.len(),
            1,
            "should capture exactly one cross-file edge"
        );
        assert_eq!(preserved[0].source_qn, "pkg.caller");
        assert_eq!(preserved[0].target_qn, "pkg.callee1");
        assert_eq!(preserved[0].edge_type, "calls");
    }

    #[test]
    fn test_skips_similar_edges() {
        let mut gbuf = GraphBuffer::new("test");

        let src = gbuf.upsert_node(
            "Function",
            "a",
            "pkg.a",
            "src/unchanged.rs",
            1,
            5,
            props(&[]),
        );
        let tgt = gbuf.upsert_node(
            "Function",
            "b",
            "pkg.b",
            "src/changed.rs",
            10,
            15,
            props(&[]),
        );

        // SIMILAR_TO edge should be skipped.
        gbuf.insert_edge(src, tgt, "SIMILAR_TO", props(&[]));
        // Regular calls edge should still be captured.
        gbuf.insert_edge(src, tgt, "calls", props(&[]));

        let changed = vec!["src/changed.rs".to_string()];
        let preserved = snapshot_cross_file_edges(&gbuf, &changed);

        assert_eq!(
            preserved.len(),
            1,
            "SIMILAR_TO should be skipped but calls should remain"
        );
        assert_eq!(preserved[0].edge_type, "calls");

        // Also test SEMANTICALLY_RELATED isolation.
        let mut gbuf2 = GraphBuffer::new("test");
        let s2 = gbuf2.upsert_node("Function", "a", "pkg.a", "u.rs", 1, 5, props(&[]));
        let t2 = gbuf2.upsert_node("Function", "b", "pkg.b", "c.rs", 10, 15, props(&[]));
        gbuf2.insert_edge(s2, t2, "SEMANTICALLY_RELATED", props(&[]));

        let preserved2 = snapshot_cross_file_edges(&gbuf2, &["c.rs".to_string()]);
        assert!(
            preserved2.is_empty(),
            "SEMANTICALLY_RELATED should be skipped"
        );
    }

    #[test]
    fn test_empty_changed_set_returns_empty() {
        let mut gbuf = GraphBuffer::new("test");

        let src = gbuf.upsert_node("Function", "a", "pkg.a", "a.rs", 1, 5, props(&[]));
        let tgt = gbuf.upsert_node("Function", "b", "pkg.b", "b.rs", 10, 15, props(&[]));
        gbuf.insert_edge(src, tgt, "calls", props(&[]));

        let preserved = snapshot_cross_file_edges(&gbuf, &[]);
        assert!(
            preserved.is_empty(),
            "empty changed set should return empty"
        );
    }

    #[test]
    fn test_empty_graph_buffer_returns_empty() {
        let gbuf = GraphBuffer::Empty;
        let changed = vec!["a.rs".to_string()];
        let preserved = snapshot_cross_file_edges(&gbuf, &changed);
        assert!(
            preserved.is_empty(),
            "Empty variant should return empty vec"
        );
    }

    #[test]
    fn test_snapshot_preserves_properties() {
        let mut gbuf = GraphBuffer::new("test");

        let src = gbuf.upsert_node("Function", "a", "pkg.a", "u.rs", 1, 5, props(&[]));
        let tgt = gbuf.upsert_node("Function", "b", "pkg.b", "c.rs", 10, 15, props(&[]));
        gbuf.insert_edge(
            src,
            tgt,
            "calls",
            props(&[("inline", "true"), ("depth", "1")]),
        );

        let preserved = snapshot_cross_file_edges(&gbuf, &["c.rs".to_string()]);

        assert_eq!(preserved.len(), 1);
        assert_eq!(preserved[0].properties.get("inline").unwrap(), "true");
        assert_eq!(preserved[0].properties.get("depth").unwrap(), "1");
    }

    #[test]
    fn test_skips_self_file_edges() {
        // Edges where both source and target are in the changed file should
        // not be snapshotted.
        let mut gbuf = GraphBuffer::new("test");

        let n1 = gbuf.upsert_node("Function", "a", "pkg.a", "c.rs", 1, 5, props(&[]));
        let n2 = gbuf.upsert_node("Function", "b", "pkg.b", "c.rs", 10, 15, props(&[]));
        gbuf.insert_edge(n1, n2, "calls", props(&[]));

        let preserved = snapshot_cross_file_edges(&gbuf, &["c.rs".to_string()]);
        assert!(
            preserved.is_empty(),
            "both endpoints in changed file should not be captured"
        );
    }

    // ── relink_edges ───────────────────────────────────────────────

    #[test]
    fn test_edge_preservation_incremental_full_cycle() {
        // Full integration: snapshot -> purge -> re-add -> relink -> verify.
        let mut gbuf = GraphBuffer::new("test");

        // Nodes in unchanged file.
        let src = gbuf.upsert_node(
            "Function",
            "caller",
            "pkg.caller",
            "src/unchanged.rs",
            1,
            5,
            props(&[("key", "val")]),
        );
        // Node in changed file.
        let tgt = gbuf.upsert_node(
            "Function",
            "callee",
            "pkg.callee",
            "src/changed.rs",
            10,
            15,
            props(&[]),
        );

        // Cross-file edge with properties.
        gbuf.insert_edge(src, tgt, "calls", props(&[("inline", "true")]));

        let changed = vec!["src/changed.rs".to_string()];

        // Step 1: Snapshot.
        let preserved = snapshot_cross_file_edges(&gbuf, &changed);
        assert_eq!(preserved.len(), 1);

        // Step 2: Delete changed file nodes (simulating purge).
        gbuf.delete_by_file("src/changed.rs");
        assert!(gbuf.find_by_qn("pkg.callee").is_none());
        assert_eq!(
            gbuf.edge_count(),
            0,
            "edges referencing deleted nodes are cascade-deleted"
        );

        // Step 3: Re-add nodes (simulating re-extraction).
        let new_tgt = gbuf.upsert_node(
            "Function",
            "callee",
            "pkg.callee",
            "src/changed.rs",
            10,
            15,
            props(&[]),
        );
        assert_ne!(tgt, new_tgt, "new node should have a different NodeId");

        // Step 4: Relink.
        relink_edges(&mut gbuf, &preserved);

        // Step 5: Verify edge was restored.
        assert_eq!(gbuf.edge_count(), 1, "edge should be restored after relink");
        let restored_src = gbuf.find_by_qn("pkg.caller").unwrap().id;
        let restored_tgt = gbuf.find_by_qn("pkg.callee").unwrap().id;
        assert!(
            gbuf.edge_dedup_key(restored_src, restored_tgt, "calls"),
            "edge dedup key should exist after relink"
        );
    }

    #[test]
    fn test_relink_restores_edges() {
        // Alias to the full-cycle test — both exercise the same path.
        test_edge_preservation_incremental_full_cycle();
    }

    #[test]
    fn test_dedup_after_relink() {
        let mut gbuf = GraphBuffer::new("test");

        let src = gbuf.upsert_node("Function", "a", "pkg.a", "u.rs", 1, 5, props(&[]));
        let tgt = gbuf.upsert_node("Function", "b", "pkg.b", "c.rs", 10, 15, props(&[]));
        gbuf.insert_edge(src, tgt, "calls", props(&[]));

        let changed = vec!["c.rs".to_string()];

        // Snapshot.
        let preserved = snapshot_cross_file_edges(&gbuf, &changed);
        assert_eq!(preserved.len(), 1);

        // Purge and re-add nodes.
        gbuf.delete_by_file("c.rs");
        let new_src = gbuf.find_by_qn("pkg.a").unwrap().id;
        let new_tgt = gbuf.upsert_node("Function", "b", "pkg.b", "c.rs", 10, 15, props(&[]));

        // Simulate re-extraction recreating this edge (e.g., the resolver
        // already regenerated it).
        gbuf.insert_edge(new_src, new_tgt, "calls", props(&[]));
        assert_eq!(gbuf.edge_count(), 1, "one edge after re-extraction");

        // Relink — should not cause duplication.
        relink_edges(&mut gbuf, &preserved);

        assert_eq!(
            gbuf.edge_count(),
            1,
            "relink should not create duplicates"
        );
    }

    #[test]
    fn test_relink_skips_missing_qn() {
        let mut gbuf = GraphBuffer::new("test");

        // Create edge from pkg.a (u.rs) -> pkg.b (c.rs).
        let src = gbuf.upsert_node("Function", "a", "pkg.a", "u.rs", 1, 5, props(&[]));
        let tgt = gbuf.upsert_node("Function", "b", "pkg.b", "c.rs", 10, 15, props(&[]));
        gbuf.insert_edge(src, tgt, "calls", props(&[]));

        let preserved = snapshot_cross_file_edges(&gbuf, &["c.rs".to_string()]);
        assert_eq!(preserved.len(), 1);

        // Purge changed file nodes.
        gbuf.delete_by_file("c.rs");

        // Do NOT re-add pkg.b (simulate symbol deletion).
        // Relink should silently skip it.
        relink_edges(&mut gbuf, &preserved);

        // No edge should exist since target QN is missing.
        assert_eq!(gbuf.edge_count(), 0);
    }

    #[test]
    fn test_relink_on_empty_buffer_is_noop() {
        let preserved = vec![PreservedEdge {
            source_qn: "pkg.a".to_string(),
            target_qn: "pkg.b".to_string(),
            edge_type: "calls".to_string(),
            properties: HashMap::new(),
        }];
        let mut gbuf = GraphBuffer::Empty;
        // Should not panic.
        relink_edges(&mut gbuf, &preserved);
        assert_eq!(gbuf.edge_count(), 0);
    }
}
