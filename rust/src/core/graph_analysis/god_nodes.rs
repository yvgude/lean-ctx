//! God-Nodes: the most connected abstractions in the dependency graph.

use std::collections::{HashMap, HashSet};

use serde::Serialize;

use super::dependency_edges;
use crate::core::graph_provider::EdgeInfo;

/// A highly connected file in the dependency graph.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct GodNode {
    pub path: String,
    /// Files that depend on this one (fan-in).
    pub in_degree: usize,
    /// Files this one depends on (fan-out).
    pub out_degree: usize,
    /// `in_degree + out_degree`.
    pub degree: usize,
}

/// Ranks files by total dependency degree (fan-in + fan-out) and returns the top
/// `limit`. Deterministic: ties broken by path so the output is stable across
/// rebuilds.
#[must_use]
pub fn compute_god_nodes(edges: &[EdgeInfo], limit: usize) -> Vec<GodNode> {
    let deps = dependency_edges(edges);
    let mut incoming: HashMap<&str, usize> = HashMap::new();
    let mut outgoing: HashMap<&str, usize> = HashMap::new();
    let mut nodes: HashSet<&str> = HashSet::new();

    for (from, to) in &deps {
        *outgoing.entry(*from).or_default() += 1;
        *incoming.entry(*to).or_default() += 1;
        nodes.insert(*from);
        nodes.insert(*to);
    }

    let mut ranked: Vec<GodNode> = nodes
        .into_iter()
        .map(|n| {
            let in_degree = incoming.get(n).copied().unwrap_or(0);
            let out_degree = outgoing.get(n).copied().unwrap_or(0);
            GodNode {
                path: n.to_string(),
                in_degree,
                out_degree,
                degree: in_degree + out_degree,
            }
        })
        .collect();

    ranked.sort_by(|a, b| b.degree.cmp(&a.degree).then_with(|| a.path.cmp(&b.path)));
    ranked.truncate(limit);
    ranked
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(from: &str, to: &str, kind: &str) -> EdgeInfo {
        EdgeInfo {
            from: from.into(),
            to: to.into(),
            kind: kind.into(),
            weight: 1.0,
        }
    }

    #[test]
    fn ranks_by_total_degree() {
        let edges = vec![
            e("a.rs", "b.rs", "import"),
            e("a.rs", "c.rs", "import"),
            e("d.rs", "a.rs", "import"),
        ];
        let god = compute_god_nodes(&edges, 10);
        assert_eq!(god[0].path, "a.rs");
        assert_eq!(god[0].out_degree, 2);
        assert_eq!(god[0].in_degree, 1);
        assert_eq!(god[0].degree, 3);
    }

    #[test]
    fn ignores_heuristic_edges() {
        // sibling / cochange are co-location heuristics, not dependencies.
        let edges = vec![e("a.rs", "b.rs", "sibling"), e("a.rs", "c.rs", "cochange")];
        assert!(compute_god_nodes(&edges, 10).is_empty());
    }

    #[test]
    fn respects_limit_and_is_deterministic() {
        let edges = vec![
            e("a.rs", "x.rs", "import"),
            e("b.rs", "x.rs", "import"),
            e("c.rs", "x.rs", "import"),
        ];
        let god = compute_god_nodes(&edges, 2);
        assert_eq!(god.len(), 2);
        // x.rs has degree 3 (top); next are a/b/c with degree 1, tie-broken by path.
        assert_eq!(god[0].path, "x.rs");
        assert_eq!(god[1].path, "a.rs");
    }
}
