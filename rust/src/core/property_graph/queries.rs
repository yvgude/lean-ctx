//! Graph traversal queries: dependents, dependencies, impact analysis,
//! dependency chains (BFS-based shortest path).
//!
//! All traversal queries support multi-edge traversal: imports, calls,
//! exports, type_ref, tested_by, and more. Edge kinds are weighted
//! for impact scoring.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

use rusqlite::{Connection, params};

#[derive(Debug, Clone)]
pub struct GraphQuery;

#[derive(Debug, Clone)]
pub struct ImpactResult {
    pub root_file: String,
    pub affected_files: Vec<String>,
    /// Affected files reachable *only* through name-match guesses
    /// (heuristic `calls` edges) — possibly not affected at all. Sorted.
    pub weak_files: Vec<String>,
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
    "'imports','calls','implements','exports','type_ref','tested_by','module','cochange','sibling'";

/// Weight multiplier per edge kind for impact scoring.
pub fn edge_weight(kind: &str) -> f64 {
    match kind {
        "imports" => 1.0,
        "calls" => 0.8,
        "exports" | "implements" => 0.7,
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
        "SELECT DISTINCT p_src.path
         FROM edges e
         JOIN nodes n_src ON e.source_id = n_src.id
         JOIN nodes n_tgt ON e.target_id = n_tgt.id
         JOIN paths p_src ON p_src.id = n_src.file_id
         JOIN paths p_tgt ON p_tgt.id = n_tgt.file_id
         WHERE p_tgt.path = ?1
           AND p_src.path != ?1
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
        "SELECT DISTINCT p_tgt.path
         FROM edges e
         JOIN nodes n_src ON e.source_id = n_src.id
         JOIN nodes n_tgt ON e.target_id = n_tgt.id
         JOIN paths p_src ON p_src.id = n_src.file_id
         JOIN paths p_tgt ON p_tgt.id = n_tgt.file_id
         WHERE p_src.path = ?1
           AND p_tgt.path != ?1
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

    let all = propagate(&reverse_graph, file_path, max_depth, false);
    // Files also reachable over evidence-backed edges alone; the rest hang on
    // name-match guesses only and are reported as such. A strong edge never
    // weighs more than the pair's strongest edge overall, so the strong set
    // is a subset of the full one.
    let strong = propagate(&reverse_graph, file_path, max_depth, true);
    let weak_files: Vec<String> = all.reached.difference(&strong.reached).cloned().collect();

    Ok(ImpactResult {
        root_file: file_path.to_string(),
        affected_files: all.reached.into_iter().collect(),
        weak_files,
        max_depth_reached: all.max_depth_reached,
        edges_traversed: all.edges_traversed,
    })
}

struct Propagation {
    reached: BTreeSet<String>,
    max_depth_reached: usize,
    edges_traversed: usize,
}

/// Weighted reverse propagation from `root` (excluded from the result) up
/// to `max_depth` hops; a file is reached when some path to it keeps a
/// cumulative weight ≥ 0.1. Exact, not first-come: a heavier path found
/// later still propagates, unless an equally heavy or heavier one already
/// arrived at the same or a shallower depth. Processing order is sorted, so
/// the result is deterministic. With `strong_only`, name-match-only edges
/// are not followed.
fn propagate(
    reverse_graph: &ReverseGraph,
    root: &str,
    max_depth: usize,
    strong_only: bool,
) -> Propagation {
    const PROPAGATION_THRESHOLD: f64 = 0.1;
    // Heaviest weight per file over all depths processed so far.
    let mut best: HashMap<&str, f64> = HashMap::from([(root, 1.0)]);
    let mut first_depth: BTreeMap<&str, usize> = BTreeMap::new();
    let mut layer: BTreeMap<&str, f64> = BTreeMap::from([(root, 1.0)]);
    let mut edges_traversed = 0;

    for depth in 1..=max_depth {
        let mut next: BTreeMap<&str, f64> = BTreeMap::new();
        for (file, weight) in &layer {
            for dep in reverse_graph.get(*file).into_iter().flatten() {
                let Some(edge_weight) = (if strong_only {
                    dep.strong_weight
                } else {
                    Some(dep.weight)
                }) else {
                    continue;
                };
                edges_traversed += 1;
                let propagated = weight * edge_weight;
                if propagated < PROPAGATION_THRESHOLD
                    || best
                        .get(dep.file.as_str())
                        .is_some_and(|b| *b >= propagated)
                {
                    continue;
                }
                let slot = next.entry(dep.file.as_str()).or_insert(0.0);
                *slot = slot.max(propagated);
            }
        }
        if next.is_empty() {
            break;
        }
        for (file, weight) in &next {
            best.insert(file, *weight);
            first_depth.entry(file).or_insert(depth);
        }
        layer = next;
    }
    first_depth.remove(root);
    Propagation {
        max_depth_reached: first_depth.values().copied().max().unwrap_or(0),
        reached: first_depth.into_keys().map(str::to_string).collect(),
        edges_traversed,
    }
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
        "SELECT p_other.path, e.kind, e.metadata
         FROM edges e
         JOIN nodes n_self ON (e.source_id = n_self.id OR e.target_id = n_self.id)
         JOIN nodes n_other ON (
             (e.source_id = n_other.id AND e.target_id = n_self.id)
             OR (e.target_id = n_other.id AND e.source_id = n_self.id)
         )
         JOIN paths p_self ON p_self.id = n_self.file_id
         JOIN paths p_other ON p_other.id = n_other.file_id
         WHERE p_self.path = ?1
           AND p_other.path != ?1
           AND e.kind IN ({STRUCTURAL_EDGE_KINDS})"
    );
    let mut stmt = conn.prepare(&sql)?;

    // One relationship can be stored several times — file→file plus
    // symbol→symbol `calls` edges for the same call. Count each (file, kind)
    // relationship once, at its strongest evidence, so finer-grained edges
    // cannot inflate a neighbour's score.
    // Ordered maps: floating-point sums must accumulate in a fixed order so
    // equal relationships always yield bit-identical scores (#498).
    let mut strongest: BTreeMap<(String, String), f64> = BTreeMap::new();
    let rows = stmt.query_map(params![file_path], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
        ))
    })?;
    for row in rows {
        let (path, kind, metadata) = row?;
        let w = evidence_weight(&kind, metadata.as_deref());
        let slot = strongest.entry((path, kind)).or_insert(0.0);
        if w > *slot {
            *slot = w;
        }
    }

    let mut scores: BTreeMap<String, f64> = BTreeMap::new();
    for ((path, _), w) in strongest {
        *scores.entry(path).or_default() += w;
    }

    let mut results: Vec<(String, f64)> = scores.into_iter().collect();
    results.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    results.truncate(limit);
    Ok(results)
}

