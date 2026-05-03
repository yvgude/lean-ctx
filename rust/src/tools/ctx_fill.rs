use std::path::Path;

use crate::core::cache::SessionCache;
use crate::core::signatures;
use crate::core::tokens::count_tokens;
use crate::tools::CrpMode;

struct FileCandidate {
    path: String,
    score: f64,
    tokens_full: usize,
    tokens_map: usize,
    tokens_sig: usize,
}

pub fn handle(
    cache: &mut SessionCache,
    paths: &[String],
    budget: usize,
    crp_mode: CrpMode,
    task: Option<&str>,
) -> String {
    if paths.is_empty() {
        return "No files specified.".to_string();
    }

    let mut candidates: Vec<FileCandidate> = Vec::new();

    for path in paths {
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };

        let ext = Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        let tokens_full = count_tokens(&content);
        let sigs = signatures::extract_signatures(&content, ext);
        let sig_text: String = sigs
            .iter()
            .map(super::super::core::signatures::Signature::to_compact)
            .collect::<Vec<_>>()
            .join("\n");
        let tokens_sig = count_tokens(&sig_text);

        let map_text = format_map(&content, ext, &sigs);
        let tokens_map = count_tokens(&map_text);

        let score = compute_relevance_score(path, &content);

        candidates.push(FileCandidate {
            path: path.clone(),
            score,
            tokens_full,
            tokens_map,
            tokens_sig,
        });
    }

    candidates.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut pop_lines: Vec<String> = Vec::new();
    if let Some(t) = task {
        if let Some(root) = paths
            .first()
            .and_then(|p| crate::core::protocol::detect_project_root(p))
        {
            let rs: Vec<crate::core::task_relevance::RelevanceScore> = candidates
                .iter()
                .map(|c| crate::core::task_relevance::RelevanceScore {
                    path: c.path.clone(),
                    score: c.score,
                    recommended_mode: "signatures",
                })
                .collect();
            let refs: Vec<&crate::core::task_relevance::RelevanceScore> = rs.iter().collect();
            let pop = crate::core::pop_pruning::decide_for_candidates(t, &root, &refs);
            if !pop.excluded_modules.is_empty() {
                let excluded: std::collections::BTreeSet<&str> = pop
                    .excluded_modules
                    .iter()
                    .map(|e| e.module.as_str())
                    .collect();
                candidates.retain(|c| {
                    let m = crate::core::pop_pruning::module_for_path(&c.path, &root);
                    !excluded.contains(m.as_str())
                });
                pop_lines.push("POP:".to_string());
                for ex in &pop.excluded_modules {
                    pop_lines.push(format!(
                        "  - exclude {}/ ({} candidates) — {}",
                        ex.module, ex.candidate_files, ex.reason
                    ));
                }
            }
        }
    }

    let mut used_tokens = 0usize;
    let mut selections: Vec<(String, String)> = Vec::new();

    for candidate in &candidates {
        if used_tokens >= budget {
            break;
        }

        let remaining = budget - used_tokens;
        let (mode, cost) = select_best_fit(candidate, remaining);

        if cost > remaining {
            let sig_cost = candidate.tokens_sig;
            if sig_cost <= remaining {
                selections.push((candidate.path.clone(), "signatures".to_string()));
                used_tokens += sig_cost;
            }
            continue;
        }

        selections.push((candidate.path.clone(), mode));
        used_tokens += cost;
    }

    let mut output_parts = Vec::new();
    output_parts.push(format!(
        "ctx_fill: {budget} token budget, {} files analyzed, {} selected",
        candidates.len(),
        selections.len()
    ));
    if !pop_lines.is_empty() {
        output_parts.push(pop_lines.join("\n"));
    }
    output_parts.push(String::new());

    for (path, mode) in &selections {
        let result = crate::tools::ctx_read::handle(cache, path, mode, crp_mode);
        output_parts.push(result);
        output_parts.push("---".to_string());
    }

    let skipped = candidates.len() - selections.len();
    if skipped > 0 {
        output_parts.push(format!("{skipped} files skipped (budget exhausted)"));
    }
    output_parts.push(format!("\nUsed: {used_tokens}/{budget} tokens"));

    output_parts.join("\n")
}

fn select_best_fit(candidate: &FileCandidate, remaining: usize) -> (String, usize) {
    if candidate.tokens_full <= remaining {
        return ("full".to_string(), candidate.tokens_full);
    }
    if candidate.tokens_map <= remaining {
        return ("map".to_string(), candidate.tokens_map);
    }
    if candidate.tokens_sig <= remaining {
        return ("signatures".to_string(), candidate.tokens_sig);
    }
    ("signatures".to_string(), candidate.tokens_sig)
}

fn compute_relevance_score(path: &str, content: &str) -> f64 {
    let mut score = 1.0;

    let name = Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    if name.contains("test") || name.contains("spec") {
        score *= 0.5;
    }
    if name.contains("config") || name.contains("types") || name.contains("schema") {
        score *= 1.3;
    }
    if name == "mod.rs" || name == "index.ts" || name == "index.js" || name == "__init__.py" {
        score *= 1.5;
    }

    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    if matches!(ext, "rs" | "ts" | "py" | "go" | "java") {
        score *= 1.2;
    }

    let lines = content.lines().count();
    if lines > 500 {
        score *= 0.8;
    }
    if lines < 50 {
        score *= 1.1;
    }

    let export_count = content
        .lines()
        .filter(|l| l.contains("pub ") || l.contains("export ") || l.contains("def "))
        .count();
    score *= 1.0 + (export_count as f64 * 0.02).min(0.5);

    score
}

fn format_map(content: &str, ext: &str, sigs: &[crate::core::signatures::Signature]) -> String {
    let deps = crate::core::deps::extract_deps(content, ext);
    let mut parts = Vec::new();
    if !deps.imports.is_empty() {
        parts.push(format!("deps: {}", deps.imports.join(", ")));
    }
    if !deps.exports.is_empty() {
        parts.push(format!("exports: {}", deps.exports.join(", ")));
    }
    let key_sigs: Vec<_> = sigs
        .iter()
        .filter(|s| s.is_exported || s.indent == 0)
        .collect();
    for sig in &key_sigs {
        parts.push(sig.to_compact());
    }
    parts.join("\n")
}
