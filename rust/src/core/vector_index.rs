use std::collections::HashMap;
use std::path::{Path, PathBuf};

use md5::{Digest, Md5};
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BM25Index {
    pub chunks: Vec<CodeChunk>,
    pub inverted: HashMap<String, Vec<(usize, f64)>>,
    pub avg_doc_len: f64,
    pub doc_count: usize,
    pub doc_freqs: HashMap<String, usize>,
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
        }
    }

    pub fn build_from_directory(root: &Path) -> Self {
        let mut index = Self::new();
        let walker = ignore::WalkBuilder::new(root)
            .hidden(true)
            .git_ignore(true)
            .max_depth(Some(10))
            .build();

        let mut file_count = 0usize;
        for entry in walker.flatten() {
            if file_count >= 2000 {
                break;
            }
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            if !is_code_file(path) {
                continue;
            }
            if let Ok(content) = std::fs::read_to_string(path) {
                let rel = path
                    .strip_prefix(root)
                    .unwrap_or(path)
                    .to_string_lossy()
                    .to_string();
                let chunks = extract_chunks(&rel, &content);
                for chunk in chunks {
                    index.add_chunk(chunk);
                }
                file_count += 1;
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
        std::fs::write(dir.join("bm25_index.json"), data)?;
        Ok(())
    }

    pub fn load(root: &Path) -> Option<Self> {
        let path = index_dir(root).join("bm25_index.json");
        let data = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&data).ok()
    }
}

fn index_dir(root: &Path) -> PathBuf {
    let mut hasher = Md5::new();
    hasher.update(root.to_string_lossy().as_bytes());
    let hash = format!("{:x}", hasher.finalize());
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".lean-ctx")
        .join("vectors")
        .join(hash)
}

fn is_code_file(path: &Path) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    matches!(
        ext,
        "rs" | "ts"
            | "tsx"
            | "js"
            | "jsx"
            | "py"
            | "go"
            | "java"
            | "c"
            | "cpp"
            | "h"
            | "hpp"
            | "rb"
            | "cs"
            | "kt"
            | "swift"
            | "php"
            | "scala"
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
}
