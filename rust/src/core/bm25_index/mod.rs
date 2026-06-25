use std::collections::HashMap;

use serde::{Deserialize, Serialize};
mod build;
mod chunking;
pub use chunking::format_search_results;
pub(crate) use chunking::{enrich_for_bm25, extract_chunks, tokenize, tokenize_for_index};
mod coordinator;
pub use coordinator::{SearchIndexBuildProgress, get_or_start_build};
#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeChunk {
    pub file_path: String,
    pub symbol_name: String,
    pub kind: ChunkKind,
    pub start_line: usize,
    pub end_line: usize,
    pub content: String,
    #[serde(default)]
    pub tokens: Vec<String>,
    pub token_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ChunkKind {
    Function,
    Struct,
    Impl,
    Module,
    Class,
    Method,
    Other,
    // -- External source kinds (Context Engine) --
    Issue,
    PullRequest,
    WikiPage,
    DbSchema,
    ApiEndpoint,
    Ticket,
    ExternalOther,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct IndexedFileState {
    pub mtime_ms: u64,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub chunk_idx: usize,
    pub score: f64,
    pub file_path: String,
    pub symbol_name: String,
    pub kind: ChunkKind,
    pub start_line: usize,
    pub end_line: usize,
    pub snippet: String,
}

/// Data container for code chunks and their inverted index.
///
/// Replaces the former `BM25Index` struct. No longer provides BM25
/// ranking or search — use FTS5's native `bm25()` for that. This type
/// exists to hold chunk data for embedding, SPLADE expansion, and
/// dense/hybrid search paths that operate on chunk metadata rather than
/// BM25 scores.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkData {
    pub chunks: Vec<CodeChunk>,
    pub inverted: HashMap<String, Vec<(usize, f64)>>,
    pub avg_doc_len: f64,
    pub doc_count: usize,
    pub doc_freqs: HashMap<String, usize>,
    #[serde(default)]
    pub files: HashMap<String, IndexedFileState>,
    #[serde(default, skip)]
    pub content_truncated: bool,
}

/// Backward-compatible alias — struct was renamed to ChunkData.
/// New code should use `ChunkData` directly.
pub type BM25Index = ChunkData;

impl Default for ChunkData {
    fn default() -> Self {
        Self::new()
    }
}

impl ChunkData {
    /// Create an empty ChunkData.
    pub fn new() -> Self {
        Self {
            chunks: Vec::new(),
            inverted: HashMap::new(),
            avg_doc_len: 0.0,
            doc_count: 0,
            doc_freqs: HashMap::new(),
            files: HashMap::new(),
            content_truncated: false,
        }
    }

    /// Build a `ChunkData` from the chunks stored in `code_index.db`.
    ///
    /// Opens the SQLite database at `{vectors_dir}/code_index.db`, reads all
    /// rows from the `chunks` table (ordered by `id`), converts them to
    /// [`index_types::CodeChunk`] (the format `from_chunks` expects), and
    /// runs the full tokenisation + inverted-index pipeline.
    ///
    /// Returns an empty `ChunkData` when the database is missing or empty.
    pub fn build_from_directory(project_root: impl AsRef<std::path::Path>) -> Self {
        let dir = crate::core::index_namespace::vectors_dir(project_root.as_ref());
        let db_path = dir.join("code_index.db");
        if !db_path.exists() {
            return ChunkData::new();
        }

        let conn = match crate::core::db::WalConnection::open(&db_path) {
            Ok(c) => c,
            Err(_) => return ChunkData::new(),
        };

        let mut stmt = match conn.prepare(
            "SELECT file_path, content, content_hash, start_line, end_line, language
             FROM chunks ORDER BY id",
        ) {
            Ok(s) => s,
            Err(_) => return ChunkData::new(),
        };

        let rows = match stmt.query_map([], |row| {
            Ok(crate::core::index_types::CodeChunk {
                file_path: row.get(0)?,
                content: row.get(1)?,
                content_hash: row.get(2)?,
                start_line: row.get::<_, i64>(3)? as u32,
                end_line: row.get::<_, i64>(4)? as u32,
                language: row.get(5)?,
            })
        }) {
            Ok(r) => r,
            Err(_) => return ChunkData::new(),
        };

        let mut code_chunks: Vec<crate::core::index_types::CodeChunk> = Vec::new();
        for row in rows {
            match row {
                Ok(chunk) => code_chunks.push(chunk),
                Err(_) => continue,
            }
        }

        if code_chunks.is_empty() {
            return ChunkData::new();
        }

        ChunkData::from_chunks(&code_chunks)
    }

