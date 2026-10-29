//! Undirected, weighted adjacency representation of the file-level code graph.
//!
//! Parallel edges are merged (weights summed) and the adjacency is symmetric so
//! modularity and connectivity computations are correct. The same structure is
//! built from the PropertyGraph (SQLite) and from any [`GraphProvider`], which
//! keeps community detection storage-agnostic.

use std::collections::HashMap;

use rusqlite::Connection;

use crate::core::graph_provider::GraphProvider;
use crate::core::property_graph::GraphSource;

/// Edge weight by semantic kind. Calls bind tighter than plain imports; purely
/// structural `defines`/`exports` edges are weak ties. Accepts both the
/// PropertyGraph kind spelling (`imports`, `calls`) and the graph-index spelling
/// (`import`, `call`) so either backend yields identical weights.
pub fn edge_weight(kind: &str) -> f64 {
    match kind {
        "imports" | "import" => 1.0,
        "calls" | "call" => 1.5,
        "type_ref" | "type" => 0.8,
        "defines" | "exports" | "export" => 0.3,
        _ => 0.5,
    }
}

pub struct AdjGraph {
    pub node_ids: Vec<String>,
    #[cfg_attr(not(test), allow(dead_code))] // read by community integration tests
    pub node_to_idx: HashMap<String, usize>,
    /// Symmetric adjacency: every undirected edge `{i,j}` appears as `(i→j)` in
    /// `adj[i]` and `(j→i)` in `adj[j]`. Neighbor lists are sorted by index.
    pub adj: Vec<Vec<(usize, f64)>>,
    pub degree: Vec<f64>,
    /// Sum of unique undirected edge weights (each edge counted once).
    pub total_weight: f64,
}

impl AdjGraph {
    pub fn node_count(&self) -> usize {
        self.node_ids.len()
    }

    /// Number of unique undirected edges.
    pub fn edge_count(&self) -> usize {
        self.adj.iter().map(Vec::len).sum::<usize>() / 2
    }

    /// Build from already-resolved index pairs. `raw` holds `(from, to, weight)`
    /// in node-index space; self-loops and out-of-range endpoints are dropped and
    /// duplicate/parallel edges are merged by summing weights.
    fn from_pairs(node_ids: Vec<String>, raw: &[(usize, usize, f64)]) -> Self {
        let n = node_ids.len();
        let mut node_to_idx = HashMap::with_capacity(n);
        for (idx, name) in node_ids.iter().enumerate() {
            node_to_idx.insert(name.clone(), idx);
        }

        let mut merged: HashMap<(usize, usize), f64> = HashMap::new();
        for &(a, b, w) in raw {
            if a == b || a >= n || b >= n {
                continue;
            }
            let key = if a < b { (a, b) } else { (b, a) };
            *merged.entry(key).or_default() += w;
        }

        let mut adj: Vec<Vec<(usize, f64)>> = vec![Vec::new(); n];
        let mut degree = vec![0.0; n];
        let mut total_weight = 0.0;

        let mut entries: Vec<((usize, usize), f64)> = merged.into_iter().collect();
        entries.sort_by_key(|e| e.0);
        for ((i, j), w) in entries {
            adj[i].push((j, w));
            adj[j].push((i, w));
            degree[i] += w;
            degree[j] += w;
            total_weight += w;
        }
        for list in &mut adj {
            list.sort_by_key(|e| e.0);
        }

        Self {
            node_ids,
            node_to_idx,
            adj,
            degree,
            total_weight,
        }
    }

    /// The file graph of `source` among the files it serves now (G04): a
    /// withdrawn file is no node, so it carries no edge, no degree and no
    /// weight into the partition, the edge counts, cohesion or modularity.
    /// Under protection a graph with more files than one request can decide
    /// yields the empty graph (`GraphGate::ranking_files`, no seeds).
    pub fn from_property_graph<S: GraphSource + ?Sized>(source: &S) -> Self {
        let conn = source.store();
        let edges = query_file_edges(conn);
        let file_pairs: Vec<(String, String)> = edges
            .iter()
            .map(|(a, b, _)| (a.clone(), b.clone()))
            .collect();
        let served = source.rankable_files(query_files(conn), &file_pairs, &[]);
        // `query_files` order (by path) is the node order.
        let mut files: Vec<String> = served.into_iter().collect();
        files.sort();
        let idx: HashMap<&str, usize> = files
            .iter()
            .enumerate()
            .map(|(i, f)| (f.as_str(), i))
            .collect();
        let pairs: Vec<(usize, usize, f64)> = edges
            .iter()
            .filter_map(|(a, b, w)| Some((*idx.get(a.as_str())?, *idx.get(b.as_str())?, *w)))
            .collect();
        Self::from_pairs(files, &pairs)
    }

