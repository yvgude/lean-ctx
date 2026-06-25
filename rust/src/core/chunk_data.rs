use std::collections::HashMap;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

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

/// Backward-compatible alias — struct was renamed to `ChunkData`.
/// New code should use `ChunkData` directly.
pub type BM25Index = ChunkData;

impl Default for ChunkData {
    fn default() -> Self {
        Self::new()
    }
}

impl ChunkData {
    /// Create an empty `ChunkData`.
    #[must_use]
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
    /// [`crate::core::index_types::CodeChunk`] (the format `from_chunks` expects), and
    /// runs the full tokenisation + inverted-index pipeline.
    ///
    /// Returns an empty `ChunkData` when the database is missing or empty.
    pub fn build_from_directory(project_root: impl AsRef<std::path::Path>) -> Self {
        let dir = crate::core::index_namespace::vectors_dir(project_root.as_ref());
        let db_path = dir.join("code_index.db");
        if !db_path.exists() {
            return ChunkData::new();
        }

        let Ok(conn) = crate::core::db::WalConnection::open(&db_path) else {
            return ChunkData::new();
        };

        let Ok(mut stmt) = conn.prepare(
            "SELECT file_path, content, content_hash, start_line, end_line, language, symbol_name, kind
              FROM chunks ORDER BY id",
        ) else {
            return ChunkData::new();
        };

        let Ok(rows) = stmt.query_map([], |row| {
            Ok(crate::core::index_types::CodeChunk {
                file_path: row.get(0)?,
                content: row.get(1)?,
                content_hash: row.get(2)?,
                start_line: row.get::<_, i64>(3)? as u32,
                end_line: row.get::<_, i64>(4)? as u32,
                language: row.get(5)?,
                symbol_name: row.get(6)?,
                kind: row.get(7)?,
            })
        }) else {
            return ChunkData::new();
        };

        let mut code_chunks: Vec<crate::core::index_types::CodeChunk> = Vec::new();
        for row in rows.flatten() {
            code_chunks.push(row);
        }

        if code_chunks.is_empty() {
            return ChunkData::new();
        }

        ChunkData::from_chunks(&code_chunks)
    }

    /// Build a `ChunkData` from pre-extracted pipeline chunks.
    ///
    /// Takes chunks already extracted by the tree-sitter pipeline,
    /// converts to the internal `CodeChunk` format, tokenizes in parallel
    /// via rayon, and builds the inverted index sequentially.
    #[must_use]
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
                let kind = if chunk.kind.is_empty() {
                    ChunkKind::Other
                } else {
                    serde_json::from_str(&chunk.kind).unwrap_or(ChunkKind::Other)
                };
                let code_chunk = CodeChunk {
                    file_path: chunk.file_path.clone(),
                    symbol_name: chunk.symbol_name.clone(),
                    kind,
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
    #[must_use]
    pub fn external_chunk_count(&self) -> usize {
        self.chunks
            .iter()
            .filter(|c| c.file_path.contains("://"))
            .count()
    }

    /// Approximate heap memory used by this data in bytes.
    #[must_use]
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

// ---------------------------------------------------------------------------
// BM25 search
// ---------------------------------------------------------------------------

const BM25_K1: f64 = 1.2;
const BM25_B: f64 = 0.75;

/// BM25 search on [`ChunkData`] (used by artifact index, dense/hybrid fusion,
/// and SPLADE expansion paths that have no direct FTS5 access).
///
/// For code index search, prefer FTS5's native `bm25()` via the `chunks_fts`
/// virtual table — it's faster, deterministic, and avoids maintaining a separate
/// in-memory inverted index copy.
#[must_use]
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

#[must_use]
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

// ---------------------------------------------------------------------------
// Tokenization / chunking helpers
// ---------------------------------------------------------------------------

pub(crate) fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();

    for ch in text.chars() {
        if ch.is_alphanumeric() || ch == '_' {
            current.push(ch);
        } else {
            if current.len() >= 2 {
                tokens.push(current.clone());
            }
            current.clear();
        }
    }
    if current.len() >= 2 {
        tokens.push(current);
    }

    split_camel_case_tokens(&tokens)
}

pub(crate) fn tokenize_for_index(text: &str) -> Vec<String> {
    tokenize(text)
}

pub(crate) fn split_camel_case_tokens(tokens: &[String]) -> Vec<String> {
    let mut result = Vec::new();
    for token in tokens {
        result.push(token.clone());
        let mut start = 0;
        let chars: Vec<char> = token.chars().collect();
        for i in 1..chars.len() {
            if chars[i].is_uppercase() && (i + 1 >= chars.len() || !chars[i + 1].is_uppercase()) {
                let part: String = chars[start..i].iter().collect();
                if part.len() >= 2 {
                    result.push(part);
                }
                start = i;
            }
        }
        if start > 0 {
            let part: String = chars[start..].iter().collect();
            if part.len() >= 2 {
                result.push(part);
            }
        }
    }
    result
}

/// Fallback only: used by the non-tree-sitter regex path and incremental builder.
/// Prefer `BM25Index::from_chunks` (Phase 5) which takes pre-extracted pipeline chunks
/// and avoids file re-reads.
pub(crate) fn extract_chunks(file_path: &str, content: &str) -> Vec<CodeChunk> {
    #[cfg(feature = "tree-sitter")]
    {
        let ext = std::path::Path::new(file_path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        if let Some(chunks) = crate::core::chunks_ts::extract_chunks_ts(file_path, content, ext) {
            return chunks;
        }
    }

    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        return Vec::new();
    }

    let mut chunks = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        let trimmed = lines[i].trim();

        if let Some((name, kind)) = detect_symbol(trimmed) {
            let start = i;
            let end = find_block_end(&lines, i);
            let block: String = lines[start..=end.min(lines.len() - 1)].to_vec().join("\n");
            let token_count = tokenize(&block).len();

            chunks.push(CodeChunk {
                file_path: file_path.to_string(),
                symbol_name: name,
                kind,
                start_line: start + 1,
                end_line: end + 1,
                content: block,
                tokens: Vec::new(),
                token_count,
            });

            i = end + 1;
        } else {
            i += 1;
        }
    }

