use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::core::bm25_index::external_origin::{ChunkOrigin, DocumentOrigin, MAX_DOCUMENT_BYTES};
use crate::core::bm25_index::{BM25Index, ChunkKind, CodeChunk, IndexedFileState};
use crate::core::context_admission::stores::StoreAdmission;

const MAX_ARTIFACT_BYTES: u64 = MAX_DOCUMENT_BYTES;
const MAX_CHUNKS_PER_FILE: usize = 50;
/// Tool identity a document artifact is read for (path decision, events).
const ARTIFACT_TOOL: &str = "ctx_artifacts";

/// The origin on record for the document `rel`, if `idx` has one.
fn document_origin<'a>(idx: &'a BM25Index, rel: &str) -> Option<&'a DocumentOrigin> {
    match idx.provider_origins.get(rel) {
        Some(ChunkOrigin::Document(origin)) => Some(origin),
        _ => None,
    }
}

pub(crate) fn index_file_path(project_root: &Path) -> PathBuf {
    let code_idx = BM25Index::index_file_path(project_root);
    let dir = code_idx.parent().unwrap_or_else(|| Path::new("."));
    dir.join("bm25_artifacts_index.json")
}

/// The persisted artifact index, handed out for `project_root`: a search over
/// it serves a chunk only while the admitted reader still returns its text
/// (`bm25_index::source_gate`, G04).
pub(crate) fn load(project_root: &Path) -> Option<BM25Index> {
    let path = index_file_path(project_root);
    let data = std::fs::read_to_string(&path).ok()?;
    let index: BM25Index = serde_json::from_str(&data).ok()?;
    // Only an index admitted under the current policy: one built under
    // another policy, or before the builder admitted its sources (no digest),
    // is neither served nor reused — the rule of `BM25Index::load`.
    let admission = StoreAdmission::current();
    (index.admission_policy.as_deref() == Some(admission.digest()))
        .then(|| index.rooted_at(project_root).with_artifact_origins(&path))
}

pub(crate) fn save(project_root: &Path, idx: &BM25Index) -> std::io::Result<()> {
    let path = index_file_path(project_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let data = serde_json::to_string(idx).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, &path)?;
    // The document origins, beside the index and after it.
    idx.save_artifact_origins(&path);
    Ok(())
}

pub(crate) fn load_or_build(project_root: &Path) -> (BM25Index, Vec<String>) {
    let (files_now, mut warnings) = list_artifact_files(project_root);
    if files_now.is_empty() {
        // No artifact is declared or present now: nothing derived from
        // artifacts is served. The persisted index is not handed out.
        return (BM25Index::new(), warnings);
    }

    if let Some(prev) = load(project_root) {
        if !index_looks_stale(&prev, project_root, &files_now) {
            return (prev, warnings);
        }
        let rebuilt = if prev.files.is_empty() {
            build_full(project_root, &files_now, &mut warnings)
        } else {
            rebuild_incremental(project_root, &prev, &files_now, &mut warnings)
        };
        let _ = save(project_root, &rebuilt);
        return (rebuilt.rooted_at(project_root), warnings);
    }

    let built = build_full(project_root, &files_now, &mut warnings);
    let _ = save(project_root, &built);
    (built.rooted_at(project_root), warnings)
}

pub(crate) fn rebuild_from_scratch(project_root: &Path) -> (BM25Index, Vec<String>) {
    let (files_now, mut warnings) = list_artifact_files(project_root);
    let idx = build_full(project_root, &files_now, &mut warnings);
    let _ = save(project_root, &idx);
    (idx, warnings)
}

fn index_looks_stale(idx: &BM25Index, project_root: &Path, files_now: &[String]) -> bool {
    if files_now.is_empty() {
        return false;
    }
    if idx.files.is_empty() {
        return true;
    }

    let now_set: HashSet<&str> = files_now.iter().map(String::as_str).collect();

    for (rel, old_state) in &idx.files {
        let abs = project_root.join(rel);
        if !abs.exists() {
            return true;
        }
        let Some(cur) = file_state(&abs) else {
            return true;
        };
        if &cur != old_state {
            return true;
        }
        if !now_set.contains(rel.as_str()) {
            return true;
        }
        // A document without an origin on record (extracted before origins
        // were kept), or whose bytes are no longer the ones that were
        // extracted although size and mtime match, is extracted again.
        if is_pdf(&abs)
            && !document_origin(idx, rel)
                .is_some_and(|origin| origin.unchanged(project_root, rel, ARTIFACT_TOOL))
        {
            return true;
        }
    }

    for rel in files_now {
        if !idx.files.contains_key(rel) {
            return true;
        }
    }

    false
}

