//! Import-cycle detection via strongly-connected components.
//!
//! A cycle is an SCC of size >= 2 in the directed dependency graph: a group of
//! files that (transitively) import each other. Tarjan's algorithm is used in an
//! *iterative* form so deep dependency chains in large repos cannot overflow the
//! stack.

use std::collections::HashMap;

use serde::Serialize;

use super::dependency_edges;
use crate::core::graph_provider::EdgeInfo;

/// A circular dependency: a set of files that import each other.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ImportCycle {
    pub files: Vec<String>,
    pub size: usize,
}

/// Finds import cycles (SCCs of size >= 2), largest first, capped at `limit`.
/// Output is deterministic.
#[must_use]
pub fn find_import_cycles(edges: &[EdgeInfo], limit: usize) -> Vec<ImportCycle> {
    let deps = dependency_edges(edges);
    if deps.is_empty() {
        return Vec::new();
    }

    // Intern node names to dense indices and build the adjacency list.
    let mut idx_of: HashMap<&str, usize> = HashMap::new();
    let mut names: Vec<&str> = Vec::new();
    let mut adj: Vec<Vec<usize>> = Vec::new();
    for (from, to) in &deps {
        for s in [*from, *to] {
            if !idx_of.contains_key(s) {
                idx_of.insert(s, names.len());
                names.push(s);
                adj.push(Vec::new());
            }
        }
    }
    for (from, to) in &deps {
        let f = idx_of[*from];
        let t = idx_of[*to];
        adj[f].push(t);
    }

    let sccs = tarjan_scc(&adj);

    let mut cycles: Vec<ImportCycle> = sccs
        .into_iter()
        .filter(|c| c.len() >= 2)
        .map(|c| {
            let mut files: Vec<String> = c.into_iter().map(|i| names[i].to_string()).collect();
            files.sort();
            ImportCycle {
                size: files.len(),
                files,
            }
        })
        .collect();

    cycles.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.files.cmp(&b.files)));
    cycles.truncate(limit);
    cycles
}

/// Iterative Tarjan strongly-connected-components.
fn tarjan_scc(adj: &[Vec<usize>]) -> Vec<Vec<usize>> {
    let n = adj.len();
    const UNVISITED: usize = usize::MAX;

    let mut indices = vec![UNVISITED; n];
    let mut lowlink = vec![0usize; n];
    let mut on_stack = vec![false; n];
    let mut tarjan_stack: Vec<usize> = Vec::new();
    let mut sccs: Vec<Vec<usize>> = Vec::new();
    let mut counter = 0usize;

    for start in 0..n {
        if indices[start] != UNVISITED {
            continue;
        }
        // Explicit DFS stack of (node, next-neighbour-index).
        let mut call_stack: Vec<(usize, usize)> = vec![(start, 0)];
        while let Some(&(v, edge_i)) = call_stack.last() {
            if edge_i == 0 {
                indices[v] = counter;
                lowlink[v] = counter;
                counter += 1;
                tarjan_stack.push(v);
                on_stack[v] = true;
            }

            if edge_i < adj[v].len() {
                call_stack.last_mut().unwrap().1 += 1;
                let w = adj[v][edge_i];
                if indices[w] == UNVISITED {
                    call_stack.push((w, 0));
                } else if on_stack[w] {
                    lowlink[v] = lowlink[v].min(indices[w]);
                }
            } else {
                // v fully explored: if it is an SCC root, pop the component.
                if lowlink[v] == indices[v] {
                    let mut component = Vec::new();
                    loop {
                        let w = tarjan_stack.pop().expect("tarjan stack non-empty");
                        on_stack[w] = false;
                        component.push(w);
                        if w == v {
                            break;
                        }
                    }
                    sccs.push(component);
                }
                call_stack.pop();
                if let Some(&(parent, _)) = call_stack.last() {
                    lowlink[parent] = lowlink[parent].min(lowlink[v]);
                }
            }
        }
    }

    sccs
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
    fn detects_three_node_cycle() {
        let edges = vec![
            e("a.rs", "b.rs", "import"),
            e("b.rs", "c.rs", "import"),
            e("c.rs", "a.rs", "import"),
            e("a.rs", "d.rs", "import"), // d is a non-cyclic dependency
        ];
        let cycles = find_import_cycles(&edges, 10);
        assert_eq!(cycles.len(), 1);
        assert_eq!(cycles[0].size, 3);
        assert_eq!(cycles[0].files, vec!["a.rs", "b.rs", "c.rs"]);
    }

    #[test]
    fn detects_two_node_cycle() {
        let edges = vec![e("a.rs", "b.rs", "import"), e("b.rs", "a.rs", "reexport")];
        let cycles = find_import_cycles(&edges, 10);
        assert_eq!(cycles.len(), 1);
        assert_eq!(cycles[0].files, vec!["a.rs", "b.rs"]);
    }

    #[test]
    fn acyclic_graph_has_no_cycles() {
        let edges = vec![e("a.rs", "b.rs", "import"), e("b.rs", "c.rs", "import")];
        assert!(find_import_cycles(&edges, 10).is_empty());
    }

    #[test]
    fn self_loops_and_heuristics_excluded() {
        let edges = vec![
            e("a.rs", "a.rs", "import"),   // self-loop: not a cycle
            e("b.rs", "c.rs", "sibling"),  // heuristic, ignored
            e("c.rs", "b.rs", "cochange"), // heuristic, ignored
        ];
        assert!(find_import_cycles(&edges, 10).is_empty());
    }

    #[test]
    fn two_separate_cycles_sorted_by_size() {
        let edges = vec![
            // 2-cycle
            e("x.rs", "y.rs", "import"),
            e("y.rs", "x.rs", "import"),
            // 3-cycle
            e("a.rs", "b.rs", "import"),
            e("b.rs", "c.rs", "import"),
            e("c.rs", "a.rs", "import"),
        ];
        let cycles = find_import_cycles(&edges, 10);
        assert_eq!(cycles.len(), 2);
        assert_eq!(cycles[0].size, 3); // largest first
        assert_eq!(cycles[1].size, 2);
    }
}