    if chunks.is_empty() && !content.is_empty() {
        // Fallback: when no symbols are detected, chunk the file into stable, content-defined
        // segments (rolling-hash) to enable meaningful semantic search over non-code assets.
        //
        // Safety note: rabin_karp uses byte offsets; we must slice bytes and decode safely.
        let bytes = content.as_bytes();
        let rk_chunks = crate::core::rabin_karp::chunk(content);
        if !rk_chunks.is_empty() && rk_chunks.len() <= 200 {
            for (idx, c) in rk_chunks.into_iter().take(50).enumerate() {
                let end = (c.offset + c.length).min(bytes.len());
                let slice = &bytes[c.offset..end];
                let chunk_text = String::from_utf8_lossy(slice).into_owned();
                let token_count = tokenize(&chunk_text).len();
                let start_line = 1 + bytecount::count(&bytes[..c.offset], b'\n');
                let end_line = start_line + bytecount::count(slice, b'\n');
                chunks.push(CodeChunk {
                    file_path: file_path.to_string(),
                    symbol_name: format!("{file_path}#chunk-{idx}"),
                    kind: ChunkKind::Module,
                    start_line,
                    end_line: end_line.max(start_line),
                    content: chunk_text,
                    tokens: Vec::new(),
                    token_count,
                });
            }
        } else {
            let token_count = tokenize(content).len();
            let snippet = lines
                .iter()
                .take(50)
                .copied()
                .collect::<Vec<_>>()
                .join("\n");
            chunks.push(CodeChunk {
                file_path: file_path.to_string(),
                symbol_name: file_path.to_string(),
                kind: ChunkKind::Module,
                start_line: 1,
                end_line: lines.len(),
                content: snippet,
                tokens: Vec::new(),
                token_count,
            });
        }
    }

    chunks
}

