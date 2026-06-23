//! Edge snapshot — captures cross-file edges that must survive incremental
//! rebuilds, then restores them into the rebuilt graph.
//!
//! # Why
//!
//! When file B changes but file A does NOT, cross-file edges from A → B must be
//! preserved. The incremental rebuild:
//!
//! 1. Drops all edges originating from or targeting changed files.
//! 2. Re-extracts and re-resolves all edge types from changed files.
//! 3. **Snapshot** saves edges that cross from UNCHANGED files into CHANGED files.
//! 4. **Restore** re-inserts those saved edges after the rebuild step.
//!
//! Edge kinds that post-passes recompute (SIMILAR_TO, SEMANTICALLY_RELATED) are
//! excluded — they'll be recomputed in the post-pass.

use std::collections::HashSet;

use crate::core::graph_index::{IndexEdge, ProjectIndex};

/// Edge kinds that post-passes recompute — never snapshot these.
pub const RECOMPUTED_EDGE_KINDS: &[&str] = &["SIMILAR_TO", "SEMANTICALLY_RELATED"];

/// A single captured edge for snapshot/restore.
#[derive(Debug, Clone, PartialEq)]
pub struct CapturedEdge {
    /// Source file path (relative).
    pub source_file: String,
    /// Target file path (relative).
    pub target_file: String,
    /// Edge kind string (e.g. "import", "module", "namespace").
    pub kind: String,
    /// Edge weight.
    pub weight: f32,
}