fn build_full(project_root: &Path, files: &[String], warnings: &mut Vec<String>) -> BM25Index {
    let mut idx = BM25Index::new();
    // One policy for the whole build, recorded with the index (G5, E3).
    let admission = StoreAdmission::current();
    idx.admission_policy = Some(admission.digest().to_owned());

    for rel in files {
        let abs = project_root.join(rel);
        let Some(state) = file_state(&abs) else {
            continue;
        };
        let (content, origin) = match read_artifact(project_root, rel) {
            Ok(read) => read,
            Err(e) => {
                warnings.push(format!("artifact read failed: {rel} ({e})"));
                continue;
            }
        };
        // Derived store: only admitted text, never restricted.
        let Some(content) = admission.admit(&content, &abs) else {
            continue;
        };
        if let Some(origin) = origin {
            idx.record_origin(rel, ChunkOrigin::Document(origin));
        }

        let mut chunks = extract_artifact_chunks(rel, &content);
        chunks.sort_by(|a, b| {
            a.start_line
                .cmp(&b.start_line)
                .then_with(|| a.end_line.cmp(&b.end_line))
                .then_with(|| a.symbol_name.cmp(&b.symbol_name))
        });
        for chunk in chunks {
            add_chunk(&mut idx, chunk);
        }
        idx.files.insert(rel.clone(), state);
    }

    finalize(&mut idx);
    idx
}

fn rebuild_incremental(
    project_root: &Path,
    prev: &BM25Index,
    files: &[String],
    warnings: &mut Vec<String>,
) -> BM25Index {
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

    let mut idx = BM25Index::new();
    let admission = StoreAdmission::current();
    idx.admission_policy = Some(admission.digest().to_owned());
    // Reused chunks must have been admitted under the current policy.
    let reusable = prev.admission_policy.as_deref() == Some(admission.digest());

    for rel in files {
        let abs = project_root.join(rel);
        let Some(state) = file_state(&abs) else {
            continue;
        };

        // A document's chunks are reused only with the origin of the bytes
        // they were extracted from, and only while the file still holds them.
        let recorded = document_origin(prev, rel);
        let unchanged = reusable
            && prev.files.get(rel).is_some_and(|old| *old == state)
            && (!is_pdf(&abs)
                || recorded
                    .is_some_and(|origin| origin.unchanged(project_root, rel, ARTIFACT_TOOL)));
        if unchanged && let Some(chunks) = old_by_file.get(rel) {
            for chunk in chunks {
                add_chunk(&mut idx, chunk.clone());
            }
            idx.files.insert(rel.clone(), state);
            if let Some(origin) = recorded {
                idx.record_origin(rel, ChunkOrigin::Document(origin.clone()));
            }
            continue;
        }

        let (content, origin) = match read_artifact(project_root, rel) {
            Ok(read) => read,
            Err(e) => {
                warnings.push(format!("artifact read failed: {rel} ({e})"));
                continue;
            }
        };
        let Some(content) = admission.admit(&content, &abs) else {
            continue;
        };
        if let Some(origin) = origin {
            idx.record_origin(rel, ChunkOrigin::Document(origin));
        }

        let mut chunks = extract_artifact_chunks(rel, &content);
        chunks.sort_by(|a, b| {
            a.start_line
                .cmp(&b.start_line)
                .then_with(|| a.end_line.cmp(&b.end_line))
                .then_with(|| a.symbol_name.cmp(&b.symbol_name))
        });
        for chunk in chunks {
            add_chunk(&mut idx, chunk);
        }
        idx.files.insert(rel.clone(), state);
    }

    finalize(&mut idx);
    idx
}

