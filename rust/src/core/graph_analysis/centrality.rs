//! Betweenness centrality (bridge nodes) via Brandes' algorithm.
//!
//! A bridge node lies on many shortest paths between other files — removing it
//! would fragment the dependency graph. Computed on the undirected dependency
//! graph (import / reexport) with the exact, unweighted Brandes algorithm
//! (O(V·E)), which is fine for repo-sized graphs.

use std::collections::{HashMap, VecDeque};

use serde::Serialize;

use super::dependency_edges;
use crate::core::graph_provider::EdgeInfo;

/// A file with high betweenness centrality.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct BridgeNode {
    pub path: String,
    /// Betweenness normalized to `0.0..=1.0` (relative to the top node).
    pub betweenness: f64,
}

/// Betweenness result with sampling provenance, so callers can disclose when the
/// values are an estimate (large graphs) rather than the exact Brandes result.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct BridgeCentrality {
    pub nodes: Vec<BridgeNode>,
    /// `true` when betweenness was estimated from a sampled subset of sources.
    pub sampled: bool,
    /// Total nodes in the dependency graph.
    pub total_nodes: usize,
    /// Source nodes actually used (== `total_nodes` unless sampled).
    pub sources_used: usize,
}

/// Returns the top `limit` bridge nodes (betweenness > 0), highest first.
/// Thin wrapper over [`compute_bridge_centrality`] for callers that don't need
/// the sampling metadata.
#[must_use]
pub fn compute_bridge_nodes(edges: &[EdgeInfo], limit: usize) -> Vec<BridgeNode> {
    compute_bridge_centrality(edges, limit).nodes
}

