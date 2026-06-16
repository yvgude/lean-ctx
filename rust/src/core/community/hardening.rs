//! Robustness passes layered on top of the base Leiden partition.
//!
//! Real code graphs have pathological structure that vanilla modularity handles
//! poorly:
//!   * **Super-hubs** (a `utils` module imported everywhere) bridge unrelated
//!     clusters and collapse them into one giant blob. We detect hubs, partition
//!     the graph *without* them, then reattach each hub by majority vote.
//!   * **Oversized / low-cohesion communities** still slip through. We recursively
//!     re-partition any community that is too large or too loosely connected.

use std::collections::{HashMap, HashSet};

use super::graph::{AdjGraph, cohesion_of};
use super::leiden;

const HUB_MIN_NODES: usize = 16;
const HUB_PERCENTILE: f64 = 0.95;
const HUB_MEDIAN_FACTOR: f64 = 2.0;
const HUB_MAX_FRACTION: f64 = 0.10;

const MAX_COMMUNITY_FRACTION: f64 = 0.25;
const RESPLIT_MIN_SIZE: usize = 8;
const RESPLIT_MIN_COHESION: f64 = 0.25;
const MAX_RESPLIT_DEPTH: usize = 3;

/// Partition with hub-exclusion. Falls back to a plain partition when the graph
/// is too small or has no clear hubs.
pub(super) fn partition_with_hub_exclusion(graph: &AdjGraph) -> Vec<usize> {
    let n = graph.node_count();
    if n == 0 {
        return Vec::new();
    }

    let hubs = detect_hubs(graph);
    if hubs.is_empty() {
        return leiden::partition(graph);
    }

    let hub_set: HashSet<usize> = hubs.iter().copied().collect();
    let members: Vec<usize> = (0..n).filter(|i| !hub_set.contains(i)).collect();
    if members.len() < 2 {
        return leiden::partition(graph);
    }

    let (sub, local_to_global) = graph.induced_subgraph(&members);
    let sub_assignment = leiden::partition(&sub);

    let mut assignment = vec![usize::MAX; n];
    let mut next_comm = 0usize;
    let mut comm_remap: HashMap<usize, usize> = HashMap::new();
    for (local, &global) in local_to_global.iter().enumerate() {
        let mapped = *comm_remap.entry(sub_assignment[local]).or_insert_with(|| {
            let v = next_comm;
            next_comm += 1;
            v
        });
        assignment[global] = mapped;
    }

    reattach_hubs(graph, &mut assignment, &hubs, &mut next_comm);
    assignment
}

/// Hubs = nodes whose weighted degree exceeds both the 95th-percentile degree and
/// `HUB_MEDIAN_FACTOR × median`. Only engaged on graphs with enough nodes, and
/// never excludes more than `HUB_MAX_FRACTION` of them.
fn detect_hubs(graph: &AdjGraph) -> Vec<usize> {
    let n = graph.node_count();
    if n < HUB_MIN_NODES {
        return Vec::new();
    }

    let mut degrees: Vec<f64> = graph.degree.clone();
    degrees.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = degrees[n / 2];
    let percentile_idx = ((n as f64 * HUB_PERCENTILE) as usize).min(n - 1);
    let percentile = degrees[percentile_idx];
    let threshold = percentile.max(median * HUB_MEDIAN_FACTOR);
    if threshold <= 0.0 {
        return Vec::new();
    }

    let mut hubs: Vec<usize> = (0..n).filter(|&i| graph.degree[i] > threshold).collect();
    let max_hubs = ((n as f64 * HUB_MAX_FRACTION) as usize).max(1);
    if hubs.len() > max_hubs {
        // Keep the strongest hubs only (degree desc, index asc for determinism).
        hubs.sort_by(|&a, &b| {
            graph.degree[b]
                .partial_cmp(&graph.degree[a])
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.cmp(&b))
        });
        hubs.truncate(max_hubs);
    }
    hubs.sort_unstable();
    hubs
}

