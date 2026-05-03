use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeChunk {
    pub file_path: String,
    pub symbol_name: String,
    pub kind: ChunkKind,
    pub start_line: usize,
    pub end_line: usize,
    pub content: String,
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
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct IndexedFileState {
    pub mtime_ms: u64,
    pub size_bytes: u64,
}

impl IndexedFileState {
    fn from_path(path: &Path) -> Option<Self> {
        let meta = path.metadata().ok()?;
        let size_bytes = meta.len();
        let mtime_ms = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64)?;
        Some(Self {
            mtime_ms,
            size_bytes,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BM25Index {
    pub chunks: Vec<CodeChunk>,
    pub inverted: HashMap<String, Vec<(usize, f64)>>,
    pub avg_doc_len: f64,
    pub doc_count: usize,
    pub doc_freqs: HashMap<String, usize>,
    #[serde(default)]
    pub files: HashMap<String, IndexedFileState>,
}

#[derive(Debug, Clone)]
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

const BM25_K1: f64 = 1.2;
const BM25_B: f64 = 0.75;

impl Default for BM25Index {
    fn default() -> Self {
        Self::new()
    }
}

impl BM25Index {
    pub fn new() -> Self {
        Self {
            chunks: Vec::new(),
            inverted: HashMap::new(),
            avg_doc_len: 0.0,
            doc_count: 0,
            doc_freqs: HashMap::new(),
            files: HashMap::new(),
        }
    }

    pub fn build_from_directory(root: &Path) -> Self {
        let mut index = Self::new();
        let files = list_code_files(root);
        for rel in files {
            let abs = root.join(&rel);
            let Some(state) = IndexedFileState::from_path(&abs) else {
                continue;
            };
            if let Ok(content) = std::fs::read_to_string(&abs) {
                let mut chunks = extract_chunks(&rel, &content);
                chunks.sort_by(|a, b| {
                    a.start_line
                        .cmp(&b.start_line)
                        .then_with(|| a.end_line.cmp(&b.end_line))
                        .then_with(|| a.symbol_name.cmp(&b.symbol_name))
                });
                for chunk in chunks {
                    index.add_chunk(chunk);
                }
                index.files.insert(rel, state);
            }
        }

        index.finalize();
        index
    }

    pub fn rebuild_incremental(root: &Path, prev: &BM25Index) -> Self {
        let mut old_by_file: HashMap<String, Vec<CodeChunk>> = HashMap::new();
        for c in &prev.chunks {
            old_by_file
                .entry(c.file_path.clone())
                .or_default()
                .push(c.clone());
        }
        for v in old_by_file.values_mut() {
            v.sort_by(|a, b| {
                a.start_line
                    .cmp(&b.start_line)
                    .then_with(|| a.end_line.cmp(&b.end_line))
                    .then_with(|| a.symbol_name.cmp(&b.symbol_name))
            });
        }

        let mut index = Self::new();
        let files = list_code_files(root);
        for rel in files {
            let abs = root.join(&rel);
            let Some(state) = IndexedFileState::from_path(&abs) else {
                continue;
            };

            let unchanged = prev.files.get(&rel).is_some_and(|old| *old == state);
            if unchanged {
                if let Some(chunks) = old_by_file.get(&rel) {
                    for chunk in chunks {
                        index.add_chunk(chunk.clone());
                    }
                    index.files.insert(rel, state);
                    continue;
                }
            }

            if let Ok(content) = std::fs::read_to_string(&abs) {
                let mut chunks = extract_chunks(&rel, &content);
                chunks.sort_by(|a, b| {
                    a.start_line
                        .cmp(&b.start_line)
                        .then_with(|| a.end_line.cmp(&b.end_line))
                        .then_with(|| a.symbol_name.cmp(&b.symbol_name))
                });
                for chunk in chunks {
                    index.add_chunk(chunk);
                }
                index.files.insert(rel, state);
            }
        }

        index.finalize();
        index
    }

    fn add_chunk(&mut self, chunk: CodeChunk) {
        let idx = self.chunks.len();

        for token in &chunk.tokens {
            let lower = token.to_lowercase();
            self.inverted.entry(lower).or_default().push((idx, 1.0));
        }

        self.chunks.push(chunk);
    }

    fn finalize(&mut self) {
        self.doc_count = self.chunks.len();
        if self.doc_count == 0 {
            return;
        }

        let total_len: usize = self.chunks.iter().map(|c| c.token_count).sum();
        self.avg_doc_len = total_len as f64 / self.doc_count as f64;

        self.doc_freqs.clear();
        for (term, postings) in &self.inverted {
            let unique_docs: std::collections::HashSet<usize> =
                postings.iter().map(|(idx, _)| *idx).collect();
            self.doc_freqs.insert(term.clone(), unique_docs.len());
        }
    }

    pub fn search(&self, query: &str, top_k: usize) -> Vec<SearchResult> {
        let query_tokens = tokenize(query);
        if query_tokens.is_empty() || self.doc_count == 0 {
            return Vec::new();
        }

        let mut scores: HashMap<usize, f64> = HashMap::new();

        for token in &query_tokens {
            let lower = token.to_lowercase();
            let df = *self.doc_freqs.get(&lower).unwrap_or(&0) as f64;
            if df == 0.0 {
                continue;
            }

            let idf = ((self.doc_count as f64 - df + 0.5) / (df + 0.5) + 1.0).ln();

            if let Some(postings) = self.inverted.get(&lower) {
                let mut doc_tfs: HashMap<usize, f64> = HashMap::new();
                for (idx, weight) in postings {
                    *doc_tfs.entry(*idx).or_insert(0.0) += weight;
                }

                for (doc_idx, tf) in &doc_tfs {
                    let doc_len = self.chunks[*doc_idx].token_count as f64;
                    let norm_len = doc_len / self.avg_doc_len.max(1.0);
                    let bm25 = idf * (tf * (BM25_K1 + 1.0))
                        / (tf + BM25_K1 * (1.0 - BM25_B + BM25_B * norm_len));

                    *scores.entry(*doc_idx).or_insert(0.0) += bm25;
                }
            }
        }

        let mut results: Vec<SearchResult> = scores
            .into_iter()
            .map(|(idx, score)| {
                let chunk = &self.chunks[idx];
                let snippet = chunk.content.lines().take(5).collect::<Vec<_>>().join("\n");
                SearchResult {
                    chunk_idx: idx,
                    score,
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
        });
        results.truncate(top_k);
        results
    }

    pub fn save(&self, root: &Path) -> std::io::Result<()> {
        let dir = index_dir(root);
        std::fs::create_dir_all(&dir)?;
        let data = serde_json::to_string(self).map_err(std::io::Error::other)?;
        let target = dir.join("bm25_index.json");
        let tmp = dir.join("bm25_index.json.tmp");
        std::fs::write(&tmp, data)?;
        std::fs::rename(&tmp, &target)?;
        Ok(())
    }

    pub fn load(root: &Path) -> Option<Self> {
        let path = index_dir(root).join("bm25_index.json");
        let data = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&data).ok()
    }

    pub fn load_or_build(root: &Path) -> Self {
        if let Some(idx) = Self::load(root) {
            if !vector_index_looks_stale(&idx, root) {
                return idx;
            }
            tracing::warn!(
                "[vector_index: stale index detected for {}; rebuilding]",
                root.display()
            );
            let rebuilt = if idx.files.is_empty() {
                Self::build_from_directory(root)
            } else {
                Self::rebuild_incremental(root, &idx)
            };
            let _ = rebuilt.save(root);
            return rebuilt;
        }

        let built = Self::build_from_directory(root);
        let _ = built.save(root);
        built
    }

    pub fn index_file_path(root: &Path) -> PathBuf {
        index_dir(root).join("bm25_index.json")
    }
}

fn vector_index_looks_stale(index: &BM25Index, root: &Path) -> bool {
    if index.chunks.is_empty() {
        return false;
    }

    if index.files.is_empty() {
        // Legacy index (pre file-state tracking): only detect missing files.
        let mut seen = std::collections::HashSet::<&str>::new();
        for chunk in &index.chunks {
            let rel = chunk.file_path.trim_start_matches(['/', '\\']);
            if rel.is_empty() {
                continue;
            }
            if !seen.insert(rel) {
                continue;
            }
            if !root.join(rel).exists() {
                return true;
            }
        }
        return false;
    }

    // Missing or modified tracked files.
    for (rel, old_state) in &index.files {
        let abs = root.join(rel);
        if !abs.exists() {
            return true;
        }
        let Some(cur) = IndexedFileState::from_path(&abs) else {
            return true;
        };
        if &cur != old_state {
            return true;
        }
    }

    // New files (present on disk but not in index).
    for rel in list_code_files(root) {
        if !index.files.contains_key(&rel) {
            return true;
        }
    }

    false
}

fn index_dir(root: &Path) -> PathBuf {
    crate::core::index_namespace::vectors_dir(root)
}

fn list_code_files(root: &Path) -> Vec<String> {
    let walker = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .build();

    let mut files: Vec<String> = Vec::new();
    for entry in walker.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if !is_code_file(path) {
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();
        if rel.is_empty() {
            continue;
        }
        files.push(rel);
    }

    files.sort();
    files.dedup();
    files
}

pub fn is_code_file(path: &Path) -> bool {
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

fn tokenize(text: &str) -> Vec<String> {
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

fn split_camel_case_tokens(tokens: &[String]) -> Vec<String> {
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

fn extract_chunks(file_path: &str, content: &str) -> Vec<CodeChunk> {
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
            let tokens = tokenize(&block);
            let token_count = tokens.len();

            chunks.push(CodeChunk {
                file_path: file_path.to_string(),
                symbol_name: name,
                kind,
                start_line: start + 1,
                end_line: end + 1,
                content: block,
                tokens,
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
                let tokens = tokenize(&chunk_text);
                let token_count = tokens.len();
                let start_line = 1 + bytecount::count(&bytes[..c.offset], b'\n');
                let end_line = start_line + bytecount::count(slice, b'\n');
                chunks.push(CodeChunk {
                    file_path: file_path.to_string(),
                    symbol_name: format!("{file_path}#chunk-{idx}"),
                    kind: ChunkKind::Module,
                    start_line,
                    end_line: end_line.max(start_line),
                    content: chunk_text,
                    tokens,
                    token_count,
                });
            }
        } else {
            let tokens = tokenize(content);
            let token_count = tokens.len();
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
                tokens,
                token_count,
            });
        }
    }

    chunks
}

fn detect_symbol(line: &str) -> Option<(String, ChunkKind)> {
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
        ("class ", ChunkKind::Class),
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

fn find_block_end(lines: &[&str], start: usize) -> usize {
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

pub fn format_search_results(results: &[SearchResult], compact: bool) -> String {
    if results.is_empty() {
        return "No results found.".to_string();
    }

    let mut out = String::new();
    for (i, r) in results.iter().enumerate() {
        if compact {
            out.push_str(&format!(
                "{}. {:.2} {}:{}-{} {:?} {}\n",
                i + 1,
                r.score,
                r.file_path,
                r.start_line,
                r.end_line,
                r.kind,
                r.symbol_name,
            ));
        } else {
            out.push_str(&format!(
                "\n--- Result {} (score: {:.2}) ---\n{} :: {} [{:?}] (L{}-{})\n{}\n",
                i + 1,
                r.score,
                r.file_path,
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn tokenize_splits_code() {
        let tokens = tokenize("fn calculate_total(items: Vec<Item>) -> f64");
        assert!(tokens.contains(&"calculate_total".to_string()));
        assert!(tokens.contains(&"items".to_string()));
        assert!(tokens.contains(&"Vec".to_string()));
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

    #[test]
    fn bm25_search_finds_relevant() {
        let mut index = BM25Index::new();
        index.add_chunk(CodeChunk {
            file_path: "auth.rs".into(),
            symbol_name: "validate_token".into(),
            kind: ChunkKind::Function,
            start_line: 1,
            end_line: 10,
            content: "fn validate_token(token: &str) -> bool { check_jwt_expiry(token) }".into(),
            tokens: tokenize("fn validate_token token str bool check_jwt_expiry token"),
            token_count: 8,
        });
        index.add_chunk(CodeChunk {
            file_path: "db.rs".into(),
            symbol_name: "connect_database".into(),
            kind: ChunkKind::Function,
            start_line: 1,
            end_line: 5,
            content: "fn connect_database(url: &str) -> Pool { create_pool(url) }".into(),
            tokens: tokenize("fn connect_database url str Pool create_pool url"),
            token_count: 7,
        });
        index.finalize();

        let results = index.search("jwt token validation", 5);
        assert!(!results.is_empty());
        assert_eq!(results[0].symbol_name, "validate_token");
    }

    #[test]
    fn vector_index_is_stale_when_any_indexed_file_is_missing() {
        let td = tempdir().expect("tempdir");
        let root = td.path();
        std::fs::write(root.join("a.rs"), "pub fn a() {}\n").expect("write a.rs");

        let idx = BM25Index::build_from_directory(root);
        assert!(!vector_index_looks_stale(&idx, root));

        std::fs::remove_file(root.join("a.rs")).expect("remove a.rs");
        assert!(vector_index_looks_stale(&idx, root));
    }

    #[test]
    #[cfg(unix)]
    fn bm25_incremental_rebuild_reuses_unchanged_files_without_reading() {
        let td = tempdir().expect("tempdir");
        let root = td.path();

        std::fs::write(root.join("a.rs"), "pub fn a() { println!(\"A\"); }\n").expect("write a.rs");
        std::fs::write(root.join("b.rs"), "pub fn b() { println!(\"B\"); }\n").expect("write b.rs");

        let idx1 = BM25Index::build_from_directory(root);
        assert!(idx1.files.contains_key("a.rs"));
        assert!(idx1.files.contains_key("b.rs"));

        // Make a.rs unreadable. Incremental rebuild must keep it indexed by reusing prior chunks.
        let a_path = root.join("a.rs");
        let mut perms = std::fs::metadata(&a_path).expect("meta a.rs").permissions();
        perms.set_mode(0o000);
        std::fs::set_permissions(&a_path, perms).expect("chmod a.rs");

        // Change b.rs (size changes) to force a re-read for that file.
        std::fs::write(root.join("b.rs"), "pub fn b() { println!(\"B2\"); }\n")
            .expect("rewrite b.rs");

        let idx2 = BM25Index::rebuild_incremental(root, &idx1);
        assert!(
            idx2.files.contains_key("a.rs"),
            "a.rs should be kept via reuse"
        );
        assert!(idx2.files.contains_key("b.rs"));

        let b_has_b2 = idx2
            .chunks
            .iter()
            .any(|c| c.file_path == "b.rs" && c.content.contains("B2"));
        assert!(b_has_b2, "b.rs should be re-read and re-chunked");

        // Restore permissions to avoid cleanup surprises.
        let mut perms = std::fs::metadata(&a_path).expect("meta a.rs").permissions();
        perms.set_mode(0o644);
        let _ = std::fs::set_permissions(&a_path, perms);
    }
}
