//! Graph primitives for `ctx_graph`: `neighbors`, `path` (shortest path) and
//! `explain`. These are graphify-style traversal/inspection helpers that work on
//! the same file-level graph the dashboard renders (`GraphProvider::edges`), so
//! the MCP answers and the visual graph always agree.
//!
//! All three accept `format="json"` for machine consumption; otherwise they emit
//! compact, token-light text with a `[ctx_graph <action>: N tok]` footer like the
//! other actions.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::core::graph_analysis::edge_confidence;
use crate::core::graph_index;
use crate::core::graph_provider::{self, EdgeInfo};
use crate::core::protocol::shorten_path;
use crate::core::tokens::count_tokens;

/// One adjacency entry: a neighbour reached via an edge of `kind`/`weight`.
struct NeighborRef {
    node: String,
    kind: String,
    weight: f64,
}

/// A directed, file-level adjacency view built once from `GraphProvider::edges`.
/// Node ids are repo-relative file paths — identical to the dashboard graph.
struct Adj {
    nodes: Vec<String>,
    node_set: HashSet<String>,
    out: HashMap<String, Vec<NeighborRef>>,
    inc: HashMap<String, Vec<NeighborRef>>,
}

impl Adj {
    fn build(edges: &[EdgeInfo], file_paths: &[String]) -> Self {
        let mut out: HashMap<String, Vec<NeighborRef>> = HashMap::new();
        let mut inc: HashMap<String, Vec<NeighborRef>> = HashMap::new();
        let mut node_set: HashSet<String> = HashSet::new();
        for p in file_paths {
            node_set.insert(p.clone());
        }
        for e in edges {
            node_set.insert(e.from.clone());
            node_set.insert(e.to.clone());
            out.entry(e.from.clone()).or_default().push(NeighborRef {
                node: e.to.clone(),
                kind: e.kind.clone(),
                weight: e.weight,
            });
            inc.entry(e.to.clone()).or_default().push(NeighborRef {
                node: e.from.clone(),
                kind: e.kind.clone(),
                weight: e.weight,
            });
        }
        let mut nodes: Vec<String> = node_set.iter().cloned().collect();
        nodes.sort();
        Self {
            nodes,
            node_set,
            out,
            inc,
        }
    }

