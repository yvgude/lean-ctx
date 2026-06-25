//! Multi-layer graph descriptors inspired by GNN message passing:
//! `PageRank` (global importance), local clustering, HITS hubs/authorities,
//! and weakly-connected community ids.

use std::collections::{HashMap, HashSet};

use rusqlite::Connection;

use crate::core::pagerank::{PageRankInput, compute as pagerank_compute};

/// Aggregated graph-derived features per file node.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphFeatures {
    pub centrality: f64,
    pub clustering_coeff: f64,
    pub hub_score: f64,
    pub authority_score: f64,
    pub community_id: Option<usize>,
}

struct UnionFind {
    parent: Vec<usize>,
    rank: Vec<u8>,
}

impl UnionFind {
    fn new(n: usize) -> Self {
        Self {
            parent: (0..n).collect(),
            rank: vec![0; n],
        }
    }

    fn find(&mut self, x: usize) -> usize {
        if self.parent[x] != x {
            self.parent[x] = self.find(self.parent[x]);
        }
        self.parent[x]
    }

    fn union(&mut self, a: usize, b: usize) {
        let mut ra = self.find(a);
        let mut rb = self.find(b);
        if ra == rb {
            return;
        }
        if self.rank[ra] < self.rank[rb] {
            std::mem::swap(&mut ra, &mut rb);
        }
        self.parent[rb] = ra;
        if self.rank[ra] == self.rank[rb] {
            self.rank[ra] += 1;
        }
    }
}

fn file_adjacency(input: &PageRankInput) -> HashMap<String, HashSet<String>> {
    let mut adj: HashMap<String, HashSet<String>> = HashMap::new();

    for f in &input.files {
        adj.entry(f.clone()).or_default();
    }

    for (u, outs) in &input.forward {
        for v in outs {
            if !input.files.contains(v) || u == v {
                continue;
            }
            adj.entry(u.clone()).or_default().insert(v.clone());
            adj.entry(v.clone()).or_default().insert(u.clone());
        }
    }

    adj
}

fn clustering_coefficients(adj: &HashMap<String, HashSet<String>>) -> HashMap<String, f64> {
    let mut out = HashMap::new();

    for (node, neigh) in adj {
        let k = neigh.len();
        if k < 2 {
            out.insert(node.clone(), 0.0);
            continue;
        }

        let neigh_vec: Vec<&String> = neigh.iter().collect();
        let mut edges_between = 0usize;
        for (i, ni) in neigh_vec.iter().enumerate() {
            for nj in &neigh_vec[(i + 1)..] {
                if adj.get(*ni).is_some_and(|s| s.contains(*nj)) {
                    edges_between += 1;
                }
            }
        }

        let denom = k * (k - 1) / 2;
        let c = if denom > 0 {
            edges_between as f64 / denom as f64
        } else {
            0.0
        };
        out.insert(node.clone(), c);
    }

    out
}

fn hits_scores(
    forward: &HashMap<String, Vec<String>>,
    files: &HashSet<String>,
    iterations: usize,
) -> (HashMap<String, f64>, HashMap<String, f64>) {
    let n = files.len();
    if n == 0 {
        return (HashMap::new(), HashMap::new());
    }

    let mut hubs: HashMap<String, f64> = files.iter().map(|f| (f.clone(), 1.0)).collect();
    let mut authorities: HashMap<String, f64> = files.iter().map(|f| (f.clone(), 1.0)).collect();

    for _ in 0..iterations {
        let mut new_auth: HashMap<String, f64> = files.iter().map(|f| (f.clone(), 0.0)).collect();
        for (u, outs) in forward {
            let hu = *hubs.get(u).unwrap_or(&0.0);
            for v in outs {
                if files.contains(v) {
                    *new_auth.entry(v.clone()).or_insert(0.0) += hu;
                }
            }
        }

        let a_sum: f64 = new_auth.values().sum::<f64>().max(1e-12);
        for v in new_auth.values_mut() {
            *v /= a_sum;
        }

        let mut new_hub: HashMap<String, f64> = files.iter().map(|f| (f.clone(), 0.0)).collect();
        for (u, outs) in forward {
            let mut s = 0.0_f64;
            for v in outs {
                if files.contains(v) {
                    s += new_auth.get(v).copied().unwrap_or(0.0);
                }
            }
            *new_hub.entry(u.clone()).or_insert(0.0) += s;
        }

        let h_sum: f64 = new_hub.values().sum::<f64>().max(1e-12);
        for v in new_hub.values_mut() {
            *v /= h_sum;
        }

        hubs = new_hub;
        authorities = new_auth;
    }

    (hubs, authorities)
}

