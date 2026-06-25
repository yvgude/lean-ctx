//! Graph traversal queries: dependents, dependencies, impact analysis,
//! dependency chains (BFS-based shortest path).
//!
//! All traversal queries support multi-edge traversal: imports, calls,
//! exports, type_ref, tested_by, and more. Edge kinds are weighted
//! for impact scoring.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;

use rusqlite::{Connection, params};
use serde::Serialize;

#[derive(Debug, Clone)]
pub struct GraphQuery;

/// A single affected file with its hop distance from the root.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct AffectedEntry {
    pub file_path: String,
    pub hop: usize,
}

impl fmt::Display for AffectedEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (hop {})", self.file_path, self.hop)
    }
}

#[derive(Debug, Clone)]
pub struct ImpactResult {
    pub root_file: String,
    pub affected_files: Vec<AffectedEntry>,
    pub max_depth_reached: usize,
    pub edges_traversed: usize,
}

#[derive(Debug, Clone)]
pub struct DependencyChain {
    pub path: Vec<String>,
    pub depth: usize,
}

/// Edge kinds considered structural (code connectivity).
const STRUCTURAL_EDGE_KINDS: &str =
    "'imports','calls','exports','type_ref','tested_by','module','cochange','sibling'";

/// Weight multiplier per edge kind for impact scoring.
pub fn edge_weight(kind: &str) -> f64 {
    match kind {
        "imports" => 1.0,
        "calls" => 0.8,
        "exports" => 0.7,
        "module" => 0.6,
        "type_ref" => 0.5,
        "tested_by" => 0.4,
        "cochange" => 0.35,
        "defines" => 0.3,
        "sibling" => 0.25,
        "changed_in" => 0.2,
        _ => 0.1,
    }
}

/// Files that depend on `file_path` via structural edges (imports, calls, type_ref, etc.).
pub(super) fn dependents(conn: &Connection, file_path: &str) -> anyhow::Result<Vec<String>> {
    let sql = format!(
        "SELECT DISTINCT n_src.file_path
         FROM edges e
         JOIN nodes n_src ON e.source_id = n_src.id
         JOIN nodes n_tgt ON e.target_id = n_tgt.id
         WHERE n_tgt.file_path = ?1
           AND n_src.file_path != ?1
           AND e.kind IN ({STRUCTURAL_EDGE_KINDS})"
    );
    let mut stmt = conn.prepare(&sql)?;

    let mut results: Vec<String> = stmt
        .query_map(params![file_path], |row| row.get(0))?
        .filter_map(std::result::Result::ok)
        .collect();

    results.sort();
    results.dedup();
    Ok(results)
}

/// Files that `file_path` depends on via structural edges.
pub(super) fn dependencies(conn: &Connection, file_path: &str) -> anyhow::Result<Vec<String>> {
    let sql = format!(
        "SELECT DISTINCT n_tgt.file_path
         FROM edges e
         JOIN nodes n_src ON e.source_id = n_src.id
         JOIN nodes n_tgt ON e.target_id = n_tgt.id
         WHERE n_src.file_path = ?1
           AND n_tgt.file_path != ?1
           AND e.kind IN ({STRUCTURAL_EDGE_KINDS})"
    );
    let mut stmt = conn.prepare(&sql)?;

    let mut results: Vec<String> = stmt
        .query_map(params![file_path], |row| row.get(0))?
        .filter_map(std::result::Result::ok)
        .collect();

    results.sort();
    results.dedup();
    Ok(results)
}

/// Weighted BFS from `file_path` following reverse structural edges up to `max_depth`.
/// Edge weights attenuate propagation: calls edges carry less impact than imports.
/// Nodes only propagate when cumulative weight exceeds the threshold (0.1).
pub(super) fn impact_analysis(
    conn: &Connection,
    file_path: &str,
    max_depth: usize,
) -> anyhow::Result<ImpactResult> {
    // Graph node keys use canonical `/` separators (see the builder walk);
    // accept native Windows input too.
    let file_path = file_path.replace('\\', "/");
    let file_path = file_path.as_str();
    let reverse_graph = build_weighted_reverse_graph(conn)?;
    const PROPAGATION_THRESHOLD: f64 = 0.1;

    let mut visited: HashMap<String, usize> = HashMap::new();
    let mut queue: VecDeque<(String, usize, f64)> = VecDeque::new();
    let mut max_depth_reached = 0;
    let mut edges_traversed = 0;

    visited.insert(file_path.to_string(), 0);
    queue.push_back((file_path.to_string(), 0, 1.0));

    while let Some((current, depth, weight)) = queue.pop_front() {
        if depth >= max_depth {
            continue;
        }

        if let Some(dependents) = reverse_graph.get(&current) {
            for (dep, ew) in dependents {
                edges_traversed += 1;
                let propagated = weight * ew;
                if propagated < PROPAGATION_THRESHOLD {
                    continue;
                }
                if !visited.contains_key(dep) {
                    let new_depth = depth + 1;
                    visited.insert(dep.clone(), new_depth);
                    if new_depth > max_depth_reached {
                        max_depth_reached = new_depth;
                    }
                    queue.push_back((dep.clone(), new_depth, propagated));
                }
            }
        }
    }

    visited.remove(file_path);

    let mut affected: Vec<AffectedEntry> = visited
        .into_iter()
        .map(|(file_path, hop)| AffectedEntry { file_path, hop })
        .collect();
    affected.sort_by(|a, b| a.file_path.cmp(&b.file_path));

    Ok(ImpactResult {
        root_file: file_path.to_string(),
        affected_files: affected,
        max_depth_reached,
        edges_traversed,
    })
}

