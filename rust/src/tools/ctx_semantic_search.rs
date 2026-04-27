use std::collections::HashSet;
use std::path::Path;
use std::sync::OnceLock;
use std::time::SystemTime;

use crate::core::embedding_index::EmbeddingIndex;
#[cfg(feature = "embeddings")]
use crate::core::embeddings::EmbeddingEngine;
use crate::core::hybrid_search::{format_hybrid_results, HybridConfig};
use crate::core::vector_index::{format_search_results, BM25Index};
#[cfg(feature = "embeddings")]
use crate::core::{
    embeddings::cosine_similarity,
    hybrid_search::{hybrid_search, DenseSearchResult, HybridResult},
};
use crate::tools::CrpMode;

/// Performs semantic code search using BM25, dense embeddings, or hybrid ranking.
pub fn handle(
    query: &str,
    path: &str,
    top_k: usize,
    crp_mode: CrpMode,
    languages: Option<&[String]>,
    path_glob: Option<&str>,
    mode: Option<&str>,
) -> String {
    let root = Path::new(path);
    if !root.exists() {
        return format!("ERR: path does not exist: {path}");
    }

    let root = if root.is_file() {
        root.parent().unwrap_or(root)
    } else {
        root
    };

    let index = load_or_refresh_bm25(root);
    if index.doc_count == 0 {
        return "No code files found to index.".to_string();
    }

    let filter = match SearchFilter::new(languages, path_glob) {
        Ok(f) => f,
        Err(e) => return format!("ERR: invalid filter: {e}"),
    };

    let compact = crp_mode.is_tdd();
    let mode = mode.unwrap_or("hybrid").to_lowercase();

    match mode.as_str() {
        "bm25" => {
            let mut results = index.search(query, filtered_candidate_k(top_k, filter.is_active()));
            if filter.is_active() {
                results.retain(|x| filter.matches(&x.file_path));
            }
            results.truncate(top_k);

            let header = if compact {
                format!(
                    "semantic_search(bm25,{top_k}) → {} results, {} chunks indexed\n",
                    results.len(),
                    index.doc_count
                )
            } else {
                format!(
                    "Semantic search (BM25): \"{}\" ({} results from {} indexed chunks)\n",
                    truncate_query(query, 60),
                    results.len(),
                    index.doc_count,
                )
            };
            format!("{header}{}", format_search_results(&results, compact))
        }
        "dense" => dense_search_mode(query, root, &index, top_k, compact, &filter),
        _ => hybrid_search_mode(query, root, &index, top_k, compact, &filter),
    }
}

/// Rebuilds the BM25 search index for the given directory from scratch.
pub fn handle_reindex(path: &str) -> String {
    let root = Path::new(path);
    if !root.exists() {
        return format!("ERR: path does not exist: {path}");
    }
    let root = if root.is_file() {
        root.parent().unwrap_or(root)
    } else {
        root
    };

    let idx = BM25Index::build_from_directory(root);
    let count = idx.doc_count;
    let chunks = idx.chunks.len();
    let _ = idx.save(root);

    format!("Reindexed {path}: {count} files, {chunks} chunks")
}

fn truncate_query(q: &str, max: usize) -> &str {
    if q.len() <= max {
        q
    } else {
        &q[..max]
    }
}

fn load_or_refresh_bm25(root: &Path) -> BM25Index {
    let loaded = BM25Index::load(root);
    let stale = loaded.as_ref().is_some_and(|idx| idx.doc_count > 0) && index_is_stale(root);
    if let Some(idx) = loaded {
        if !stale && idx.doc_count > 0 {
            return idx;
        }
    }

    let idx = BM25Index::build_from_directory(root);
    let _ = idx.save(root);
    idx
}

fn index_is_stale(root: &Path) -> bool {
    let index_path = BM25Index::index_file_path(root);
    let Ok(index_mtime) = std::fs::metadata(&index_path).and_then(|m| m.modified()) else {
        return true;
    };

    let mut newest: Option<SystemTime> = None;
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
        if !crate::core::vector_index::is_code_file(path) {
            continue;
        }
        file_count += 1;
        let Ok(mtime) = path.metadata().and_then(|m| m.modified()) else {
            continue;
        };
        newest = Some(match newest {
            Some(cur) if cur > mtime => cur,
            _ => mtime,
        });
    }

    newest.is_some_and(|t| t > index_mtime)
}