    /// Resolve a user-supplied path to a concrete graph node. Tries an exact
    /// repo-relative match first, then a unique path/basename suffix match.
    fn resolve(&self, input: &str, root: &str) -> Result<String, String> {
        let rel = graph_index::graph_relative_key(input, root);
        if self.node_set.contains(&rel) {
            return Ok(rel);
        }
        let needle = graph_index::graph_match_key(&rel);
        if self.node_set.contains(&needle) {
            return Ok(needle);
        }
        let base = needle.rsplit('/').next().unwrap_or(&needle).to_string();
        let suffix = format!("/{needle}");
        let base_suffix = format!("/{base}");
        let cands: Vec<&String> = self
            .nodes
            .iter()
            .filter(|n| {
                let nk = graph_index::graph_match_key(n);
                nk == needle || nk.ends_with(&suffix) || nk == base || nk.ends_with(&base_suffix)
            })
            .collect();
        match cands.len() {
            0 => Err(format!(
                "Node not found in graph: {input}\nRun ctx_graph action='build' to (re)index, or pass a path that exists in the project."
            )),
            1 => Ok(cands[0].clone()),
            _ => {
                // Show full repo-relative paths (not basenames) so the caller
                // can actually tell the candidates apart.
                let list = cands
                    .iter()
                    .take(10)
                    .map(|c| format!("  {c}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                let more = if cands.len() > 10 {
                    format!("\n  … and {} more", cands.len() - 10)
                } else {
                    String::new()
                };
                Err(format!(
                    "'{input}' is ambiguous ({} matches) — pass a more specific path:\n{list}{more}",
                    cands.len()
                ))
            }
        }
    }

    fn outgoing(&self, node: &str) -> Vec<&NeighborRef> {
        let mut v: Vec<&NeighborRef> = self
            .out
            .get(node)
            .map(|x| x.iter().collect())
            .unwrap_or_default();
        v.sort_by(|a, b| a.node.cmp(&b.node).then_with(|| a.kind.cmp(&b.kind)));
        v
    }

    fn incoming(&self, node: &str) -> Vec<&NeighborRef> {
        let mut v: Vec<&NeighborRef> = self
            .inc
            .get(node)
            .map(|x| x.iter().collect())
            .unwrap_or_default();
        v.sort_by(|a, b| a.node.cmp(&b.node).then_with(|| a.kind.cmp(&b.kind)));
        v
    }

    /// Unique undirected neighbours (out ∪ inc), deterministically sorted.
    fn undirected_neighbors(&self, node: &str) -> Vec<String> {
        let mut set: HashSet<&str> = HashSet::new();
        if let Some(v) = self.out.get(node) {
            for nb in v {
                set.insert(nb.node.as_str());
            }
        }
        if let Some(v) = self.inc.get(node) {
            for nb in v {
                set.insert(nb.node.as_str());
            }
        }
        let mut out: Vec<String> = set.into_iter().map(str::to_string).collect();
        out.sort();
        out
    }

    /// The strongest edge directly connecting `a` and `b`, with its direction.
    /// Direction: `Forward` = a→b, `Backward` = b→a.
    fn edge_between(&self, a: &str, b: &str) -> Option<(Direction, String, f64)> {
        let mut best: Option<(Direction, String, f64)> = None;
        let mut consider = |dir: Direction, kind: &str, weight: f64| {
            let conf = edge_confidence(kind, weight);
            if best.as_ref().is_none_or(|(_, _, c)| conf > *c) {
                best = Some((dir, kind.to_string(), conf));
            }
        };
        if let Some(v) = self.out.get(a) {
            for nb in v.iter().filter(|nb| nb.node == b) {
                consider(Direction::Forward, &nb.kind, nb.weight);
            }
        }
        if let Some(v) = self.inc.get(a) {
            for nb in v.iter().filter(|nb| nb.node == b) {
                consider(Direction::Backward, &nb.kind, nb.weight);
            }
        }
        best
    }

    /// Shortest undirected path `from → to` (BFS, deterministic). Includes both
    /// endpoints. `None` when the two nodes are in different components.
    fn bfs_path(&self, from: &str, to: &str) -> Option<Vec<String>> {
        if from == to {
            return Some(vec![from.to_string()]);
        }
        let mut prev: HashMap<String, String> = HashMap::new();
        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<String> = VecDeque::new();
        visited.insert(from.to_string());
        queue.push_back(from.to_string());
        while let Some(cur) = queue.pop_front() {
            for nb in self.undirected_neighbors(&cur) {
                if visited.contains(&nb) {
                    continue;
                }
                visited.insert(nb.clone());
                prev.insert(nb.clone(), cur.clone());
                if nb == to {
                    return Some(reconstruct(&prev, from, to));
                }
                queue.push_back(nb);
            }
        }
        None
    }

    /// BFS distance rings from `start` (undirected), capped at `max_depth`.
    /// Returns distance → sorted node list (excludes the start node).
    fn bfs_rings(&self, start: &str, max_depth: usize) -> Vec<(usize, Vec<String>)> {
        let mut dist: HashMap<String, usize> = HashMap::new();
        let mut queue: VecDeque<String> = VecDeque::new();
        dist.insert(start.to_string(), 0);
        queue.push_back(start.to_string());
        while let Some(cur) = queue.pop_front() {
            let d = dist[&cur];
            if d >= max_depth {
                continue;
            }
            for nb in self.undirected_neighbors(&cur) {
                if !dist.contains_key(&nb) {
                    dist.insert(nb.clone(), d + 1);
                    queue.push_back(nb);
                }
            }
        }
        let mut rings: HashMap<usize, Vec<String>> = HashMap::new();
        for (node, d) in dist {
            if d == 0 {
                continue;
            }
            rings.entry(d).or_default().push(node);
        }
        let mut out: Vec<(usize, Vec<String>)> = rings
            .into_iter()
            .map(|(d, mut nodes)| {
                nodes.sort();
                (d, nodes)
            })
            .collect();
        out.sort_by_key(|(d, _)| *d);
        out
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Direction {
    Forward,
    Backward,
}

impl Direction {
    fn arrow(self) -> &'static str {
        match self {
            Direction::Forward => "->",
            Direction::Backward => "<-",
        }
    }
}

fn reconstruct(prev: &HashMap<String, String>, from: &str, to: &str) -> Vec<String> {
    let mut chain = vec![to.to_string()];
    let mut cur = to.to_string();
    while cur != from {
        match prev.get(&cur) {
            Some(p) => {
                chain.push(p.clone());
                cur = p.clone();
            }
            None => break,
        }
    }
    chain.reverse();
    chain
}

fn open_graph(root: &str) -> Result<graph_provider::OpenGraphProvider, String> {
    graph_provider::open_or_build(root)
        .ok_or_else(|| "No graph index found. Run ctx_graph with action='build' first.".to_string())
}

fn is_json(format: Option<&str>) -> bool {
    matches!(format, Some(f) if f.eq_ignore_ascii_case("json"))
}

/// `ctx_graph action=neighbors` — immediate (and optionally multi-hop) graph
/// neighbours of a file, split by direction and annotated with edge kind.
pub fn neighbors(
    path: Option<&str>,
    root: &str,
    depth: Option<usize>,
    format: Option<&str>,
) -> String {
    let Some(input) = path else {
        return "path is required for 'neighbors' action".to_string();
    };
    let open = match open_graph(root) {
        Ok(o) => o,
        Err(e) => return e,
    };
    let gp = &open.provider;
    let adj = Adj::build(&gp.edges(), &gp.file_paths());
    let node = match adj.resolve(input, root) {
        Ok(n) => n,
        Err(e) => return e,
    };
    let depth = depth.unwrap_or(1).clamp(1, 6);
    let outgoing = adj.outgoing(&node);
    let incoming = adj.incoming(&node);
    let rings = if depth > 1 {
        adj.bfs_rings(&node, depth)
    } else {
        Vec::new()
    };

    if is_json(format) {
        let mut nodes_set: std::collections::HashSet<&str> = std::collections::HashSet::new();
        nodes_set.insert(&node);
        for n in &outgoing {
            nodes_set.insert(&n.node);
        }
        for n in &incoming {
            nodes_set.insert(&n.node);
        }
        for (_, ns) in &rings {
            for n in ns {
                nodes_set.insert(n);
            }
        }

        let nodes_json: Vec<_> = nodes_set.iter().map(|n| graph_node_json(n)).collect();

        let mut edges_json: Vec<serde_json::Value> = outgoing
            .iter()
            .map(|n| graph_edge_json(&node, &n.node, &n.kind))
            .collect();
        edges_json.extend(
            incoming
                .iter()
                .map(|n| graph_edge_json(&n.node, &node, &n.kind)),
        );

        let val = if depth > 1 {
        let rings_json: Vec<_> = rings
            .iter()
            .map(|(d, ns)| serde_json::json!({ "depth": d, "count": ns.len(), "nodes": ns }))
            .collect();
        let total: usize = rings.iter().map(|(_, n)| n.len()).sum();
        serde_json::json!({
            "nodes": nodes_json,
            "edges": edges_json,
            "rings": { "depths": rings_json, "total": total },
        })
        } else {
            serde_json::json!({
                "nodes": nodes_json,
                "edges": edges_json,
            })
        };
        return serde_json::to_string_pretty(&val).unwrap_or_else(|_| "{}".to_string());
    }

    let mut out = format!("Neighbors of {}\n", shorten_path(&node));
    out.push_str(&format!(
        "\nOutgoing ({}) — this file depends on / references:\n",
        outgoing.len()
    ));
    if outgoing.is_empty() {
        out.push_str("  (none)\n");
    } else {
        for n in &outgoing {
            out.push_str(&format!(
                "  -> {:<48} {:<10} conf {:.2}\n",
                shorten_path(&n.node),
                n.kind,
                edge_confidence(&n.kind, n.weight)
            ));
        }
    }
    out.push_str(&format!(
        "\nIncoming ({}) — files that depend on / reference this:\n",
        incoming.len()
    ));
    if incoming.is_empty() {
        out.push_str("  (none)\n");
    } else {
        for n in &incoming {
            out.push_str(&format!(
                "  <- {:<48} {:<10} conf {:.2}\n",
                shorten_path(&n.node),
                n.kind,
                edge_confidence(&n.kind, n.weight)
            ));
        }
    }
    if depth > 1 {
        let total: usize = rings.iter().map(|(_, n)| n.len()).sum();
        out.push_str(&format!("\nReachable within {depth} hops: {total} nodes\n"));
        for (d, nodes) in &rings {
            out.push_str(&format!("  {} hop(s): {} nodes\n", d, nodes.len()));
        }
    }
    let tokens = count_tokens(&out);
    format!("{out}[ctx_graph neighbors: {tokens} tok]")
}

/// `ctx_graph action=path` — shortest connection between two files, with the
/// edge kind/direction of each hop.
pub fn shortest_path(
    from: Option<&str>,
    to: Option<&str>,
    root: &str,
    format: Option<&str>,
) -> String {
    let (Some(a), Some(b)) = (from, to) else {
        return "Both 'path' (from) and 'to' are required for 'path' action".to_string();
    };
    let open = match open_graph(root) {
        Ok(o) => o,
        Err(e) => return e,
    };
    let gp = &open.provider;
    let adj = Adj::build(&gp.edges(), &gp.file_paths());
    let na = match adj.resolve(a, root) {
        Ok(n) => n,
        Err(e) => return e,
    };
    let nb = match adj.resolve(b, root) {
        Ok(n) => n,
        Err(e) => return e,
    };

    let Some(chain) = adj.bfs_path(&na, &nb) else {
        if is_json(format) {
            return serde_json::json!({
                "from": na, "to": nb, "found": false, "path": [],
            })
            .to_string();
        }
        return format!(
            "No path between {} and {} — they live in different components of the dependency graph.",
            shorten_path(&na),
            shorten_path(&nb)
        );
    };

    let hops = chain.len().saturating_sub(1);
    if is_json(format) {
        let nodes_json: Vec<_> = chain.iter().map(|n| graph_node_json(n)).collect();
        let edges_json: Vec<_> = chain
            .windows(2)
            .map(|w| {
                let (_, kind, _) = adj.edge_between(&w[0], &w[1]).unwrap_or((
                    Direction::Forward,
                    "related".to_string(),
                    0.5,
                ));
                graph_edge_json(&w[0], &w[1], &kind)
            })
            .collect();
        let val = serde_json::json!({
            "nodes": nodes_json,
            "edges": edges_json,
            "from": na, "to": nb, "found": true, "hops": hops,
        });
        return serde_json::to_string_pretty(&val).unwrap_or_else(|_| "{}".to_string());
    }

    let mut out = format!(
        "Shortest path {} -> {} ({} hops):\n\n",
        shorten_path(&na),
        shorten_path(&nb),
        hops
    );
    out.push_str(&format!("  {}\n", shorten_path(&chain[0])));
    for w in chain.windows(2) {
        let (dir, kind, conf) = adj.edge_between(&w[0], &w[1]).unwrap_or((
            Direction::Forward,
            "related".to_string(),
            0.5,
        ));
        out.push_str(&format!(
            "    {} {} (conf {:.2})\n  {}\n",
            dir.arrow(),
            kind,
            conf,
            shorten_path(&w[1])
        ));
    }
    let tokens = count_tokens(&out);
    format!("{out}[ctx_graph path: {tokens} tok]")
}

/// `ctx_graph action=explain` — why a file matters: degree, community, bridge
/// score, god-node rank and its most important couplings. Reuses the same
/// analyses the dashboard shows.
pub fn explain(path: Option<&str>, root: &str, format: Option<&str>) -> String {
    let Some(input) = path else {
        return "path is required for 'explain' action".to_string();
    };
    let open = match open_graph(root) {
        Ok(o) => o,
        Err(e) => return e,
    };
    let gp = &open.provider;
    let edges = gp.edges();
    let adj = Adj::build(&edges, &gp.file_paths());
    let node = match adj.resolve(input, root) {
        Ok(n) => n,
        Err(e) => return e,
    };

    let community = crate::core::community::detect_communities_for_provider(gp, root);
    let community_map = community.assignment_min_size(2);
    let god = crate::core::graph_analysis::compute_god_nodes(&edges, usize::MAX);
    let bridges = crate::core::graph_analysis::compute_bridge_nodes(&edges, usize::MAX);
    let surprising = crate::core::graph_analysis::find_surprising_connections(
        &edges,
        &community_map,
        usize::MAX,
    );

    let god_entry = god.iter().enumerate().find(|(_, g)| g.path == node);
    let (dep_in, dep_out, dep_degree) =
        god_entry.map_or((0, 0, 0), |(_, g)| (g.in_degree, g.out_degree, g.degree));
    let god_rank = god_entry.map(|(i, _)| i + 1);
    let bridge_entry = bridges.iter().enumerate().find(|(_, b)| b.path == node);
    let community_id = community_map.get(&node).copied();
    let community_info =
        community_id.and_then(|id| community.communities.iter().find(|c| c.id == id));
    let surprising_here: Vec<_> = surprising
        .iter()
        .filter(|s| s.from == node || s.to == node)
        .take(8)
        .collect();

    let out_all = adj.outgoing(&node);
    let inc_all = adj.incoming(&node);

    if is_json(format) {
        // Build contextual graph nodes & edges around the explained node.
        let mut ctx_nodes: std::collections::HashSet<String> = std::collections::HashSet::new();
        ctx_nodes.insert(node.clone());
        let top_dep_in: Vec<&String> = inc_all.iter().take(8).map(|n| &n.node).collect();
        let top_dep_out: Vec<&String> = out_all.iter().take(8).map(|n| &n.node).collect();
        for n in &top_dep_in {
            ctx_nodes.insert(n.to_string());
        }
        for n in &top_dep_out {
            ctx_nodes.insert(n.to_string());
        }

        let ctx_nodes_json: Vec<_> = {
            let mut v: Vec<_> = ctx_nodes.iter().collect();
            v.sort();
            v.iter().map(|n| graph_node_json(n)).collect()
        };
        let mut ctx_edges_json: Vec<serde_json::Value> = out_all
            .iter()
            .filter(|n| ctx_nodes.contains(&n.node))
            .map(|n| graph_edge_json(&node, &n.node, &n.kind))
            .collect();
        ctx_edges_json.extend(
            inc_all
                .iter()
                .filter(|n| ctx_nodes.contains(&n.node))
                .map(|n| graph_edge_json(&n.node, &node, &n.kind)),
        );

        let val = serde_json::json!({
            "nodes": ctx_nodes_json,
            "edges": ctx_edges_json,
            "node": node,
            "degree": { "in": dep_in, "out": dep_out, "total": dep_degree },
            "god_rank": god_rank,
            "is_god": god_rank.is_some_and(|r| r <= 12),
            "bridge": bridge_entry.map(|(i, b)| serde_json::json!({
                "rank": i + 1, "betweenness": round3(b.betweenness),
            })),
            "community": community_info.map(|c| serde_json::json!({
                "id": c.id, "file_count": c.files.len(),
                "cohesion": round3(c.cohesion),
                "internal_edges": c.internal_edges, "external_edges": c.external_edges,
            })),
            "neighbors": { "out": out_all.len(), "in": inc_all.len() },
            "surprising": surprising_here.iter().map(|s| serde_json::json!({
                "from": s.from, "to": s.to, "score": round3(s.score),
                "cross_community": s.cross_community,
            })).collect::<Vec<_>>(),
        });
        return serde_json::to_string_pretty(&val).unwrap_or_else(|_| "{}".to_string());
    }

    let mut out = format!("Why {} matters\n\n", shorten_path(&node));
    out.push_str(&format!(
        "Dependency degree: {dep_degree} (in {dep_in} · out {dep_out})\n"
    ));
    match god_rank {
        Some(r) if r <= 12 => {
            out.push_str(&format!("God-node: yes — rank #{r} (most connected)\n"));
        }
        Some(r) => out.push_str(&format!("God-node rank: #{r}\n")),
        None => out.push_str("God-node: no dependency edges\n"),
    }
    match bridge_entry {
        Some((i, b)) => out.push_str(&format!(
            "Bridge (betweenness): {:.2} — rank #{} (sits on many shortest paths)\n",
            b.betweenness,
            i + 1
        )),
        None => out.push_str("Bridge: not on critical paths\n"),
    }
    match community_info {
        Some(c) => out.push_str(&format!(
            "Community: #{} — {} files, cohesion {:.2} (internal {} / external {})\n",
            c.id,
            c.files.len(),
            c.cohesion,
            c.internal_edges,
            c.external_edges
        )),
        None => out.push_str("Community: isolated (no module ≥2 files)\n"),
    }
    out.push_str(&format!(
        "Total neighbors (all edge kinds): {} (out {} · in {})\n",
        out_all.len() + inc_all.len(),
        out_all.len(),
        inc_all.len()
    ));

    let top_dependents: Vec<&String> = inc_all
        .iter()
        .filter(|n| crate::core::graph_analysis::is_dependency_kind(&n.kind))
        .map(|n| &n.node)
        .take(8)
        .collect();
    if !top_dependents.is_empty() {
        out.push_str(&format!(
            "\nTop dependents (fan-in, {}):\n",
            top_dependents.len()
        ));
        for d in &top_dependents {
            out.push_str(&format!("  {}\n", shorten_path(d)));
        }
    }
    let top_deps: Vec<&String> = out_all
        .iter()
        .filter(|n| crate::core::graph_analysis::is_dependency_kind(&n.kind))
        .map(|n| &n.node)
        .take(8)
        .collect();
    if !top_deps.is_empty() {
        out.push_str(&format!(
            "\nTop dependencies (fan-out, {}):\n",
            top_deps.len()
        ));
        for d in &top_deps {
            out.push_str(&format!("  {}\n", shorten_path(d)));
        }
    }
    if !surprising_here.is_empty() {
        out.push_str(&format!(
            "\nSurprising connections ({}):\n",
            surprising_here.len()
        ));
        for s in &surprising_here {
            let other = if s.from == node { &s.to } else { &s.from };
            out.push_str(&format!(
                "  {} (score {:.2}{})\n",
                shorten_path(other),
                s.score,
                if s.cross_community {
                    ", cross-community"
                } else {
                    ""
                }
            ));
        }
    }
    let tokens = count_tokens(&out);
    format!("{out}[ctx_graph explain: {tokens} tok]")
}

/// Build a  node value from a repo-relative path.
fn graph_node_json(path: &str) -> serde_json::Value {
    let name = path.rsplit('/').next().unwrap_or(path);
    serde_json::json!({ "name": name, "file": path })
}

/// Build a  edge value.
fn graph_edge_json(source: &str, target: &str, kind: &str) -> serde_json::Value {
    serde_json::json!({ "source": source, "target": target, "kind": kind })
}

fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edge(from: &str, to: &str, kind: &str) -> EdgeInfo {
        EdgeInfo {
            from: from.into(),
            to: to.into(),
            kind: kind.into(),
            weight: 1.0,
        }
    }

    /// a -> b -> c, plus an isolated d. Used by most traversal tests.
    fn sample() -> Adj {
        let edges = vec![
            edge("src/a.rs", "src/b.rs", "import"),
            edge("src/b.rs", "src/c.rs", "import"),
        ];
        let files = vec![
            "src/a.rs".to_string(),
            "src/b.rs".to_string(),
            "src/c.rs".to_string(),
            "src/d.rs".to_string(),
        ];
        Adj::build(&edges, &files)
    }

    #[test]
    fn resolve_exact_and_basename() {
        let adj = sample();
        // Exact repo-relative match.
        assert_eq!(adj.resolve("src/a.rs", "/proj").unwrap(), "src/a.rs");
        // Unique basename match.
        assert_eq!(adj.resolve("c.rs", "/proj").unwrap(), "src/c.rs");
    }

    #[test]
    fn resolve_unknown_errors() {
        let adj = sample();
        assert!(adj.resolve("nope.rs", "/proj").is_err());
    }

    #[test]
    fn resolve_ambiguous_errors() {
        let edges = vec![edge("a/mod.rs", "b/mod.rs", "import")];
        let files = vec!["a/mod.rs".to_string(), "b/mod.rs".to_string()];
        let adj = Adj::build(&edges, &files);
        let err = adj.resolve("mod.rs", "/proj").unwrap_err();
        assert!(err.contains("ambiguous"), "got: {err}");
    }

    #[test]
    fn bfs_path_finds_shortest_chain() {
        let adj = sample();
        let path = adj.bfs_path("src/a.rs", "src/c.rs").unwrap();
        assert_eq!(path, vec!["src/a.rs", "src/b.rs", "src/c.rs"]);
    }

    #[test]
    fn bfs_path_is_undirected() {
        // Reverse direction still connects (edges are followed both ways).
        let adj = sample();
        let path = adj.bfs_path("src/c.rs", "src/a.rs").unwrap();
        assert_eq!(path, vec!["src/c.rs", "src/b.rs", "src/a.rs"]);
    }

    #[test]
    fn bfs_path_none_when_disconnected() {
        let adj = sample();
        assert!(adj.bfs_path("src/a.rs", "src/d.rs").is_none());
    }

    #[test]
    fn bfs_path_same_node_is_singleton() {
        let adj = sample();
        assert_eq!(
            adj.bfs_path("src/b.rs", "src/b.rs").unwrap(),
            vec!["src/b.rs"]
        );
    }

    #[test]
    fn rings_group_by_distance() {
        let adj = sample();
        let rings = adj.bfs_rings("src/a.rs", 3);
        assert_eq!(rings[0], (1, vec!["src/b.rs".to_string()]));
        assert_eq!(rings[1], (2, vec!["src/c.rs".to_string()]));
    }

    #[test]
    fn edge_between_reports_direction() {
        let adj = sample();
        let (dir, kind, conf) = adj.edge_between("src/a.rs", "src/b.rs").unwrap();
        assert_eq!(dir, Direction::Forward);
        assert_eq!(kind, "import");
        assert!((conf - 1.0).abs() < 1e-9);
        // Reverse view is Backward.
        let (dir2, _, _) = adj.edge_between("src/b.rs", "src/a.rs").unwrap();
        assert_eq!(dir2, Direction::Backward);
    }

    #[test]
    fn edge_between_prefers_higher_confidence() {
        // Two parallel edges: a weak sibling and a strong import. import wins.
        let edges = vec![
            edge("x.rs", "y.rs", "sibling"),
            edge("x.rs", "y.rs", "import"),
        ];
        let adj = Adj::build(&edges, &[]);
        let (_, kind, conf) = adj.edge_between("x.rs", "y.rs").unwrap();
        assert_eq!(kind, "import");
        assert!((conf - 1.0).abs() < 1e-9);
    }

    #[test]
    fn neighbors_split_in_and_out() {
        let adj = sample();
        let out = adj.outgoing("src/b.rs");
        let inc = adj.incoming("src/b.rs");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].node, "src/c.rs");
        assert_eq!(inc.len(), 1);
        assert_eq!(inc[0].node, "src/a.rs");
    }
}
