//! Renormalization-Inspired Multi-Scale Indexing.
//!
//! Scientific basis: Kenneth Wilson's Renormalization Group (Nobel Prize, 1982) —
//! describes systems at different scales using consistent transformations.
//! Each scale captures progressively coarser features:
//!
//! - Mikro (Chunk): Individual code chunks — precise symbol search
//! - Meso (File): Aggregated per-file representations — "which files are relevant?"
//! - Makro (Directory): Module-level aggregations — architecture queries
//!
//! The query-type classifier from `search_reranking` determines the entry scale:
//! - Symbol queries → Mikro directly
//! - NL queries → Meso → Mikro refinement
//! - Architecture queries → Makro → Meso → Mikro cascade

use std::collections::HashMap;

/// A scale-aggregated representation for search.
#[derive(Debug, Clone)]
pub struct ScaleEntry {
    pub path: String,
    pub tfidf_keywords: Vec<(String, f64)>,
    pub total_chunks: usize,
    pub avg_chunk_tokens: usize,
}

/// Multi-scale index holding representations at three granularities.
pub struct MultiScaleIndex {
    /// Mikro: individual chunks (delegated to `BM25Index`)
    pub micro_chunk_count: usize,
    /// Meso: per-file aggregated keywords and statistics
    pub meso_files: HashMap<String, ScaleEntry>,
    /// Makro: per-directory aggregated keywords
    pub macro_dirs: HashMap<String, ScaleEntry>,
}

impl MultiScaleIndex {
    #[must_use]
    pub fn new() -> Self {
        Self {
            micro_chunk_count: 0,
            meso_files: HashMap::new(),
            macro_dirs: HashMap::new(),
        }
    }

    /// Build meso and macro scales from chunk-level data.
    #[must_use]
    pub fn build_from_chunks(chunks: &[super::chunk_data::CodeChunk]) -> Self {
        let mut meso: HashMap<String, FileAccumulator> = HashMap::new();

        // Aggregate chunks into file-level entries
        for chunk in chunks {
            let acc = meso.entry(chunk.file_path.clone()).or_default();
            acc.chunk_count += 1;
            acc.total_tokens += chunk.token_count;
            for token in &chunk.tokens {
                *acc.token_freqs.entry(token.to_lowercase()).or_insert(0) += 1;
            }
        }

        // Build meso-scale entries with TF-IDF-like scoring
        let num_files = meso.len().max(1) as f64;
        let mut doc_freqs: HashMap<String, usize> = HashMap::new();
        for acc in meso.values() {
            for token in acc.token_freqs.keys() {
                *doc_freqs.entry(token.clone()).or_insert(0) += 1;
            }
        }

        let mut meso_files = HashMap::new();
        for (path, acc) in &meso {
            let mut keywords: Vec<(String, f64)> = acc
                .token_freqs
                .iter()
                .map(|(token, &tf)| {
                    let df = *doc_freqs.get(token).unwrap_or(&1) as f64;
                    let idf = (num_files / df).ln() + 1.0;
                    let tfidf = tf as f64 * idf;
                    (token.clone(), tfidf)
                })
                .collect();
            keywords.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            keywords.truncate(20); // Keep top 20 keywords per file

            meso_files.insert(
                path.clone(),
                ScaleEntry {
                    path: path.clone(),
                    tfidf_keywords: keywords,
                    total_chunks: acc.chunk_count,
                    avg_chunk_tokens: acc.total_tokens.checked_div(acc.chunk_count).unwrap_or(0),
                },
            );
        }

        // Build macro-scale: aggregate files into directories
        let mut macro_acc: HashMap<String, FileAccumulator> = HashMap::new();
        for (path, entry) in &meso_files {
            let dir = parent_dir(path);
            let acc = macro_acc.entry(dir).or_default();
            acc.chunk_count += entry.total_chunks;
            acc.total_tokens += entry.avg_chunk_tokens * entry.total_chunks;
            for (kw, score) in &entry.tfidf_keywords {
                *acc.token_freqs.entry(kw.clone()).or_insert(0) += *score as usize;
            }
        }

        let mut macro_dirs = HashMap::new();
        for (dir, acc) in &macro_acc {
            let mut keywords: Vec<(String, f64)> = acc
                .token_freqs
                .iter()
                .map(|(token, &count)| (token.clone(), count as f64))
                .collect();
            keywords.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            keywords.truncate(30); // Top 30 keywords per directory

            macro_dirs.insert(
                dir.clone(),
                ScaleEntry {
                    path: dir.clone(),
                    tfidf_keywords: keywords,
                    total_chunks: acc.chunk_count,
                    avg_chunk_tokens: acc.total_tokens.checked_div(acc.chunk_count).unwrap_or(0),
                },
            );
        }

        Self {
            micro_chunk_count: chunks.len(),
            meso_files,
            macro_dirs,
        }
    }