fn filtered_candidate_k(top_k: usize, filtered: bool) -> usize {
    if !filtered {
        return top_k;
    }
    let candidates = (top_k.max(10)).saturating_mul(10);
    candidates.clamp(50, 500)
}

fn hybrid_search_mode(
    query: &str,
    root: &Path,
    index: &BM25Index,
    top_k: usize,
    compact: bool,
    filter: &SearchFilter,
) -> String {
    #[cfg(feature = "embeddings")]
    {
        let (engine, mut embed_idx) = match load_engine_and_index(root) {
            Ok(v) => v,
            Err(e) => return format!("ERR: {e}"),
        };

        let (aligned, coverage) = match ensure_embeddings(root, index, engine, &mut embed_idx) {
            Ok(v) => v,
            Err(e) => return format!("ERR: {e}"),
        };

        let cfg = HybridConfig::default();
        let mut results = hybrid_search(query, index, Some(engine), Some(&aligned), top_k, &cfg);
        if filter.is_active() {
            results.retain(|r| filter.matches(&r.file_path));
        }
        results.truncate(top_k);

        let header = if compact {
            format!(
                "semantic_search(hybrid,{top_k}) → {} results, {} chunks, embed_cov={:.0}%\n",
                results.len(),
                index.doc_count,
                coverage * 100.0
            )
        } else {
            format!(
                "Semantic search (Hybrid): \"{}\" ({} results from {} indexed chunks, embeddings coverage {:.0}%)\n",
                truncate_query(query, 60),
                results.len(),
                index.doc_count,
                coverage * 100.0
            )
        };

        format!("{header}{}", format_hybrid_results(&results, compact))
    }
    #[cfg(not(feature = "embeddings"))]
    {
        let mut results = index.search(query, filtered_candidate_k(top_k, filter.is_active()));
        if filter.is_active() {
            results.retain(|x| filter.matches(&x.file_path));
        }
        results.truncate(top_k);
        let header = if compact {
            format!(
                "semantic_search(bm25,{top_k}) → {} results, {} chunks indexed\n",
                results.len(),
                index.doc_count
            )
        } else {
            format!(
                "Semantic search (BM25): \"{}\" ({} results from {} indexed chunks)\n",
                truncate_query(query, 60),
                results.len(),
                index.doc_count,
            )
        };
        format!("{header}{}", format_search_results(&results, compact))
    }
}

