//! `PageRank` computation on the Property Graph.
//!
//! Provides a reusable `compute` function that can be called by
//! `ctx_architecture`, `ctx_overview`, and `ctx_fill` for importance-weighted
//! context selection.

use std::collections::{HashMap, HashSet};

use rusqlite::Connection;

pub struct PageRankInput {
    pub files: HashSet<String>,
    pub forward: HashMap<String, Vec<String>>,
}

impl PageRankInput {
    pub fn from_connection(conn: &Connection) -> Self {
        let mut files: HashSet<String> = HashSet::new();
        let mut forward: HashMap<String, Vec<String>> = HashMap::new();

        if let Ok(mut stmt) =
            conn.prepare("SELECT DISTINCT file_path FROM nodes WHERE kind = 'file'")
            && let Ok(rows) = stmt.query_map([], |row| row.get::<_, String>(0))
        {
            for f in rows.flatten() {
                files.insert(f);
            }
        }

        let edge_sql = "
            SELECT DISTINCT n1.file_path, n2.file_path
            FROM edges e
            JOIN nodes n1 ON e.source_id = n1.id
            JOIN nodes n2 ON e.target_id = n2.id
            WHERE n1.kind = 'file' AND n2.kind = 'file'
              AND n1.file_path != n2.file_path
        ";
        if let Ok(mut stmt) = conn.prepare(edge_sql)
            && let Ok(rows) = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
        {
            for row in rows.flatten() {
                let (src, tgt) = row;
                forward.entry(src).or_default().push(tgt);
            }
        }

        for deps in forward.values_mut() {
            deps.sort();
            deps.dedup();
        }

        Self { files, forward }
    }
}

#[must_use]
pub fn compute(input: &PageRankInput, damping: f64, iterations: usize) -> HashMap<String, f64> {
    compute_personalized(input, damping, iterations, &[])
}

/// Personalized `PageRank`: if `seed_files` is non-empty, teleportation bias goes
/// to those files instead of uniform distribution. Handles dangling nodes
/// (nodes with no outgoing edges) by redistributing their rank.
#[must_use]
pub fn compute_personalized(
    input: &PageRankInput,
    damping: f64,
    iterations: usize,
    seed_files: &[String],
) -> HashMap<String, f64> {
    let n = input.files.len();
    if n == 0 {
        return HashMap::new();
    }

    let personalization: HashMap<String, f64> = if seed_files.is_empty() {
        let uniform = 1.0 / n as f64;
        input.files.iter().map(|f| (f.clone(), uniform)).collect()
    } else {
        let valid_seeds: Vec<&String> = seed_files
            .iter()
            .filter(|f| input.files.contains(*f))
            .collect();
        if valid_seeds.is_empty() {
            let uniform = 1.0 / n as f64;
            input.files.iter().map(|f| (f.clone(), uniform)).collect()
        } else {
            let weight = 1.0 / valid_seeds.len() as f64;
            let mut p = HashMap::new();
            for f in &valid_seeds {
                p.insert((*f).clone(), weight);
            }
            p
        }
    };

    let dangling: HashSet<&String> = input
        .files
        .iter()
        .filter(|f| !input.forward.contains_key(*f) || input.forward[*f].is_empty())
        .collect();

    let init = 1.0 / n as f64;
    let mut rank: HashMap<String, f64> = input.files.iter().map(|f| (f.clone(), init)).collect();

    let eps = 1e-8;
    for _ in 0..iterations {
        let dangling_sum: f64 = dangling
            .iter()
            .map(|f| rank.get(*f).copied().unwrap_or(0.0))
            .sum();

        let mut new_rank: HashMap<String, f64> = HashMap::with_capacity(n);

        for f in &input.files {
            let teleport = personalization.get(f).copied().unwrap_or(0.0);
            let dangling_contrib = personalization.get(f).copied().unwrap_or(0.0) * dangling_sum;
            new_rank.insert(
                f.clone(),
                (1.0 - damping) * teleport + damping * dangling_contrib,
            );
        }

        for (node, neighbors) in &input.forward {
            if neighbors.is_empty() {
                continue;
            }
            let share = rank.get(node).copied().unwrap_or(0.0) / neighbors.len() as f64;
            for neighbor in neighbors {
                if let Some(nr) = new_rank.get_mut(neighbor) {
                    *nr += damping * share;
                }
            }
        }

        let diff: f64 = input
            .files
            .iter()
            .map(|f| {
                (rank.get(f).copied().unwrap_or(0.0) - new_rank.get(f).copied().unwrap_or(0.0))
                    .abs()
            })
            .sum();
        rank = new_rank;

        if diff < eps {
            break;
        }
    }

    rank
}

