//! Cross-source graph edges — connects external data to code via the graph index.
//!
//! When provider data (issues, PRs, DB schemas) references code files, this module
//! creates `IndexEdge` entries that the graph index uses for related-file discovery.
//!
//! Edge kinds:
//!   - `mentions`   — issue/PR body references a code file
//!   - `queries`    — code file queries a DB table
//!   - `documents`  — wiki page documents a code module
//!   - `resolves`   — PR resolves/fixes an issue
//!
//! Scientific basis: Scale-free networks (Barabasi-Albert) — cross-source edges
//! follow preferential attachment: files mentioned in many issues become graph hubs.

use crate::core::content_chunk::ContentChunk;
use crate::core::graph_index::IndexEdge;

/// Edge kind constants for cross-source relationships.
pub const EDGE_MENTIONS: &str = "mentions";
pub const EDGE_QUERIES: &str = "queries";
pub const EDGE_DOCUMENTS: &str = "documents";
pub const EDGE_RESOLVES: &str = "resolves";

/// Extract cross-source edges from a set of `ContentChunks`.
///
/// For each external chunk, creates edges from the chunk's URI to every
/// file path in its `references` list.
#[must_use]
pub fn extract_cross_source_edges(chunks: &[ContentChunk]) -> Vec<IndexEdge> {
    let mut edges = Vec::new();

    for chunk in chunks {
        if !chunk.is_external() || chunk.references.is_empty() {
            continue;
        }

        let edge_kind = chunk_to_edge_kind(chunk);

        for ref_path in &chunk.references {
            edges.push(IndexEdge {
                from: chunk.file_path.clone(),
                to: ref_path.clone(),
                kind: edge_kind.to_string(),
                weight: edge_weight_for_kind(edge_kind),
            });

            edges.push(IndexEdge {
                from: ref_path.clone(),
                to: chunk.file_path.clone(),
                kind: "mentioned_in".to_string(),
                weight: edge_weight_for_kind(edge_kind) * 0.8,
            });
        }
    }

    edges
}

/// Determine the edge kind based on the chunk's `ChunkKind`.
fn chunk_to_edge_kind(chunk: &ContentChunk) -> &'static str {
    use crate::core::chunk_data::ChunkKind;
    match chunk.kind {
        ChunkKind::PullRequest => EDGE_RESOLVES,
        ChunkKind::WikiPage => EDGE_DOCUMENTS,
        ChunkKind::DbSchema => EDGE_QUERIES,
        _ => EDGE_MENTIONS,
    }
}

/// Higher weight = stronger relationship. Issues and PRs that reference
/// code are high-value signals.
fn edge_weight_for_kind(kind: &str) -> f32 {
    match kind {
        EDGE_RESOLVES => 1.5,
        EDGE_QUERIES => 1.2,
        EDGE_DOCUMENTS => 0.8,
        _ => 1.0,
    }
}

