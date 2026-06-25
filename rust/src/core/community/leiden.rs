//! Leiden algorithm (Traag, Waltman, van Eck 2019): modularity-based clustering
//! with a connectivity refinement pass. This implementation is **deterministic**
//! — candidate communities are evaluated in ascending-id order and ties are
//! broken towards the smaller id — so the same graph always yields the same
//! partition, which is a prerequisite for stable community ids across rebuilds.

use std::collections::HashMap;

use super::graph::AdjGraph;

/// Outer Leiden iterations (local-moving + refinement). The loop also exits
/// early as soon as a local-moving pass moves no node, so on typical graphs it
/// converges in far fewer rounds; this is only the hard upper bound.
const MAX_ITERATIONS: usize = 20;
/// Hard cap on the inner local-moving sweeps within a single iteration. Each
/// sweep is O(V + E); the cap guarantees the whole partition stays bounded
/// (`O(MAX_ITERATIONS` · `MAX_LOCAL_PASSES` · (V + E))) even on very large graphs.
const MAX_LOCAL_PASSES: usize = 50;
const GAMMA: f64 = 1.0;

/// Partition a graph into communities. Returns one community id per node index.
///
/// Complexity is bounded (see `MAX_ITERATIONS` / `MAX_LOCAL_PASSES`) and the
/// algorithm converges early, so it scales to large graphs without an explicit
/// node cap; callers that need a hard node limit should pre-filter the graph.
pub(super) fn partition(graph: &AdjGraph) -> Vec<usize> {
    let n = graph.node_count();
    let mut assignment: Vec<usize> = (0..n).collect();
    if n == 0 {
        return assignment;
    }
    let m2 = graph.total_weight.max(1.0) * 2.0;

    for _ in 0..MAX_ITERATIONS {
        let moved = local_moving(graph, &mut assignment, m2);
        if !moved {
            break;
        }
        refine_communities(graph, &mut assignment, m2);
    }

    assignment
}

/// Phase 1 — local moving: greedily move each node to the neighbouring community
/// that yields the highest modularity gain.
fn local_moving(graph: &AdjGraph, assignment: &mut [usize], m2: f64) -> bool {
    let n = assignment.len();
    let mut comm_total: Vec<f64> = vec![0.0; n];
    for (i, &c) in assignment.iter().enumerate() {
        comm_total[c] += graph.degree[i];
    }

    let mut changed = false;
    let mut improved = true;
    let mut passes = 0usize;

    while improved && passes < MAX_LOCAL_PASSES {
        passes += 1;
        improved = false;
        for i in 0..n {
            let current = assignment[i];
            let ki = graph.degree[i];

            let mut neighbor_comm_weight: HashMap<usize, f64> = HashMap::new();
            for &(j, w) in &graph.adj[i] {
                *neighbor_comm_weight.entry(assignment[j]).or_default() += w;
            }

            let sigma_current = comm_total[current];
            let ki_in_current = neighbor_comm_weight.get(&current).copied().unwrap_or(0.0);
            let delta_remove = -2.0 * (ki_in_current - ki * (sigma_current - ki) / m2) / m2;

            // Deterministic candidate order (ascending community id).
            let mut candidates: Vec<(usize, f64)> =
                neighbor_comm_weight.iter().map(|(&c, &w)| (c, w)).collect();
            candidates.sort_by_key(|c| c.0);

            let mut best_delta = 0.0f64;
            let mut best_comm = current;
            for (c, ki_in) in candidates {
                if c == current {
                    continue;
                }
                let sigma_c = comm_total[c];
                let delta_add = 2.0 * (ki_in - GAMMA * ki * sigma_c / m2) / m2;
                let delta = delta_add + delta_remove;
                if delta > best_delta {
                    best_delta = delta;
                    best_comm = c;
                }
            }

            if best_comm != current {
                comm_total[current] -= ki;
                comm_total[best_comm] += ki;
                assignment[i] = best_comm;
                improved = true;
                changed = true;
            }
        }
    }

    changed
}

