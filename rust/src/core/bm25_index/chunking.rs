//! Tokenization, code-chunk extraction and search-result formatting.
//! Split out of `bm25_index/mod.rs`; `use super::*` re-imports parent items.

#[allow(clippy::wildcard_imports)]
use super::*;
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

/// Whether `content` is a bundled/minified payload rather than human-authored
/// source — a whole program packed onto a handful of enormous lines.
///
/// These are the chunker's pathological input, not merely wasted index space.
/// Tree-sitter emits one chunk per nested scope and each chunk carries its full
/// subtree text, so a bundle with thousands of nested closures gets tokenized
/// once per enclosing scope. #1739 measured 15 MB of committed vendor bundles
/// producing a >10 GB in-memory index, while the surrounding 150 MB of real
/// source produced 1.4 MB.
///
/// Glob lists cannot carry this alone: the bundles in #1739 were named
/// `swagger-ui-bundle.js` and `redoc.standalone.js`, which no `*.min.js`
/// pattern matches. The shape of the content is the reliable signal, so all
/// three conditions must hold together — big enough to matter, dominated by
/// huge lines, and huge on average — which prose and generated-but-readable
/// code do not satisfy.
pub(crate) fn looks_minified(content: &str) -> bool {
    /// Below this a pathological file cannot move the index materially.
    const MIN_BYTES: usize = 64 * 1024;
    /// No hand-written line comes close; minified bundles run to megabytes.
    const MIN_LONGEST_LINE_BYTES: usize = 5_000;
    /// Real source averages well under this, unwrapped prose included.
    const MIN_AVERAGE_LINE_BYTES: usize = 500;

    if content.len() < MIN_BYTES {
        return false;
    }
    let mut lines = 0_usize;
    let mut longest = 0_usize;
    for line in content.lines() {
        lines += 1;
        longest = longest.max(line.len());
    }
    if longest < MIN_LONGEST_LINE_BYTES {
        return false;
    }
    content.len() / lines.max(1) >= MIN_AVERAGE_LINE_BYTES
}

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
    let path = Path::new(&chunk.file_path);
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

#[cfg(test)]
mod chunking_shape_tests {
    use super::looks_minified;

    #[test]
    fn minified_bundles_are_recognised_without_a_glob() {
        // #1739: the offending files were `swagger-ui-bundle.js` and
        // `redoc.standalone.js` — no `*.min.js` pattern matches either.
        let bundle = format!("!function(){{{}}}();", "var aB=1;".repeat(20_000));
        assert!(bundle.len() > 64 * 1024);
        assert!(looks_minified(&bundle));
    }

    #[test]
    fn ordinary_source_and_prose_are_not_minified() {
        // Real source: big file, ordinary lines.
        let source = "pub fn compute(value: i32) -> i32 {\n    value * 2\n}\n".repeat(3_000);
        assert!(source.len() > 64 * 1024);
        assert!(!looks_minified(&source));

        // Unwrapped prose: long lines, but nowhere near a minified bundle.
        let prose = format!("{}\n", "lorem ipsum dolor sit amet ".repeat(60)).repeat(60);
        assert!(prose.len() > 64 * 1024);
        assert!(!looks_minified(&prose));

        // A small file is never worth skipping, however it is shaped.
        let tiny = format!("var a={};", "0".repeat(6_000));
        assert!(!looks_minified(&tiny));

        // One long line inside an otherwise normal large file (an embedded
        // data URI, say) must not disqualify the file.
        let mostly_normal = format!(
            "{}\nconst DATA = \"{}\";\n",
            "let x = 1;\n".repeat(20_000),
            "A".repeat(9_000)
        );
        assert!(!looks_minified(&mostly_normal));
    }
}