/// Snapshot of cross-file edges that must survive incremental rebuild.
#[derive(Debug, Clone, Default)]
pub struct EdgeSnapshot {
    pub edges: Vec<CapturedEdge>,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Capture edges from unchanged files into changed files.
///
/// For each edge in `existing`:
/// - If `target_file` is in `changed_files` AND `source_file` is NOT in
///   `changed_files` AND edge kind is NOT in [`RECOMPUTED_EDGE_KINDS`] → save.
///
/// Returns the snapshot preserving these edges.
#[must_use]
pub fn snapshot_inbound_edges(
    existing: &ProjectIndex,
    changed_files: &[String],
) -> EdgeSnapshot {
    let changed_set: HashSet<&str> = changed_files.iter().map(String::as_str).collect();

    let edges: Vec<CapturedEdge> = existing
        .edges
        .iter()
        .filter(|e| {
            // Target is a changed file (inbound edge).
            changed_set.contains(e.to.as_str())
                // Source is NOT a changed file (it's an unchanged file).
                && !changed_set.contains(e.from.as_str())
                // Not a recomputed edge kind.
                && !RECOMPUTED_EDGE_KINDS.contains(&e.kind.as_str())
        })
        .map(|e| CapturedEdge {
            source_file: e.from.clone(),
            target_file: e.to.clone(),
            kind: e.kind.clone(),
            weight: e.weight,
        })
        .collect();

    EdgeSnapshot { edges }
}

/// Restore previously captured edges into the rebuilt graph.
///
/// For each captured edge:
/// - If both source and target files exist in the graph AND the edge doesn't
///   already exist → insert it.
/// - If target file not found (deleted file case), silently skip.
///
/// Returns the number of edges restored.
#[must_use]
pub fn restore_edges(graph: &mut ProjectIndex, snapshot: &EdgeSnapshot) -> usize {
    let mut restored = 0;

    for cap in &snapshot.edges {
        // Skip if source or target file no longer exists in the graph
        // (e.g. the target file was deleted).
        if !graph.files.contains_key(&cap.source_file)
            || !graph.files.contains_key(&cap.target_file)
        {
            continue;
        }

        // Dedup: skip if the edge already exists.
        let already_exists = graph.edges.iter().any(|e| {
            e.from == cap.source_file && e.to == cap.target_file && e.kind == cap.kind
        });

        if !already_exists {
            graph.edges.push(IndexEdge {
                from: cap.source_file.clone(),
                to: cap.target_file.clone(),
                kind: cap.kind.clone(),
                weight: cap.weight,
            });
            restored += 1;
        }
    }

    restored
}

/// Remove all edges and symbols belonging to the given files.
///
/// Used before rebuilding changed files so the re-extraction starts clean.
pub fn drop_edges_for_files(index: &mut ProjectIndex, files: &[String]) {
    if files.is_empty() {
        return;
    }

    let files_set: HashSet<&str> = files.iter().map(String::as_str).collect();

    // Remove edges where source OR target is one of the dropped files.
    index
        .edges
        .retain(|e| !files_set.contains(e.from.as_str()) && !files_set.contains(e.to.as_str()));

    // Remove symbols that belong to any of the dropped files.
    index
        .symbols
        .retain(|_, s| !files_set.contains(s.file.as_str()));

    // Remove the file entries themselves.
    for file in files {
        index.files.remove(file);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::graph_index::FileEntry;

    /// Helper: build a minimal `FileEntry` for a given path.
    fn file_entry(path: &str) -> FileEntry {
        FileEntry {
            path: path.to_string(),
            hash: String::new(),
            language: "rs".to_string(),
            line_count: 0,
            token_count: 0,
            exports: Vec::new(),
            summary: String::new(),
        }
    }

    /// Helper: create a `ProjectIndex` with the given file paths and edges.
    fn test_index(
        file_paths: &[&str],
        edges: Vec<(&str, &str, &str, f32)>, // (from, to, kind, weight)
    ) -> ProjectIndex {
        let mut index = ProjectIndex::new("/test");
        for path in file_paths {
            index.files.insert(path.to_string(), file_entry(path));
        }
        for (from, to, kind, weight) in edges {
            index.edges.push(IndexEdge {
                from: from.to_string(),
                to: to.to_string(),
                kind: kind.to_string(),
                weight,
            });
        }
        index
    }

    // -----------------------------------------------------------------------
    // snapshot_inbound_edges tests
    // -----------------------------------------------------------------------

    #[test]
    fn captures_cross_file_unchanged_to_changed() {
        // Edge from unchanged.rs → changed.rs: must be captured.
        let index = test_index(
            &["unchanged.rs", "changed.rs"],
            vec![("unchanged.rs", "changed.rs", "import", 1.0)],
        );
        let changed = vec!["changed.rs".to_string()];

        let snapshot = snapshot_inbound_edges(&index, &changed);

        assert_eq!(snapshot.edges.len(), 1);
        assert_eq!(snapshot.edges[0].source_file, "unchanged.rs");
        assert_eq!(snapshot.edges[0].target_file, "changed.rs");
        assert_eq!(snapshot.edges[0].kind, "import");
    }

    #[test]
    fn skips_same_file_edge_when_both_changed() {
        // Edge from changed.rs to other.rs where changed.rs is in changed list.
        let index = test_index(
            &["changed.rs", "other.rs"],
            vec![("changed.rs", "other.rs", "import", 1.0)],
        );
        let changed = vec!["changed.rs".to_string()];

        let snapshot = snapshot_inbound_edges(&index, &changed);

        assert!(
            snapshot.edges.is_empty(),
            "edges originating from a changed file must not be captured"
        );
    }

    #[test]
    fn skips_similarly_to_edge() {
        // SIMILAR_TO edge from unchanged → changed must NOT be captured.
        let index = test_index(
            &["unchanged.rs", "changed.rs"],
            vec![("unchanged.rs", "changed.rs", "SIMILAR_TO", 0.7)],
        );
        let changed = vec!["changed.rs".to_string()];

        let snapshot = snapshot_inbound_edges(&index, &changed);

        assert!(
            snapshot.edges.is_empty(),
            "SIMILAR_TO edges must not be captured"
        );
    }

    #[test]
    fn skips_semantically_related_edge() {
        let index = test_index(
            &["unchanged.rs", "changed.rs"],
            vec![("unchanged.rs", "changed.rs", "SEMANTICALLY_RELATED", 0.6)],
        );
        let changed = vec!["changed.rs".to_string()];

        let snapshot = snapshot_inbound_edges(&index, &changed);

        assert!(
            snapshot.edges.is_empty(),
            "SEMANTICALLY_RELATED edges must not be captured"
        );
    }

    #[test]
    fn empty_snapshot_when_no_changed_files() {
        let index = test_index(
            &["a.rs", "b.rs"],
            vec![("a.rs", "b.rs", "import", 1.0)],
        );
        let changed: Vec<String> = vec![];

        let snapshot = snapshot_inbound_edges(&index, &changed);

        assert!(snapshot.edges.is_empty());
    }

    #[test]
    fn empty_snapshot_when_no_cross_file_edges() {
        let index = test_index(
            &["a.rs", "b.rs"],
            vec![("a.rs", "a.rs", "import", 1.0)], // self-loop, not cross-file
        );
        let changed = vec!["b.rs".to_string()];

        let snapshot = snapshot_inbound_edges(&index, &changed);

        assert!(
            snapshot.edges.is_empty(),
            "no cross-file edges should yield empty snapshot"
        );
    }

    #[test]
    fn captures_only_inbound_edges_not_outbound() {
        // Edge from changed → unchanged (outbound from changed) must NOT be
        // captured. Only unchanged → changed (inbound to changed) is captured.
        let index = test_index(
            &["unchanged.rs", "changed.rs"],
            vec![
                ("changed.rs", "unchanged.rs", "import", 1.0), // outbound
                ("unchanged.rs", "changed.rs", "import", 1.0), // inbound ← capture
            ],
        );
        let changed = vec!["changed.rs".to_string()];

        let snapshot = snapshot_inbound_edges(&index, &changed);

        assert_eq!(snapshot.edges.len(), 1);
        assert_eq!(snapshot.edges[0].source_file, "unchanged.rs");
        assert_eq!(snapshot.edges[0].target_file, "changed.rs");
    }

    #[test]
    fn multiple_changed_files_captured_independently() {
        let index = test_index(
            &["a.rs", "b.rs", "c.rs"],
            vec![
                ("a.rs", "b.rs", "import", 1.0),
                ("a.rs", "c.rs", "import", 1.0),
                ("b.rs", "c.rs", "import", 1.0),
            ],
        );
        // b.rs and c.rs changed; a.rs unchanged.
        let changed = vec!["b.rs".to_string(), "c.rs".to_string()];

        let snapshot = snapshot_inbound_edges(&index, &changed);

        // Both edges from a.rs should be captured; b.rs→c.rs NOT captured
        // (b.rs is also changed).
        assert_eq!(snapshot.edges.len(), 2);
        for cap in &snapshot.edges {
            assert_eq!(cap.source_file, "a.rs");
            assert!(cap.target_file == "b.rs" || cap.target_file == "c.rs");
        }
    }

    // -----------------------------------------------------------------------
    // restore_edges tests
    // -----------------------------------------------------------------------

    #[test]
    fn restores_captured_edges_into_graph() {
        let mut graph = test_index(
            &["unchanged.rs", "changed.rs"],
            vec![], // start empty
        );
        let snapshot = EdgeSnapshot {
            edges: vec![CapturedEdge {
                source_file: "unchanged.rs".to_string(),
                target_file: "changed.rs".to_string(),
                kind: "import".to_string(),
                weight: 1.0,
            }],
        };

        let count = restore_edges(&mut graph, &snapshot);

        assert_eq!(count, 1);
        assert_eq!(graph.edges.len(), 1);
        assert_eq!(graph.edges[0].from, "unchanged.rs");
        assert_eq!(graph.edges[0].to, "changed.rs");
        assert_eq!(graph.edges[0].kind, "import");
    }

    #[test]
    fn skips_edge_when_target_file_deleted() {
        let mut graph = test_index(
            &["unchanged.rs"], // target changed.rs is missing (deleted)
            vec![],
        );
        let snapshot = EdgeSnapshot {
            edges: vec![CapturedEdge {
                source_file: "unchanged.rs".to_string(),
                target_file: "changed.rs".to_string(),
                kind: "import".to_string(),
                weight: 1.0,
            }],
        };

        let count = restore_edges(&mut graph, &snapshot);

        assert_eq!(count, 0, "edge to deleted file must be silently dropped");
        assert!(graph.edges.is_empty());
    }

    #[test]
    fn skips_edge_when_source_file_deleted() {
        let mut graph = test_index(
            &["changed.rs"], // source unchanged.rs is missing
            vec![],
        );
        let snapshot = EdgeSnapshot {
            edges: vec![CapturedEdge {
                source_file: "unchanged.rs".to_string(),
                target_file: "changed.rs".to_string(),
                kind: "import".to_string(),
                weight: 1.0,
            }],
        };

        let count = restore_edges(&mut graph, &snapshot);

        assert_eq!(count, 0, "edge from deleted file must be silently dropped");
    }

    #[test]
    fn does_not_duplicate_existing_edge() {
        let mut graph = test_index(
            &["unchanged.rs", "changed.rs"],
            vec![("unchanged.rs", "changed.rs", "import", 1.0)],
        );
        let snapshot = EdgeSnapshot {
            edges: vec![CapturedEdge {
                source_file: "unchanged.rs".to_string(),
                target_file: "changed.rs".to_string(),
                kind: "import".to_string(),
                weight: 1.0,
            }],
        };

        let count = restore_edges(&mut graph, &snapshot);

        assert_eq!(count, 0, "duplicate edge must not be inserted");
        assert_eq!(graph.edges.len(), 1);
    }

    #[test]
    fn restores_preserves_edge_weight() {
        let mut graph = test_index(&["a.rs", "b.rs"], vec![]);
        let snapshot = EdgeSnapshot {
            edges: vec![CapturedEdge {
                source_file: "a.rs".to_string(),
                target_file: "b.rs".to_string(),
                kind: "cochange".to_string(),
                weight: 0.5,
            }],
        };

        let _ = restore_edges(&mut graph, &snapshot);

        assert_eq!(graph.edges[0].weight, 0.5);
    }

    // -----------------------------------------------------------------------
    // drop_edges_for_files tests
    // -----------------------------------------------------------------------

    #[test]
    fn drops_edges_and_symbols_and_files() {
        let mut index = test_index(
            &["keep.rs", "drop.rs"],
            vec![
                ("keep.rs", "drop.rs", "import", 1.0),
                ("drop.rs", "keep.rs", "import", 1.0),
                ("keep.rs", "keep.rs", "import", 1.0), // internal; should stay
            ],
        );
        index.symbols.insert(
            "drop.rs::foo".to_string(),
            crate::core::graph_index::SymbolEntry {
                file: "drop.rs".to_string(),
                name: "foo".to_string(),
                kind: "fn".to_string(),
                start_line: 1,
                end_line: 3,
                is_exported: false,
            },
        );
        index.symbols.insert(
            "keep.rs::bar".to_string(),
            crate::core::graph_index::SymbolEntry {
                file: "keep.rs".to_string(),
                name: "bar".to_string(),
                kind: "fn".to_string(),
                start_line: 1,
                end_line: 3,
                is_exported: false,
            },
        );

        drop_edges_for_files(&mut index, &["drop.rs".to_string()]);

        // Edge drop.rs → keep.rs removed (source is drop.rs).
        // Edge keep.rs → drop.rs removed (target is drop.rs).
        // Edge keep.rs → keep.rs stays (neither source nor target is drop.rs).
        assert_eq!(index.edges.len(), 1);
        assert_eq!(index.edges[0].from, "keep.rs");
        assert_eq!(index.edges[0].to, "keep.rs");

        // Symbol for drop.rs removed; symbol for keep.rs stays.
        assert!(!index.symbols.contains_key("drop.rs::foo"));
        assert!(index.symbols.contains_key("keep.rs::bar"));

        // File entry removed.
        assert!(!index.files.contains_key("drop.rs"));
        assert!(index.files.contains_key("keep.rs"));
    }

    #[test]
    fn drop_empty_files_is_noop() {
        let mut index = test_index(
            &["a.rs", "b.rs"],
            vec![("a.rs", "b.rs", "import", 1.0)],
        );
        let before_count = index.edges.len();

        drop_edges_for_files(&mut index, &[]);

        assert_eq!(index.edges.len(), before_count);
        assert!(index.files.contains_key("a.rs"));
        assert!(index.files.contains_key("b.rs"));
    }

    // -----------------------------------------------------------------------
    // Integration: snapshot → rebuild → restore round-trip
    // -----------------------------------------------------------------------

    #[test]
    fn round_trip_cross_file_snapshot_and_restore() {
        // Simulate an incremental rebuild:
        // 1. Start with known state (A→B edge, A unchanged, B changed).
        // 2. Snapshot inbound edges to B.
        // 3. Drop all edges for changed files (B).
        // 4. Rebuild (no-op here, just verify graph is clean).
        // 5. Restore the snapshot.
        let mut index = test_index(
            &["a.rs", "b.rs"],
            vec![("a.rs", "b.rs", "import", 1.0)],
        );
        let changed = vec!["b.rs".to_string()];

        // Step 2: snapshot
        let snapshot = snapshot_inbound_edges(&index, &changed);
        assert_eq!(snapshot.edges.len(), 1);

        // Step 3: drop
        drop_edges_for_files(&mut index, &changed);
        // After drop: no edges involving b.rs remain.
        assert!(index.edges.is_empty());
        assert!(!index.files.contains_key("b.rs"));

        // Step 4: re-add b.rs as if it was re-extracted (fresh, no edges yet).
        index
            .files
            .insert("b.rs".to_string(), file_entry("b.rs"));

        // Step 5: restore
        let count = restore_edges(&mut index, &snapshot);
        assert_eq!(count, 1, "cross-file edge must be restored");
        assert_eq!(index.edges.len(), 1);
        assert_eq!(index.edges[0].from, "a.rs");
        assert_eq!(index.edges[0].to, "b.rs");
        assert_eq!(index.edges[0].kind, "import");
    }

    #[test]
    fn deterministic_behaviour() {
        let index = test_index(
            &["a.rs", "b.rs", "c.rs"],
            vec![
                ("a.rs", "b.rs", "import", 1.0),
                ("c.rs", "b.rs", "import", 1.0),
            ],
        );
        let changed = vec!["b.rs".to_string()];

        let r1 = snapshot_inbound_edges(&index, &changed);
        let r2 = snapshot_inbound_edges(&index, &changed);

        assert_eq!(r1.edges.len(), r2.edges.len());
        for (e1, e2) in r1.edges.iter().zip(r2.edges.iter()) {
            assert_eq!(e1.source_file, e2.source_file);
            assert_eq!(e1.target_file, e2.target_file);
            assert_eq!(e1.kind, e2.kind);
        }
    }
}
