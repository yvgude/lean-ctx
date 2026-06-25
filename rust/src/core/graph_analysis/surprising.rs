//! Surprising connections: dependency edges that bridge otherwise-unrelated
//! parts of the codebase.
//!
//! Composite score per edge `(u, v)`:
//!   `(1 - jaccard(N(u), N(v))) * cross_community_factor * ln(1 + deg(u) + deg(v))`
//!
//! High score = the two files share few neighbours (low overlap), live in
//! different communities, and are non-trivially connected — i.e. an unexpected
//! coupling worth a human's attention (graphify-style).

use std::collections::{HashMap, HashSet};

use serde::Serialize;

use super::dependency_edges;
use crate::core::graph_provider::EdgeInfo;

/// An unexpected coupling between two files.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SurprisingConnection {
    pub from: String,
    pub to: String,
    pub score: f64,
    pub cross_community: bool,
    pub shared_neighbors: usize,
}

/// Returns the top `limit` surprising connections, highest score first.
/// Deterministic. `community` maps file path → community id (may be partial).
#[must_use]
pub fn find_surprising_connections(
    edges: &[EdgeInfo],
    community: &HashMap<String, usize>,
    limit: usize,
) -> Vec<SurprisingConnection> {
    let deps = dependency_edges(edges);
    if deps.is_empty() {
        return Vec::new();
    }

    // Undirected neighbour sets over dependency edges.
    let mut adj: HashMap<&str, HashSet<&str>> = HashMap::new();
    for (u, v) in &deps {
        adj.entry(*u).or_default().insert(*v);
        adj.entry(*v).or_default().insert(*u);
    }

    let empty: HashSet<&str> = HashSet::new();
    let mut seen: HashSet<(&str, &str)> = HashSet::new();
    let mut out: Vec<SurprisingConnection> = Vec::new();

    for (u, v) in &deps {
        // Canonical undirected key so a↔b is scored once.
        let key = if u <= v { (*u, *v) } else { (*v, *u) };
        if !seen.insert(key) {
            continue;
        }

        let nu = adj.get(*u).unwrap_or(&empty);
        let nv = adj.get(*v).unwrap_or(&empty);
        if nu.len() < 2 || nv.len() < 2 {
            continue; // trivial leaf edge: not "surprising", just sparse
        }

        let shared = nu
            .intersection(nv)
            .filter(|n| **n != *u && **n != *v)
            .count();
        let mut union: HashSet<&str> = nu.iter().copied().collect();
        union.extend(nv.iter().copied());
        union.remove(*u);
        union.remove(*v);
        let union_len = union.len().max(1);
        let jaccard = shared as f64 / union_len as f64;

        let cross = match (community.get(*u), community.get(*v)) {
            (Some(a), Some(b)) => a != b,
            _ => true,
        };

        let deg = (nu.len() + nv.len()) as f64;
        let score = (1.0 - jaccard) * (if cross { 1.0 } else { 0.35 }) * (1.0 + deg).ln();

        out.push(SurprisingConnection {
            from: (*u).to_string(),
            to: (*v).to_string(),
            score: (score * 1000.0).round() / 1000.0,
            cross_community: cross,
            shared_neighbors: shared,
        });
    }

    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.from.cmp(&b.from))
            .then_with(|| a.to.cmp(&b.to))
    });
    out.truncate(limit);
    out
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
    fn bridge_edge_ranks_above_intra_cluster_edge() {
        // Two triangles {a,b,c} and {x,y,z}, joined by a single bridge c<->x.
        let edges = vec![
            e("a", "b"),
            e("b", "c"),
            e("a", "c"),
            e("x", "y"),
            e("y", "z"),
            e("x", "z"),
            e("c", "x"), // the surprising bridge
        ];
        let mut community = HashMap::new();
        for n in ["a", "b", "c"] {
            community.insert(n.to_string(), 0);
        }
        for n in ["x", "y", "z"] {
            community.insert(n.to_string(), 1);
        }

        let surprising = find_surprising_connections(&edges, &community, 10);
        assert!(!surprising.is_empty());
        let top = &surprising[0];
        assert_eq!((top.from.as_str(), top.to.as_str()), ("c", "x"));
        assert!(top.cross_community);
    }

    #[test]
    fn empty_without_dependency_edges() {
        let edges = vec![EdgeInfo {
            from: "a".into(),
            to: "b".into(),
            kind: "sibling".into(),
            weight: 1.0,
        }];
        assert!(find_surprising_connections(&edges, &HashMap::new(), 10).is_empty());
    }
}