/// Merge cross-source edges into an existing `ProjectIndex` edge list.
/// Deduplicates edges with the same (from, to, kind) triple, keeping
/// the higher weight.
pub fn merge_edges(existing: &mut Vec<IndexEdge>, new_edges: Vec<IndexEdge>) -> usize {
    let mut added = 0usize;
    for edge in new_edges {
        let duplicate = existing
            .iter_mut()
            .find(|e| e.from == edge.from && e.to == edge.to && e.kind == edge.kind);

        if let Some(existing_edge) = duplicate {
            if edge.weight > existing_edge.weight {
                existing_edge.weight = edge.weight;
            }
        } else {
            existing.push(edge);
            added += 1;
        }
    }
    added
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::chunk_data::ChunkKind;
    use crate::core::content_chunk::ContentChunk;

    fn issue_chunk(id: &str, refs: Vec<&str>) -> ContentChunk {
        ContentChunk::from_provider(
            "github",
            "issues",
            id,
            &format!("Issue #{id}"),
            ChunkKind::Issue,
            format!("Body of issue #{id}"),
            refs.into_iter().map(String::from).collect(),
            None,
        )
    }

    fn pr_chunk(id: &str, refs: Vec<&str>) -> ContentChunk {
        ContentChunk::from_provider(
            "github",
            "pull_requests",
            id,
            &format!("PR #{id}"),
            ChunkKind::PullRequest,
            format!("PR #{id} fixes auth"),
            refs.into_iter().map(String::from).collect(),
            None,
        )
    }

    fn wiki_chunk(id: &str, refs: Vec<&str>) -> ContentChunk {
        ContentChunk::from_provider(
            "confluence",
            "wikis",
            id,
            &format!("Wiki {id}"),
            ChunkKind::WikiPage,
            format!("Documentation for {id}"),
            refs.into_iter().map(String::from).collect(),
            None,
        )
    }

    #[test]
    fn issue_creates_mentions_edges() {
        let chunks = vec![issue_chunk("42", vec!["src/auth.rs", "src/db.rs"])];
        let edges = extract_cross_source_edges(&chunks);

        assert_eq!(edges.len(), 4); // 2 forward + 2 reverse
        assert!(edges.iter().any(|e| e.from.contains("issues/42")
            && e.to == "src/auth.rs"
            && e.kind == EDGE_MENTIONS));
        assert!(edges.iter().any(|e| e.from == "src/auth.rs"
            && e.to.contains("issues/42")
            && e.kind == "mentioned_in"));
    }

    #[test]
    fn pr_creates_resolves_edges() {
        let chunks = vec![pr_chunk("10", vec!["src/handler.rs"])];
        let edges = extract_cross_source_edges(&chunks);

        assert!(edges.iter().any(|e| e.kind == EDGE_RESOLVES));
        assert_eq!(
            edges
                .iter()
                .find(|e| e.kind == EDGE_RESOLVES)
                .unwrap()
                .weight,
            1.5
        );
    }

    #[test]
    fn wiki_creates_documents_edges() {
        let chunks = vec![wiki_chunk("auth-guide", vec!["src/auth/mod.rs"])];
        let edges = extract_cross_source_edges(&chunks);

        assert!(edges.iter().any(|e| e.kind == EDGE_DOCUMENTS));
    }

    #[test]
    fn no_edges_for_file_source_chunks() {
        let code_chunk = ContentChunk::from(crate::core::chunk_data::CodeChunk {
            file_path: "src/main.rs".into(),
            symbol_name: "main".into(),
            kind: ChunkKind::Function,
            start_line: 1,
            end_line: 10,
            content: "fn main() {}".into(),
            tokens: vec![],
            token_count: 0,
        });
        let edges = extract_cross_source_edges(&[code_chunk]);
        assert!(edges.is_empty());
    }

    #[test]
    fn no_edges_for_chunks_without_references() {
        let chunk = ContentChunk::from_provider(
            "github",
            "issues",
            "1",
            "Title",
            ChunkKind::Issue,
            "No file refs".into(),
            vec![],
            None,
        );
        let edges = extract_cross_source_edges(&[chunk]);
        assert!(edges.is_empty());
    }

    #[test]
    fn merge_edges_deduplicates() {
        let mut existing = vec![IndexEdge {
            from: "a".into(),
            to: "b".into(),
            kind: EDGE_MENTIONS.into(),
            weight: 1.0,
        }];

        let new = vec![
            IndexEdge {
                from: "a".into(),
                to: "b".into(),
                kind: EDGE_MENTIONS.into(),
                weight: 0.5, // lower weight, should not replace
            },
            IndexEdge {
                from: "a".into(),
                to: "c".into(),
                kind: EDGE_MENTIONS.into(),
                weight: 1.0,
            },
        ];

        let added = merge_edges(&mut existing, new);
        assert_eq!(added, 1);
        assert_eq!(existing.len(), 2);
        assert_eq!(existing.iter().find(|e| e.to == "b").unwrap().weight, 1.0);
    }

    #[test]
    fn merge_edges_upgrades_weight() {
        let mut existing = vec![IndexEdge {
            from: "a".into(),
            to: "b".into(),
            kind: EDGE_MENTIONS.into(),
            weight: 0.5,
        }];

        let new = vec![IndexEdge {
            from: "a".into(),
            to: "b".into(),
            kind: EDGE_MENTIONS.into(),
            weight: 2.0,
        }];

        merge_edges(&mut existing, new);
        assert_eq!(existing[0].weight, 2.0);
    }

    #[test]
    fn multiple_issues_referencing_same_file_creates_hub() {
        let chunks = vec![
            issue_chunk("1", vec!["src/auth.rs"]),
            issue_chunk("2", vec!["src/auth.rs"]),
            issue_chunk("3", vec!["src/auth.rs"]),
        ];

        let edges = extract_cross_source_edges(&chunks);
        let auth_incoming = edges
            .iter()
            .filter(|e| e.to == "src/auth.rs" && e.kind == EDGE_MENTIONS)
            .count();
        assert_eq!(auth_incoming, 3);
    }
}