pub(crate) fn detect_symbol(line: &str) -> Option<(String, ChunkKind)> {
    let trimmed = line.trim();

    let patterns: &[(&str, ChunkKind)] = &[
        ("pub async fn ", ChunkKind::Function),
        ("async fn ", ChunkKind::Function),
        ("pub fn ", ChunkKind::Function),
        ("fn ", ChunkKind::Function),
        ("pub struct ", ChunkKind::Struct),
        ("struct ", ChunkKind::Struct),
        ("pub enum ", ChunkKind::Struct),
        ("enum ", ChunkKind::Struct),
        ("impl ", ChunkKind::Impl),
        ("pub trait ", ChunkKind::Struct),
        ("trait ", ChunkKind::Struct),
        ("export function ", ChunkKind::Function),
        ("export async function ", ChunkKind::Function),
        ("export default function ", ChunkKind::Function),
        ("function ", ChunkKind::Function),
        ("async function ", ChunkKind::Function),
        ("export class ", ChunkKind::Class),
        ("class ", ChunkKind::Class),
        ("export interface ", ChunkKind::Struct),
        ("interface ", ChunkKind::Struct),
        ("def ", ChunkKind::Function),
        ("async def ", ChunkKind::Function),
        ("func ", ChunkKind::Function),
    ];

    for (prefix, kind) in patterns {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '<')
                .take_while(|c| *c != '<')
                .collect();
            if !name.is_empty() {
                return Some((name, kind.clone()));
            }
        }
    }

    None
}

pub(crate) fn find_block_end(lines: &[&str], start: usize) -> usize {
    let mut depth = 0i32;
    let mut found_open = false;

    for (i, line) in lines.iter().enumerate().skip(start) {
        for ch in line.chars() {
            match ch {
                '{' | '(' if !found_open || depth > 0 => {
                    depth += 1;
                    found_open = true;
                }
                '}' | ')' if depth > 0 => {
                    depth -= 1;
                    if depth == 0 && found_open {
                        return i;
                    }
                }
                _ => {}
            }
        }

        if found_open && depth <= 0 && i > start {
            return i;
        }

        if !found_open && i > start + 2 {
            let trimmed = lines[i].trim();
            if trimmed.is_empty()
                || (!trimmed.starts_with(' ') && !trimmed.starts_with('\t') && i > start)
            {
                return i.saturating_sub(1);
            }
        }
    }

    (start + 50).min(lines.len().saturating_sub(1))
}

#[must_use]
pub fn format_search_results(results: &[SearchResult], compact: bool) -> String {
    if results.is_empty() {
        return "No results found.".to_string();
    }

    let mut out = String::new();
    for (i, r) in results.iter().enumerate() {
        let is_external = r.file_path.contains("://");
        // Forward-slash normalize local paths so Windows backslashes are never
        // dropped/escape-mangled by client render layers (issue #324). External
        // URIs (provider results, e.g. `github://`) are left untouched.
        let normalized;
        let file_path: &str = if is_external {
            &r.file_path
        } else {
            normalized = crate::core::protocol::display_path(&r.file_path);
            &normalized
        };
        if compact {
            if is_external {
                out.push_str(&format!(
                    "{}. {:.2} [{:?}] {} — {}\n",
                    i + 1,
                    r.score,
                    r.kind,
                    file_path,
                    r.symbol_name,
                ));
            } else {
                out.push_str(&format!(
                    "{}. {:.2} {}:{}-{} {:?} {}\n",
                    i + 1,
                    r.score,
                    file_path,
                    r.start_line,
                    r.end_line,
                    r.kind,
                    r.symbol_name,
                ));
            }
        } else if is_external {
            out.push_str(&format!(
                "\n--- Result {} (score: {:.2}) [{:?}] ---\n{} — {}\n{}\n",
                i + 1,
                r.score,
                r.kind,
                file_path,
                r.symbol_name,
                r.snippet,
            ));
        } else {
            out.push_str(&format!(
                "\n--- Result {} (score: {:.2}) ---\n{} :: {} [{:?}] (L{}-{})\n{}\n",
                i + 1,
                r.score,
                file_path,
                r.symbol_name,
                r.kind,
                r.start_line,
                r.end_line,
                r.snippet,
            ));
        }
    }
    out
}

/// Enrich chunk content with file-path components for BM25 path-matching.
///
/// SACL (EMNLP 2025) shows that augmenting code with structural information
/// improves retrieval by 7-12.8%. We append the file stem twice (for boost)
/// and the immediate parent directory once, enabling queries like "auth handler"
/// to match `src/auth/handler.rs`.
pub(crate) fn enrich_for_bm25(chunk: &CodeChunk) -> String {
    let path = std::path::Path::new(&chunk.file_path);
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let dir = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|d| d.to_str())
        .unwrap_or("");

    if stem.is_empty() {
        return chunk.content.clone();
    }

    format!("{} {} {} {}", chunk.content, stem, stem, dir)
}