/// Phase 2 — refinement: split disconnected communities into their connected
/// components, then try to absorb leftover singletons into a neighbour.
fn refine_communities(graph: &AdjGraph, assignment: &mut [usize], m2: f64) {
    let mut comm_members: HashMap<usize, Vec<usize>> = HashMap::new();
    for (i, &c) in assignment.iter().enumerate() {
        comm_members.entry(c).or_default().push(i);
    }

    let mut next_id = *assignment.iter().max().unwrap_or(&0) + 1;

    // Deterministic processing order.
    let mut comm_keys: Vec<usize> = comm_members.keys().copied().collect();
    comm_keys.sort_unstable();

    for key in comm_keys {
        let members = &comm_members[&key];
        if members.len() <= 1 {
            continue;
        }
        let components = find_connected_components(graph, members);
        if components.len() <= 1 {
            continue;
        }
        for component in components.iter().skip(1) {
            let new_comm = next_id;
            next_id += 1;
            for &node in component {
                assignment[node] = new_comm;
            }
        }
    }

    merge_singleton_communities(graph, assignment, m2);
}

/// Connected components within `members`, returned in ascending start-node order.
pub(super) fn find_connected_components(graph: &AdjGraph, members: &[usize]) -> Vec<Vec<usize>> {
    let member_set: std::collections::HashSet<usize> = members.iter().copied().collect();
    let mut visited = std::collections::HashSet::new();
    let mut components = Vec::new();

    let mut ordered: Vec<usize> = members.to_vec();
    ordered.sort_unstable();

    for start in ordered {
        if visited.contains(&start) {
            continue;
        }
        let mut component = Vec::new();
        let mut stack = vec![start];
        while let Some(node) = stack.pop() {
            if !visited.insert(node) {
                continue;
            }
            component.push(node);
            for &(neighbor, _) in &graph.adj[node] {
                if member_set.contains(&neighbor) && !visited.contains(&neighbor) {
                    stack.push(neighbor);
                }
            }
        }
        component.sort_unstable();
        components.push(component);
    }

    components
}

/// Absorb singleton communities into the neighbour community with the strongest
/// connection (ties broken towards the smaller community id).
fn merge_singleton_communities(graph: &AdjGraph, assignment: &mut [usize], m2: f64) {
    let n = assignment.len();
    let max_comm = assignment.iter().copied().max().unwrap_or(0);
    let mut comm_total: Vec<f64> = vec![0.0; max_comm + 1];
    for (i, &c) in assignment.iter().enumerate() {
        comm_total[c] += graph.degree[i];
    }

    let mut comm_sizes: HashMap<usize, usize> = HashMap::new();
    for &c in assignment.iter() {
        *comm_sizes.entry(c).or_default() += 1;
    }

    for i in 0..n {
        let current = assignment[i];
        if *comm_sizes.get(&current).unwrap_or(&0) > 1 {
            continue;
        }

        let ki = graph.degree[i];
        let mut neighbor_comm_weight: HashMap<usize, f64> = HashMap::new();
        for &(j, w) in &graph.adj[i] {
            *neighbor_comm_weight.entry(assignment[j]).or_default() += w;
        }

        let mut candidates: Vec<(usize, f64)> =
            neighbor_comm_weight.iter().map(|(&c, &w)| (c, w)).collect();
        candidates.sort_by_key(|c| c.0);

        let mut best_delta = 0.0f64;
        let mut best_comm = current;
        for (c, ki_in) in candidates {
            if c == current {
                continue;
            }
            let sigma_c = comm_total.get(c).copied().unwrap_or(0.0);
            let delta = 2.0 * (ki_in - GAMMA * ki * sigma_c / m2) / m2;
            if delta > best_delta {
                best_delta = delta;
                best_comm = c;
            }
        }

        if best_comm != current {
            comm_total[current] -= ki;
            comm_total[best_comm] += ki;
            *comm_sizes.entry(current).or_default() -= 1;
            *comm_sizes.entry(best_comm).or_default() += 1;
            assignment[i] = best_comm;
        }
    }
}

/// Newman modularity of an assignment (`-0.5..=1.0`).
pub(super) fn compute_modularity(graph: &AdjGraph, community: &[usize]) -> f64 {
    let m2 = graph.total_weight.max(1.0) * 2.0;
    let mut q = 0.0;
    for (i, neighbors) in graph.adj.iter().enumerate() {
        for &(j, w) in neighbors {
            if community[i] == community[j] {
                let ki = graph.degree[i];
                let kj = graph.degree[j];
                q += w - (ki * kj) / m2;
            }
        }
    }
    q / m2
}
