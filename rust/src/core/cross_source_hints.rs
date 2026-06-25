//! Cross-source hints — lateral connections between cortical columns.
//!
//! When `ctx_read` delivers a file, this module appends hints about related
//! data from other sources (issues, PRs, DB schemas, wiki pages) discovered
//! via the graph index's cross-source edges.
//!
//! Scientific basis: Lateral connections in V1 cortex (Stettler et al., 2002)
//! enable feature integration across cortical columns.

use crate::core::graph_index::IndexEdge;

/// A hint about related data from another source.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CrossSourceHint {
    pub source_uri: String,
    pub relation: String,
    pub weight: f32,
}

/// Find cross-source hints for a given file path by looking up
/// edges in the graph index that connect to external URIs.
/// Matches both absolute and project-relative paths since edges
/// store relative paths while `ctx_read` passes absolute ones.
#[must_use]
pub fn hints_for_file(
    file_path: &str,
    edges: &[IndexEdge],
    project_root: &str,
) -> Vec<CrossSourceHint> {
    let rel = crate::core::graph_index::graph_relative_key(file_path, project_root);

    let matches_path = |edge_path: &str| -> bool { edge_path == file_path || edge_path == rel };

    let mut hints: Vec<CrossSourceHint> = edges
        .iter()
        .filter(|e| {
            (matches_path(&e.from) && is_external_uri(&e.to))
                || (matches_path(&e.to) && is_external_uri(&e.from))
        })
        .map(|e| {
            if matches_path(&e.from) {
                CrossSourceHint {
                    source_uri: e.to.clone(),
                    relation: e.kind.clone(),
                    weight: e.weight,
                }
            } else {
                CrossSourceHint {
                    source_uri: e.from.clone(),
                    relation: e.kind.clone(),
                    weight: e.weight,
                }
            }
        })
        .collect();

    hints.sort_by(|a, b| {
        b.weight
            .partial_cmp(&a.weight)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    hints.dedup_by(|a, b| a.source_uri == b.source_uri);
    hints.truncate(5);
    hints
}

/// Format hints as a compact string for appending to `ctx_read` output.
#[must_use]
pub fn format_hints(hints: &[CrossSourceHint]) -> String {
    if hints.is_empty() {
        return String::new();
    }

    let mut out = String::from("\n--- Cross-Source Hints ---\n");
    for hint in hints {
        out.push_str(&format!(
            "  {} [{}] w={:.1}\n",
            hint.source_uri, hint.relation, hint.weight
        ));
    }
    out
}

fn is_external_uri(path: &str) -> bool {
    path.contains("://")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::graph_index::IndexEdge;

    fn edge(from: &str, to: &str, kind: &str, weight: f32) -> IndexEdge {
        IndexEdge {
            from: from.into(),
            to: to.into(),
            kind: kind.into(),
            weight,
        }
    }

    const ROOT: &str = "/project";

    #[test]
    fn finds_hints_from_forward_edges() {
        let edges = vec![
            edge("src/auth.rs", "github://issues/42", "mentions", 1.0),
            edge("src/auth.rs", "postgres://schemas/sessions", "queries", 1.2),
        ];

        let hints = hints_for_file("src/auth.rs", &edges, ROOT);
        assert_eq!(hints.len(), 2);
        assert!(hints.iter().any(|h| h.source_uri.contains("issues/42")));
        assert!(
            hints
                .iter()
                .any(|h| h.source_uri.contains("schemas/sessions"))
        );
    }

    #[test]
    fn finds_hints_from_reverse_edges() {
        let edges = vec![edge(
            "github://issues/42",
            "src/auth.rs",
            "mentioned_in",
            0.8,
        )];

        let hints = hints_for_file("src/auth.rs", &edges, ROOT);
        assert_eq!(hints.len(), 1);
        assert!(hints[0].source_uri.contains("issues/42"));
    }

    #[test]
    fn finds_hints_with_absolute_path() {
        let edges = vec![edge("src/auth.rs", "github://issues/42", "mentions", 1.0)];
        let hints = hints_for_file("/project/src/auth.rs", &edges, "/project");
        assert_eq!(hints.len(), 1, "absolute path should match relative edge");
    }

    #[test]
    fn ignores_code_to_code_edges() {
        let edges = vec![edge("src/auth.rs", "src/db.rs", "imports", 1.0)];

        let hints = hints_for_file("src/auth.rs", &edges, ROOT);
        assert!(hints.is_empty());
    }

    #[test]
    fn deduplicates_and_limits_to_5() {
        let edges: Vec<IndexEdge> = (0..10)
            .map(|i| {
                edge(
                    "src/auth.rs",
                    &format!("github://issues/{i}"),
                    "mentions",
                    1.0,
                )
            })
            .collect();

        let hints = hints_for_file("src/auth.rs", &edges, ROOT);
        assert_eq!(hints.len(), 5);
    }

    #[test]
    fn sorts_by_weight_descending() {
        let edges = vec![
            edge("src/auth.rs", "github://issues/1", "mentions", 0.5),
            edge("src/auth.rs", "github://issues/2", "mentions", 1.5),
            edge("src/auth.rs", "github://issues/3", "mentions", 1.0),
        ];

        let hints = hints_for_file("src/auth.rs", &edges, ROOT);
        assert_eq!(hints[0].source_uri, "github://issues/2");
        assert_eq!(hints[1].source_uri, "github://issues/3");
        assert_eq!(hints[2].source_uri, "github://issues/1");
    }

    #[test]
    fn format_hints_empty_returns_empty() {
        assert!(format_hints(&[]).is_empty());
    }

    #[test]
    fn format_hints_produces_readable_output() {
        let hints = vec![CrossSourceHint {
            source_uri: "github://issues/42".into(),
            relation: "mentions".into(),
            weight: 1.0,
        }];

        let output = format_hints(&hints);
        assert!(output.contains("Cross-Source Hints"));
        assert!(output.contains("github://issues/42"));
        assert!(output.contains("[mentions]"));
    }
}
