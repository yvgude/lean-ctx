//! Post-pass: `SIMILAR_TO` edges from `MinHash` fingerprints.
//!
//! Reads `"fp"` (hex-encoded `MinHash`) from Function/Method node properties,
//! builds a band-based LSH index, finds candidate pairs, computes exact
//! Jaccard similarity, and emits `SIMILAR_TO` edges.
//!
//! Designed to work exclusively with `GraphBuffer` — no file I/O, no
//! `ProjectIndex` dependency. The LSH bands match the existing Rust
//! implementation (16 bands × 4 values, XOR-combined).
//!
//! Deterministic: sorting entries by node ID and sorting candidates by
//! Jaccard ensures stable output across runs.

use std::collections::{HashMap, HashSet};

use crate::core::graph_buffer::GraphBuffer;
use crate::core::index_types::{Minhash, NodeId};

// ── Constants ──

/// Number of LSH bands (16 bands of 4 minhash values each).
const BANDS: usize = 16;

/// `MinHash` values per band.
const ROWS_PER_BAND: usize = 4;

/// Maximum `SIMILAR_TO` edges emitted per node (matches C's `MAX_EDGES_PER_NODE`).
const MAX_EDGES_PER_NODE: usize = 10;

// ── FP entry ──

/// A node with a valid `MinHash` fingerprint extracted from properties.
#[derive(Debug, Clone)]
struct FpEntry {
    node_id: NodeId,
    minhash: Minhash,
}

// ── Public API ──

/// Run similarity pass: read `"fp"` from Function/Method nodes, build LSH,
/// and emit `SIMILAR_TO` edges for pairs with Jaccard ≥ `threshold`.
///
/// Deduplication is handled by `GraphBuffer::insert_edge` (which returns the
/// existing `EdgeId` for duplicate `(source_id, target_id, edge_type)`).
/// `MAX_EDGES_PER_NODE` is enforced per source node.
pub fn compute_similar_to(gbuf: &mut GraphBuffer, threshold: f32) {
    // Phase 1: collect all Function/Method nodes that have an "fp" property.
    let entries = collect_fp_entries(gbuf);
    if entries.len() < 2 {
        return;
    }

    // Phase 2: build LSH bucket index (16 bands, 4 values/band, XOR-combined).
    //   bucket_key = (band << 32) | xor_result
    let mut bucket_map: HashMap<u64, Vec<usize>> = HashMap::new();
    for (idx, entry) in entries.iter().enumerate() {
        for band in 0..BANDS {
            let start = band * ROWS_PER_BAND;
            let xor = entry.minhash.0[start]
                ^ entry.minhash.0[start + 1]
                ^ entry.minhash.0[start + 2]
                ^ entry.minhash.0[start + 3];
            let bucket = (band as u64) << 32 | u64::from(xor);
            bucket_map.entry(bucket).or_default().push(idx);
        }
    }

    // Phase 3: score candidates via LSH pre-filtering.
    // Collect deferred edges first, then emit (avoids borrow conflicts with gbuf).
    #[derive(Debug)]
    struct DeferredEdge {
        source_id: NodeId,
        target_id: NodeId,
        jaccard: f32,
    }

    let mut deferred: Vec<DeferredEdge> = Vec::new();
    let mut edge_counts: HashMap<NodeId, usize> = HashMap::new();

    for i in 0..entries.len() {
        let src = &entries[i];
        let count_a = *edge_counts.get(&src.node_id).unwrap_or(&0);
        if count_a >= MAX_EDGES_PER_NODE {
            continue;
        }

        // Collect unique candidate indices from all bands
        let mut seen = HashSet::new();
        let mut candidates: Vec<(f32, NodeId)> = Vec::new();

        // Check dedup key only once per source
        let skip_dedup: HashSet<NodeId> = {
            let edges = gbuf.find_edges_by_source_type(src.node_id, "SIMILAR_TO");
            edges.iter().map(|e| e.target_id).collect()
        };

        for band in 0..BANDS {
            let start = band * ROWS_PER_BAND;
            let xor = src.minhash.0[start]
                ^ src.minhash.0[start + 1]
                ^ src.minhash.0[start + 2]
                ^ src.minhash.0[start + 3];
            let bucket = (band as u64) << 32 | u64::from(xor);

            let Some(indices) = bucket_map.get(&bucket) else {
                continue;
            };

            for &j in indices {
                if j <= i {
                    continue;
                }
                if !seen.insert(j) {
                    continue;
                }

                let tgt = &entries[j];
                if src.node_id == tgt.node_id {
                    continue;
                }

                // Skip already-existing SIMILAR_TO edge
                if skip_dedup.contains(&tgt.node_id) {
                    continue;
                }

                let count_b = *edge_counts.get(&tgt.node_id).unwrap_or(&0);
                if count_b >= MAX_EDGES_PER_NODE {
                    continue;
                }

                let jaccard = minhash_jaccard(&src.minhash.0, &tgt.minhash.0);
                if jaccard >= threshold {
                    candidates.push((jaccard, tgt.node_id));
                }
            }
        }

        // Sort by Jaccard descending
        candidates.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        // Emit respecting MAX_EDGES_PER_NODE
        let mut emitted = 0;
        for (jaccard, tgt_id) in &candidates {
            let cur = *edge_counts.get(&src.node_id).unwrap_or(&0) + emitted;
            if cur >= MAX_EDGES_PER_NODE {
                break;
            }
            deferred.push(DeferredEdge {
                source_id: src.node_id,
                target_id: *tgt_id,
                jaccard: *jaccard,
            });
            emitted += 1;
        }

        if emitted > 0 {
            *edge_counts.entry(src.node_id).or_insert(0) += emitted;
        }
    }

    // Phase 4: insert all deferred edges into the graph buffer.
    for de in &deferred {
        let mut props = HashMap::new();
        props.insert("jaccard".to_string(), format!("{:.3}", de.jaccard));
        gbuf.insert_edge(de.source_id, de.target_id, "SIMILAR_TO", props);
    }
}

