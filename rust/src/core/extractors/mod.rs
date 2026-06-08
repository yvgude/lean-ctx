//! Format extractors & format-aware chunkers (`extractors-v1`, EPIC 12.13).
//!
//! The front-door that turns a non-code document/data file into clean LLM text
//! plus structure-aware chunks. It complements [`super::ingestion`] (which
//! decides *whether* to index) by deciding *how* to read a given format:
//!
//! | Format | Extractor | Chunking |
//! |--------|-----------|----------|
//! | JSON   | [`json`] | per array element / object entry |
//! | CSV/TSV| [`csv`]  | header-prefixed row groups |
//! | EML    | [`eml`]  | header summary + body paragraphs |
//! | HTML   | [`super::web::html_to_text`] | paragraphs of rendered Markdown |
//! | PDF    | [`super::web::pdf`] | paragraphs of extracted text |
//! | text   | (verbatim) | paragraphs |
//!
//! The text-based formats also register as named [`Chunker`]s in the
//! [`extension_registry`](super::extension_registry) so they are discoverable
//! via `/v1/capabilities` and exercised by the conformance suite. Every
//! extractor degrades gracefully — arbitrary input never panics and non-empty
//! input always yields at least one non-empty chunk.

// Chunker::name returns &str; literals here would otherwise trip the lint.
#![allow(clippy::unnecessary_literal_bound)]

pub mod csv;
pub mod eml;
pub mod json;

use std::path::Path;
use std::sync::Arc;

use super::extension_registry::{Chunker, ExtensionRegistry};

/// The result of extracting one document: a stable kind tag, clean text, and
/// structure-aware chunks.
#[derive(Debug, Clone)]
pub struct Extracted {
    pub kind: &'static str,
    pub text: String,
    pub chunks: Vec<String>,
}

/// Extract clean text + chunks from raw `bytes`, dispatching on `path`'s
/// extension. Binary formats (PDF) read from bytes; text formats decode UTF-8
/// lossily so malformed encodings still produce content.
#[must_use]
pub fn extract(path: &Path, bytes: &[u8]) -> Extracted {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    match ext.as_str() {
        "json" | "jsonl" | "ndjson" => {
            let s = String::from_utf8_lossy(bytes);
            Extracted {
                kind: "json",
                text: json::to_text(&s),
                chunks: json::chunks(&s),
            }
        }
        "csv" | "tsv" => {
            let s = String::from_utf8_lossy(bytes);
            let delim = if ext == "tsv" { '\t' } else { ',' };
            Extracted {
                kind: "csv",
                text: csv::to_text(&s, delim),
                chunks: csv::chunks(&s, delim),
            }
        }
        "eml" => {
            let s = String::from_utf8_lossy(bytes);
            Extracted {
                kind: "eml",
                text: eml::to_text(&s),
                chunks: eml::chunks(&s),
            }
        }
        "html" | "htm" | "xhtml" => {
            let s = String::from_utf8_lossy(bytes);
            let doc = super::web::html_to_text::parse(&s);
            Extracted {
                kind: "html",
                chunks: paragraph_chunks(&doc.markdown),
                text: doc.markdown,
            }
        }
        "pdf" => match super::web::pdf::extract_text(bytes) {
            Ok(text) => Extracted {
                kind: "pdf",
                chunks: paragraph_chunks(&text),
                text,
            },
            Err(e) => Extracted {
                kind: "pdf",
                text: String::new(),
                chunks: vec![format!("[pdf extraction failed: {e}]")],
            },
        },
        _ => {
            let s = String::from_utf8_lossy(bytes).to_string();
            Extracted {
                kind: "text",
                chunks: paragraph_chunks(&s),
                text: s,
            }
        }
    }
}

/// Whether `path` is a binary document format that must be read through
/// [`extract`] from raw bytes because it is not valid UTF-8 text. Text and
/// structured formats (json/csv/eml/html/markdown/…) index fine as raw UTF-8;
/// only true binary documents — currently PDF — need byte-level extraction
/// before they can enter the text index. Grows as binary extractors (DOCX,
/// XLSX, …) are added. Single source of truth for the indexer's read path.
#[must_use]
pub fn is_binary_document(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("pdf")
    )
}

/// Split `text` into paragraph chunks on blank-line boundaries, trimming and
/// dropping empties. The shared fallback chunker for prose-like formats.
#[must_use]
pub fn paragraph_chunks(text: &str) -> Vec<String> {
    text.split("\n\n")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

/// Register the text-based format chunkers into `reg`. Called from
/// [`ExtensionRegistry::with_builtins`] so the formats are first-class,
/// discoverable, and conformance-checked.
pub fn register_into(reg: &mut ExtensionRegistry) {
    reg.register_chunker(Arc::new(FormatChunker {
        name: "csv",
        f: |s| csv::chunks(s, ','),
    }));
    reg.register_chunker(Arc::new(FormatChunker {
        name: "json",
        f: json::chunks,
    }));
    reg.register_chunker(Arc::new(FormatChunker {
        name: "eml",
        f: eml::chunks,
    }));
    reg.register_chunker(Arc::new(FormatChunker {
        name: "html",
        f: |s| paragraph_chunks(&super::web::html_to_text::parse(s).markdown),
    }));
}

/// Adapter exposing a format chunk function as a named registry [`Chunker`].
struct FormatChunker {
    name: &'static str,
    f: fn(&str) -> Vec<String>,
}

impl Chunker for FormatChunker {
    fn name(&self) -> &str {
        self.name
    }
    fn chunk(&self, input: &str) -> Vec<String> {
        (self.f)(input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatches_json_by_extension() {
        let e = extract(Path::new("data.json"), br#"[{"a":1}]"#);
        assert_eq!(e.kind, "json");
        assert_eq!(e.chunks.len(), 1);
    }

    #[test]
    fn dispatches_csv_and_tsv() {
        let csv = extract(Path::new("t.csv"), b"a,b\n1,2");
        assert_eq!(csv.kind, "csv");
        assert!(csv.text.contains("a: 1 | b: 2"));
        let tsv = extract(Path::new("t.tsv"), b"a\tb\n1\t2");
        assert!(tsv.text.contains("a: 1 | b: 2"));
    }

    #[test]
    fn dispatches_html_to_markdown() {
        let e = extract(Path::new("p.html"), b"<h1>Title</h1><p>Body</p>");
        assert_eq!(e.kind, "html");
        assert!(e.text.contains("Title"));
    }

    #[test]
    fn unknown_extension_is_text_paragraphs() {
        let e = extract(Path::new("notes.txt"), b"one\n\ntwo");
        assert_eq!(e.kind, "text");
        assert_eq!(e.chunks, vec!["one", "two"]);
    }

    #[test]
    fn binary_document_predicate_matches_pdf_only() {
        assert!(is_binary_document(Path::new("a.pdf")));
        assert!(is_binary_document(Path::new("A.PDF")));
        for f in ["p.html", "d.json", "t.csv", "m.eml", "n.txt", "s.rs"] {
            assert!(!is_binary_document(Path::new(f)), "{f}");
        }
    }

    #[test]
    fn format_chunkers_register_and_run() {
        let mut reg = ExtensionRegistry::new();
        register_into(&mut reg);
        for name in ["csv", "json", "eml", "html"] {
            let c = reg
                .chunker(name)
                .unwrap_or_else(|| panic!("{name} missing"));
            assert!(c.chunk("").is_empty(), "{name} empty input must be empty");
            assert!(
                !c.chunk("hello world").is_empty(),
                "{name} non-empty input must chunk"
            );
        }
    }
}