fn add_chunk(idx: &mut BM25Index, chunk: CodeChunk) {
    let chunk_idx = idx.chunks.len();
    let tokens = crate::core::bm25_index::tokenize_for_index(&chunk.content);
    for token in &tokens {
        let lower = token.to_lowercase();
        idx.inverted
            .entry(lower)
            .or_default()
            .push((chunk_idx, 1.0));
    }
    idx.chunks.push(CodeChunk {
        token_count: tokens.len(),
        tokens: Vec::new(),
        ..chunk
    });
}

fn finalize(idx: &mut BM25Index) {
    idx.doc_count = idx.chunks.len();
    if idx.doc_count == 0 {
        idx.avg_doc_len = 0.0;
        idx.doc_freqs.clear();
        return;
    }

    let total_len: usize = idx.chunks.iter().map(|c| c.token_count).sum();
    idx.avg_doc_len = total_len as f64 / idx.doc_count as f64;

    idx.doc_freqs.clear();
    for (term, postings) in &idx.inverted {
        let unique_docs: HashSet<usize> = postings.iter().map(|(i, _)| *i).collect();
        idx.doc_freqs.insert(term.clone(), unique_docs.len());
    }
}

fn list_artifact_files(project_root: &Path) -> (Vec<String>, Vec<String>) {
    let resolved = crate::core::artifacts::load_resolved(project_root);
    let mut warnings = resolved.warnings;

    let cfg = crate::core::config::Config::load();
    let extra_ignores: Vec<glob::Pattern> = cfg
        .extra_ignore_patterns
        .iter()
        .filter_map(|p| glob::Pattern::new(p).ok())
        .collect();

    let mut files: Vec<String> = Vec::new();
    for a in resolved.artifacts {
        if !a.exists {
            warnings.push(format!("artifact missing: {} ({})", a.name, a.path));
            continue;
        }

        let abs = project_root.join(&a.path);
        if a.is_dir {
            let walker = ignore::WalkBuilder::new(&abs)
                .hidden(true)
                .git_ignore(true)
                .git_global(true)
                .git_exclude(true)
                .require_git(false)
                .filter_entry(crate::core::walk_filter::keep_entry)
                .build();
            for entry in walker.flatten() {
                let path = entry.path();
                if !path.is_file() {
                    continue;
                }
                if path.components().any(|c| c.as_os_str() == ".git") {
                    continue;
                }
                if !is_artifact_text_file(path) {
                    continue;
                }
                if let Ok(meta) = path.metadata()
                    && meta.len() > MAX_ARTIFACT_BYTES
                {
                    continue;
                }
                // Forward slashes on every platform: these strings are index
                // keys and must match `ResolvedArtifact::path` semantics.
                let rel = path
                    .strip_prefix(project_root)
                    .unwrap_or(path)
                    .to_string_lossy()
                    .replace('\\', "/");
                if rel.is_empty() {
                    continue;
                }
                // Secret-like paths never enter the persisted artifact index,
                // same rule as the BM25 walk (GHSA-rxv5-4c3x-g4q6 A).
                if crate::core::io_boundary::is_secret_like(Path::new(&rel)).is_some() {
                    continue;
                }
                if extra_ignores.iter().any(|p| p.matches(&rel)) {
                    continue;
                }
                files.push(rel);
            }
        } else {
            if !abs.is_file() {
                continue;
            }
            if !is_artifact_text_file(&abs)
                || crate::core::io_boundary::is_secret_like(Path::new(&a.path)).is_some()
            {
                continue;
            }
            if let Ok(meta) = abs.metadata()
                && meta.len() > MAX_ARTIFACT_BYTES
            {
                continue;
            }
            if extra_ignores.iter().any(|p| p.matches(&a.path)) {
                continue;
            }
            files.push(a.path);
        }
    }

    files.sort();
    files.dedup();
    (files, warnings)
}

fn is_artifact_text_file(path: &Path) -> bool {
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    if name.eq_ignore_ascii_case("Dockerfile") {
        return true;
    }
    if name.eq_ignore_ascii_case(".env") {
        return false;
    }

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    matches!(
        ext.as_str(),
        "md" | "mdx"
            | "txt"
            | "json"
            | "yaml"
            | "yml"
            | "toml"
            | "sql"
            | "proto"
            | "tf"
            | "tfvars"
            | "hcl"
            | "rego"
            | "graphql"
            | "gql"
            | "sh"
            | "bash"
            | "zsh"
            | "pdf"
    )
}

