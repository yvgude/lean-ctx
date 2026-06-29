//! Module coupling metrics — Robert C. Martin's afferent/efferent coupling and
//! instability.
//!
//! High coupling makes a change ripple across files, which inflates the agent's
//! blast radius (and token cost) per edit. Computed from a directed dependency
//! edge list so it is pure and deterministic; the graph wiring (interconnection
//! phase) feeds real import edges from the property graph, while tests drive it
//! directly without an index.

use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// Coupling figures for one module (file).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModuleCoupling {
    pub module: String,
    /// Afferent coupling `Ca`: number of modules that depend on this one.
    pub afferent: usize,
    /// Efferent coupling `Ce`: number of modules this one depends on.
    pub efferent: usize,
    /// Instability `I = Ce / (Ca + Ce)` in `0.0..=1.0` (0 = stable, 1 = unstable).
    pub instability: f64,
}

/// Compute per-module coupling from directed dependency edges
/// `(dependent_module, dependency_module)`. Self-edges and duplicates are
/// ignored. Output is sorted by module name for determinism.
pub fn module_coupling(edges: &[(String, String)]) -> Vec<ModuleCoupling> {
    let mut unique: BTreeSet<(&str, &str)> = BTreeSet::new();
    for (from, to) in edges {
        if from != to {
            unique.insert((from.as_str(), to.as_str()));
        }
    }

    let mut efferent: BTreeMap<&str, usize> = BTreeMap::new();
    let mut afferent: BTreeMap<&str, usize> = BTreeMap::new();
    let mut modules: BTreeSet<&str> = BTreeSet::new();
    for (from, to) in &unique {
        *efferent.entry(from).or_default() += 1;
        *afferent.entry(to).or_default() += 1;
        modules.insert(from);
        modules.insert(to);
    }

    modules
        .into_iter()
        .map(|m| {
            let ce = efferent.get(m).copied().unwrap_or(0);
            let ca = afferent.get(m).copied().unwrap_or(0);
            let instability = if ca + ce == 0 {
                0.0
            } else {
                ce as f64 / (ca + ce) as f64
            };
            ModuleCoupling {
                module: m.to_string(),
                afferent: ca,
                efferent: ce,
                instability,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edge(a: &str, b: &str) -> (String, String) {
        (a.to_string(), b.to_string())
    }

    #[test]
    fn computes_ca_ce_and_instability() {
        // a -> b, a -> c, d -> a
        let edges = [edge("a", "b"), edge("a", "c"), edge("d", "a")];
        let cps = module_coupling(&edges);
        let a = cps.iter().find(|c| c.module == "a").unwrap();
        assert_eq!(a.efferent, 2, "a depends on b and c");
        assert_eq!(a.afferent, 1, "d depends on a");
        assert!((a.instability - (2.0 / 3.0)).abs() < 1e-9);
    }

    #[test]
    fn dedups_and_ignores_self_edges() {
        let edges = [edge("a", "b"), edge("a", "b"), edge("a", "a")];
        let cps = module_coupling(&edges);
        let a = cps.iter().find(|c| c.module == "a").unwrap();
        assert_eq!(a.efferent, 1);
        assert!(
            (a.instability - 1.0).abs() < 1e-9,
            "only outgoing => unstable"
        );
    }

    #[test]
    fn stable_module_has_zero_instability() {
        let edges = [edge("x", "core"), edge("y", "core")];
        let cps = module_coupling(&edges);
        let core = cps.iter().find(|c| c.module == "core").unwrap();
        assert_eq!(core.afferent, 2);
        assert_eq!(core.efferent, 0);
        assert!((core.instability - 0.0).abs() < 1e-9);
    }

    #[test]
    fn deterministic_order() {
        let edges = [edge("b", "a"), edge("c", "a")];
        assert_eq!(module_coupling(&edges), module_coupling(&edges));
    }
}
