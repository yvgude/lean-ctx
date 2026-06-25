//! Community detection on the code graph.
//!
//! A hardened Leiden engine (`leiden`) clusters files into cohesive modules,
//! with robustness passes for super-hubs and oversized/low-cohesion communities
//! (`hardening`) and **stable ids across rebuilds** (`stable_ids`). The
//! engine is storage-agnostic: it runs on the `PropertyGraph` (SQLite) and on any
//! [`GraphProvider`], so `ctx_architecture` and the dashboard graph view share
//! one implementation and report identical community ids.

use std::collections::HashMap;

use rusqlite::Connection;
use serde::Serialize;

use crate::core::graph_provider::GraphProvider;

mod graph;
mod hardening;
mod leiden;
mod stable_ids;
#[cfg(test)]
mod tests;

use graph::{AdjGraph, edge_counts};

#[derive(Debug, Clone, Serialize)]
pub struct Community {
    pub id: usize,
    pub files: Vec<String>,
    pub internal_edges: usize,
    pub external_edges: usize,
    pub cohesion: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CommunityResult {
    pub communities: Vec<Community>,
    pub modularity: f64,
    pub node_count: usize,
    pub edge_count: usize,
}

impl CommunityResult {
    fn empty() -> Self {
        Self {
            communities: Vec::new(),
            modularity: 0.0,
            node_count: 0,
            edge_count: 0,
        }
    }

    /// `file_path → community_id` for every assigned node.
    #[must_use]
    pub fn assignment(&self) -> HashMap<String, usize> {
        self.assignment_min_size(1)
    }

    /// `file_path → community_id`, restricted to communities with at least
    /// `min_size` members. Singletons (isolated files) are dropped so the
    /// dashboard can fall back to a neutral/language colour instead of painting
    /// every orphan file a distinct hue.
    #[must_use]
    pub fn assignment_min_size(&self, min_size: usize) -> HashMap<String, usize> {
        let mut map = HashMap::new();
        for community in &self.communities {
            if community.files.len() < min_size {
                continue;
            }
            for file in &community.files {
                map.insert(file.clone(), community.id);
            }
        }
        map
    }
}

/// Detect communities on the `PropertyGraph` (ids are deterministic but not
/// remapped to a previous run). Prefer [`detect_communities_stable`] when a
/// project root is available.
pub fn detect_communities(conn: &Connection) -> CommunityResult {
    let graph = AdjGraph::from_property_graph(conn);
    analyze(&graph, None).1
}

/// Detect communities on the `PropertyGraph` with ids kept stable across rebuilds
/// (remapped to, and persisted alongside, the project's previous assignment).
pub fn detect_communities_stable(conn: &Connection, project_root: &str) -> CommunityResult {
    let graph = AdjGraph::from_property_graph(conn);
    detect_stable(&graph, project_root)
}

/// Detect communities on any [`GraphProvider`] (`PropertyGraph` or graph index)
/// with stable ids. This is the entry point for the dashboard graph view.
pub fn detect_communities_for_provider(gp: &GraphProvider, project_root: &str) -> CommunityResult {
    let graph = AdjGraph::from_provider(gp);
    detect_stable(&graph, project_root)
}

fn detect_stable(graph: &AdjGraph, project_root: &str) -> CommunityResult {
    if graph.node_count() == 0 {
        return CommunityResult::empty();
    }
    let previous = stable_ids::load_previous(project_root);
    let (assignment, result) = analyze(graph, previous.as_ref());
    stable_ids::save_assignment(project_root, &graph.node_ids, &assignment);
    result
}

/// Full pipeline: partition (hub-aware) → resplit → canonicalize → optional
/// remap to the previous assignment. Returns the final per-node assignment and
/// the presentation-ready result.
fn analyze(
    graph: &AdjGraph,
    previous: Option<&HashMap<String, usize>>,
) -> (Vec<usize>, CommunityResult) {
    let n = graph.node_count();
    if n == 0 {
        return (Vec::new(), CommunityResult::empty());
    }

    let mut assignment = hardening::partition_with_hub_exclusion(graph);
    hardening::split_oversized_and_incohesive(graph, &mut assignment);
    let mut assignment = stable_ids::canonicalize(graph, &assignment);
    if let Some(prev) = previous
        && !prev.is_empty()
    {
        assignment = stable_ids::remap_to_previous(graph, &assignment, prev);
    }

    let result = build_result(graph, &assignment);
    (assignment, result)
}

fn build_result(graph: &AdjGraph, assignment: &[usize]) -> CommunityResult {
    let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
    for (i, &c) in assignment.iter().enumerate() {
        groups.entry(c).or_default().push(i);
    }

    let mut communities: Vec<Community> = groups
        .into_iter()
        .map(|(id, mut members)| {
            members.sort_unstable();
            let (internal, external) = edge_counts(graph, &members);
            let total = (internal + external).max(1) as f64;
            Community {
                id,
                files: members.iter().map(|&i| graph.node_ids[i].clone()).collect(),
                internal_edges: internal,
                external_edges: external,
                cohesion: internal as f64 / total,
            }
        })
        .collect();

    // Largest, most cohesive communities first; ids stay meaningful/stable.
    communities.sort_by(|a, b| {
        b.files
            .len()
            .cmp(&a.files.len())
            .then(
                b.cohesion
                    .partial_cmp(&a.cohesion)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
            .then(a.id.cmp(&b.id))
    });

    CommunityResult {
        communities,
        modularity: leiden::compute_modularity(graph, assignment),
        node_count: graph.node_count(),
        edge_count: graph.edge_count(),
    }
}