fn dense_search_mode(
    query: &str,
    root: &Path,
    index: &BM25Index,
    top_k: usize,
    compact: bool,
    filter: &SearchFilter,
) -> String {
    #[cfg(feature = "embeddings")]
    {
        let (engine, mut embed_idx) = match load_engine_and_index(root) {
            Ok(v) => v,
            Err(e) => return format!("ERR: {e}"),
        };

        let (aligned, coverage) = match ensure_embeddings(root, index, engine, &mut embed_idx) {
            Ok(v) => v,
            Err(e) => return format!("ERR: {e}"),
        };

        let query_embedding = match engine.embed(query) {
            Ok(v) => v,
            Err(e) => return format!("ERR: embedding failed: {e}"),
        };

        let mut scored: Vec<(usize, f32)> = aligned
            .iter()
            .enumerate()
            .filter(|(i, _)| {
                if !filter.is_active() {
                    return true;
                }
                index
                    .chunks
                    .get(*i)
                    .is_some_and(|c| filter.matches(&c.file_path))
            })
            .map(|(i, emb)| (i, cosine_similarity(&query_embedding, emb)))
            .collect();

        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(top_k);

        let results: Vec<HybridResult> = scored
            .into_iter()
            .filter_map(|(idx, sim)| {
                let chunk = index.chunks.get(idx)?;
                let snippet = chunk.content.lines().take(5).collect::<Vec<_>>().join("\n");
                let dense = DenseSearchResult {
                    chunk_idx: idx,
                    similarity: sim,
                    file_path: chunk.file_path.clone(),
                    symbol_name: chunk.symbol_name.clone(),
                    kind: chunk.kind.clone(),
                    start_line: chunk.start_line,
                    end_line: chunk.end_line,
                    snippet,
                };
                Some(HybridResult {
                    file_path: dense.file_path,
                    symbol_name: dense.symbol_name,
                    kind: dense.kind,
                    start_line: dense.start_line,
                    end_line: dense.end_line,
                    snippet: dense.snippet,
                    rrf_score: dense.similarity as f64,
                    bm25_score: None,
                    dense_score: Some(dense.similarity),
                    bm25_rank: None,
                    dense_rank: None,
                })
            })
            .collect();

        let header = if compact {
            format!(
                "semantic_search(dense,{top_k}) → {} results, {} chunks, embed_cov={:.0}%\n",
                results.len(),
                index.doc_count,
                coverage * 100.0
            )
        } else {
            format!(
                "Semantic search (Dense): \"{}\" ({} results from {} indexed chunks, embeddings coverage {:.0}%)\n",
                truncate_query(query, 60),
                results.len(),
                index.doc_count,
                coverage * 100.0
            )
        };

        format!("{header}{}", format_hybrid_results(&results, compact))
    }
    #[cfg(not(feature = "embeddings"))]
    {
        "ERR: embeddings feature not enabled".to_string()
    }
}

#[cfg(feature = "embeddings")]
fn load_engine_and_index(
    root: &Path,
) -> Result<(&'static EmbeddingEngine, EmbeddingIndex), String> {
    static ENGINE: OnceLock<anyhow::Result<EmbeddingEngine>> = OnceLock::new();
    let engine = ENGINE
        .get_or_init(EmbeddingEngine::load_default)
        .as_ref()
        .map_err(|e| format!("embedding engine load failed: {e}"))?;

    let mut idx =
        EmbeddingIndex::load(root).unwrap_or_else(|| EmbeddingIndex::new(engine.dimensions()));
    if idx.dimensions != engine.dimensions() {
        idx = EmbeddingIndex::new(engine.dimensions());
    }
    Ok((engine, idx))
}

#[cfg(feature = "embeddings")]
fn ensure_embeddings(
    root: &Path,
    index: &BM25Index,
    engine: &EmbeddingEngine,
    embed_idx: &mut EmbeddingIndex,
) -> Result<(Vec<Vec<f32>>, f64), String> {
    let mut changed = embed_idx.files_needing_update(&index.chunks);
    changed.sort();
    changed.dedup();

    if !changed.is_empty() {
        let changed_set: std::collections::HashSet<&str> =
            changed.iter().map(std::string::String::as_str).collect();
        let mut new_embeddings: Vec<(usize, Vec<f32>)> = Vec::new();
        for (i, c) in index.chunks.iter().enumerate() {
            if !changed_set.contains(c.file_path.as_str()) {
                continue;
            }
            let emb = engine
                .embed(&c.content)
                .map_err(|e| format!("embed failed for {}: {e}", c.file_path))?;
            new_embeddings.push((i, emb));
        }
        embed_idx.update(&index.chunks, &new_embeddings, &changed);
        embed_idx
            .save(root)
            .map_err(|e| format!("save embeddings failed: {e}"))?;
    }

    if let Some(aligned) = embed_idx.get_aligned_embeddings(&index.chunks) {
        let coverage = embed_idx.coverage(index.chunks.len());
        return Ok((aligned, coverage));
    }

    // Alignment missing: rebuild everything once.
    let mut all_files: Vec<String> = index.chunks.iter().map(|c| c.file_path.clone()).collect();
    all_files.sort();
    all_files.dedup();

    let mut new_embeddings: Vec<(usize, Vec<f32>)> = Vec::with_capacity(index.chunks.len());
    for (i, c) in index.chunks.iter().enumerate() {
        let emb = engine
            .embed(&c.content)
            .map_err(|e| format!("embed failed for {}: {e}", c.file_path))?;
        new_embeddings.push((i, emb));
    }

    embed_idx.update(&index.chunks, &new_embeddings, &all_files);
    embed_idx
        .save(root)
        .map_err(|e| format!("save embeddings failed: {e}"))?;

    let aligned = embed_idx
        .get_aligned_embeddings(&index.chunks)
        .ok_or_else(|| "embedding alignment failed after full rebuild".to_string())?;
    let coverage = embed_idx.coverage(index.chunks.len());
    Ok((aligned, coverage))
}

