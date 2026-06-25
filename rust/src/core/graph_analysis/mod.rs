//! Static dependency-graph analyses (God-Nodes, import cycles).
//!
//! These run on the *real* directed dependency edges — `import` and `reexport` —
//! and deliberately exclude the co-location heuristics (`sibling`, `cochange`)
//! and the ambiguous `module` edges. That way the results reflect genuine code
//! dependencies (graphify-style) instead of directory layout coincidences.

mod centrality;
mod cycles;
mod god_nodes;
mod surprising;

pub use centrality::{
    BridgeCentrality, BridgeNode, compute_bridge_centrality, compute_bridge_nodes,
};
pub use cycles::{ImportCycle, find_import_cycles};
pub use god_nodes::{GodNode, compute_god_nodes};
pub use surprising::{SurprisingConnection, find_surprising_connections};

use crate::core::graph_provider::EdgeInfo;

/// Edge kinds that represent a genuine *directed* code dependency
/// (`from` depends on `to`).
pub const DEP_EDGE_KINDS: [&str; 2] = ["import", "reexport"];

/// True when an edge kind is a genuine directed dependency.
#[must_use]
pub fn is_dependency_kind(kind: &str) -> bool {
    DEP_EDGE_KINDS.contains(&kind)
}

/// Directed dependency edges as `(from, to)` pairs, with self-loops removed.
/// Borrows from `edges` to avoid allocating new strings.
#[must_use]
pub fn dependency_edges(edges: &[EdgeInfo]) -> Vec<(&str, &str)> {
    edges
        .iter()
        .filter(|e| is_dependency_kind(&e.kind))
        .filter(|e| e.from != e.to)
        .map(|e| (e.from.as_str(), e.to.as_str()))
        .collect()
}

/// Confidence that an edge represents a *real* relationship, in `0.0..=1.0`.
///
/// Explicit code references (`import`/`reexport`) are certain; structural
/// `module` grouping is fairly reliable; the co-location/co-change heuristics
/// are weaker and scale with their observed `weight` (e.g. how often two files
/// changed together). The dashboard styles edges by this score so heuristic
/// links read as faint/dashed while real dependencies stay solid (#273).
#[must_use]
pub fn edge_confidence(kind: &str, weight: f64) -> f64 {
    match kind {
        "import" => 1.0,
        "reexport" => 0.95,
        "module" => 0.6,
        // More co-changes ⇒ more confidence, with diminishing returns.
        "cochange" => (0.30 + weight.max(0.0).ln_1p() * 0.18).clamp(0.30, 0.85),
        // Learned co-access (files opened together in real sessions, #289): a
        // behavioural signal that strengthens with repeated reinforcement.
        "co_access" => (0.35 + weight.max(0.0).ln_1p() * 0.16).clamp(0.35, 0.80),
        "sibling" => 0.25,
        _ => 0.5,
    }
}

#[cfg(test)]
mod confidence_tests {
    use super::edge_confidence;

    #[test]
    fn explicit_refs_rank_above_heuristics() {
        let import = edge_confidence("import", 0.0);
        let reexport = edge_confidence("reexport", 0.0);
        let module = edge_confidence("module", 0.0);
        let sibling = edge_confidence("sibling", 0.0);
        assert!(import >= reexport);
        assert!(reexport > module);
        assert!(module > sibling);
        assert!((0.0..=1.0).contains(&sibling));
    }

    #[test]
    fn cochange_scales_with_weight_and_is_bounded() {
        let low = edge_confidence("cochange", 1.0);
        let high = edge_confidence("cochange", 50.0);
        assert!(high > low, "more co-changes should raise confidence");
        assert!((0.30..=0.85).contains(&low));
        assert!((0.30..=0.85).contains(&high));
    }

    #[test]
    fn unknown_kind_is_neutral() {
        assert_eq!(edge_confidence("mystery", 0.0), 0.5);
    }

    #[test]
    fn co_access_scales_with_weight_and_is_bounded() {
        let low = edge_confidence("co_access", 1.0);
        let high = edge_confidence("co_access", 50.0);
        assert!(high > low, "more reinforcement should raise confidence");
        assert!((0.35..=0.80).contains(&low));
        assert!((0.35..=0.80).contains(&high));
        // Behavioural co-access sits above pure co-location (sibling) but below
        // an explicit import.
        assert!(low > edge_confidence("sibling", 0.0));
        assert!(high < edge_confidence("import", 0.0));
    }
}
