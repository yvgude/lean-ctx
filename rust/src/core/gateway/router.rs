//! Query router (#210): rank the aggregated catalog against a query.
//!
//! Reuses lean-ctx's existing [`crate::core::chunk_data::BM25Index`] (ephemeral, in-memory, built per
//! query batch) so ranking behaviour matches `ctx_search`/`ctx_semantic_search`
//! and stays deterministic for a fixed catalog.

use crate::core::chunk_data::{ChunkData, bm25_search};
use crate::core::content_chunk::{ContentChunk, ContentSource};

use super::catalog::{Catalog, CatalogEntry};

/// A ranked downstream tool.
#[derive(Debug, Clone, PartialEq)]
pub struct ScoredTool {
    pub entry: CatalogEntry,
    pub score: f64,
}

/// Rank the catalog against `query`, returning at most `top_n` tools.
///
/// Deterministic for a fixed catalog: BM25 relevance descending, ties broken by
/// the `server::tool` handle ascending. An empty query returns a stable prefix
/// of the (already handle-sorted) catalog so `ctx_tools find` always answers.
pub fn shortlist(catalog: &Catalog, query: &str, top_n: usize) -> Vec<ScoredTool> {
    if catalog.entries.is_empty() || top_n == 0 {
        return Vec::new();
    }

    let q = query.trim();
    if q.is_empty() {
        return catalog
            .entries
            .iter()
            .take(top_n)
            .cloned()
            .map(|entry| ScoredTool { entry, score: 0.0 })
            .collect();
    }

    let mut index = ChunkData::new();
    index.ingest_content_chunks(catalog.entries.iter().map(entry_to_chunk));

    // Over-fetch so post-dedup we can still fill top_n.
    let raw = bm25_search(&index, q, top_n.saturating_mul(2).max(top_n));
    let mut seen = std::collections::HashSet::new();
    let mut scored: Vec<ScoredTool> = Vec::new();
    for r in raw {
        if !seen.insert(r.file_path.clone()) {
            continue;
        }
        if let Some(entry) = catalog.find(&r.file_path) {
            scored.push(ScoredTool {
                entry: entry.clone(),
                score: r.score,
            });
        }
        if scored.len() >= top_n {
            break;
        }
    }
    // Stable tie-break (BM25 order is already score-desc; enforce handle asc on ties).
    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.entry.namespaced.cmp(&b.entry.namespaced))
    });
    scored
}

/// Build a searchable chunk for one tool. The tool name + server + handle are
/// included up front so exact tool-name queries rank highly; the description and
/// parameters add recall.
fn entry_to_chunk(e: &CatalogEntry) -> ContentChunk {
    let content = format!(
        "{tool} {server} {ns}\n{desc}\nparameters: {params}",
        tool = e.tool,
        server = e.server,
        ns = e.namespaced,
        desc = e.description,
        params = e.params,
    );
    ContentChunk {
        file_path: e.namespaced.clone(),
        symbol_name: e.tool.clone(),
        kind: crate::core::chunk_data::ChunkKind::Other,
        start_line: 0,
        end_line: 0,
        content,
        tokens: Vec::new(),
        token_count: 0,
        source: ContentSource::default(),
        references: Vec::new(),
        metadata: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(server: &str, tool: &str, desc: &str) -> CatalogEntry {
        CatalogEntry {
            server: server.into(),
            tool: tool.into(),
            namespaced: format!("{server}::{tool}"),
            description: desc.into(),
            params: String::new(),
        }
    }

    fn sample() -> Catalog {
        Catalog {
            entries: vec![
                entry("fs", "read_file", "Read the contents of a file from disk"),
                entry("fs", "write_file", "Write contents to a file on disk"),
                entry("git", "commit", "Create a git commit with a message"),
                entry("git", "log", "Show the git commit history log"),
                entry("web", "fetch", "Fetch a web page over http and return html"),
            ],
            errors: vec![],
        }
    }

    #[test]
    fn ranks_relevant_tool_first() {
        let cat = sample();
        let top = shortlist(&cat, "commit message to git", 3);
        assert!(!top.is_empty());
        assert_eq!(top[0].entry.namespaced, "git::commit");
        assert!(top.len() <= 3);
    }

    #[test]
    fn is_deterministic_across_runs() {
        let cat = sample();
        let a = shortlist(&cat, "read file from disk", 4);
        let b = shortlist(&cat, "read file from disk", 4);
        let ha: Vec<&str> = a.iter().map(|s| s.entry.namespaced.as_str()).collect();
        let hb: Vec<&str> = b.iter().map(|s| s.entry.namespaced.as_str()).collect();
        assert_eq!(ha, hb);
        assert_eq!(a[0].entry.namespaced, "fs::read_file");
    }

    #[test]
    fn empty_query_returns_stable_prefix() {
        let cat = sample();
        let top = shortlist(&cat, "   ", 2);
        assert_eq!(top.len(), 2);
        // catalog is handle-sorted upstream; here the sample is already sorted.
        assert_eq!(top[0].entry.namespaced, "fs::read_file");
    }

    #[test]
    fn empty_catalog_or_zero_n_is_empty() {
        let empty = Catalog::default();
        assert!(shortlist(&empty, "anything", 5).is_empty());
        assert!(shortlist(&sample(), "anything", 0).is_empty());
    }

    #[test]
    fn never_exceeds_top_n() {
        let cat = sample();
        let top = shortlist(&cat, "file disk git web fetch commit log read write", 2);
        assert!(top.len() <= 2);
    }
}