struct SearchFilter {
    allowed_exts: Option<HashSet<String>>,
    path_glob: Option<glob::Pattern>,
}

impl SearchFilter {
    fn new(languages: Option<&[String]>, path_glob: Option<&str>) -> Result<Self, String> {
        let allowed_exts = languages.map(normalize_languages);
        let path_glob = match path_glob {
            None => None,
            Some(s) if s.trim().is_empty() => None,
            Some(s) => Some(glob::Pattern::new(s).map_err(|e| e.msg.to_string())?),
        };
        Ok(Self {
            allowed_exts,
            path_glob,
        })
    }

    fn is_active(&self) -> bool {
        self.allowed_exts.is_some() || self.path_glob.is_some()
    }

    fn matches(&self, rel_path: &str) -> bool {
        let rel_path = rel_path.replace('\\', "/");
        if let Some(p) = &self.path_glob {
            if !p.matches(&rel_path) {
                return false;
            }
        }
        if let Some(exts) = &self.allowed_exts {
            let ext = Path::new(&rel_path)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_lowercase();
            if ext.is_empty() || !exts.contains(&ext) {
                return false;
            }
        }
        true
    }
}

fn normalize_languages(langs: &[String]) -> HashSet<String> {
    let mut out = HashSet::new();
    for l in langs {
        let raw = l.trim().trim_start_matches('.').to_lowercase();
        match raw.as_str() {
            "rust" | "rs" => {
                out.insert("rs".to_string());
            }
            "ts" | "typescript" => {
                out.insert("ts".to_string());
                out.insert("tsx".to_string());
            }
            "js" | "javascript" => {
                out.insert("js".to_string());
                out.insert("jsx".to_string());
                out.insert("mjs".to_string());
                out.insert("cjs".to_string());
            }
            "py" | "python" => {
                out.insert("py".to_string());
            }
            "go" => {
                out.insert("go".to_string());
            }
            "java" => {
                out.insert("java".to_string());
            }
            "ruby" | "rb" => {
                out.insert("rb".to_string());
            }
            "php" => {
                out.insert("php".to_string());
            }
            "c" => {
                out.insert("c".to_string());
                out.insert("h".to_string());
            }
            "cpp" | "c++" | "cc" => {
                out.insert("cpp".to_string());
                out.insert("hpp".to_string());
                out.insert("cc".to_string());
                out.insert("hh".to_string());
            }
            "cs" | "csharp" => {
                out.insert("cs".to_string());
            }
            "swift" => {
                out.insert("swift".to_string());
            }
            "kt" | "kotlin" => {
                out.insert("kt".to_string());
                out.insert("kts".to_string());
            }
            "json" => {
                out.insert("json".to_string());
            }
            "yaml" | "yml" => {
                out.insert("yaml".to_string());
                out.insert("yml".to_string());
            }
            other if !other.is_empty() => {
                out.insert(other.to_string());
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod filter_tests {
    use super::*;

    #[test]
    fn filter_language_rust() {
        let f = SearchFilter::new(Some(&["rust".into()]), None).unwrap();
        assert!(f.matches("src/main.rs"));
        assert!(!f.matches("src/main.ts"));
    }

    #[test]
    fn filter_path_glob() {
        let f = SearchFilter::new(None, Some("rust/src/**")).unwrap();
        assert!(f.matches("rust/src/core/mod.rs"));
        assert!(!f.matches("website/src/pages/index.astro"));
    }
}