    /// Build a ChunkData from pre-extracted pipeline chunks.
    ///
    /// Takes chunks already extracted by the tree-sitter pipeline,
    /// converts to the internal `CodeChunk` format, tokenizes in parallel
    /// via rayon, and builds the inverted index sequentially.
    pub fn from_chunks(chunks: &[crate::core::index_types::CodeChunk]) -> Self {
        use rayon::prelude::*;

        let mut data = Self {
            chunks: Vec::new(),
            inverted: HashMap::new(),
            avg_doc_len: 0.0,
            doc_count: 0,
            doc_freqs: HashMap::new(),
            files: HashMap::new(),
            content_truncated: false,
        };

        // Phase 1: convert + enrich + tokenize in parallel
        let prepared: Vec<(CodeChunk, Vec<String>)> = chunks
            .par_iter()
            .map(|chunk| {
                let code_chunk = CodeChunk {
                    file_path: chunk.file_path.clone(),
                    symbol_name: String::new(),
                    kind: ChunkKind::Other,
                    start_line: chunk.start_line as usize,
                    end_line: chunk.end_line as usize,
                    content: chunk.content.clone(),
                    tokens: Vec::new(),
                    token_count: 0,
                };
                let enriched = enrich_for_bm25(&code_chunk);
                let tokens = tokenize(&enriched);
                (code_chunk, tokens)
            })
            .collect();

        // Phase 2: sequential inverted index insert
        for (code_chunk, tokens) in prepared {
            let idx = data.chunks.len();
            for token in &tokens {
                let lower = token.to_lowercase();
                let postings = data.inverted.entry(lower.clone()).or_default();
                if postings.last().map(|(last_idx, _)| *last_idx) != Some(idx) {
                    *data.doc_freqs.entry(lower).or_insert(0) += 1;
                }
                postings.push((idx, 1.0));
            }
            data.chunks.push(CodeChunk {
                token_count: tokens.len(),
                tokens: Vec::new(),
                ..code_chunk
            });
        }

        data.doc_count = data.chunks.len();
        if data.doc_count > 0 {
            let total_len: usize = data.chunks.iter().map(|c| c.token_count).sum();
            data.avg_doc_len = total_len as f64 / data.doc_count as f64;
        }

        data
    }

    /// Build a ChunkData from explicit CodeChunks (unit tests).
    #[cfg(test)]
    pub(crate) fn from_chunks_for_test(chunks: Vec<CodeChunk>) -> Self {
        let mut data = Self {
            chunks: Vec::new(),
            inverted: HashMap::new(),
            avg_doc_len: 0.0,
            doc_count: 0,
            doc_freqs: HashMap::new(),
            files: HashMap::new(),
            content_truncated: false,
        };

        for chunk in chunks {
            if chunk.token_count == 0 {
                tokenize(&chunk.content);
            }
            let idx = data.chunks.len();
            let enriched = enrich_for_bm25(&chunk);
            let tokens = tokenize(&enriched);
            for token in &tokens {
                let lower = token.to_lowercase();
                let postings = data.inverted.entry(lower.clone()).or_default();
                if postings.last().map(|(last_idx, _)| *last_idx) != Some(idx) {
                    *data.doc_freqs.entry(lower).or_insert(0) += 1;
                }
                postings.push((idx, 1.0));
            }
            data.chunks.push(CodeChunk {
                token_count: tokens.len(),
                tokens: Vec::new(),
                ..chunk
            });
        }

        data.doc_count = data.chunks.len();
        if data.doc_count > 0 {
            let total_len: usize = data.chunks.iter().map(|c| c.token_count).sum();
            data.avg_doc_len = total_len as f64 / data.doc_count as f64;
        }

        data
    }

    /// Ingest external `ContentChunk`s into the chunk data.
    /// Converts each chunk to a `CodeChunk` (backward-compatible) and
    /// rebuilds the inverted index. Returns the number of chunks ingested.
    pub fn ingest_content_chunks(
        &mut self,
        chunks: impl IntoIterator<Item = super::content_chunk::ContentChunk>,
    ) -> usize {
        let mut count = 0usize;
        for cc in chunks {
            let code_chunk: CodeChunk = cc.into();
            let idx = self.chunks.len();
            let enriched = enrich_for_bm25(&code_chunk);
            let tokens = tokenize(&enriched);
            for token in &tokens {
                let lower = token.to_lowercase();
                let postings = self.inverted.entry(lower.clone()).or_default();
                if postings.last().map(|(last_idx, _)| *last_idx) != Some(idx) {
                    *self.doc_freqs.entry(lower).or_insert(0) += 1;
                }
                postings.push((idx, 1.0));
            }
            self.chunks.push(CodeChunk {
                token_count: tokens.len(),
                tokens: Vec::new(),
                ..code_chunk
            });
            count += 1;
        }
        if count > 0 {
            self.doc_count = self.chunks.len();
            if self.doc_count > 0 {
                let total_len: usize = self.chunks.iter().map(|c| c.token_count).sum();
                self.avg_doc_len = total_len as f64 / self.doc_count as f64;
            }
        }
        count
    }

    /// Number of chunks originating from external providers.
    pub fn external_chunk_count(&self) -> usize {
        self.chunks
            .iter()
            .filter(|c| c.file_path.contains("://"))
            .count()
    }