/// BFS shortest path from `from` to `to` following structural edges.
pub(super) fn dependency_chain(
    conn: &Connection,
    from: &str,
    to: &str,
) -> anyhow::Result<Option<DependencyChain>> {
    // Same canonicalization as `impact_analysis`.
    let from = from.replace('\\', "/");
    let to = to.replace('\\', "/");
    let from = from.as_str();
    let to = to.as_str();
    let forward_graph = build_forward_graph(conn)?;

    let mut visited: HashSet<String> = HashSet::new();
    let mut parent: HashMap<String, String> = HashMap::new();
    let mut queue: VecDeque<String> = VecDeque::new();

    visited.insert(from.to_string());
    queue.push_back(from.to_string());

    while let Some(current) = queue.pop_front() {
        if current == to {
            let mut path = vec![to.to_string()];
            let mut cursor = to.to_string();
            while let Some(prev) = parent.get(&cursor) {
                path.push(prev.clone());
                cursor = prev.clone();
            }
            path.reverse();
            let depth = path.len() - 1;
            return Ok(Some(DependencyChain { path, depth }));
        }

        if let Some(deps) = forward_graph.get(&current) {
            for dep in deps {
                if visited.insert(dep.clone()) {
                    parent.insert(dep.clone(), current.clone());
                    queue.push_back(dep.clone());
                }
            }
        }
    }

    Ok(None)
}

/// Related files for a given path: direct neighbors via any structural edge,
/// sorted by edge weight (strongest relationship first). Returns (path, weight) pairs.
pub fn related_files(
    conn: &Connection,
    file_path: &str,
    limit: usize,
) -> anyhow::Result<Vec<(String, f64)>> {
    let sql = format!(
        "SELECT n_other.file_path, e.kind
         FROM edges e
         JOIN nodes n_self ON (e.source_id = n_self.id OR e.target_id = n_self.id)
         JOIN nodes n_other ON (
             (e.source_id = n_other.id AND e.target_id = n_self.id)
             OR (e.target_id = n_other.id AND e.source_id = n_self.id)
         )
         WHERE n_self.file_path = ?1
           AND n_other.file_path != ?1
           AND e.kind IN ({STRUCTURAL_EDGE_KINDS})"
    );
    let mut stmt = conn.prepare(&sql)?;

    let mut scores: HashMap<String, f64> = HashMap::new();
    let rows = stmt.query_map(params![file_path], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;

    for row in rows {
        let (path, kind) = row?;
        *scores.entry(path).or_default() += edge_weight(&kind);
    }

    let mut results: Vec<(String, f64)> = scores.into_iter().collect();
    results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    results.truncate(limit);
    Ok(results)
}

/// Graph connectivity stats for a file: incoming/outgoing edge counts by kind.
pub fn file_connectivity(
    conn: &Connection,
    file_path: &str,
) -> anyhow::Result<HashMap<String, (usize, usize)>> {
    let mut result: HashMap<String, (usize, usize)> = HashMap::new();

    let mut stmt_out = conn.prepare(
        "SELECT e.kind, COUNT(*)
         FROM edges e JOIN nodes n ON e.source_id = n.id
         WHERE n.file_path = ?1
         GROUP BY e.kind",
    )?;
    let rows = stmt_out.query_map(params![file_path], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    for row in rows {
        let (kind, count) = row?;
        result.entry(kind).or_insert((0, 0)).0 = count as usize;
    }

    let mut stmt_in = conn.prepare(
        "SELECT e.kind, COUNT(*)
         FROM edges e JOIN nodes n ON e.target_id = n.id
         WHERE n.file_path = ?1
         GROUP BY e.kind",
    )?;
    let rows = stmt_in.query_map(params![file_path], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    for row in rows {
        let (kind, count) = row?;
        result.entry(kind).or_insert((0, 0)).1 = count as usize;
    }

    Ok(result)
}

fn build_weighted_reverse_graph(
    conn: &Connection,
) -> anyhow::Result<HashMap<String, Vec<(String, f64)>>> {
    let sql = format!(
        "SELECT n_tgt.file_path, n_src.file_path, e.kind
         FROM edges e
         JOIN nodes n_src ON e.source_id = n_src.id
         JOIN nodes n_tgt ON e.target_id = n_tgt.id
         WHERE e.kind IN ({STRUCTURAL_EDGE_KINDS})
           AND n_src.file_path != n_tgt.file_path"
    );
    let mut stmt = conn.prepare(&sql)?;

    let mut graph: HashMap<String, HashMap<String, f64>> = HashMap::new();
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;

    for row in rows {
        let (target, source, kind) = row?;
        let w = edge_weight(&kind);
        let entry = graph
            .entry(target)
            .or_default()
            .entry(source)
            .or_insert(0.0);
        if w > *entry {
            *entry = w;
        }
    }

    Ok(graph
        .into_iter()
        .map(|(k, v)| (k, v.into_iter().collect()))
        .collect())
}

fn build_forward_graph(conn: &Connection) -> anyhow::Result<HashMap<String, Vec<String>>> {
    let sql = format!(
        "SELECT DISTINCT n_src.file_path, n_tgt.file_path
         FROM edges e
         JOIN nodes n_src ON e.source_id = n_src.id
         JOIN nodes n_tgt ON e.target_id = n_tgt.id
         WHERE e.kind IN ({STRUCTURAL_EDGE_KINDS})
           AND n_src.file_path != n_tgt.file_path"
    );
    let mut stmt = conn.prepare(&sql)?;

    let mut graph: HashMap<String, Vec<String>> = HashMap::new();
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;

    for row in rows {
        let (source, target) = row?;
        graph.entry(source).or_default().push(target);
    }

    for deps in graph.values_mut() {
        deps.sort();
        deps.dedup();
    }
    Ok(graph)
}