/// Like [`compute_bridge_nodes`] but also reports whether the result was sampled
/// and over how many sources, for honest disclosure in reports/UI.
pub fn compute_bridge_centrality(edges: &[EdgeInfo], limit: usize) -> BridgeCentrality {
    let deps = dependency_edges(edges);
    if deps.is_empty() {
        return BridgeCentrality {
            nodes: Vec::new(),
            sampled: false,
            total_nodes: 0,
            sources_used: 0,
        };
    }

    // Intern node names and build an undirected adjacency list.
    let mut index_of: HashMap<&str, usize> = HashMap::new();
    let mut names: Vec<&str> = Vec::new();
    let mut adj: Vec<Vec<usize>> = Vec::new();
    for (u, v) in &deps {
        for s in [*u, *v] {
            if !index_of.contains_key(s) {
                index_of.insert(s, names.len());
                names.push(s);
                adj.push(Vec::new());
            }
        }
    }
    for (u, v) in &deps {
        let a = index_of[*u];
        let b = index_of[*v];
        adj[a].push(b);
        adj[b].push(a);
    }
    let n = names.len();

    let mut betweenness = vec![0f64; n];

    // Exact Brandes is O(V·E). Above a threshold we estimate betweenness from a
    // deterministic sample of source nodes (Brandes–Pich): cost drops to O(k·E)
    // and, because the result is normalized to the top node, the *relative*
    // ranking of bridges is preserved.
    const EXACT_SOURCE_CAP: usize = 1500;
    const SAMPLE_SOURCES: usize = 500;
    let sampled = n > EXACT_SOURCE_CAP;
    let sources: Vec<usize> = if sampled {
        let step = (n / SAMPLE_SOURCES).max(1);
        (0..n).step_by(step).collect()
    } else {
        (0..n).collect()
    };
    let sources_used = sources.len();

    for &s in &sources {
        let mut stack: Vec<usize> = Vec::new();
        let mut preds: Vec<Vec<usize>> = vec![Vec::new(); n];
        let mut sigma = vec![0f64; n];
        let mut dist = vec![-1i64; n];
        sigma[s] = 1.0;
        dist[s] = 0;

        let mut queue: VecDeque<usize> = VecDeque::new();
        queue.push_back(s);
        while let Some(v) = queue.pop_front() {
            stack.push(v);
            for &w in &adj[v] {
                if dist[w] < 0 {
                    dist[w] = dist[v] + 1;
                    queue.push_back(w);
                }
                if dist[w] == dist[v] + 1 {
                    sigma[w] += sigma[v];
                    preds[w].push(v);
                }
            }
        }

        let mut delta = vec![0f64; n];
        while let Some(w) = stack.pop() {
            let preds_w = std::mem::take(&mut preds[w]);
            for v in preds_w {
                if sigma[w] > 0.0 {
                    delta[v] += (sigma[v] / sigma[w]) * (1.0 + delta[w]);
                }
            }
            if w != s {
                betweenness[w] += delta[w];
            }
        }
    }

    // Undirected graph: every shortest path is counted in both directions.
    for b in &mut betweenness {
        *b /= 2.0;
    }
    let max = betweenness.iter().copied().fold(0.0f64, f64::max);

    let mut nodes: Vec<BridgeNode> = (0..n)
        .map(|i| BridgeNode {
            path: names[i].to_string(),
            betweenness: if max > 0.0 {
                (betweenness[i] / max * 1000.0).round() / 1000.0
            } else {
                0.0
            },
        })
        .collect();

    nodes.sort_by(|a, b| {
        b.betweenness
            .partial_cmp(&a.betweenness)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.path.cmp(&b.path))
    });
    nodes.retain(|node| node.betweenness > 0.0);
    nodes.truncate(limit);
    BridgeCentrality {
        nodes,
        sampled,
        total_nodes: n,
        sources_used,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(from: &str, to: &str) -> EdgeInfo {
        EdgeInfo {
            from: from.into(),
            to: to.into(),
            kind: "import".into(),
            weight: 1.0,
        }
    }

    #[test]
    fn path_graph_center_is_top_bridge() {
        // a - b - c : b sits between a and c.
        let edges = vec![e("a.rs", "b.rs"), e("b.rs", "c.rs")];
        let bridges = compute_bridge_nodes(&edges, 10);
        assert_eq!(bridges.len(), 1); // only b has betweenness > 0
        assert_eq!(bridges[0].path, "b.rs");
        assert_eq!(bridges[0].betweenness, 1.0);
    }

    #[test]
    fn star_center_is_top_bridge() {
        let edges = vec![
            e("hub.rs", "a.rs"),
            e("hub.rs", "b.rs"),
            e("hub.rs", "c.rs"),
        ];
        let bridges = compute_bridge_nodes(&edges, 10);
        assert_eq!(bridges[0].path, "hub.rs");
        assert_eq!(bridges[0].betweenness, 1.0);
    }

    #[test]
    fn fully_connected_has_no_bridges() {
        // Triangle: no node is on a unique shortest path.
        let edges = vec![e("a", "b"), e("b", "c"), e("a", "c")];
        assert!(compute_bridge_nodes(&edges, 10).is_empty());
    }

    #[test]
    fn large_graph_uses_sampling_and_still_finds_hub() {
        // > EXACT_SOURCE_CAP nodes forces the sampled estimator; the star center
        // must still rank as the top bridge.
        let edges: Vec<EdgeInfo> = (0..2000)
            .map(|i| e("hub.rs", &format!("leaf{i}.rs")))
            .collect();
        let bridges = compute_bridge_nodes(&edges, 10);
        assert_eq!(bridges[0].path, "hub.rs");
        assert_eq!(bridges[0].betweenness, 1.0);
    }

    #[test]
    fn centrality_reports_sampling_provenance() {
        // Small graph → exact (not sampled), every node used as a source.
        let small = vec![e("a.rs", "b.rs"), e("b.rs", "c.rs")];
        let bc = compute_bridge_centrality(&small, 10);
        assert!(!bc.sampled, "small graph must be computed exactly");
        assert_eq!(bc.sources_used, bc.total_nodes);

        // Large graph → sampled, fewer sources than nodes, ranking preserved.
        let big: Vec<EdgeInfo> = (0..2000)
            .map(|i| e("hub.rs", &format!("leaf{i}.rs")))
            .collect();
        let bc = compute_bridge_centrality(&big, 10);
        assert!(bc.sampled, "large graph must report sampling");
        assert!(
            bc.sources_used < bc.total_nodes,
            "sampling must use fewer sources than nodes"
        );
        assert_eq!(bc.nodes[0].path, "hub.rs");
    }
}