fn community_labels(
    input: &PageRankInput,
    index_of: &HashMap<String, usize>,
) -> HashMap<String, usize> {
    let mut uf = UnionFind::new(index_of.len());

    for (u, outs) in &input.forward {
        let Some(&iu) = index_of.get(u) else {
            continue;
        };
        for v in outs {
            let Some(&iv) = index_of.get(v) else {
                continue;
            };
            if iu != iv {
                uf.union(iu, iv);
            }
        }
    }

    let mut root_map: HashMap<usize, usize> = HashMap::new();
    let mut next_id = 0usize;
    let mut labels: HashMap<String, usize> = HashMap::new();

    let mut file_vec: Vec<&String> = input.files.iter().collect();
    file_vec.sort();

    for f in file_vec {
        let i = index_of[f];
        let r = uf.find(i);
        let cid = *root_map.entry(r).or_insert_with(|| {
            let id = next_id;
            next_id += 1;
            id
        });
        labels.insert(f.clone(), cid);
    }

    labels
}

/// Computes per-file graph features using the same file-level projection as `PageRank`.
pub fn compute_graph_features(conn: &Connection) -> HashMap<String, GraphFeatures> {
    let input = PageRankInput::from_connection(conn);
    let files = &input.files;

    if files.is_empty() {
        return HashMap::new();
    }

    let ranks = pagerank_compute(&input, 0.85, 50);
    let adj = file_adjacency(&input);
    let clustering = clustering_coefficients(&adj);
    let (hubs, authorities) = hits_scores(&input.forward, files, 40);

    let mut sorted_files: Vec<String> = files.iter().cloned().collect();
    sorted_files.sort();
    let index_of: HashMap<String, usize> = sorted_files
        .iter()
        .enumerate()
        .map(|(i, p)| (p.clone(), i))
        .collect();

    let communities = community_labels(&input, &index_of);

    let mut result = HashMap::with_capacity(files.len());
    for f in files {
        result.insert(
            f.clone(),
            GraphFeatures {
                centrality: ranks.get(f).copied().unwrap_or(0.0),
                clustering_coeff: clustering.get(f).copied().unwrap_or(0.0),
                hub_score: hubs.get(f).copied().unwrap_or(0.0),
                authority_score: authorities.get(f).copied().unwrap_or(0.0),
                community_id: communities.get(f).copied(),
            },
        );
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property_graph::{CodeGraph, Edge, EdgeKind, Node};

    #[test]
    fn triangle_boosts_clustering() {
        let g = CodeGraph::open_in_memory().unwrap();
        let a = g.upsert_node(&Node::file("a.rs")).unwrap();
        let b = g.upsert_node(&Node::file("b.rs")).unwrap();
        let c = g.upsert_node(&Node::file("c.rs")).unwrap();

        g.upsert_edge(&Edge::new(a, b, EdgeKind::Imports)).unwrap();
        g.upsert_edge(&Edge::new(a, c, EdgeKind::Imports)).unwrap();
        g.upsert_edge(&Edge::new(b, c, EdgeKind::Imports)).unwrap();

        let feats = compute_graph_features(g.connection());
        let ca = feats.get("a.rs").expect("a.rs");
        let cb = feats.get("b.rs").expect("b.rs");
        assert!(
            ca.clustering_coeff > 0.9 && cb.clustering_coeff > 0.9,
            "triangle graph should have ~1 clustering: a={} b={}",
            ca.clustering_coeff,
            cb.clustering_coeff
        );
    }

    #[test]
    fn authority_on_star() {
        let g = CodeGraph::open_in_memory().unwrap();
        let hub = g.upsert_node(&Node::file("hub.rs")).unwrap();
        let leaf_a = g.upsert_node(&Node::file("leaf_a.rs")).unwrap();
        let leaf_b = g.upsert_node(&Node::file("leaf_b.rs")).unwrap();

        g.upsert_edge(&Edge::new(hub, leaf_a, EdgeKind::Imports))
            .unwrap();
        g.upsert_edge(&Edge::new(hub, leaf_b, EdgeKind::Imports))
            .unwrap();

        let feats = compute_graph_features(g.connection());
        let h = feats.get("hub.rs").unwrap();
        let la = feats.get("leaf_a.rs").unwrap();

        assert!(
            la.authority_score > h.authority_score,
            "leaf should have higher authority than hub in out-star"
        );
        assert!(
            h.hub_score > la.hub_score,
            "hub node should have larger hub score"
        );
    }

    #[test]
    fn disconnected_components_differ_community() {
        let g = CodeGraph::open_in_memory().unwrap();
        let a = g.upsert_node(&Node::file("x.rs")).unwrap();
        let b = g.upsert_node(&Node::file("y.rs")).unwrap();
        let _c = g.upsert_node(&Node::file("z.rs")).unwrap();
        g.upsert_edge(&Edge::new(a, b, EdgeKind::Imports)).unwrap();

        let feats = compute_graph_features(g.connection());
        assert_ne!(
            feats["x.rs"].community_id, feats["z.rs"].community_id,
            "isolated file should not share weak component with x-y pair"
        );
    }

    #[test]
    fn empty_graph() {
        let g = CodeGraph::open_in_memory().unwrap();
        assert!(compute_graph_features(g.connection()).is_empty());
    }
}
