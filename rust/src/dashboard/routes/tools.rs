use std::collections::HashMap;
use std::path::Path;

use super::helpers::{
    detect_project_root_for_dashboard, extract_query_param, normalize_dashboard_demo_path,
};

pub(super) fn handle(
    path: &str,
    query_str: &str,
    _method: &str,
    _body: &str,
) -> Option<(&'static str, &'static str, String)> {
    match path {
        "/api/search-index" => {
            let root_s = detect_project_root_for_dashboard();
            let root = Path::new(&root_s);
            match crate::core::bm25_index::get_or_start_build(root) {
                Ok(index) => {
                    let summary = bm25_index_summary_json(&index);
                    let json = serde_json::to_string(&summary).unwrap_or_else(|_| {
                        "{\"error\":\"failed to serialize search index summary\"}".to_string()
                    });
                    Some(("200 OK", "application/json", json))
                }
                Err(progress) => Some(search_building_response(&progress)),
            }
        }
        "/api/search" => {
            let q = extract_query_param(query_str, "q").unwrap_or_default();
            let limit: usize = extract_query_param(query_str, "limit")
                .and_then(|l| l.parse().ok())
                .unwrap_or(20);
            if q.trim().is_empty() {
                Some((
                    "200 OK",
                    "application/json",
                    r#"{"results":[]}"#.to_string(),
                ))
            } else {
                let root_s = detect_project_root_for_dashboard();
                let root = Path::new(&root_s);
                match crate::core::bm25_index::get_or_start_build(root) {
                    Ok(index) => {
                        let hits = index.search(&q, limit);
                        let results: Vec<serde_json::Value> = hits
                            .iter()
                            .map(|r| {
                                serde_json::json!({
                                    "score": (r.score * 100.0).round() / 100.0,
                                    "file_path": r.file_path,
                                    "symbol_name": r.symbol_name,
                                    "kind": r.kind,
                                    "start_line": r.start_line,
                                    "end_line": r.end_line,
                                    "snippet": r.snippet,
                                })
                            })
                            .collect();
                        let json = serde_json::json!({ "results": results }).to_string();
                        Some(("200 OK", "application/json", json))
                    }
                    Err(progress) => Some(search_building_response(&progress)),
                }
            }
        }
        "/api/compression-demo" => {
            let body = match extract_query_param(query_str, "path") {
                None => r#"{"error":"missing path query parameter"}"#.to_string(),
                Some(rel) => {
                    let task = extract_query_param(query_str, "task");
                    let root = detect_project_root_for_dashboard();
                    let root_pb = Path::new(&root);
                    let rel = normalize_dashboard_demo_path(&rel);
                    let candidate = Path::new(&rel);

                    let mut tried_paths: Vec<String> = Vec::new();
                    let mut full: Option<std::path::PathBuf> = None;
                    let mut content: Option<String> = None;

                    let mut attempts: Vec<std::path::PathBuf> = Vec::new();
                    if candidate.is_absolute() {
                        attempts.push(candidate.to_path_buf());
                    } else {
                        attempts.push(root_pb.join(&rel));
                        attempts.push(root_pb.join("rust").join(&rel));
                    }

                    for p in attempts {
                        tried_paths.push(p.to_string_lossy().to_string());
                        let Ok(p) = crate::core::pathjail::jail_path(&p, root_pb) else {
                            continue;
                        };

                        if let Ok(c) = std::fs::read_to_string(&p) {
                            full = Some(p);
                            content = Some(c);
                            break;
                        }
                    }

                    let mut resolved_from: Option<String> = None;
                    let mut candidates: Vec<String> = Vec::new();

                    if content.is_none() && !candidate.is_absolute() && !rel.trim().is_empty() {
                        // Premium path healing: try to map stale paths to current indexed files.
                        let index = crate::core::graph_index::load_or_build(&root);
                        let requested_key = crate::core::graph_index::graph_match_key(&rel);
                        let requested_name = requested_key.rsplit('/').next().unwrap_or("");

                        let mut exact: Vec<String> = Vec::new();
                        let mut suffix: Vec<String> = Vec::new();
                        let mut filename: Vec<String> = Vec::new();
                        let mut seen = std::collections::HashSet::<&str>::new();

                        for p in index.files.keys() {
                            let p_str = p.as_str();
                            if !seen.insert(p_str) {
                                continue;
                            }
                            let p_key = crate::core::graph_index::graph_match_key(p_str);
                            if p_key == requested_key {
                                exact.push(p_str.to_string());
                            } else if !requested_key.is_empty() && p_key.ends_with(&requested_key) {
                                suffix.push(p_str.to_string());
                            } else if !requested_name.is_empty()
                                && p_key
                                    .rsplit('/')
                                    .next()
                                    .is_some_and(|n| n == requested_name)
                            {
                                filename.push(p_str.to_string());
                            }
                        }

                        let mut best = if !exact.is_empty() {
                            exact
                        } else if !suffix.is_empty() {
                            suffix
                        } else {
                            filename
                        };
                        best.sort_by_key(String::len);

                        if best.len() == 1 {
                            let rel2 = best[0].clone();
                            let p2 = root_pb.join(rel2.trim_start_matches(['/', '\\']));
                            tried_paths.push(p2.to_string_lossy().to_string());
                            if let Ok(p2) = crate::core::pathjail::jail_path(&p2, root_pb) {
                                if let Ok(c2) = std::fs::read_to_string(&p2) {
                                    full = Some(p2);
                                    content = Some(c2);
                                    resolved_from = Some(rel2);
                                } else {
                                    candidates = best;
                                }
                            } else {
                                candidates = best;
                            }
                        } else if best.len() > 1 {
                            best.truncate(10);
                            candidates = best;
                        }
                    }

                    match (full, content) {
                        (Some(full), Some(content)) => {
                            let ext = full.extension().and_then(|e| e.to_str()).unwrap_or("rs");
                            let path_str = full.to_string_lossy().to_string();
                            let original_lines = content.lines().count();
                            let original_tokens = crate::core::tokens::count_tokens(&content);
                            let modes = compression_demo_modes_json(
                                &content,
                                &path_str,
                                ext,
                                original_tokens,
                                task.as_deref(),
                            );
                            let original_preview: String = content.chars().take(8000).collect();
                            serde_json::json!({
                                "path": path_str,
                                "task": task,
                                "original_lines": original_lines,
                                "original_tokens": original_tokens,
                                "original": original_preview,
                                "modes": modes,
                                "resolved_from": resolved_from,
                            })
                            .to_string()
                        }
                        _ => serde_json::json!({
                            "error": "failed to read file",
                            "project_root": root,
                            "requested_path": rel,
                            "candidates": candidates,
                            "tried_paths": tried_paths,
                        })
                        .to_string(),
                    }
                }
            };
            Some(("200 OK", "application/json", body))
        }
        _ => None,
    }
}