    /// Search at the meso (file) scale. Returns file paths with relevance scores.
    #[must_use]
    pub fn search_meso(&self, query_tokens: &[String], top_k: usize) -> Vec<(String, f64)> {
        let mut scores: Vec<(String, f64)> = self
            .meso_files
            .iter()
            .map(|(path, entry)| {
                let score = query_match_score(query_tokens, &entry.tfidf_keywords);
                (path.clone(), score)
            })
            .filter(|(_, s)| *s > 0.0)
            .collect();

        scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scores.truncate(top_k);
        scores
    }

    /// Search at the macro (directory) scale. Returns directory paths with relevance.
    #[must_use]
    pub fn search_macro(&self, query_tokens: &[String], top_k: usize) -> Vec<(String, f64)> {
        let mut scores: Vec<(String, f64)> = self
            .macro_dirs
            .iter()
            .map(|(dir, entry)| {
                let score = query_match_score(query_tokens, &entry.tfidf_keywords);
                (dir.clone(), score)
            })
            .filter(|(_, s)| *s > 0.0)
            .collect();

        scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scores.truncate(top_k);
        scores
    }

    /// Determine which scale to start search from based on query type.
    #[must_use]
    pub fn entry_scale(query_type: &super::search_reranking::QueryType) -> Scale {
        match query_type {
            super::search_reranking::QueryType::Symbol => Scale::Micro,
            super::search_reranking::QueryType::NaturalLanguage => Scale::Meso,
            super::search_reranking::QueryType::Architecture => Scale::Macro,
        }
    }
}

impl Default for MultiScaleIndex {
    fn default() -> Self {
        Self::new()
    }
}

/// Scale levels for the renormalization cascade.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Scale {
    Micro,
    Meso,
    Macro,
}

#[derive(Default)]
struct FileAccumulator {
    chunk_count: usize,
    total_tokens: usize,
    token_freqs: HashMap<String, usize>,
}

fn parent_dir(path: &str) -> String {
    let p = std::path::Path::new(path);
    p.parent()
        .map_or_else(|| ".".to_string(), |d| d.to_string_lossy().to_string())
}

fn query_match_score(query_tokens: &[String], keywords: &[(String, f64)]) -> f64 {
    let mut score = 0.0;
    for qt in query_tokens {
        let lower = qt.to_lowercase();
        for (kw, weight) in keywords {
            if kw.contains(&lower) || lower.contains(kw.as_str()) {
                score += weight;
            }
        }
    }
    score
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::chunk_data::{ChunkKind, CodeChunk};

    fn make_chunk(path: &str, content: &str, tokens: &[&str]) -> CodeChunk {
        CodeChunk {
            file_path: path.to_string(),
            symbol_name: "test".to_string(),
            kind: ChunkKind::Function,
            start_line: 1,
            end_line: 10,
            content: content.to_string(),
            tokens: tokens.iter().copied().map(str::to_string).collect(),
            token_count: tokens.len(),
        }
    }

    #[test]
    fn builds_meso_from_chunks() {
        let chunks = vec![
            make_chunk("src/auth.rs", "fn login() {}", &["fn", "login"]),
            make_chunk("src/auth.rs", "fn logout() {}", &["fn", "logout"]),
            make_chunk("src/db.rs", "fn query() {}", &["fn", "query", "sql"]),
        ];

        let index = MultiScaleIndex::build_from_chunks(&chunks);
        assert_eq!(index.meso_files.len(), 2);
        assert!(index.meso_files.contains_key("src/auth.rs"));
        assert!(index.meso_files.contains_key("src/db.rs"));
    }

    #[test]
    fn builds_macro_from_chunks() {
        let chunks = vec![
            make_chunk("src/auth/login.rs", "fn login() {}", &["login"]),
            make_chunk("src/auth/session.rs", "fn session() {}", &["session"]),
            make_chunk("src/db/pool.rs", "fn pool() {}", &["pool", "connection"]),
        ];

        let index = MultiScaleIndex::build_from_chunks(&chunks);
        assert!(index.macro_dirs.contains_key("src/auth"));
        assert!(index.macro_dirs.contains_key("src/db"));
    }

    #[test]
    fn meso_search_returns_relevant_files() {
        let chunks = vec![
            make_chunk(
                "src/auth.rs",
                "fn authenticate() {}",
                &["authenticate", "token", "jwt"],
            ),
            make_chunk("src/db.rs", "fn query() {}", &["query", "sql", "database"]),
        ];

        let index = MultiScaleIndex::build_from_chunks(&chunks);
        let results = index.search_meso(&["token".to_string(), "auth".to_string()], 5);
        assert!(!results.is_empty());
        assert_eq!(results[0].0, "src/auth.rs");
    }

    #[test]
    fn entry_scale_for_query_types() {
        use crate::core::search_reranking::QueryType;
        assert_eq!(
            MultiScaleIndex::entry_scale(&QueryType::Symbol),
            Scale::Micro
        );
        assert_eq!(
            MultiScaleIndex::entry_scale(&QueryType::NaturalLanguage),
            Scale::Meso
        );
        assert_eq!(
            MultiScaleIndex::entry_scale(&QueryType::Architecture),
            Scale::Macro
        );
    }
}
