use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::core::vector_index::{BM25Index, ChunkKind, CodeChunk, IndexedFileState};

const MAX_ARTIFACT_BYTES: u64 = 2_000_000;
const MAX_CHUNKS_PER_FILE: usize = 50;

pub fn index_file_path(project_root: &Path) -> PathBuf {
    let code_idx = BM25Index::index_file_path(project_root);
    let dir = code_idx.parent().unwrap_or_else(|| Path::new("."));
    dir.join("bm25_artifacts_index.json")
}

pub fn load(project_root: &Path) -> Option<BM25Index> {
    let path = index_file_path(project_root);
    let data = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

pub fn save(project_root: &Path, idx: &BM25Index) -> std::io::Result<()> {
    let path = index_file_path(project_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let data = serde_json::to_string(idx).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

pub fn load_or_build(project_root: &Path) -> (BM25Index, Vec<String>) {
    let (files_now, mut warnings) = list_artifact_files(project_root);
    if files_now.is_empty() {
        return (load(project_root).unwrap_or_default(), warnings);
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
        return (rebuilt, warnings);
    }

    let built = build_full(project_root, &files_now, &mut warnings);
    let _ = save(project_root, &built);
    (built, warnings)
}

pub fn rebuild_from_scratch(project_root: &Path) -> (BM25Index, Vec<String>) {
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

    for rel in files {
        let abs = project_root.join(rel);
        let Some(state) = file_state(&abs) else {
            continue;
        };
        let content = match std::fs::read_to_string(&abs) {
            Ok(s) => s,
            Err(e) => {
                warnings.push(format!("artifact read failed: {rel} ({e})"));
                continue;
            }
        };

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

    for rel in files {
        let abs = project_root.join(rel);
        let Some(state) = file_state(&abs) else {
            continue;
        };

        let unchanged = prev.files.get(rel).is_some_and(|old| *old == state);
        if unchanged {
            if let Some(chunks) = old_by_file.get(rel) {
                for chunk in chunks {
                    add_chunk(&mut idx, chunk.clone());
                }
                idx.files.insert(rel.clone(), state);
                continue;
            }
        }

        let content = match std::fs::read_to_string(&abs) {
            Ok(s) => s,
            Err(e) => {
                warnings.push(format!("artifact read failed: {rel} ({e})"));
                continue;
            }
        };

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
    for token in &chunk.tokens {
        let lower = token.to_lowercase();
        idx.inverted
            .entry(lower)
            .or_default()
            .push((chunk_idx, 1.0));
    }
    idx.chunks.push(chunk);
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
                if let Ok(meta) = path.metadata() {
                    if meta.len() > MAX_ARTIFACT_BYTES {
                        continue;
                    }
                }
                let rel = path
                    .strip_prefix(project_root)
                    .unwrap_or(path)
                    .to_string_lossy()
                    .to_string();
                if rel.is_empty() {
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
            if !is_artifact_text_file(&abs) {
                continue;
            }
            if let Ok(meta) = abs.metadata() {
                if meta.len() > MAX_ARTIFACT_BYTES {
                    continue;
                }
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
    )
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
            let tokens = crate::core::vector_index::tokenize_for_index(&chunk_text);
            let token_count = tokens.len();
            let start_line = 1 + bytecount::count(&bytes[..c.offset], b'\n');
            let end_line = start_line + bytecount::count(slice, b'\n');
            out.push(CodeChunk {
                file_path: file_path.to_string(),
                symbol_name: format!("{file_path}#chunk-{idx}"),
                kind: ChunkKind::Other,
                start_line,
                end_line: end_line.max(start_line),
                content: chunk_text,
                tokens,
                token_count,
            });
        }
        return out;
    }

    let tokens = crate::core::vector_index::tokenize_for_index(content);
    let token_count = tokens.len();
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
        tokens,
        token_count,
    }]
}
