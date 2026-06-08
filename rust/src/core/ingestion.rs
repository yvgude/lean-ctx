//! Ingestion front-door (`ingestion-spec-v1`).
//!
//! Decides what reaches the index — BM25, semantic, knowledge — **independent of
//! code-ness**. Historically only files passing `is_code_file` were indexed,
//! which locked lean-ctx to source repositories. Intake is now driven by a
//! content-*kind* classification (extension fast-path + a bounded binary sniff),
//! so any text corpus (docs, data, transcripts, logs) is ingestible while
//! genuine binaries are excluded. Code repositories behave exactly as before:
//! every kind that used to index still indexes.

use std::path::Path;

/// What kind of content a path holds, for intake decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestKind {
    /// Source code — eligible for AST/symbol-aware processing downstream.
    Code,
    /// Prose / human documents (markdown, txt, html, email, …).
    Document,
    /// Structured data (json, yaml, toml, csv, xml, …).
    Data,
    /// Other UTF-8 text (unknown extension but verified textual).
    Text,
    /// Not ingestible as text (images, media, archives, binary documents).
    Binary,
}

impl IngestKind {
    /// Whether content of this kind should be fed to the index.
    #[must_use]
    pub fn is_ingestible(self) -> bool {
        !matches!(self, IngestKind::Binary)
    }

    /// Stable lowercase label (for capabilities / diagnostics).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            IngestKind::Code => "code",
            IngestKind::Document => "document",
            IngestKind::Data => "data",
            IngestKind::Text => "text",
            IngestKind::Binary => "binary",
        }
    }
}

/// Prose / document extensions.
const DOCUMENT_EXTS: &[&str] = &[
    "md", "markdown", "mdx", "txt", "text", "rst", "org", "adoc", "asciidoc", "tex", "html", "htm",
    "xhtml", "eml", "mbox", "log", "srt", "vtt",
];

/// Structured-data extensions.
const DATA_EXTS: &[&str] = &[
    "json",
    "jsonl",
    "ndjson",
    "yaml",
    "yml",
    "toml",
    "csv",
    "tsv",
    "xml",
    "ini",
    "cfg",
    "conf",
    "properties",
    "graphql",
    "proto",
];

/// Extensions we know are binary or not useful as raw text. Binary documents
/// that *do* have a dedicated extractor (currently PDF) live in
/// [`EXTRACTABLE_DOC_EXTS`] instead and are ingestible; office formats without
/// an extractor yet (DOCX, XLSX, …) stay here and are skipped.
const BINARY_EXTS: &[&str] = &[
    // images
    "png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "tif", "tiff", "heic", "avif", //
    // media
    "mp3", "wav", "flac", "ogg", "mp4", "mov", "avi", "mkv", "webm", //
    // archives / packages
    "zip", "gz", "tgz", "bz2", "xz", "zst", "7z", "rar", "tar", "jar", "war", //
    // binary docs without an extractor yet (PDF lives in EXTRACTABLE_DOC_EXTS)
    "doc", "docx", "xls", "xlsx", "ppt", "pptx", "odt", //
    // compiled / binary artifacts
    "exe", "dll", "so", "dylib", "o", "a", "class", "wasm", "bin", "dat", //
    // fonts / db / images-vector-binary
    "ttf", "otf", "woff", "woff2", "db", "sqlite", "lock",
];

/// Binary document formats that have a dedicated byte-level extractor
/// ([`super::extractors`]) and are therefore ingestible: the indexer reads their
/// raw bytes and converts them to text at index time rather than skipping them
/// as opaque binaries. Grows as binary extractors (DOCX, XLSX, …) are added.
const EXTRACTABLE_DOC_EXTS: &[&str] = &["pdf"];

/// Max bytes inspected when sniffing an unknown-extension file.
const SNIFF_BYTES: usize = 8192;

/// Classify a path into an [`IngestKind`].
///
/// Fast path is extension-based; files with an unknown extension are sniffed
/// (bounded read) so textual content is still picked up and binaries rejected.
#[must_use]
pub fn classify_path(path: &Path) -> IngestKind {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    if BINARY_EXTS.contains(&ext.as_str()) {
        return IngestKind::Binary;
    }
    // Binary documents with a dedicated extractor (PDF, …): ingestible as
    // documents — the indexer routes their bytes through `extractors::extract`
    // instead of reading them as UTF-8.
    if EXTRACTABLE_DOC_EXTS.contains(&ext.as_str()) {
        return IngestKind::Document;
    }
    if crate::core::bm25_index::is_code_file(path) {
        return IngestKind::Code;
    }
    if DOCUMENT_EXTS.contains(&ext.as_str()) {
        return IngestKind::Document;
    }
    if DATA_EXTS.contains(&ext.as_str()) {
        return IngestKind::Data;
    }
    // Unknown extension (or none): verify it is actually text before ingesting.
    if looks_textual(path) {
        IngestKind::Text
    } else {
        IngestKind::Binary
    }
}

