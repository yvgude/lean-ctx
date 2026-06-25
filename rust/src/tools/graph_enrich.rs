use std::path::{Path, PathBuf};

use rusqlite::Connection;

/// A single node from the `code_index.db` nodes table.
pub struct GraphNode {
    pub id: i64,
    pub name: String,
    pub label: String,
    pub file_path: String,
    pub start_line: usize,
    pub end_line: usize,
}

/// A grep match (<file:line> content).
pub struct GrepMatch {
    pub file: String,
    pub line: usize,
    pub content: String,
}

/// An enriched hit representing a symbol that contains grep matches.
pub struct EnrichedHit {
    pub node_id: i64,
    pub file: String,
    pub name: String,
    pub label: String,
    pub start_line: usize,
    pub end_line: usize,
    pub match_lines: Vec<usize>,
}

fn code_index_path(root: &Path) -> PathBuf {
    crate::core::index_namespace::vectors_dir(root).join("code_index.db")
}

/// Open the `code_index.db` for the given project root.
/// Returns an error string if the db doesn't exist or can't be opened.
pub fn open_code_index(root: &Path) -> Result<Connection, String> {
    let path = code_index_path(root);
    if !path.exists() {
        return Err(format!("code_index.db not found at {}", path.display()));
    }
    Connection::open(&path).map_err(|e| format!("failed to open code_index.db: {e}"))
}

/// Query all nodes that belong to the given file, ordered by `start_line`.
/// Returns an empty vec on error.
pub fn query_nodes_for_file(conn: &Connection, file_path: &str) -> Vec<GraphNode> {
    let Ok(mut stmt) = conn.prepare(
         "SELECT id, name, label, start_line, end_line FROM nodes WHERE file_path = ? ORDER BY start_line ASC",
    ) else { return Vec::new() };

    let Ok(rows) = stmt.query_map([file_path], |row| {
        let id: i64 = row.get(0)?;
        let name: String = row.get(1)?;
        let label: String = row.get(2)?;
        let start_line: i64 = row.get(3)?;
        let end_line: i64 = row.get(4)?;
        Ok(GraphNode {
            id,
            name,
            label,
            file_path: file_path.to_string(),
            start_line: start_line as usize,
            end_line: end_line as usize,
        })
    }) else {
        return Vec::new();
    };

    rows.filter_map(std::result::Result::ok).collect()
}

/// Among nodes where `start_line` <= line <= `end_line`, return the one with the
/// smallest span (`end_line` - `start_line`). Ties are broken by preferring the
/// node with the smaller `start_line`.
#[must_use]
pub fn find_tightest_node(nodes: &[GraphNode], line: usize) -> Option<&GraphNode> {
    nodes
        .iter()
        .filter(|n| n.start_line <= line && line <= n.end_line)
        .min_by(|a, b| {
            let span_a = a.end_line - a.start_line;
            let span_b = b.end_line - b.start_line;
            span_a
                .cmp(&span_b)
                .then_with(|| a.start_line.cmp(&b.start_line))
        })
}

/// Classify grep matches into enriched hits (grouped by containing node) and
/// unmatched raw matches.
///
/// Returns `(enriched_hits, unmatched_raw)` where:
/// - `enriched_hits` are deduplicated by `node_id` with accumulated `match_lines`
/// - `unmatched_raw` contains matches that fell outside any node span
#[must_use]
pub fn classify_hits(
    matches: &[GrepMatch],
    nodes: &[GraphNode],
) -> (Vec<EnrichedHit>, Vec<GrepMatch>) {
    let mut hit_map: std::collections::BTreeMap<i64, EnrichedHit> =
        std::collections::BTreeMap::new();
    let mut unmatched: Vec<GrepMatch> = Vec::new();

    for gm in matches {
        if let Some(node) = find_tightest_node(nodes, gm.line) {
            hit_map
                .entry(node.id)
                .and_modify(|hit| {
                    hit.match_lines.push(gm.line);
                })
                .or_insert(EnrichedHit {
                    node_id: node.id,
                    file: node.file_path.clone(),
                    name: node.name.clone(),
                    label: node.label.clone(),
                    start_line: node.start_line,
                    end_line: node.end_line,
                    match_lines: vec![gm.line],
                });
        } else {
            unmatched.push(GrepMatch {
                file: gm.file.clone(),
                line: gm.line,
                content: gm.content.clone(),
            });
        }
    }

    // Ensure match_lines are sorted for deterministic output
    for hit in hit_map.values_mut() {
        hit.match_lines.sort_unstable();
    }

    let enriched: Vec<EnrichedHit> = hit_map.into_values().collect();
    (enriched, unmatched)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_node(id: i64, start: usize, end: usize) -> GraphNode {
        GraphNode {
            id,
            name: format!("sym_{id}"),
            label: "function".into(),
            file_path: "test.rs".into(),
            start_line: start,
            end_line: end,
        }
    }

    #[test]
    fn find_tightest_node_selects_smallest_span() {
        let nodes = vec![
            make_node(1, 1, 20), // outer
            make_node(2, 5, 15), // inner
            make_node(3, 8, 12), // tightest
        ];
        let found = find_tightest_node(&nodes, 10);
        assert_eq!(found.map(|n| n.id), Some(3));
    }

    #[test]
    fn find_tightest_node_breaks_ties_by_start_line() {
        let nodes = vec![make_node(1, 5, 15), make_node(2, 3, 13)];
        // Both have span 10; node 2 has smaller start_line (3 < 5).
        let found = find_tightest_node(&nodes, 10);
        assert_eq!(found.map(|n| n.id), Some(2));
    }

    #[test]
    fn find_tightest_node_returns_none_when_outside_all_spans() {
        let nodes = vec![make_node(1, 1, 10)];
        assert!(find_tightest_node(&nodes, 20).is_none());
    }

    #[test]
    fn classify_hits_deduplicates_by_node_id() {
        let nodes = vec![make_node(1, 1, 20)];
        let matches = vec![
            GrepMatch {
                file: "test.rs".into(),
                line: 5,
                content: "a".into(),
            },
            GrepMatch {
                file: "test.rs".into(),
                line: 10,
                content: "b".into(),
            },
        ];
        let (hits, raw) = classify_hits(&matches, &nodes);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].match_lines, vec![5, 10]);
        assert!(raw.is_empty());
    }

    #[test]
    fn classify_hits_unmatched_matches_returned_as_raw() {
        let nodes = vec![make_node(1, 1, 10)];
        let matches = vec![
            GrepMatch {
                file: "test.rs".into(),
                line: 5,
                content: "a".into(),
            },
            GrepMatch {
                file: "test.rs".into(),
                line: 20,
                content: "b".into(),
            },
        ];
        let (hits, raw) = classify_hits(&matches, &nodes);
        assert_eq!(hits.len(), 1);
        assert_eq!(raw.len(), 1);
        assert_eq!(raw[0].line, 20);
    }
}