/// Reattach each hub to the already-assigned community it connects to most
/// strongly. Hubs are processed strongest-first; any hub with no assigned
/// neighbour (e.g. it only touches other hubs) is given its own community.
fn reattach_hubs(
    graph: &AdjGraph,
    assignment: &mut [usize],
    hubs: &[usize],
    next_comm: &mut usize,
) {
    let mut pending: Vec<usize> = hubs.to_vec();
    pending.sort_by(|&a, &b| {
        graph.degree[b]
            .partial_cmp(&graph.degree[a])
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.cmp(&b))
    });

    loop {
        let mut progressed = false;
        let mut still_pending = Vec::new();
        for &hub in &pending {
            if let Some(comm) = best_neighbor_community(graph, assignment, hub) {
                assignment[hub] = comm;
                progressed = true;
            } else {
                still_pending.push(hub);
            }
        }
        pending = still_pending;
        if pending.is_empty() || !progressed {
            break;
        }
    }

    // Hubs still unassigned form their own communities (deterministic order).
    pending.sort_unstable();
    for hub in pending {
        assignment[hub] = *next_comm;
        *next_comm += 1;
    }
}

fn best_neighbor_community(graph: &AdjGraph, assignment: &[usize], node: usize) -> Option<usize> {
    let mut weights: HashMap<usize, f64> = HashMap::new();
    for &(j, w) in &graph.adj[node] {
        let c = assignment[j];
        if c != usize::MAX {
            *weights.entry(c).or_default() += w;
        }
    }
    weights
        .into_iter()
        .max_by(|a, b| {
            a.1.partial_cmp(&b.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(b.0.cmp(&a.0)) // tie → smaller community id
        })
        .map(|(c, _)| c)
}

/// Recursively re-partition communities that are oversized (more than
/// `MAX_COMMUNITY_FRACTION` of all nodes) or have cohesion below
/// `RESPLIT_MIN_COHESION`, up to `MAX_RESPLIT_DEPTH` levels.
pub(super) fn split_oversized_and_incohesive(graph: &AdjGraph, assignment: &mut [usize]) {
    resplit_pass(graph, assignment, 0);
}

fn resplit_pass(graph: &AdjGraph, assignment: &mut [usize], depth: usize) {
    if depth >= MAX_RESPLIT_DEPTH {
        return;
    }
    let n = graph.node_count();
    if n == 0 {
        return;
    }

    let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
    for (i, &c) in assignment.iter().enumerate() {
        groups.entry(c).or_default().push(i);
    }

    let mut next_comm = assignment.iter().copied().max().unwrap_or(0) + 1;
    let mut comm_keys: Vec<usize> = groups.keys().copied().collect();
    comm_keys.sort_unstable();

    let mut changed = false;
    for key in comm_keys {
        let members = groups[&key].clone();
        if members.len() < RESPLIT_MIN_SIZE {
            continue;
        }
        let oversized = members.len() as f64 > MAX_COMMUNITY_FRACTION * n as f64;
        let incohesive = cohesion_of(graph, &members) < RESPLIT_MIN_COHESION;
        if !oversized && !incohesive {
            continue;
        }

        let (sub, local_to_global) = graph.induced_subgraph(&members);
        let sub_assignment = leiden::partition(&sub);
        let distinct: HashSet<usize> = sub_assignment.iter().copied().collect();
        if distinct.len() <= 1 {
            continue; // indivisible — leave as-is
        }

        // Keep the first sub-community on the original id; allocate fresh ids for
        // the rest (deterministic by sub-community id).
        let mut sub_keys: Vec<usize> = distinct.into_iter().collect();
        sub_keys.sort_unstable();
        let mut relabel: HashMap<usize, usize> = HashMap::new();
        for (n_th, sub_c) in sub_keys.into_iter().enumerate() {
            let target = if n_th == 0 {
                key
            } else {
                let v = next_comm;
                next_comm += 1;
                v
            };
            relabel.insert(sub_c, target);
        }
        for (local, &global) in local_to_global.iter().enumerate() {
            assignment[global] = relabel[&sub_assignment[local]];
        }
        changed = true;
    }

    if changed {
        resplit_pass(graph, assignment, depth + 1);
    }
}