/// Whether a path should be fed to the index. Single front-door replacing the
/// old `is_code_file` gate.
#[must_use]
pub fn is_ingestible(path: &Path) -> bool {
    classify_path(path).is_ingestible()
}

/// Bounded heuristic: read the first [`SNIFF_BYTES`] and decide whether the
/// content is text. A NUL byte or a high ratio of non-text control bytes marks
/// it binary. Unreadable files are treated as binary (skipped).
fn looks_textual(path: &Path) -> bool {
    use std::io::Read;

    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut buf = [0u8; SNIFF_BYTES];
    let n = match file.read(&mut buf) {
        Ok(0) => return true, // empty file: harmless to index
        Ok(n) => n,
        Err(_) => return false,
    };
    let sample = &buf[..n];

    if sample.contains(&0) {
        return false;
    }
    let suspicious = sample
        .iter()
        .filter(|&&b| b < 0x09 || (b > 0x0d && b < 0x20))
        .count();
    // Allow a small fraction of control bytes (some text files carry form-feed
    // etc.) but reject clearly binary content.
    suspicious * 100 / n.max(1) < 10
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;

    fn p(name: &str) -> PathBuf {
        PathBuf::from(name)
    }

    #[test]
    fn code_files_classify_as_code() {
        assert_eq!(classify_path(&p("src/main.rs")), IngestKind::Code);
        assert_eq!(classify_path(&p("app/index.ts")), IngestKind::Code);
        assert!(is_ingestible(&p("src/main.rs")));
    }

    #[test]
    fn documents_and_data_are_ingestible() {
        assert_eq!(classify_path(&p("README.md")), IngestKind::Document);
        assert_eq!(classify_path(&p("notes.txt")), IngestKind::Document);
        assert_eq!(classify_path(&p("page.html")), IngestKind::Document);
        assert_eq!(classify_path(&p("data.csv")), IngestKind::Data);
        assert_eq!(classify_path(&p("config.yaml")), IngestKind::Data);
        for f in ["README.md", "data.csv", "config.yaml", "page.html"] {
            assert!(is_ingestible(&p(f)), "{f} should ingest");
        }
    }

    #[test]
    fn binaries_are_excluded() {
        for f in ["logo.png", "archive.zip", "lib.so", "app.wasm"] {
            assert_eq!(classify_path(&p(f)), IngestKind::Binary, "{f}");
            assert!(!is_ingestible(&p(f)), "{f} must not ingest");
        }
    }

    #[test]
    fn pdf_is_ingestible_via_extractor() {
        // PDF has a dedicated extractor, so it classifies as an ingestible
        // document (read through `extractors::extract`, not as UTF-8).
        assert_eq!(classify_path(&p("report.pdf")), IngestKind::Document);
        assert_eq!(classify_path(&p("REPORT.PDF")), IngestKind::Document);
        assert!(is_ingestible(&p("report.pdf")));
        // Office binaries without an extractor stay excluded.
        for f in ["paper.docx", "sheet.xlsx", "deck.pptx", "doc.odt"] {
            assert_eq!(classify_path(&p(f)), IngestKind::Binary, "{f}");
            assert!(!is_ingestible(&p(f)), "{f} must not ingest");
        }
    }

    #[test]
    fn unknown_extension_is_sniffed() {
        let dir = tempfile::tempdir().unwrap();

        let textual = dir.path().join("mystery.weirdext");
        std::fs::write(&textual, "just normal text content\nwith lines\n").unwrap();
        assert_eq!(classify_path(&textual), IngestKind::Text);
        assert!(is_ingestible(&textual));

        let binary = dir.path().join("blob.weirdext");
        let mut f = std::fs::File::create(&binary).unwrap();
        f.write_all(&[0u8, 1, 2, 3, 0, 255, 254]).unwrap();
        assert_eq!(classify_path(&binary), IngestKind::Binary);
        assert!(!is_ingestible(&binary));
    }

    #[test]
    fn no_extension_textual_file_ingests() {
        let dir = tempfile::tempdir().unwrap();
        let readme = dir.path().join("LICENSE");
        std::fs::write(&readme, "MIT License\n\nPermission is hereby granted\n").unwrap();
        assert_eq!(classify_path(&readme), IngestKind::Text);
    }
}