/// Kind weight scaled by the edge's evidence grade (see
/// [`crate::core::semantic::EdgeEvidence`]); edges without evidence keep
/// their full kind weight.
pub(super) fn evidence_weight(kind: &str, metadata: Option<&str>) -> f64 {
    edge_weight(kind) * crate::core::semantic::EdgeEvidence::weight_factor_of(metadata)
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
         JOIN paths p ON p.id = n.file_id
         WHERE p.path = ?1
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
         JOIN paths p ON p.id = n.file_id
         WHERE p.path = ?1
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

/// A file depending on another, as seen from the dependency.
struct Dependent {
    file: String,
    /// Strongest edge of the pair.
    weight: f64,
    /// Strongest edge of the pair that is not a name-match guess, if any.
    strong_weight: Option<f64>,
}

/// Dependency → its dependents, both sorted by path.
type ReverseGraph = BTreeMap<String, Vec<Dependent>>;

fn build_weighted_reverse_graph(conn: &Connection) -> anyhow::Result<ReverseGraph> {
    let sql = format!(
        "SELECT p_tgt.path, p_src.path, e.kind, e.metadata
         FROM edges e
         JOIN nodes n_src ON e.source_id = n_src.id
         JOIN nodes n_tgt ON e.target_id = n_tgt.id
         JOIN paths p_src ON p_src.id = n_src.file_id
         JOIN paths p_tgt ON p_tgt.id = n_tgt.file_id
         WHERE e.kind IN ({STRUCTURAL_EDGE_KINDS})
           AND p_src.path != p_tgt.path"
    );
    let mut stmt = conn.prepare(&sql)?;

    let mut graph: BTreeMap<String, BTreeMap<String, (f64, Option<f64>)>> = BTreeMap::new();
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
        ))
    })?;

    for row in rows {
        let (target, source, kind, metadata) = row?;
        let w = evidence_weight(&kind, metadata.as_deref());
        // Only a name-match guess is weak; scope-bound, verified and
        // non-call structural edges (imports, type refs) are facts.
        let strong = crate::core::semantic::EdgeEvidence::from_metadata(metadata.as_deref())
            .is_none_or(|e| e.grade != crate::core::semantic::EvidenceGrade::HeuristicStructural);
        let entry = graph
            .entry(target)
            .or_default()
            .entry(source)
            .or_insert((0.0, None));
        entry.0 = entry.0.max(w);
        if strong {
            entry.1 = Some(entry.1.map_or(w, |s| s.max(w)));
        }
    }

    Ok(graph
        .into_iter()
        .map(|(k, v)| {
            (
                k,
                v.into_iter()
                    .map(|(file, (weight, strong_weight))| Dependent {
                        file,
                        weight,
                        strong_weight,
                    })
                    .collect(),
            )
        })
        .collect())
}

fn build_forward_graph(conn: &Connection) -> anyhow::Result<HashMap<String, Vec<String>>> {
    let sql = format!(
        "SELECT DISTINCT p_src.path, p_tgt.path
         FROM edges e
         JOIN nodes n_src ON e.source_id = n_src.id
         JOIN nodes n_tgt ON e.target_id = n_tgt.id
         JOIN paths p_src ON p_src.id = n_src.file_id
         JOIN paths p_tgt ON p_tgt.id = n_tgt.file_id
         WHERE e.kind IN ({STRUCTURAL_EDGE_KINDS})
           AND p_src.path != p_tgt.path"
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