fn is_pdf(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
}

/// Read an artifact as indexable text. PDFs go through the panic-safe
/// `pdf-extract` wrapper (GL#1132) — a scanned/image-only or malformed PDF
/// yields a warning instead of aborting the corpus build; everything else is
/// read as UTF-8 like before.
fn read_artifact_text(path: &Path) -> Result<String, String> {
    if is_pdf(path) {
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        return crate::core::web::pdf::extract_text(&bytes);
    }
    std::fs::read_to_string(path).map_err(|e| e.to_string())
}

/// Read the artifact `rel` of `project_root` as indexable text. A document
/// (PDF) is read through the rooted reader's path decision and no-follow
/// open (`DocumentOrigin::read` — the read the serve-time gate repeats) and
/// comes with the origin of the bytes that were extracted; its chunks are
/// served only while the file still holds exactly those bytes.
fn read_artifact(
    project_root: &Path,
    rel: &str,
) -> Result<(String, Option<DocumentOrigin>), String> {
    let abs = project_root.join(rel);
    if !is_pdf(&abs) {
        return read_artifact_text(&abs).map(|text| (text, None));
    }
    let (bytes, origin) = DocumentOrigin::read(project_root, rel, ARTIFACT_TOOL)
        .ok_or_else(|| "document is not readable under the project root".to_string())?;
    crate::core::web::pdf::extract_text(&bytes).map(|text| (text, Some(origin)))
}

fn file_state(path: &Path) -> Option<IndexedFileState> {
    let meta = path.metadata().ok()?;
    let size_bytes = meta.len();
    let mtime_ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)?;
    Some(IndexedFileState {
        mtime_ms,
        size_bytes,
    })
}