    /// As [`Self::from_property_graph`], for either backend: the files and
    /// edges come from one gated read (`GraphProvider::ranking_graph`).
    pub fn from_provider(gp: &GraphProvider) -> Self {
        let (files, edges) = gp.ranking_graph();
        let idx: HashMap<&str, usize> = files
            .iter()
            .enumerate()
            .map(|(i, f)| (f.as_str(), i))
            .collect();
        let pairs: Vec<(usize, usize, f64)> = edges
            .iter()
            .filter_map(|e| {
                let i = *idx.get(e.from.as_str())?;
                let j = *idx.get(e.to.as_str())?;
                Some((i, j, edge_weight(&e.kind)))
            })
            .collect();
        Self::from_pairs(files, &pairs)
    }

    /// Induced subgraph over `members` (global node indices). Returns the
    /// subgraph plus a `local → global` index map for translating results back.
    pub fn induced_subgraph(&self, members: &[usize]) -> (AdjGraph, Vec<usize>) {
        let mut local_of: HashMap<usize, usize> = HashMap::with_capacity(members.len());
        let mut node_ids = Vec::with_capacity(members.len());
        let mut local_to_global = Vec::with_capacity(members.len());
        for &g in members {
            local_of.insert(g, node_ids.len());
            node_ids.push(self.node_ids[g].clone());
            local_to_global.push(g);
        }
        let mut pairs = Vec::new();
        for &g in members {
            let li = local_of[&g];
            for &(h, w) in &self.adj[g] {
                if let Some(&lj) = local_of.get(&h)
                    && li < lj
                {
                    pairs.push((li, lj, w));
                }
            }
        }
        (AdjGraph::from_pairs(node_ids, &pairs), local_to_global)
    }

    #[cfg(test)]
    pub fn from_test_edges(node_ids: Vec<String>, edges: &[(usize, usize, &str)]) -> Self {
        let pairs: Vec<(usize, usize, f64)> = edges
            .iter()
            .map(|&(a, b, kind)| (a, b, edge_weight(kind)))
            .collect();
        Self::from_pairs(node_ids, &pairs)
    }
}

/// Internal vs. external edge endpoints for a community (symmetric adjacency, so
/// each internal undirected edge contributes twice — the cohesion ratio is
/// unaffected).
pub fn edge_counts(graph: &AdjGraph, members: &[usize]) -> (usize, usize) {
    let member_set: std::collections::HashSet<usize> = members.iter().copied().collect();
    let mut internal = 0usize;
    let mut external = 0usize;
    for &i in members {
        for &(j, _) in &graph.adj[i] {
            if member_set.contains(&j) {
                internal += 1;
            } else {
                external += 1;
            }
        }
    }
    (internal, external)
}

/// Fraction of a community's incident edges that stay inside it (`0.0..=1.0`).
pub fn cohesion_of(graph: &AdjGraph, members: &[usize]) -> f64 {
    let (internal, external) = edge_counts(graph, members);
    let total = (internal + external).max(1) as f64;
    internal as f64 / total
}

fn query_files(conn: &Connection) -> Vec<String> {
    let Ok(mut stmt) = conn.prepare(
        "SELECT DISTINCT p.path
         FROM nodes n JOIN paths p ON p.id = n.file_id
         WHERE n.kind = 'file' ORDER BY p.path",
    ) else {
        tracing::warn!("community: failed to prepare file query");
        return Vec::new();
    };
    let mut files = Vec::new();
    match stmt.query_map([], |row| row.get::<_, String>(0)) {
        Ok(rows) => files.extend(rows.filter_map(std::result::Result::ok)),
        Err(e) => tracing::warn!("community: file query failed: {e}"),
    }
    files
}

fn query_file_edges(conn: &Connection) -> Vec<(String, String, f64)> {
    let sql = "
        SELECT DISTINCT p1.path, p2.path, e.kind
        FROM edges e
        JOIN nodes n1 ON e.source_id = n1.id
        JOIN nodes n2 ON e.target_id = n2.id
        JOIN paths p1 ON p1.id = n1.file_id
        JOIN paths p2 ON p2.id = n2.file_id
        WHERE n1.kind = 'file' AND n2.kind = 'file'
          AND n1.file_id != n2.file_id
        ORDER BY p1.path, p2.path
    ";
    let Ok(mut stmt) = conn.prepare(sql) else {
        tracing::warn!("community: failed to prepare edge query");
        return Vec::new();
    };
    let mut edges = Vec::new();
    match stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    }) {
        Ok(rows) => edges.extend(
            rows.filter_map(std::result::Result::ok)
                .map(|(a, b, kind)| (a, b, edge_weight(&kind))),
        ),
        Err(e) => tracing::warn!("community: edge query failed: {e}"),
    }
    edges
}