// ---------------------------------------------------------------------------
// Build coordinator (dashboard)
// ---------------------------------------------------------------------------

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::core::index_pipeline::pipeline::IndexPipeline;

/// Build progress, serialised to the dashboard as the `202` body.
#[derive(serde::Serialize)]
pub struct SearchIndexBuildProgress {
    pub status: &'static str,
    pub files_total: usize,
    pub files_done: usize,
}

/// One in-flight build at a time (single flight). The dashboard serves a single
/// project, matching the call-graph/graph-index coordinators.
static BUILDING: OnceLock<AtomicBool> = OnceLock::new();

fn building_flag() -> &'static AtomicBool {
    BUILDING.get_or_init(|| AtomicBool::new(false))
}

/// Returns `Ok(())` if a fresh index is available, or starts a single background
/// build and returns `Err(progress)` so the caller can answer `202 Accepted`.
pub fn get_or_start_build(root: &std::path::Path) -> Result<(), SearchIndexBuildProgress> {
    // Check if the DB exists and has chunks
    let db_path = crate::core::index_namespace::vectors_dir(root).join("code_index.db");
    if db_path.exists()
        && let Ok(conn) = rusqlite::Connection::open(&db_path)
        && let Ok(count) = conn.query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get::<_, i64>(0))
        && count > 0
    {
        // DB has chunks — ready to serve FTS5 queries
        return Ok(());
    }

    // `swap(true)` wins exactly once; concurrent callers see `true` and just
    // report progress instead of fanning a second build (single flight).
    if building_flag().swap(true, Ordering::SeqCst) {
        return Err(SearchIndexBuildProgress {
            status: "building",
            files_total: 0,
            files_done: 0,
        });
    }

    let bg_root = root.to_path_buf();
    std::thread::spawn(move || {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if let Ok(pipeline) = IndexPipeline::new(bg_root).build() {
                let _ = pipeline.run();
            }
        }));
        building_flag().store(false, Ordering::SeqCst);
    });

    Err(SearchIndexBuildProgress {
        status: "building",
        files_total: 0,
        files_done: 0,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_splits_code() {
        let tokens = tokenize("fn calculate_total(items: Vec<Item>) -> f64");
        assert!(tokens.contains(&"calculate_total".to_string()));
        assert!(tokens.contains(&"items".to_string()));
        assert!(tokens.contains(&"Vec".to_string()));
    }

    #[test]
    fn format_search_results_normalizes_windows_separators() {
        let r = SearchResult {
            chunk_idx: 0,
            score: 1.0,
            file_path: r"C:\Users\zir\AppData\Local\Temp\win-build-log.txt".to_string(),
            symbol_name: "main".to_string(),
            kind: ChunkKind::Function,
            start_line: 1,
            end_line: 2,
            snippet: "x".to_string(),
        };
        let compact = format_search_results(std::slice::from_ref(&r), true);
        assert!(compact.contains("C:/Users/zir/AppData/Local/Temp/win-build-log.txt"));
        assert!(!compact.contains('\\'));

        let verbose = format_search_results(std::slice::from_ref(&r), false);
        assert!(verbose.contains("C:/Users/zir/AppData/Local/Temp/win-build-log.txt"));
        assert!(!verbose.contains('\\'));
    }

    #[test]
    fn format_search_results_leaves_external_uris_untouched() {
        let r = SearchResult {
            chunk_idx: 0,
            score: 1.0,
            file_path: "github://owner/repo/issues/42".to_string(),
            symbol_name: "issue".to_string(),
            kind: ChunkKind::Module,
            start_line: 0,
            end_line: 0,
            snippet: "y".to_string(),
        };
        let out = format_search_results(std::slice::from_ref(&r), true);
        assert!(out.contains("github://owner/repo/issues/42"));
    }

    #[test]
    fn camel_case_splitting() {
        let tokens = split_camel_case_tokens(&["calculateTotal".to_string()]);
        assert!(tokens.contains(&"calculateTotal".to_string()));
        assert!(tokens.contains(&"calculate".to_string()));
        assert!(tokens.contains(&"Total".to_string()));
    }

    #[test]
    fn detect_rust_function() {
        let (name, kind) =
            detect_symbol("pub fn process_request(req: Request) -> Response {").unwrap();
        assert_eq!(name, "process_request");
        assert_eq!(kind, ChunkKind::Function);
    }
}