    /// Approximate heap memory used by this data in bytes.
    pub fn memory_usage_bytes(&self) -> usize {
        let chunks_size: usize = self
            .chunks
            .iter()
            .map(|c| {
                c.content.len()
                    + c.file_path.len()
                    + c.symbol_name.len()
                    + c.tokens.iter().map(String::len).sum::<usize>()
                    + 64
            })
            .sum();
        let inverted_size: usize = self
            .inverted
            .iter()
            .map(|(k, v)| k.len() + v.len() * 16 + 32)
            .sum();
        let files_size: usize = self.files.keys().map(|k| k.len() + 24).sum();
        let freqs_size: usize = self.doc_freqs.keys().map(|k| k.len() + 16).sum();
        chunks_size + inverted_size + files_size + freqs_size
    }

    /// Drops all in-memory data, effectively freeing heap.
    pub fn unload(&mut self) {
        let usage = self.memory_usage_bytes();
        self.chunks = Vec::new();
        self.inverted = HashMap::new();
        self.doc_freqs = HashMap::new();
        self.files = HashMap::new();
        self.avg_doc_len = 0.0;
        self.doc_count = 0;
        tracing::info!(
            "[bm25] unloaded chunk data, freed ~{:.1}MB",
            usage as f64 / 1_048_576.0
        );
    }

    /// Shrinks each resident chunk's `content` to its first `keep_lines` lines.
    pub fn shrink_resident_content_to_snippet(&mut self, keep_lines: usize) {
        let before = self.memory_usage_bytes();
        for chunk in &mut self.chunks {
            if chunk.content.lines().nth(keep_lines).is_some() {
                let trimmed: String = chunk
                    .content
                    .lines()
                    .take(keep_lines)
                    .collect::<Vec<_>>()
                    .join("\n");
                chunk.content = trimmed;
                chunk.content.shrink_to_fit();
            }
        }
        self.content_truncated = true;
        let after = self.memory_usage_bytes();
        tracing::debug!(
            "[bm25] shrank resident content to {keep_lines} lines/chunk, freed ~{:.1}MB",
            before.saturating_sub(after) as f64 / 1_048_576.0
        );
    }
}

const BM25_K1: f64 = 1.2;
const BM25_B: f64 = 0.75;

/// BM25 search on [`ChunkData`] (used by artifact index, dense/hybrid fusion,
/// and SPLADE expansion paths that have no direct FTS5 access).
///
/// For code index search, prefer FTS5's native `bm25()` via the `chunks_fts`
/// virtual table — it's faster, deterministic, and avoids maintaining a separate
/// in-memory inverted index copy.
pub fn bm25_search(data: &ChunkData, query: &str, top_k: usize) -> Vec<SearchResult> {
    let query_tokens = tokenize(query);
    if query_tokens.is_empty() || data.doc_count == 0 {
        return Vec::new();
    }

    let n = data.chunks.len();
    let mut scores = vec![0.0f64; n];
    let mut touched = Vec::with_capacity(n.min(256));

    for token in &query_tokens {
        let lower = token.to_lowercase();
        let df = *data.doc_freqs.get(&lower).unwrap_or(&0) as f64;
        if df == 0.0 {
            continue;
        }

        let idf = ((data.doc_count as f64 - df + 0.5) / (df + 0.5) + 1.0).ln();

        if let Some(postings) = data.inverted.get(&lower) {
            for &(idx, weight) in postings {
                let doc_len = data.chunks[idx].token_count as f64;
                let norm_len = doc_len / data.avg_doc_len.max(1.0);
                let bm25 = idf * (weight * (BM25_K1 + 1.0))
                    / (weight + BM25_K1 * (1.0 - BM25_B + BM25_B * norm_len));

                if scores[idx] == 0.0 {
                    touched.push(idx);
                }
                scores[idx] += bm25;
            }
        }
    }

    let mut results: Vec<SearchResult> = touched
        .iter()
        .filter(|&&idx| scores[idx] > 0.0)
        .map(|&idx| {
            let chunk = &data.chunks[idx];
            let snippet = chunk.content.lines().take(5).collect::<Vec<_>>().join("\n");
            SearchResult {
                chunk_idx: idx,
                score: scores[idx],
                file_path: chunk.file_path.clone(),
                symbol_name: chunk.symbol_name.clone(),
                kind: chunk.kind.clone(),
                start_line: chunk.start_line,
                end_line: chunk.end_line,
                snippet,
            }
        })
        .collect();

    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.file_path.cmp(&b.file_path))
            .then_with(|| a.symbol_name.cmp(&b.symbol_name))
            .then_with(|| a.start_line.cmp(&b.start_line))
            .then_with(|| a.end_line.cmp(&b.end_line))
    });
    results.truncate(top_k);
    results
}

pub fn is_code_file(path: &std::path::Path) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    matches!(
        ext.as_str(),
        "rs" | "ts"
            | "tsx"
            | "js"
            | "jsx"
            | "py"
            | "go"
            | "java"
            | "c"
            | "cc"
            | "cpp"
            | "h"
            | "hpp"
            | "rb"
            | "cs"
            | "kt"
            | "swift"
            | "php"
            | "scala"
            | "sql"
            | "ex"
            | "exs"
            | "zig"
            | "lua"
            | "dart"
            | "vue"
            | "svelte"
    )
}
