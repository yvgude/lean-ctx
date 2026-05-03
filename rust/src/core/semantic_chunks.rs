//! Semantic Chunking with Attention Bridges.
//!
//! Groups content into semantic chunks (function bodies, import blocks, type
//! definitions) rather than treating lines independently. Orders chunks for
//! optimal LLM attention flow:
//!
//! 1. Most relevant chunk FIRST (high-attention position)
//! 2. Its immediate dependencies (imports, types it uses) adjacent
//! 3. Supporting context in the middle
//! 4. Tail anchor: brief reference back to the primary chunk (attention bridge)
//!
//! This exploits how transformer attention actually works:
//! local coherence + global anchors beats scattered high-importance lines.

use std::collections::HashSet;

#[derive(Debug, Clone)]
pub struct SemanticChunk {
    pub lines: Vec<String>,
    pub kind: ChunkKind,
    pub relevance: f64,
    pub start_line: usize,
    pub identifier: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkKind {
    Imports,
    TypeDefinition,
    FunctionDef,
    Logic,
    Empty,
}

/// Detect semantic boundaries in content and group lines into chunks.
pub fn detect_chunks(content: &str) -> Vec<SemanticChunk> {
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        return Vec::new();
    }

    let mut chunks: Vec<SemanticChunk> = Vec::new();
    let mut current_lines: Vec<String> = Vec::new();
    let mut current_kind = ChunkKind::Empty;
    let mut current_start = 0;
    let mut current_ident: Option<String> = None;
    let mut brace_depth: i32 = 0;
    let mut in_block = false;

    for (i, &line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        let line_kind = classify_line(trimmed);

        let opens = trimmed.matches('{').count() as i32;
        let closes = trimmed.matches('}').count() as i32;

        if !in_block && is_block_start(trimmed) {
            if !current_lines.is_empty() {
                chunks.push(SemanticChunk {
                    lines: current_lines.clone(),
                    kind: current_kind,
                    relevance: 0.0,
                    start_line: current_start,
                    identifier: current_ident.take(),
                });
                current_lines.clear();
            }
            current_start = i;
            current_kind = line_kind;
            current_ident = extract_identifier(trimmed);
            in_block = opens > closes;
            brace_depth = opens - closes;
            current_lines.push(line.to_string());
            continue;
        }

        if in_block {
            brace_depth += opens - closes;
            current_lines.push(line.to_string());
            if brace_depth <= 0 {
                in_block = false;
                chunks.push(SemanticChunk {
                    lines: current_lines.clone(),
                    kind: current_kind,
                    relevance: 0.0,
                    start_line: current_start,
                    identifier: current_ident.take(),
                });
                current_lines.clear();
            }
            continue;
        }

        // Boundary detection: blank lines or kind changes
        let is_boundary =
            trimmed.is_empty() || (line_kind != current_kind && !current_lines.is_empty());

        if is_boundary && !current_lines.is_empty() {
            chunks.push(SemanticChunk {
                lines: current_lines.clone(),
                kind: current_kind,
                relevance: 0.0,
                start_line: current_start,
                identifier: current_ident.take(),
            });
            current_lines.clear();
        }

        if !trimmed.is_empty() {
            if current_lines.is_empty() {
                current_start = i;
                current_kind = line_kind;
            }
            current_lines.push(line.to_string());
        }
    }

    if !current_lines.is_empty() {
        chunks.push(SemanticChunk {
            lines: current_lines,
            kind: current_kind,
            relevance: 0.0,
            start_line: current_start,
            identifier: current_ident,
        });
    }

    chunks
}

