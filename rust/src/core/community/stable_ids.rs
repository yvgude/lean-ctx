//! Stable community ids across rebuilds.
//!
//! Two mechanisms keep ids from churning when the graph changes slightly:
//!   1. [`canonicalize`] gives every partition a deterministic 0..k labelling
//!      (largest community first, ties broken by cohesion then smallest member).
//!   2. [`remap_to_previous`] re-labels a fresh partition to match the previous
//!      one by maximum membership overlap, so a community that survives a rebuild
//!      keeps its id. The previous assignment is persisted next to the graph
//!      index ([`load_previous`] / [`save_assignment`]).

use std::collections::HashMap;
use std::path::PathBuf;

use super::graph::{AdjGraph, cohesion_of};
use crate::core::graph_index::ProjectIndex;

/// Relabel an assignment into a deterministic `0..k` ordering.
pub(super) fn canonicalize(graph: &AdjGraph, assignment: &[usize]) -> Vec<usize> {
    let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
    for (i, &c) in assignment.iter().enumerate() {
        groups.entry(c).or_default().push(i);
    }

    // (size, cohesion, smallest-member-index, members)
    let mut ranked: Vec<(usize, f64, usize, Vec<usize>)> = groups
        .into_values()
        .map(|mut members| {
            members.sort_unstable();
            let cohesion = cohesion_of(graph, &members);
            let min_member = members[0];
            (members.len(), cohesion, min_member, members)
        })
        .collect();

    ranked.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then(b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal))
            .then(a.2.cmp(&b.2))
    });

    let mut out = vec![0usize; assignment.len()];
    for (new_id, (_, _, _, members)) in ranked.iter().enumerate() {
        for &i in members {
            out[i] = new_id;
        }
    }
    out
}

/// Re-label `assignment` so surviving communities keep their previous id.
///
/// Greedy by overlap: each current community claims the previous id it shares the
/// most members with (one previous id can be claimed once). Communities with no
/// overlap — or whose best previous id was already claimed — get fresh ids above
/// the previous maximum, so a reused id and a fresh id can never collide.
pub(super) fn remap_to_previous(
    graph: &AdjGraph,
    assignment: &[usize],
    prev: &HashMap<String, usize>,
) -> Vec<usize> {
    let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
    for (i, &c) in assignment.iter().enumerate() {
        groups.entry(c).or_default().push(i);
    }

    // For each current community: its overlap histogram against previous ids.
    struct Candidate {
        current: usize,
        size: usize,
        overlap: Vec<(usize, usize)>, // (prev_id, count), sorted by count desc, id asc
    }

    let mut candidates: Vec<Candidate> = groups
        .into_iter()
        .map(|(current, members)| {
            let mut hist: HashMap<usize, usize> = HashMap::new();
            for &i in &members {
                if let Some(&p) = prev.get(&graph.node_ids[i]) {
                    *hist.entry(p).or_default() += 1;
                }
            }
            let mut overlap: Vec<(usize, usize)> = hist.into_iter().collect();
            overlap.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            Candidate {
                current,
                size: members.len(),
                overlap,
            }
        })
        .collect();

    // Resolve strongest matches first.
    candidates.sort_by(|a, b| {
        let ao = a.overlap.first().map_or(0, |x| x.1);
        let bo = b.overlap.first().map_or(0, |x| x.1);
        bo.cmp(&ao)
            .then(b.size.cmp(&a.size))
            .then(a.current.cmp(&b.current))
    });

    let mut used_prev: HashMap<usize, bool> = HashMap::new();
    let mut current_to_final: HashMap<usize, usize> = HashMap::new();
    let mut fresh = prev.values().copied().max().map_or(0, |m| m + 1);

    for cand in &candidates {
        let mut chosen = None;
        for &(prev_id, count) in &cand.overlap {
            if count == 0 {
                break;
            }
            if !used_prev.get(&prev_id).copied().unwrap_or(false) {
                chosen = Some(prev_id);
                break;
            }
        }
        let final_id = if let Some(p) = chosen {
            used_prev.insert(p, true);
            p
        } else {
            let v = fresh;
            fresh += 1;
            v
        };
        current_to_final.insert(cand.current, final_id);
    }

    assignment.iter().map(|c| current_to_final[c]).collect()
}

fn sidecar_path(project_root: &str) -> Option<PathBuf> {
    ProjectIndex::index_dir(project_root).map(|dir| dir.join("communities.json"))
}

/// Load the previously persisted `file_path → community_id` assignment, if any.
pub(super) fn load_previous(project_root: &str) -> Option<HashMap<String, usize>> {
    let path = sidecar_path(project_root)?;
    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

/// Persist `file_path → community_id` next to the graph index (atomic rename).
pub(super) fn save_assignment(project_root: &str, node_ids: &[String], assignment: &[usize]) {
    let Some(path) = sidecar_path(project_root) else {
        return;
    };
    let Some(dir) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }

    let map: HashMap<&str, usize> = node_ids
        .iter()
        .zip(assignment.iter())
        .map(|(name, &c)| (name.as_str(), c))
        .collect();
    let Ok(json) = serde_json::to_string(&map) else {
        return;
    };

    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, json.as_bytes()).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}