fn compression_mode_json(output: &str, original_tokens: usize) -> serde_json::Value {
    let tokens = crate::core::tokens::count_tokens(output);
    let savings_pct = if original_tokens > 0 {
        ((original_tokens.saturating_sub(tokens)) as f64 / original_tokens as f64 * 100.0).round()
            as i64
    } else {
        0
    };
    serde_json::json!({
        "output": output,
        "tokens": tokens,
        "savings_pct": savings_pct
    })
}

fn compression_demo_modes_json(
    content: &str,
    path: &str,
    ext: &str,
    original_tokens: usize,
    task: Option<&str>,
) -> serde_json::Value {
    let map_out = crate::core::signatures::extract_file_map(path, content);
    let sig_out = crate::core::signatures::extract_signatures(content, ext)
        .iter()
        .map(crate::core::signatures::Signature::to_compact)
        .collect::<Vec<_>>()
        .join("\n");
    let aggressive_out = crate::core::filters::aggressive_filter(content);
    let entropy_out = crate::core::entropy::entropy_compress_adaptive(content, path).output;

    let mut cache = crate::core::cache::SessionCache::new();
    let reference_out =
        crate::tools::ctx_read::handle(&mut cache, path, "reference", crate::tools::CrpMode::Off);
    let task_out = task.filter(|t| !t.trim().is_empty()).map(|t| {
        crate::tools::ctx_read::handle_with_task(
            &mut cache,
            path,
            "task",
            crate::tools::CrpMode::Off,
            Some(t),
        )
    });

    serde_json::json!({
        "map": compression_mode_json(&map_out, original_tokens),
        "signatures": compression_mode_json(&sig_out, original_tokens),
        "reference": compression_mode_json(&reference_out, original_tokens),
        "aggressive": compression_mode_json(&aggressive_out, original_tokens),
        "entropy": compression_mode_json(&entropy_out, original_tokens),
        "task": task_out.as_deref().map_or(serde_json::Value::Null, |s| compression_mode_json(s, original_tokens)),
    })
}

/// `202 Accepted` body for a search route whose BM25 index is still building in
/// the background (#452). The dashboard polls the same route and renders once it
/// returns `200`.
fn search_building_response(
    progress: &crate::core::bm25_index::SearchIndexBuildProgress,
) -> (&'static str, &'static str, String) {
    let json =
        serde_json::to_string(progress).unwrap_or_else(|_| "{\"status\":\"building\"}".to_string());
    ("202 Accepted", "application/json", json)
}

fn bm25_index_summary_json(index: &crate::core::bm25_index::BM25Index) -> serde_json::Value {
    let mut sorted: Vec<&crate::core::bm25_index::CodeChunk> = index.chunks.iter().collect();
    sorted.sort_by_key(|c| std::cmp::Reverse(c.token_count));
    let top: Vec<serde_json::Value> = sorted
        .into_iter()
        .take(20)
        .map(|c| {
            serde_json::json!({
                "file_path": c.file_path,
                "symbol_name": c.symbol_name,
                "token_count": c.token_count,
                "kind": c.kind,
                "start_line": c.start_line,
                "end_line": c.end_line,
            })
        })
        .collect();
    let mut lang: HashMap<String, usize> = HashMap::new();
    for c in &index.chunks {
        let e = Path::new(&c.file_path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_string();
        *lang.entry(e).or_default() += 1;
    }
    serde_json::json!({
        "doc_count": index.doc_count,
        "chunk_count": index.chunks.len(),
        "top_chunks_by_token_count": top,
        "language_distribution": lang,
    })
}