/// Score chunks by task relevance and reorder for optimal attention flow.
pub fn order_for_attention(
    mut chunks: Vec<SemanticChunk>,
    task_keywords: &[String],
) -> Vec<SemanticChunk> {
    if chunks.is_empty() {
        return chunks;
    }

    let kw_lower: Vec<String> = task_keywords.iter().map(|k| k.to_lowercase()).collect();

    // Score each chunk
    for chunk in &mut chunks {
        let text = chunk.lines.join(" ").to_lowercase();
        let keyword_score: f64 = kw_lower
            .iter()
            .filter(|kw| text.contains(kw.as_str()))
            .count() as f64;

        let kind_weight = match chunk.kind {
            ChunkKind::FunctionDef => 2.0,
            ChunkKind::TypeDefinition => 1.8,
            ChunkKind::Imports => 1.0,
            ChunkKind::Logic => 0.8,
            ChunkKind::Empty => 0.1,
        };

        let size_factor = (chunk.lines.len() as f64 / 5.0).min(1.5);

        chunk.relevance = keyword_score * 2.0 + kind_weight + size_factor * 0.3;
    }

    // Sort by relevance (most relevant first)
    chunks.sort_by(|a, b| {
        b.relevance
            .partial_cmp(&a.relevance)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    if chunks.len() <= 2 {
        return chunks;
    }

    // Reorder: primary chunk first, then its dependencies, then rest
    let primary = &chunks[0];
    let primary_tokens: HashSet<String> = primary
        .lines
        .iter()
        .flat_map(|l| l.split_whitespace().map(str::to_lowercase))
        .collect();

    let (mut deps, mut rest): (Vec<_>, Vec<_>) = chunks[1..].iter().cloned().partition(|chunk| {
        if chunk.kind == ChunkKind::Imports || chunk.kind == ChunkKind::TypeDefinition {
            let chunk_tokens: HashSet<String> = chunk
                .lines
                .iter()
                .flat_map(|l| l.split_whitespace().map(str::to_lowercase))
                .collect();
            let overlap = primary_tokens.intersection(&chunk_tokens).count();
            overlap >= 2
        } else {
            false
        }
    });

    deps.sort_by(|a, b| {
        b.relevance
            .partial_cmp(&a.relevance)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    rest.sort_by(|a, b| {
        b.relevance
            .partial_cmp(&a.relevance)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut ordered = Vec::with_capacity(chunks.len());
    ordered.push(chunks[0].clone());
    ordered.extend(deps);
    ordered.extend(rest);

    ordered
}

/// Render chunks back to text with attention bridges.
pub fn render_with_bridges(chunks: &[SemanticChunk]) -> String {
    if chunks.is_empty() {
        return String::new();
    }

    let mut output = Vec::new();

    for (i, chunk) in chunks.iter().enumerate() {
        if i > 0 {
            output.push(String::new());
        }
        for line in &chunk.lines {
            output.push(line.clone());
        }
    }

    // Tail anchor: reference back to primary chunk
    if chunks.len() > 2 {
        if let Some(ref ident) = chunks[0].identifier {
            output.push(String::new());
            output.push(format!("[primary: {ident}]"));
        }
    }

    output.join("\n")
}

fn classify_line(trimmed: &str) -> ChunkKind {
    if trimmed.is_empty() {
        return ChunkKind::Empty;
    }
    if is_import(trimmed) {
        return ChunkKind::Imports;
    }
    if is_type_def(trimmed) {
        return ChunkKind::TypeDefinition;
    }
    if is_fn_start(trimmed) {
        return ChunkKind::FunctionDef;
    }
    ChunkKind::Logic
}

fn is_block_start(trimmed: &str) -> bool {
    is_fn_start(trimmed) || is_type_def(trimmed)
}

fn is_fn_start(line: &str) -> bool {
    let starters = [
        "fn ",
        "pub fn ",
        "async fn ",
        "pub async fn ",
        "function ",
        "export function ",
        "async function ",
        "def ",
        "async def ",
        "func ",
        "pub(crate) fn ",
        "pub(super) fn ",
    ];
    starters.iter().any(|s| line.starts_with(s))
}

fn is_type_def(line: &str) -> bool {
    let starters = [
        "struct ",
        "pub struct ",
        "enum ",
        "pub enum ",
        "trait ",
        "pub trait ",
        "type ",
        "pub type ",
        "interface ",
        "export interface ",
        "class ",
        "export class ",
    ];
    starters.iter().any(|s| line.starts_with(s))
}

fn is_import(line: &str) -> bool {
    line.starts_with("use ")
        || line.starts_with("import ")
        || line.starts_with("from ")
        || line.starts_with("#include")
}

fn extract_identifier(line: &str) -> Option<String> {
    let cleaned = line
        .replace("pub ", "")
        .replace("async ", "")
        .replace("export ", "");
    let trimmed = cleaned.trim();

    for prefix in &[
        "fn ",
        "struct ",
        "enum ",
        "trait ",
        "type ",
        "class ",
        "interface ",
        "function ",
        "def ",
        "func ",
    ] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if !name.is_empty() {
                return Some(name);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_chunks_basic() {
        let content = "use std::io;\nuse std::fs;\n\nfn main() {\n    let x = 1;\n}\n\nfn helper() {\n    let y = 2;\n}";
        let chunks = detect_chunks(content);
        assert!(
            chunks.len() >= 2,
            "should detect multiple chunks, got {}",
            chunks.len()
        );
    }

    #[test]
    fn detect_chunks_identifies_functions() {
        let content = "fn main() {\n    println!(\"hello\");\n}";
        let chunks = detect_chunks(content);
        assert!(
            chunks.iter().any(|c| c.kind == ChunkKind::FunctionDef),
            "should detect function definition"
        );
    }

    #[test]
    fn order_puts_relevant_first() {
        let content =
            "fn unrelated() {\n    let x = 1;\n}\n\nfn validate_token() {\n    check();\n}";
        let chunks = detect_chunks(content);
        let ordered = order_for_attention(chunks, &["validate".to_string()]);
        assert!(
            ordered[0].identifier.as_deref() == Some("validate_token"),
            "most relevant chunk should be first"
        );
    }

    #[test]
    fn render_with_bridges_adds_anchor() {
        let chunks = vec![
            SemanticChunk {
                lines: vec!["fn main() {".into(), "  let x = 1;".into(), "}".into()],
                kind: ChunkKind::FunctionDef,
                relevance: 5.0,
                start_line: 0,
                identifier: Some("main".into()),
            },
            SemanticChunk {
                lines: vec!["use std::io;".into()],
                kind: ChunkKind::Imports,
                relevance: 1.0,
                start_line: 5,
                identifier: None,
            },
            SemanticChunk {
                lines: vec!["fn helper() {".into(), "}".into()],
                kind: ChunkKind::FunctionDef,
                relevance: 0.5,
                start_line: 8,
                identifier: Some("helper".into()),
            },
        ];
        let result = render_with_bridges(&chunks);
        assert!(
            result.contains("[primary: main]"),
            "should have tail anchor"
        );
    }

    #[test]
    fn extract_identifier_fn() {
        assert_eq!(
            extract_identifier("pub fn validate_token() {"),
            Some("validate_token".into())
        );
        assert_eq!(extract_identifier("struct Config {"), Some("Config".into()));
        assert_eq!(extract_identifier("let x = 1;"), None);
    }
}
