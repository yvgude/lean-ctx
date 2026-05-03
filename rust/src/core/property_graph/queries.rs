//! Graph traversal queries: dependents, dependencies, impact analysis,
//! dependency chains (BFS-based shortest path).

use std::collections::{HashMap, HashSet, VecDeque};

use rusqlite::{params, Connection};

#[derive(Debug, Clone)]
pub struct GraphQuery;

#[derive(Debug, Clone)]
pub struct ImpactResult {
    pub root_file: String,
    pub affected_files: Vec<String>,
    pub max_depth_reached: usize,
    pub edges_traversed: usize,
}

#[derive(Debug, Clone)]
pub struct DependencyChain {
    pub path: Vec<String>,
    pub depth: usize,
}

/// Files that import (depend on) `file_path`.
pub fn dependents(conn: &Connection, file_path: &str) -> anyhow::Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT n_src.file_path
         FROM edges e
         JOIN nodes n_src ON e.source_id = n_src.id
         JOIN nodes n_tgt ON e.target_id = n_tgt.id
         WHERE n_tgt.file_path = ?1
           AND n_src.file_path != ?1
           AND e.kind = 'imports'",
    )?;

    let results: Vec<String> = stmt
        .query_map(params![file_path], |row| row.get(0))?
        .filter_map(std::result::Result::ok)
        .collect();

    Ok(results)
}

/// Files that `file_path` imports (depends on).
pub fn dependencies(conn: &Connection, file_path: &str) -> anyhow::Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT n_tgt.file_path
         FROM edges e
         JOIN nodes n_src ON e.source_id = n_src.id
         JOIN nodes n_tgt ON e.target_id = n_tgt.id
         WHERE n_src.file_path = ?1
           AND n_tgt.file_path != ?1
           AND e.kind = 'imports'",
    )?;

    let results: Vec<String> = stmt
        .query_map(params![file_path], |row| row.get(0))?
        .filter_map(std::result::Result::ok)
        .collect();

    Ok(results)
}

/// BFS from `file_path` following reverse import edges up to `max_depth`.
/// Returns all transitively affected files.
pub fn impact_analysis(
    conn: &Connection,
    file_path: &str,
    max_depth: usize,
) -> anyhow::Result<ImpactResult> {
    let reverse_graph = build_reverse_import_graph(conn)?;

    let mut visited: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<(String, usize)> = VecDeque::new();
    let mut max_depth_reached = 0;
    let mut edges_traversed = 0;

    visited.insert(file_path.to_string());
    queue.push_back((file_path.to_string(), 0));

    while let Some((current, depth)) = queue.pop_front() {
        if depth >= max_depth {
            continue;
        }

        if let Some(dependents) = reverse_graph.get(&current) {
            for dep in dependents {
                edges_traversed += 1;
                if visited.insert(dep.clone()) {
                    let new_depth = depth + 1;
                    if new_depth > max_depth_reached {
                        max_depth_reached = new_depth;
                    }
                    queue.push_back((dep.clone(), new_depth));
                }
            }
        }
    }

    visited.remove(file_path);

    Ok(ImpactResult {
        root_file: file_path.to_string(),
        affected_files: visited.into_iter().collect(),
        max_depth_reached,
        edges_traversed,
    })
}

/// BFS shortest path from `from` to `to` following import edges.
pub fn dependency_chain(
    conn: &Connection,
    from: &str,
    to: &str,
) -> anyhow::Result<Option<DependencyChain>> {
    let forward_graph = build_forward_import_graph(conn)?;

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

fn build_reverse_import_graph(conn: &Connection) -> anyhow::Result<HashMap<String, Vec<String>>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT n_tgt.file_path, n_src.file_path
         FROM edges e
         JOIN nodes n_src ON e.source_id = n_src.id
         JOIN nodes n_tgt ON e.target_id = n_tgt.id
         WHERE e.kind = 'imports'
           AND n_src.file_path != n_tgt.file_path",
    )?;

    let mut graph: HashMap<String, Vec<String>> = HashMap::new();
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;

    for row in rows {
        let (target, source) = row?;
        graph.entry(target).or_default().push(source);
    }

    Ok(graph)
}

fn build_forward_import_graph(conn: &Connection) -> anyhow::Result<HashMap<String, Vec<String>>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT n_src.file_path, n_tgt.file_path
         FROM edges e
         JOIN nodes n_src ON e.source_id = n_src.id
         JOIN nodes n_tgt ON e.target_id = n_tgt.id
         WHERE e.kind = 'imports'
           AND n_src.file_path != n_tgt.file_path",
    )?;

    let mut graph: HashMap<String, Vec<String>> = HashMap::new();
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;

    for row in rows {
        let (source, target) = row?;
        graph.entry(source).or_default().push(target);
    }

    Ok(graph)
}