pub fn top_files(conn: &Connection, limit: usize) -> Vec<(String, f64)> {
    top_files_personalized(conn, limit, &[])
}

pub fn top_files_personalized(
    conn: &Connection,
    limit: usize,
    seed_files: &[String],
) -> Vec<(String, f64)> {
    let input = PageRankInput::from_connection(conn);
    let ranks = compute_personalized(&input, 0.85, 50, seed_files);
    let mut sorted: Vec<(String, f64)> = ranks.into_iter().collect();
    sorted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    sorted.truncate(limit);
    sorted
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property_graph::{CodeGraph, Edge, EdgeKind, Node};

    #[test]
    fn pagerank_basic() {
        let g = CodeGraph::open_in_memory().unwrap();
        let a = g.upsert_node(&Node::file("a.rs")).unwrap();
        let b = g.upsert_node(&Node::file("b.rs")).unwrap();
        let c = g.upsert_node(&Node::file("c.rs")).unwrap();

        g.upsert_edge(&Edge::new(a, b, EdgeKind::Imports)).unwrap();
        g.upsert_edge(&Edge::new(a, c, EdgeKind::Imports)).unwrap();
        g.upsert_edge(&Edge::new(b, c, EdgeKind::Imports)).unwrap();

        let input = PageRankInput::from_connection(g.connection());
        let ranks = compute(&input, 0.85, 30);

        assert_eq!(ranks.len(), 3);
        let rank_c = ranks.get("c.rs").copied().unwrap_or(0.0);
        let rank_a = ranks.get("a.rs").copied().unwrap_or(0.0);
        assert!(
            rank_c > rank_a,
            "c.rs should rank higher (more incoming): c={rank_c} a={rank_a}"
        );
    }

    #[test]
    fn top_files_limit() {
        let g = CodeGraph::open_in_memory().unwrap();
        for i in 0..10 {
            g.upsert_node(&Node::file(&format!("f{i}.rs"))).unwrap();
        }
        let top = top_files(g.connection(), 3);
        assert!(top.len() <= 3);
    }

    #[test]
    fn empty_graph() {
        let g = CodeGraph::open_in_memory().unwrap();
        let top = top_files(g.connection(), 10);
        assert!(top.is_empty());
    }

    #[test]
    fn personalized_pagerank_boosts_seed() {
        let g = CodeGraph::open_in_memory().unwrap();
        let a = g.upsert_node(&Node::file("a.rs")).unwrap();
        let b = g.upsert_node(&Node::file("b.rs")).unwrap();
        let c = g.upsert_node(&Node::file("c.rs")).unwrap();

        g.upsert_edge(&Edge::new(a, b, EdgeKind::Imports)).unwrap();
        g.upsert_edge(&Edge::new(b, c, EdgeKind::Imports)).unwrap();

        let input = PageRankInput::from_connection(g.connection());

        let uniform = compute_personalized(&input, 0.85, 50, &[]);
        let seeded = compute_personalized(&input, 0.85, 50, &["a.rs".to_string()]);

        let a_uniform = uniform.get("a.rs").copied().unwrap_or(0.0);
        let a_seeded = seeded.get("a.rs").copied().unwrap_or(0.0);

        assert!(
            a_seeded > a_uniform,
            "seeded a.rs ({a_seeded}) should rank higher than uniform ({a_uniform})"
        );
    }

    #[test]
    fn early_convergence() {
        let g = CodeGraph::open_in_memory().unwrap();
        let a = g.upsert_node(&Node::file("a.rs")).unwrap();
        let b = g.upsert_node(&Node::file("b.rs")).unwrap();
        g.upsert_edge(&Edge::new(a, b, EdgeKind::Imports)).unwrap();
        g.upsert_edge(&Edge::new(b, a, EdgeKind::Imports)).unwrap();

        let input = PageRankInput::from_connection(g.connection());
        let ranks = compute(&input, 0.85, 1000);
        assert_eq!(ranks.len(), 2);
    }
}
