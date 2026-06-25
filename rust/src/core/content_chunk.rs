//! Universal content chunk — the atomic unit of the Context Engine.
//!
//! Extends the existing `CodeChunk` (BM25) with a source dimension so that
//! external data (GitHub issues, Jira tickets, DB schemas, wiki pages) flows
//! through the same pipeline as code: BM25, embeddings, graph, knowledge.
//!
//! Design principles:
//!   - Backward-compatible: `From<ContentChunk> for CodeChunk` preserves the
//!     existing BM25 pipeline without changes.
//!   - Source-aware: `ContentSource` tags where data came from.
//!   - Reference-carrying: `references` links chunks to code files for
//!     cross-source graph edges.
//!
//! Scientific basis: Neocortical column architecture (Mountcastle) — every
//! data source is a "column" processing different input through the same
//! computational template.

use serde::{Deserialize, Serialize};

use super::chunk_data::{ChunkKind, CodeChunk};

/// Where a content chunk originated.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentSource {
    /// Local filesystem (default, backward-compatible with `CodeChunk`).
    #[default]
    File,
    /// External data provider (GitHub, Jira, Confluence, etc.).
    Provider {
        provider_id: String,
        resource_type: String,
    },
    /// Shell command output.
    Shell { command: String },
    /// Knowledge system fact.
    Knowledge { category: String },
}

/// A universal content chunk that can represent code, issues, DB schemas,
/// wiki pages, or any other data source.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentChunk {
    pub file_path: String,
    pub symbol_name: String,
    pub kind: ChunkKind,
    pub start_line: usize,
    pub end_line: usize,
    pub content: String,
    #[serde(default)]
    pub tokens: Vec<String>,
    pub token_count: usize,

    #[serde(default)]
    pub source: ContentSource,

    /// URIs or file paths that this chunk references (for cross-source graph edges).
    #[serde(default)]
    pub references: Vec<String>,

    /// Provider-specific structured metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

impl ContentChunk {
    #[must_use]
    pub fn from_provider(
        provider_id: &str,
        resource_type: &str,
        item_id: &str,
        title: &str,
        kind: ChunkKind,
        content: String,
        references: Vec<String>,
        metadata: Option<serde_json::Value>,
    ) -> Self {
        let tokens = super::chunk_data::tokenize_for_index(&content);
        let token_count = tokens.len();
        Self {
            file_path: format!("{provider_id}://{resource_type}/{item_id}"),
            symbol_name: title.to_string(),
            kind,
            start_line: 0,
            end_line: 0,
            content,
            tokens,
            token_count,
            source: ContentSource::Provider {
                provider_id: provider_id.to_string(),
                resource_type: resource_type.to_string(),
            },
            references,
            metadata,
        }
    }

    #[must_use]
    pub fn is_external(&self) -> bool {
        !matches!(self.source, ContentSource::File)
    }

    #[must_use]
    pub fn provider_id(&self) -> Option<&str> {
        match &self.source {
            ContentSource::Provider { provider_id, .. } => Some(provider_id),
            _ => None,
        }
    }
}

impl From<ContentChunk> for CodeChunk {
    fn from(c: ContentChunk) -> Self {
        Self {
            file_path: c.file_path,
            symbol_name: c.symbol_name,
            kind: c.kind,
            start_line: c.start_line,
            end_line: c.end_line,
            content: c.content,
            tokens: c.tokens,
            token_count: c.token_count,
        }
    }
}

impl From<CodeChunk> for ContentChunk {
    fn from(c: CodeChunk) -> Self {
        Self {
            file_path: c.file_path,
            symbol_name: c.symbol_name,
            kind: c.kind,
            start_line: c.start_line,
            end_line: c.end_line,
            content: c.content,
            tokens: c.tokens,
            token_count: c.token_count,
            source: ContentSource::File,
            references: Vec::new(),
            metadata: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Chunk extraction helpers for external data
// ---------------------------------------------------------------------------

/// Extract file path references from freeform text (issue bodies, PR descriptions).
/// Looks for patterns like `src/auth.rs`, `lib/handler.ts`, `path/to/file.ext`.
pub fn extract_file_references(text: &str) -> Vec<String> {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?:^|[\s`\(\[])([a-zA-Z0-9_\-./]+\.[a-zA-Z]{1,10})(?:[\s`\)\],:;.]|$)")
            .expect("file ref regex")
    });

    let mut refs: Vec<String> = RE
        .captures_iter(text)
        .filter_map(|cap| {
            let path = cap.get(1)?.as_str();
            if path.contains('/')
                && !path.starts_with("http")
                && !path.starts_with("www.")
                && !path.contains('@')
            {
                Some(path.to_string())
            } else {
                None
            }
        })
        .collect();
    refs.sort();
    refs.dedup();
    refs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_chunk_to_code_chunk_roundtrip() {
        let cc = ContentChunk::from_provider(
            "github",
            "issues",
            "123",
            "Auth token expiry",
            ChunkKind::Other,
            "Token expires after 1h".into(),
            vec!["src/auth.rs".into()],
            None,
        );

        assert!(cc.is_external());
        assert_eq!(cc.provider_id(), Some("github"));
        assert_eq!(cc.file_path, "github://issues/123");

        let code_chunk: CodeChunk = cc.into();
        assert_eq!(code_chunk.file_path, "github://issues/123");
        assert_eq!(code_chunk.symbol_name, "Auth token expiry");
    }

    #[test]
    fn code_chunk_to_content_chunk() {
        let code = CodeChunk {
            file_path: "src/main.rs".into(),
            symbol_name: "main".into(),
            kind: ChunkKind::Function,
            start_line: 1,
            end_line: 10,
            content: "fn main() {}".into(),
            tokens: vec!["main".into()],
            token_count: 1,
        };

        let cc: ContentChunk = code.into();
        assert!(!cc.is_external());
        assert_eq!(cc.source, ContentSource::File);
        assert!(cc.references.is_empty());
    }

    #[test]
    fn extract_file_refs_from_issue_body() {
        let body = "The bug is in src/auth/handler.rs and affects lib/db.ts.\n\
                     See also tests/auth_test.rs for the failing test.";
        let refs = extract_file_references(body);
        assert!(refs.contains(&"src/auth/handler.rs".to_string()));
        assert!(refs.contains(&"lib/db.ts".to_string()));
        assert!(refs.contains(&"tests/auth_test.rs".to_string()));
    }

    #[test]
    fn extract_file_refs_ignores_urls() {
        let body = "See https://github.com/foo/bar.git and www.example.com/page.html";
        let refs = extract_file_references(body);
        assert!(refs.is_empty() || !refs.iter().any(|r| r.contains("http")));
    }

    #[test]
    fn extract_file_refs_deduplicates() {
        let body = "Changed src/auth.rs and also src/auth.rs again";
        let refs = extract_file_references(body);
        assert_eq!(refs.iter().filter(|r| *r == "src/auth.rs").count(), 1);
    }

    #[test]
    fn default_source_is_file() {
        assert_eq!(ContentSource::default(), ContentSource::File);
    }

    #[test]
    fn provider_source_serializes_with_tag() {
        let src = ContentSource::Provider {
            provider_id: "jira".into(),
            resource_type: "issues".into(),
        };
        let json = serde_json::to_string(&src).unwrap();
        assert!(json.contains("\"type\":\"provider\""));
        assert!(json.contains("\"provider_id\":\"jira\""));
    }
}
