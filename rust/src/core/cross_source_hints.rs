//! Cross-source hints — lateral connections between cortical columns.
//!
//! When `ctx_read` delivers a file, this module appends hints about related
//! data from other sources (issues, PRs, DB schemas, wiki pages) discovered
//! via the graph index's cross-source edges.
//!
//! Scientific basis: Lateral connections in V1 cortex (Stettler et al., 2002)
//! enable feature integration across cortical columns.

use crate::core::graph_index::IndexEdge;

/// Weight factor of a hint reached through a directory reference rather than
/// the file itself.
const DIRECTORY_MATCH_FACTOR: f32 = 0.9;

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
/// store relative paths while ctx_read passes absolute ones.
pub fn hints_for_file(
    file_path: &str,
    edges: &[IndexEdge],
    project_root: &str,
) -> Vec<CrossSourceHint> {
    hints_for_file_matching(file_path, edges, project_root, |_| true)
}

/// Find cross-source hints, applying `include` before the five-item output cap.
///
/// Range-scoped reads use this to discard file-level and out-of-window hints
/// without allowing higher-weight irrelevant hints to hide a relevant one.
pub(crate) fn hints_for_file_matching(
    file_path: &str,
    edges: &[IndexEdge],
    project_root: &str,
    mut include: impl FnMut(&CrossSourceHint) -> bool,
) -> Vec<CrossSourceHint> {
    let rel = crate::core::graph_index::graph_relative_key(file_path, project_root);

    // The file itself, or a directory a provider declared the item to be
    // about (an emspace page pinned to `rust/src/dashboard`). A directory
    // match ranks below an exact one of the same weight.
    let path_weight = |edge_path: &str| -> Option<f32> {
        if edge_path == file_path || edge_path == rel {
            Some(1.0)
        } else if !edge_path.is_empty()
            && !edge_path.contains("://")
            // Separator-agnostic: Windows keys may carry `\`.
            && rel
                .replace('\\', "/")
                .strip_prefix(edge_path.replace('\\', "/").as_str())
                .is_some_and(|rest| rest.starts_with('/'))
        {
            Some(DIRECTORY_MATCH_FACTOR)
        } else {
            None
        }
    };

    let mut hints: Vec<CrossSourceHint> = edges
        .iter()
        .filter_map(|e| {
            if is_external_uri(&e.to)
                && let Some(factor) = path_weight(&e.from)
            {
                Some(CrossSourceHint {
                    source_uri: e.to.clone(),
                    relation: e.kind.clone(),
                    weight: e.weight * factor,
                })
            } else if is_external_uri(&e.from)
                && let Some(factor) = path_weight(&e.to)
            {
                Some(CrossSourceHint {
                    source_uri: e.from.clone(),
                    relation: e.kind.clone(),
                    weight: e.weight * factor,
                })
            } else {
                None
            }
        })
        .collect();

    hints.retain(|hint| include(hint));
    // Total order (weight, then URI, then relation) so the output is a function
    // of the graph's contents, not of the order its rows come back in (#498).
    hints.sort_by(|a, b| {
        b.weight
            .partial_cmp(&a.weight)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.source_uri.cmp(&b.source_uri))
            .then_with(|| a.relation.cmp(&b.relation))
    });
    // One hint per source, at its strongest relation: a provider edge is
    // stored in both directions (`documents` and `mentioned_in`), and the two
    // need not be adjacent after the sort.
    let mut seen = std::collections::HashSet::new();
    hints.retain(|hint| seen.insert(hint.source_uri.clone()));
    hints.truncate(5);
    hints
}

/// Format hints as a compact string for appending to ctx_read output.
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
    fn a_directory_reference_hints_files_below_it_after_exact_ones() {
        let edges = vec![
            edge("emspace://pages/core", "src/core", "documents", 1.0),
            edge(
                "emspace://pages/core/auth",
                "src/core/auth.rs",
                "documents",
                1.0,
            ),
            edge("emspace://pages/corelib", "src/corelib", "documents", 1.0),
        ];

        let hints = hints_for_file("src/core/auth.rs", &edges, ROOT);
        let uris: Vec<&str> = hints.iter().map(|h| h.source_uri.as_str()).collect();
        // `src/corelib` is not a parent of `src/core/auth.rs`.
        assert_eq!(
            uris,
            vec!["emspace://pages/core/auth", "emspace://pages/core"]
        );
        assert_eq!(hints_for_file("src/core", &edges, ROOT).len(), 1);
    }

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
    fn hint_output_is_byte_stable_for_the_same_graph_in_any_row_order() {
        let edges = vec![
            edge("emspace://pages/core", "src/a.rs", "documents", 0.8),
            edge("src/a.rs", "emspace://pages/core", "mentioned_in", 0.64),
            edge("github://issues/9", "src/a.rs", "mentions", 0.8),
            edge("github://issues/3", "src/a.rs", "mentions", 0.8),
            edge("emspace://pages/src", "src", "documents", 0.8),
            edge("jira://tickets/A-1", "src/a.rs", "mentions", 0.5),
        ];
        let expected = format_hints(&hints_for_file("src/a.rs", &edges, ROOT));
        assert!(expected.contains("emspace://pages/core [documents]"));
        assert_eq!(expected.matches("emspace://pages/core ").count(), 1);
        let mut rotated = edges.clone();
        for _ in 0..edges.len() {
            rotated.rotate_left(1);
            let mut reversed = rotated.clone();
            reversed.reverse();
            for order in [&rotated, &reversed] {
                assert_eq!(
                    format_hints(&hints_for_file("src/a.rs", order, ROOT)),
                    expected
                );
            }
        }
    }

    #[test]
    fn a_source_linked_both_ways_is_hinted_once_at_its_strongest_relation() {
        let edges = vec![
            edge("emspace://pages/core", "src/a.rs", "documents", 0.8),
            edge("github://issues/7", "src/a.rs", "mentions", 0.7),
            edge("src/a.rs", "emspace://pages/core", "mentioned_in", 0.64),
        ];
        let hints = hints_for_file("src/a.rs", &edges, ROOT);
        let seen: Vec<(&str, &str)> = hints
            .iter()
            .map(|h| (h.source_uri.as_str(), h.relation.as_str()))
            .collect();
        assert_eq!(
            seen,
            vec![
                ("emspace://pages/core", "documents"),
                ("github://issues/7", "mentions")
            ]
        );
    }

    #[test]
    fn range_filter_runs_before_output_limit() {
        let mut edges: Vec<IndexEdge> = (0..5)
            .map(|i| {
                edge(
                    "src/auth.rs",
                    &format!("health://complexity/src/auth.rs#irrelevant_{i}"),
                    "health_hotspot",
                    10.0,
                )
            })
            .collect();
        edges.push(edge(
            "src/auth.rs",
            "health://complexity/src/auth.rs#requested",
            "health_hotspot",
            1.0,
        ));

        let hints = hints_for_file_matching("src/auth.rs", &edges, ROOT, |hint| {
            hint.source_uri.ends_with("#requested")
        });
        assert_eq!(hints.len(), 1);
        assert!(hints[0].source_uri.ends_with("#requested"));
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
        {
            let found = &format_hints(&[]);
            assert!(found.is_empty(), "{found:?}");
        }
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