fn extract_artifact_chunks(file_path: &str, content: &str) -> Vec<CodeChunk> {
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        return Vec::new();
    }

    let bytes = content.as_bytes();
    let rk_chunks = crate::core::rabin_karp::chunk(content);
    if !rk_chunks.is_empty() && rk_chunks.len() <= 200 {
        let mut out: Vec<CodeChunk> = Vec::new();
        for (idx, c) in rk_chunks.into_iter().take(MAX_CHUNKS_PER_FILE).enumerate() {
            let end = (c.offset + c.length).min(bytes.len());
            let slice = &bytes[c.offset..end];
            let chunk_text = String::from_utf8_lossy(slice).into_owned();
            let token_count = crate::core::bm25_index::tokenize_for_index(&chunk_text).len();
            let start_line = 1 + bytecount::count(&bytes[..c.offset], b'\n');
            let end_line = start_line + bytecount::count(slice, b'\n');
            out.push(CodeChunk {
                file_path: file_path.to_string(),
                symbol_name: format!("{file_path}#chunk-{idx}"),
                kind: ChunkKind::Other,
                start_line,
                end_line: end_line.max(start_line),
                content: chunk_text,
                tokens: Vec::new(),
                token_count,
            });
        }
        return out;
    }

    let token_count = crate::core::bm25_index::tokenize_for_index(content).len();
    let snippet = lines
        .iter()
        .take(50)
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    vec![CodeChunk {
        file_path: file_path.to_string(),
        symbol_name: file_path.to_string(),
        kind: ChunkKind::Other,
        start_line: 1,
        end_line: lines.len(),
        content: snippet,
        tokens: Vec::new(),
        token_count,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Assemble a minimal, syntactically valid single-page PDF whose content
    /// stream draws `text` — offsets in the xref table are computed, so the
    /// fixture stays valid however the text changes.
    fn tiny_pdf(text: &str) -> Vec<u8> {
        let stream = format!("BT /F1 12 Tf 72 720 Td ({text}) Tj ET");
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
             /Resources << /Font << /F1 5 0 R >> >> >>"
                .to_string(),
            format!(
                "<< /Length {} >>\nstream\n{stream}\nendstream",
                stream.len()
            ),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        ];

        let mut out = String::from("%PDF-1.4\n");
        let mut offsets = Vec::with_capacity(objects.len());
        for (i, body) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.push_str(&format!("{} 0 obj\n{body}\nendobj\n", i + 1));
        }
        let xref_at = out.len();
        out.push_str(&format!("xref\n0 {}\n", objects.len() + 1));
        out.push_str("0000000000 65535 f \n");
        for off in &offsets {
            out.push_str(&format!("{off:010} 00000 n \n"));
        }
        out.push_str(&format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF\n",
            objects.len() + 1
        ));
        out.into_bytes()
    }

    fn project_with_docs(files: &[(&str, &[u8])]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let docs = dir.path().join("docs");
        std::fs::create_dir_all(&docs).unwrap();
        for (name, bytes) in files {
            std::fs::write(docs.join(name), bytes).unwrap();
        }
        std::fs::write(
            dir.path().join(".lean-ctx-artifacts.json"),
            r#"{"artifacts":[{"name":"docs","path":"docs","description":"doc corpus"}]}"#,
        )
        .unwrap();
        dir
    }

    /// An artifact the storage admission withholds (a restricted marking)
    /// contributes no chunk, and nothing of it reaches the persisted index.
    #[test]
    fn an_artifact_the_admission_withholds_is_not_persisted() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let dir = project_with_docs(&[
            (
                "plan.md",
                b"TOP SECRET\nroadmap canary 7319 ships in spring".as_slice(),
            ),
            ("notes.md", b"public notes about the release train"),
        ]);

        let (index, _) = load_or_build(dir.path());
        assert!(index.files.contains_key("docs/notes.md"));
        assert!(!index.files.contains_key("docs/plan.md"));
        assert!(index.chunks.iter().all(|c| c.file_path != "docs/plan.md"));
        let persisted = std::fs::read_to_string(index_file_path(dir.path())).unwrap();
        assert!(persisted.contains("release train"));
        assert!(!persisted.contains("7319"), "withheld text was persisted");
    }

    #[test]
    fn artifact_text_file_accepts_pdf_and_rejects_env() {
        assert!(is_artifact_text_file(Path::new("docs/spec.pdf")));
        assert!(is_artifact_text_file(Path::new("docs/Spec.PDF")));
        assert!(is_artifact_text_file(Path::new("notes.md")));
        assert!(!is_artifact_text_file(Path::new(".env")));
        assert!(!is_artifact_text_file(Path::new("logo.png")));
    }

    #[test]
    fn read_artifact_text_extracts_pdf_body() {
        let dir = tempfile::tempdir().unwrap();
        let pdf_path = dir.path().join("spec.pdf");
        std::fs::write(&pdf_path, tiny_pdf("Latency budget is 42ms")).unwrap();

        let text = read_artifact_text(&pdf_path).unwrap();
        assert!(
            text.contains("Latency budget is 42ms"),
            "extracted: {text:?}"
        );
    }

    #[test]
    fn read_artifact_text_reports_malformed_pdf_instead_of_panicking() {
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("broken.pdf");
        std::fs::write(&bad, b"%PDF-1.4\ngarbage without structure").unwrap();

        let err = read_artifact_text(&bad).unwrap_err();
        assert!(!err.is_empty(), "`err` is empty");
    }

    #[test]
    fn corpus_build_indexes_markdown_and_pdf_deterministically() {
        let dir = project_with_docs(&[
            (
                "runbook.md",
                b"# Incident runbook\nRotate the signing key quarterly.".as_slice(),
            ),
            ("spec.pdf", &tiny_pdf("Latency budget is 42ms")),
        ]);

        let (files, warnings) = list_artifact_files(dir.path());
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            files,
            vec!["docs/runbook.md".to_string(), "docs/spec.pdf".to_string()]
        );

        let mut w = Vec::new();
        let idx = build_full(dir.path(), &files, &mut w);
        assert!(w.is_empty(), "{w:?}");

        let md_hits = idx.search("signing key quarterly", 5);
        assert!(md_hits.iter().any(|r| r.file_path == "docs/runbook.md"));
        let pdf_hits = idx.search("latency budget", 5);
        assert!(
            pdf_hits.iter().any(|r| r.file_path == "docs/spec.pdf"),
            "pdf chunk not found: {pdf_hits:?}"
        );

        // Determinism (#498): rebuilding the unchanged corpus yields the same
        // chunk sequence, byte for byte.
        let mut w2 = Vec::new();
        let idx2 = build_full(dir.path(), &files, &mut w2);
        let flat = |i: &BM25Index| {
            i.chunks
                .iter()
                .map(|c| {
                    format!(
                        "{}|{}|{}|{}|{}",
                        c.file_path, c.symbol_name, c.start_line, c.end_line, c.content
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(flat(&idx), flat(&idx2));
    }

    /// GHSA-rxv5-4c3x-g4q6 A: secret-like files inside a declared artifact
    /// directory never enter the persisted artifact index.
    #[test]
    fn artifact_walk_skips_secret_like_files() {
        let dir = project_with_docs(&[
            ("runbook.md", b"# Runbook".as_slice()),
            ("secrets.yaml", b"api_key: x".as_slice()),
            ("credentials.json", b"{\"k\":\"x\"}".as_slice()),
            ("service-account-prod.json", b"{\"k\":\"x\"}".as_slice()),
        ]);
        let (files, _) = list_artifact_files(dir.path());
        assert_eq!(files, vec!["docs/runbook.md".to_string()]);
    }

    #[test]
    fn incremental_rebuild_skips_unchanged_pdf() {
        let dir = project_with_docs(&[("spec.pdf", &tiny_pdf("Latency budget is 42ms"))]);
        let (files, _) = list_artifact_files(dir.path());

        let mut w = Vec::new();
        let full = build_full(dir.path(), &files, &mut w);
        let rebuilt = rebuild_incremental(dir.path(), &full, &files, &mut w);
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(full.chunks.len(), rebuilt.chunks.len());
        assert!(rebuilt.files.contains_key("docs/spec.pdf"));
    }

    /// A chunk extracted from a document is served where a source authority
    /// is required while the file is admitted and still holds the extracted
    /// bytes; not once the project policy rewrites its text, and not once
    /// the file changed (the next build extracts it again). Before, a
    /// document chunk had no source on record and was withheld as a whole.
    #[test]
    fn a_document_chunk_is_served_only_while_its_file_is_admitted_and_unchanged() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _live_policy = crate::core::policy::runtime::lock_test_override();
        let pdf = tiny_pdf("Latency budget heliodor is 42ms");
        let dir = project_with_docs(&[("spec.pdf", pdf.as_slice())]);
        let root = dir.path().canonicalize().expect("canonical root");
        let policy = root.join(".lean-ctx").join("policy.toml");
        let pack = "name='document-origin'\nversion='1.0.0'\ndescription='fixture'\n";
        std::fs::create_dir_all(root.join(".lean-ctx")).expect("policy directory");
        std::fs::write(&policy, pack).expect("policy pack");

        let (index, warnings) = load_or_build(&root);
        assert!(warnings.is_empty(), "{warnings:?}");
        let served = |index: &BM25Index, term: &str| !index.search(term, 5).is_empty();
        assert!(served(&index, "heliodor"), "admitted and unchanged");
        assert!(served(&load(&root).expect("persisted"), "heliodor"));

        // The pack now rewrites the chunk's text: not the copy that is held.
        std::fs::write(&policy, format!("{pack}[redaction]\nbudget = 'heliodor'\n"))
            .expect("tightened pack");
        assert!(!served(&index, "heliodor"), "refused by the project policy");
        std::fs::write(&policy, pack).expect("pack restored");
        assert!(served(&index, "heliodor"));

        // The file changes: the resident chunk is stale, the next build
        // extracts the new bytes through admission.
        std::fs::write(
            root.join("docs").join("spec.pdf"),
            tiny_pdf("Latency budget tourmaline is 7ms"),
        )
        .expect("changed document");
        assert!(!served(&index, "heliodor"), "extracted from other bytes");
        let (rebuilt, _) = load_or_build(&root);
        assert!(served(&rebuilt, "tourmaline"));
        assert!(!served(&rebuilt, "heliodor"));
    }
}
