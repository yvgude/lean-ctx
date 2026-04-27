use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;

use crate::core::cache::SessionCache;
use crate::core::graph_index::ProjectIndex;
use crate::core::protocol;
use crate::core::task_relevance::{compute_relevance, parse_task_hints};
use crate::tools::CrpMode;

const DEFAULT_MAX_FILES: usize = 10;

pub fn handle(
    cache: &mut SessionCache,
    root: &str,
    task: Option<&str>,
    changed_files: Option<&[String]>,
    budget_tokens: usize,
    max_files: Option<usize>,
    crp_mode: CrpMode,
) -> String {
    let project_root = if root.trim().is_empty() { "." } else { root };
    let index = crate::core::graph_index::load_or_build(project_root);

    let mut candidates: BTreeMap<String, f64> = BTreeMap::new(); // path -> score

    if let Some(t) = task {
        let (task_files, task_keywords) = parse_task_hints(t);
        let relevance = compute_relevance(&index, &task_files, &task_keywords);
        for r in relevance.iter().take(50) {
            if r.score < 0.1 {
                break;
            }
            candidates.insert(r.path.clone(), r.score);
        }
    }

    if let Some(changed) = changed_files {
        for p in changed {
            let rel = normalize_rel_path(p, project_root);
            for (path, dist) in blast_radius(&index, &rel, 2) {
                let boost = 1.0 / (dist.max(1) as f64);
                candidates
                    .entry(path)
                    .and_modify(|s| *s = (*s + boost).min(1.0))
                    .or_insert(boost.min(1.0));
            }
        }
    }

    if candidates.is_empty() {
        return "ctx_prefetch: no candidates (provide task or changed_files)".to_string();
    }

    let mut scored: Vec<(String, f64)> = candidates.into_iter().collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let max_files = max_files.unwrap_or(DEFAULT_MAX_FILES).max(1);
    let mut picked: Vec<String> = Vec::new();
    for (p, _s) in scored {
        picked.push(p);
        if picked.len() >= max_files {
            break;
        }
    }

    let mut total = 0usize;
    let mut prefetched: Vec<(String, String)> = Vec::new(); // (path, mode)
    for p in &picked {
        let full = to_fs_path(project_root, p);
        let Ok(content) = std::fs::read_to_string(&full) else {
            continue;
        };
        let tokens = crate::core::tokens::count_tokens(&content);
        total = total.saturating_add(tokens);

        let mode = if budget_tokens > 0 {
            let ratio = budget_tokens as f64 / total.max(1) as f64;
            if ratio >= 0.8 {
                "full"
            } else if ratio >= 0.4 {
                "map"
            } else {
                "signatures"
            }
        } else {
            "signatures"
        };

        let _ =
            crate::tools::ctx_read::handle_with_task_resolved(cache, &full, mode, crp_mode, task);
        prefetched.push((full.clone(), mode.to_string()));
    }

    let mut lines = vec![
        format!(
            "ctx_prefetch: prefetched {} file(s) (max_files={})",
            prefetched.len(),
            max_files
        ),
        format!("  root: {}", project_root),
    ];
    for (p, mode) in prefetched.iter().take(20) {
        let r = cache.get_file_ref(p);
        let short = protocol::shorten_path(p);
        lines.push(format!("  - [{r}] {short} mode={mode}"));
    }
    lines.join("\n")
}

fn blast_radius(index: &ProjectIndex, start_rel: &str, max_depth: usize) -> Vec<(String, usize)> {
    let mut adj: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for e in &index.edges {
        adj.entry(e.from.as_str()).or_default().push(e.to.as_str());
        adj.entry(e.to.as_str()).or_default().push(e.from.as_str());
    }

    let mut out = Vec::new();
    let mut q: VecDeque<(String, usize)> = VecDeque::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();

    q.push_back((start_rel.to_string(), 0));
    seen.insert(start_rel.to_string());

    while let Some((node, depth)) = q.pop_front() {
        out.push((node.clone(), depth));
        if depth >= max_depth {
            continue;
        }
        if let Some(nbrs) = adj.get(node.as_str()) {
            for &n in nbrs {
                let ns = n.to_string();
                if seen.insert(ns.clone()) {
                    q.push_back((ns, depth + 1));
                }
            }
        }
    }
    out
}

fn normalize_rel_path(path: &str, project_root: &str) -> String {
    let p = Path::new(path);
    if p.is_absolute() {
        if let Ok(stripped) = p.strip_prefix(project_root) {
            return stripped
                .to_string_lossy()
                .trim_start_matches('/')
                .to_string();
        }
    }
    path.trim_start_matches('/').to_string()
}

fn to_fs_path(project_root: &str, rel_or_abs: &str) -> String {
    let p = Path::new(rel_or_abs);
    if p.is_absolute() {
        return rel_or_abs.to_string();
    }
    Path::new(project_root)
        .join(rel_or_abs)
        .to_string_lossy()
        .to_string()
}