// ── Internal helpers ──

/// Collect all nodes with label "Function" or "Method" that have an `"fp"`
/// property containing a valid hex-encoded `MinHash`.
fn collect_fp_entries(gbuf: &GraphBuffer) -> Vec<FpEntry> {
    let mut entries = Vec::new();
    for label in &["Function", "Method"] {
        let nodes = gbuf.find_nodes_by_label(label);
        for node in nodes {
            if let Some(fp_hex) = node.properties.get("fp")
                && let Some(mh) = Minhash::from_hex(fp_hex)
            {
                entries.push(FpEntry {
                    node_id: node.id,
                    minhash: mh,
                });
            }
        }
    }
    // Sort by node ID for deterministic bucket traversal
    entries.sort_by_key(|e| e.node_id);
    entries
}

/// Compute `MinHash` Jaccard similarity between two 64-element arrays.
///
/// Counts positions where `a[i] == b[i]` and divides by 64.
/// This is an unbiased estimate of the true Jaccard similarity.
fn minhash_jaccard(a: &[u32; 64], b: &[u32; 64]) -> f32 {
    let equal = a.iter().zip(b.iter()).filter(|(x, y)| x == y).count();
    equal as f32 / 64.0
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn props(kvs: &[(&str, &str)]) -> HashMap<String, String> {
        kvs.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// Build a minhash hex string where all 64 values equal `val`.
    fn mh_hex_all(val: u32) -> String {
        let mh = [val; 64];
        Minhash(mh).to_hex()
    }

    /// Build a minhash where first N values are `a`, rest are `b`.
    #[allow(dead_code)]
    fn mh_hex_split(a: u32, b: u32, first_n: usize) -> String {
        let mut arr = [b; 64];
        for item in arr.iter_mut().take(first_n.min(64)) {
            *item = a;
        }
        Minhash(arr).to_hex()
    }

    fn setup_gbuf() -> GraphBuffer {
        let mut gb = GraphBuffer::new("test");
        gb.upsert_node(
            "Function",
            "func_a",
            "pkg.func_a",
            "src/a.rs",
            1,
            10,
            props(&[("fp", &mh_hex_all(42))]),
        );
        gb.upsert_node(
            "Function",
            "func_b",
            "pkg.func_b",
            "src/b.rs",
            1,
            10,
            props(&[("fp", &mh_hex_all(42))]),
        );
        gb
    }

    // ── collect_fp_entries ──

    #[test]
    fn test_collect_fp_entries_filters_nodes() {
        let mut gb = GraphBuffer::new("test");
        // Function with fp
        gb.upsert_node(
            "Function",
            "a",
            "pkg.a",
            "a.rs",
            1,
            5,
            props(&[("fp", &mh_hex_all(1))]),
        );
        // Method with fp
        gb.upsert_node(
            "Method",
            "b",
            "pkg.b",
            "b.rs",
            1,
            5,
            props(&[("fp", &mh_hex_all(2))]),
        );
        // Function without fp
        gb.upsert_node("Function", "c", "pkg.c", "c.rs", 1, 5, props(&[]));
        // Class node (should be ignored)
        gb.upsert_node(
            "Class",
            "d",
            "pkg.d",
            "d.rs",
            1,
            5,
            props(&[("fp", &mh_hex_all(3))]),
        );

        let entries = collect_fp_entries(&gb);
        assert_eq!(entries.len(), 2, "only Function and Method with fp");
    }

    #[test]
    fn test_collect_fp_entries_sorts_by_node_id() {
        let mut gb = GraphBuffer::new("test");
        let id_a = gb.upsert_node(
            "Function",
            "a",
            "pkg.a",
            "a.rs",
            1,
            5,
            props(&[("fp", &mh_hex_all(1))]),
        );
        let id_b = gb.upsert_node(
            "Function",
            "b",
            "pkg.b",
            "b.rs",
            1,
            5,
            props(&[("fp", &mh_hex_all(2))]),
        );
        let entries = collect_fp_entries(&gb);
        // Should be sorted by node_id ascending: a (1) < b (2)
        assert_eq!(entries[0].node_id, id_a);
        assert_eq!(entries[1].node_id, id_b);
        assert!(id_a.0 < id_b.0, "id_a should be less than id_b");
    }

    // ── minhash_jaccard ──

    #[test]
    fn test_minhash_jaccard_identical() {
        let a = [42u32; 64];
        let b = [42u32; 64];
        assert!((minhash_jaccard(&a, &b) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_minhash_jaccard_disjoint() {
        let a = [1u32; 64];
        let b = [2u32; 64];
        assert!((minhash_jaccard(&a, &b) - 0.0).abs() < 1e-6);
    }

    #[test]
    fn test_minhash_jaccard_partial() {
        let mut a = [0u32; 64];
        let mut b = [0u32; 64];
        for (i, (ae, be)) in a.iter_mut().zip(b.iter_mut()).enumerate().take(64) {
            let val = i as u32;
            *ae = val;
            *be = val + if i < 32 { 0 } else { 100 };
        }
        let sim = minhash_jaccard(&a, &b);
        assert!((sim - 0.5).abs() < 1e-6, "expected ~0.5, got {sim}");
    }

    // ── compute_similar_to ──

    #[test]
    fn test_similar_to_identical_fp_produces_edge() {
        let mut gb = setup_gbuf();
        let before = gb.edge_count();
        compute_similar_to(&mut gb, 0.5);
        assert!(gb.edge_count() > before, "should add SIMILAR_TO edge");
    }

    #[test]
    fn test_similar_to_no_edge_for_low_threshold() {
        let mut gb = GraphBuffer::new("test");
        // Create two functions with very different minhashes
        gb.upsert_node(
            "Function",
            "a",
            "pkg.a",
            "a.rs",
            1,
            5,
            props(&[("fp", &mh_hex_all(1))]),
        );
        gb.upsert_node(
            "Function",
            "b",
            "pkg.b",
            "b.rs",
            1,
            5,
            props(&[("fp", &mh_hex_all(9999))]),
        );
        let before = gb.edge_count();
        compute_similar_to(&mut gb, 0.99); // threshold too high
        assert_eq!(gb.edge_count(), before, "no edge for dissimilar minhashes");
    }

    #[test]
    fn test_similar_to_only_function_and_method() {
        let mut gb = GraphBuffer::new("test");
        // Two Functions with identical fp
        gb.upsert_node(
            "Function",
            "a",
            "pkg.a",
            "a.rs",
            1,
            5,
            props(&[("fp", &mh_hex_all(42))]),
        );
        gb.upsert_node(
            "Function",
            "b",
            "pkg.b",
            "b.rs",
            1,
            5,
            props(&[("fp", &mh_hex_all(42))]),
        );
        // A Class (should be ignored) even with same fp
        gb.upsert_node(
            "Class",
            "c",
            "pkg.c",
            "c.rs",
            1,
            5,
            props(&[("fp", &mh_hex_all(42))]),
        );

        compute_similar_to(&mut gb, 0.5);
        // Should only have edge between a and b, not involving c
        gb.foreach_edge(&mut |e| {
            let src = gb.find_by_id(e.source_id).unwrap();
            let tgt = gb.find_by_id(e.target_id).unwrap();
            assert!(src.label == "Function" || src.label == "Method");
            assert!(tgt.label == "Function" || tgt.label == "Method");
        });
    }

    #[test]
    fn test_no_duplicate_similar_to_edges() {
        let mut gb = setup_gbuf();
        compute_similar_to(&mut gb, 0.5);
        let count_after_first = gb.edge_count();
        compute_similar_to(&mut gb, 0.5);
        assert_eq!(
            gb.edge_count(),
            count_after_first,
            "no duplicate SIMILAR_TO edges"
        );
    }

    #[test]
    fn test_no_self_edges() {
        let mut gb = GraphBuffer::new("test");
        gb.upsert_node(
            "Function",
            "a",
            "pkg.a",
            "a.rs",
            1,
            5,
            props(&[("fp", &mh_hex_all(42))]),
        );
        compute_similar_to(&mut gb, 0.5);
        gb.foreach_edge(&mut |e| {
            assert_ne!(e.source_id, e.target_id, "no self edges");
        });
    }

    #[test]
    fn test_similar_to_empty_graph() {
        let mut gb = GraphBuffer::new("test");
        compute_similar_to(&mut gb, 0.5); // should not crash
        assert_eq!(gb.edge_count(), 0);
    }

    #[test]
    fn test_similar_to_single_node() {
        let mut gb = GraphBuffer::new("test");
        gb.upsert_node(
            "Function",
            "a",
            "pkg.a",
            "a.rs",
            1,
            5,
            props(&[("fp", &mh_hex_all(42))]),
        );
        compute_similar_to(&mut gb, 0.5); // should not crash
        assert_eq!(gb.edge_count(), 0);
    }

    #[test]
    fn test_max_edges_per_node() {
        // Create one source node with fp=42 and 20 target nodes all with fp=42.
        // Only MAX_EDGES_PER_NODE should be emitted from the source.
        let mut gb = GraphBuffer::new("test");
        let src_id = gb.upsert_node(
            "Function",
            "src",
            "pkg.src",
            "src.rs",
            1,
            5,
            props(&[("fp", &mh_hex_all(42))]),
        );
        for i in 0..20usize {
            gb.upsert_node(
                "Function",
                &format!("tgt_{i}"),
                &format!("pkg.tgt_{i}"),
                "tgt.rs",
                1,
                5,
                props(&[("fp", &mh_hex_all(42))]),
            );
        }

        compute_similar_to(&mut gb, 0.5);

        let src_edge_count = gb.find_edges_by_source_type(src_id, "SIMILAR_TO").len();
        assert!(
            src_edge_count <= MAX_EDGES_PER_NODE,
            "source node has {src_edge_count} SIMILAR_TO edges, max {MAX_EDGES_PER_NODE}"
        );
    }

    #[test]
    fn test_deterministic_output() {
        let mut gb1 = setup_gbuf();
        let mut gb2 = setup_gbuf();
        compute_similar_to(&mut gb1, 0.5);
        compute_similar_to(&mut gb2, 0.5);
        assert_eq!(gb1.edge_count(), gb2.edge_count());
        // Collect edges in order for comparison
        let mut edges1: Vec<(NodeId, NodeId, String)> = Vec::new();
        gb1.foreach_edge(&mut |e| {
            edges1.push((e.source_id, e.target_id, e.edge_type.clone()));
        });
        let mut edges2: Vec<(NodeId, NodeId, String)> = Vec::new();
        gb2.foreach_edge(&mut |e| {
            edges2.push((e.source_id, e.target_id, e.edge_type.clone()));
        });
        for (e1, e2) in edges1.iter().zip(edges2.iter()) {
            assert_eq!(e1, e2);
        }
    }

    #[test]
    fn test_similar_to_multiple_matches() {
        // 3 functions all with identical fp=42 → should produce edges.
        // With 3 nodes there should be at most C(3,2)=3 unique SIMILAR_TO edges.
        let mut gb = GraphBuffer::new("test");
        gb.upsert_node(
            "Function",
            "a",
            "pkg.a",
            "a.rs",
            1,
            5,
            props(&[("fp", &mh_hex_all(42))]),
        );
        gb.upsert_node(
            "Function",
            "b",
            "pkg.b",
            "b.rs",
            1,
            5,
            props(&[("fp", &mh_hex_all(42))]),
        );
        gb.upsert_node(
            "Function",
            "c",
            "pkg.c",
            "c.rs",
            1,
            5,
            props(&[("fp", &mh_hex_all(42))]),
        );

        compute_similar_to(&mut gb, 0.5);

        // Check dedup: each (src, tgt) pair appears at most once
        let mut seen = HashSet::new();
        gb.foreach_edge(&mut |e| {
            let key = (e.source_id, e.target_id);
            assert!(seen.insert(key), "duplicate ({:?}, {:?})", key.0, key.1);
        });
    }
}
