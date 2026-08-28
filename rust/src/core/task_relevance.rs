use std::collections::{HashMap, HashSet};

use super::graph_provider::{EdgeInfo, GraphProvider};

#[derive(Debug, Clone)]
pub struct RelevanceScore {
    pub path: String,
    pub score: f64,
    pub recommended_mode: &'static str,
}

pub fn compute_relevance(
    gp: &GraphProvider,
    task_files: &[String],
    task_keywords: &[String],
) -> Vec<RelevanceScore> {
    let all_edges = gp.edges();
    let file_set: HashSet<String> = gp.file_paths().into_iter().collect();
    let adj = build_adjacency_resolved(&all_edges, &file_set);
    let all_nodes: Vec<String> = file_set.into_iter().collect();
    if all_nodes.is_empty() {
        return Vec::new();
    }

    let node_idx: HashMap<&str, usize> = all_nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.as_str(), i))
        .collect();
    let n = all_nodes.len();

    // Build degree-normalized adjacency for heat diffusion
    let degrees: Vec<f64> = all_nodes
        .iter()
        .map(|node| {
            adj.get(node)
                .map_or(0.0, |neigh| neigh.len() as f64)
                .max(1.0)
        })
        .collect();

    // Seed vector: task files get 1.0
    let mut heat: Vec<f64> = vec![0.0; n];
    for f in task_files {
        if let Some(&idx) = node_idx.get(f.as_str()) {
            heat[idx] = 1.0;
        }
    }

    // Heat diffusion: h(t+1) = (1-alpha)*h(t) + alpha * A_norm * h(t)
    // Run for k iterations
    let alpha = 0.5;
    let iterations = 4;
    for _ in 0..iterations {
        let mut new_heat = vec![0.0; n];
        for (i, node) in all_nodes.iter().enumerate() {
            let self_term = (1.0 - alpha) * heat[i];
            let mut neighbor_sum = 0.0;
            if let Some(neighbors) = adj.get(node) {
                for neighbor in neighbors {
                    if let Some(&j) = node_idx.get(neighbor.as_str()) {
                        neighbor_sum += heat[j] / degrees[j];
                    }
                }
            }
            new_heat[i] = self_term + alpha * neighbor_sum;
        }
        heat = new_heat;
    }

    // PageRank centrality for gateway detection
    let mut pagerank = vec![1.0 / n as f64; n];
    let damping = 0.85;
    for _ in 0..8 {
        let mut new_pr = vec![(1.0 - damping) / n as f64; n];
        for (i, node) in all_nodes.iter().enumerate() {
            if let Some(neighbors) = adj.get(node) {
                let out_deg = neighbors.len().max(1) as f64;
                for neighbor in neighbors {
                    if let Some(&j) = node_idx.get(neighbor.as_str()) {
                        new_pr[j] += damping * pagerank[i] / out_deg;
                    }
                }
            }
        }
        pagerank = new_pr;
    }

    // Combine: heat (primary) + pagerank centrality (gateway bonus)
    let mut scores: HashMap<String, f64> = HashMap::new();
    let heat_max = heat.iter().copied().fold(0.0_f64, f64::max).max(1e-10);
    let pr_max = pagerank.iter().copied().fold(0.0_f64, f64::max).max(1e-10);

    for (i, node) in all_nodes.iter().enumerate() {
        let h = heat[i] / heat_max;
        let pr = pagerank[i] / pr_max;
        let combined = h * 0.8 + pr * 0.2;
        if combined > 0.01 {
            scores.insert(node.clone(), combined);
        }
    }

    if !task_keywords.is_empty() {
        let kw_lower: Vec<String> = task_keywords.iter().map(|k| k.to_lowercase()).collect();
        for file_path in &all_nodes {
            let path_lower = file_path.to_lowercase();
            let mut keyword_hits = 0;
            for kw in &kw_lower {
                if path_lower.contains(kw) {
                    keyword_hits += 1;
                }
                if let Some(entry) = gp.get_file_entry(file_path) {
                    for export in &entry.exports {
                        if export.to_lowercase().contains(kw) {
                            keyword_hits += 1;
                        }
                    }
                }
            }
            if keyword_hits > 0 {
                let boost = (keyword_hits as f64 * 0.15).min(0.6);
                let entry = scores.entry(file_path.clone()).or_insert(0.0);
                *entry = (*entry + boost).min(1.0);
            }
        }
    }

    let mut result: Vec<RelevanceScore> = scores
        .into_iter()
        .map(|(path, score)| {
            let mode = recommend_mode(score);
            RelevanceScore {
                path,
                score,
                recommended_mode: mode,
            }
        })
        .collect();

    result.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    result
}

pub fn compute_relevance_from_intent(
    gp: &GraphProvider,
    intent: &super::intent_engine::StructuredIntent,
) -> Vec<RelevanceScore> {
    use super::intent_engine::IntentScope;

    let mut file_seeds: Vec<String> = Vec::new();
    let mut extra_keywords: Vec<String> = intent.keywords.clone();

    let file_paths = gp.file_paths();
    for target in &intent.targets {
        if target.contains('.') || target.contains('/') {
            let matched = resolve_target_to_files(&file_paths, target);
            if matched.is_empty() {
                extra_keywords.push(target.clone());
            } else {
                file_seeds.extend(matched);
            }
        } else {
            let from_symbol = resolve_symbol_to_files(gp, target);
            if from_symbol.is_empty() {
                extra_keywords.push(target.clone());
            } else {
                file_seeds.extend(from_symbol);
            }
        }
    }

    if let Some(lang) = &intent.language_hint {
        let lang_ext = match lang.as_str() {
            "rust" => Some("rs"),
            "typescript" => Some("ts"),
            "javascript" => Some("js"),
            "python" => Some("py"),
            "go" => Some("go"),
            "ruby" => Some("rb"),
            "java" => Some("java"),
            _ => None,
        };
        if let Some(ext) = lang_ext
            && file_seeds.is_empty()
        {
            for path in &file_paths {
                if path.ends_with(&format!(".{ext}")) {
                    extra_keywords.push(
                        std::path::Path::new(path)
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("")
                            .to_string(),
                    );
                    break;
                }
            }
        }
    }

    let mut result = compute_relevance(gp, &file_seeds, &extra_keywords);

    match intent.scope {
        IntentScope::SingleFile => {
            result.truncate(5);
        }
        IntentScope::MultiFile => {
            result.truncate(15);
        }
        IntentScope::CrossModule | IntentScope::ProjectWide => {}
    }

    result
}

fn resolve_target_to_files(file_paths: &[String], target: &str) -> Vec<String> {
    file_paths
        .iter()
        .filter(|path| path.ends_with(target) || path.contains(target))
        .cloned()
        .collect()
}

fn resolve_symbol_to_files(gp: &GraphProvider, symbol: &str) -> Vec<String> {
    let found = gp.find_symbols(symbol, None, None);
    let mut matches: Vec<String> = found
        .into_iter()
        .map(|s| s.file)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    if matches.is_empty() {
        let sym_lower = symbol.to_lowercase();
        for path in gp.file_paths() {
            if let Some(entry) = gp.get_file_entry(&path)
                && entry
                    .exports
                    .iter()
                    .any(|e| e.to_lowercase().contains(&sym_lower))
                && !matches.contains(&path)
            {
                matches.push(path);
            }
        }
    }
    matches
}

fn recommend_mode(score: f64) -> &'static str {
    if score >= 0.8 {
        "full"
    } else if score >= 0.5 {
        "signatures"
    } else if score >= 0.2 {
        "map"
    } else {
        "reference"
    }
}

fn build_adjacency_resolved(
    edges: &[EdgeInfo],
    file_set: &HashSet<String>,
) -> HashMap<String, Vec<String>> {
    let file_paths_vec: Vec<&str> = file_set.iter().map(String::as_str).collect();
    let module_to_file = build_module_map(edges, file_set, &file_paths_vec);
    let mut adj: HashMap<String, Vec<String>> = HashMap::new();

    for edge in edges {
        let from = &edge.from;
        let to_resolved = module_to_file
            .get(&edge.to)
            .cloned()
            .unwrap_or_else(|| edge.to.clone());

        if file_set.contains(from) && file_set.contains(&to_resolved) {
            adj.entry(from.clone())
                .or_default()
                .push(to_resolved.clone());
            adj.entry(to_resolved).or_default().push(from.clone());
        }
    }
    adj
}

fn build_module_map(
    edges: &[EdgeInfo],
    file_set: &HashSet<String>,
    file_paths: &[&str],
) -> HashMap<String, String> {
    let mut mapping: HashMap<String, String> = HashMap::new();

    let edge_targets: HashSet<String> = edges.iter().map(|e| e.to.clone()).collect();

    for target in &edge_targets {
        if file_set.contains(target) {
            mapping.insert(target.clone(), target.clone());
            continue;
        }

        if let Some(resolved) = resolve_module_to_file(target, file_paths) {
            mapping.insert(target.clone(), resolved);
        }
    }

    mapping
}

fn resolve_module_to_file(module_path: &str, file_paths: &[&str]) -> Option<String> {
    let cleaned = module_path
        .trim_start_matches("crate::")
        .trim_start_matches("super::");

    // Strip trailing symbol (e.g. `core::tokens::count_tokens` → `core::tokens`)
    let parts: Vec<&str> = cleaned.split("::").collect();

    // Try progressively shorter prefixes to find a matching file
    for end in (1..=parts.len()).rev() {
        let candidate = parts[..end].join("/");

        // Try as .rs file
        for fp in file_paths {
            let fp_normalized = fp
                .trim_start_matches("rust/src/")
                .trim_start_matches("src/");

            if fp_normalized == format!("{candidate}.rs")
                || fp_normalized == format!("{candidate}/mod.rs")
                || fp.ends_with(&format!("/{candidate}.rs"))
                || fp.ends_with(&format!("/{candidate}/mod.rs"))
            {
                return Some(fp.to_string());
            }
        }
    }

    // Fallback: match by last segment as filename stem
    if let Some(last) = parts.last() {
        let stem = format!("{last}.rs");
        for fp in file_paths {
            if fp.ends_with(&stem) {
                return Some(fp.to_string());
            }
        }
    }

    None
}

/// Extract likely task-relevant file paths and keywords from a task description.
pub fn parse_task_hints(task_description: &str) -> (Vec<String>, Vec<String>) {
    let mut files = Vec::new();
    let mut keywords = Vec::new();

    for word in task_description.split_whitespace() {
        let clean = word.trim_matches(|c: char| {
            !c.is_alphanumeric() && c != '.' && c != '/' && c != '_' && c != '-'
        });
        if clean.contains('.') && {
            let p = std::path::Path::new(clean);
            clean.contains('/')
                || p.extension().is_some_and(|e| {
                    e.eq_ignore_ascii_case("rs")
                        || e.eq_ignore_ascii_case("ts")
                        || e.eq_ignore_ascii_case("py")
                        || e.eq_ignore_ascii_case("go")
                        || e.eq_ignore_ascii_case("js")
                })
        } {
            files.push(clean.to_string());
        } else if clean.len() >= 3 && !STOP_WORDS.contains(&clean.to_lowercase().as_str()) {
            keywords.push(clean.to_string());
        }
    }

    (files, keywords)
}

const STOP_WORDS: &[&str] = &[
    "the", "and", "for", "that", "this", "with", "from", "have", "has", "was", "are", "been",
    "not", "but", "all", "can", "had", "her", "one", "our", "out", "you", "its", "will", "each",
    "make", "like", "fix", "add", "use", "get", "set", "run", "new", "old", "should", "would",
    "could", "into", "also", "than", "them", "then", "when", "just", "only", "very", "some",
    "more", "other", "nach", "und", "die", "der", "das", "ist", "ein", "eine", "nicht", "auf",
    "mit",
];

struct StructuralWeights {
    error_handling: f64,
    definition: f64,
    control_flow: f64,
    closing_brace: f64,
    other: f64,
}

impl StructuralWeights {
    const DEFAULT: Self = Self {
        error_handling: 1.5,
        definition: 1.0,
        control_flow: 0.5,
        closing_brace: 0.15,
        other: 0.3,
    };

    fn for_task_type(task_type: Option<super::intent_engine::TaskType>) -> Self {
        use super::intent_engine::TaskType;
        match task_type {
            Some(TaskType::FixBug) => Self {
                error_handling: 2.0,
                definition: 0.8,
                control_flow: 0.8,
                closing_brace: 0.1,
                other: 0.2,
            },
            Some(TaskType::Debug) => Self {
                error_handling: 2.0,
                definition: 0.6,
                control_flow: 1.0,
                closing_brace: 0.1,
                other: 0.2,
            },
            Some(TaskType::Generate) => Self {
                error_handling: 0.8,
                definition: 1.5,
                control_flow: 0.3,
                closing_brace: 0.15,
                other: 0.4,
            },
            Some(TaskType::Refactor) => Self {
                error_handling: 1.0,
                definition: 1.5,
                control_flow: 0.6,
                closing_brace: 0.2,
                other: 0.3,
            },
            Some(TaskType::Test) => Self {
                error_handling: 1.2,
                definition: 1.3,
                control_flow: 0.4,
                closing_brace: 0.15,
                other: 0.3,
            },
            Some(TaskType::Review) => Self {
                error_handling: 1.3,
                definition: 1.2,
                control_flow: 0.6,
                closing_brace: 0.15,
                other: 0.3,
            },
            None | Some(TaskType::Explore | _) => Self::DEFAULT,
        }
    }
}

/// Information Bottleneck filter v3 — Mutual Information scoring, QUITO-X inspired.
///
/// IB principle: maximize I(T;Y) (task relevance) while minimizing I(T;X) (input redundancy).
/// v3: MI(line, task) approximated via token overlap + IDF weighting + structural importance.
///
/// Key changes from v2:
///   - Mutual Information scoring: MI(line, task) = H(line) - H(line|task)
///   - Adaptive budget allocation based on task type via TaskClassifier
///   - Token-level IDF computed over full document for better term weighting
///   - Maintains L-curve attention, MMR dedup, error-handling priority from v2
pub fn information_bottleneck_filter(
    content: &str,
    task_keywords: &[String],
    budget_ratio: f64,
    force_keep: &[String],
) -> String {
    information_bottleneck_filter_typed(content, task_keywords, budget_ratio, None, force_keep)
}

/// #840: IB filter with optional markdown section-header preservation.
/// When `preserve_section_headers` is true, the nearest preceding `#`-header
/// for each selected line is injected into the output so fragments remain
/// attributable to their enclosing sections.
pub fn information_bottleneck_filter_with_headers(
    content: &str,
    task_keywords: &[String],
    budget_ratio: f64,
    force_keep: &[String],
    preserve_section_headers: bool,
) -> String {
    if !preserve_section_headers {
        return information_bottleneck_filter(content, task_keywords, budget_ratio, force_keep);
    }
    let filtered = information_bottleneck_filter(content, task_keywords, budget_ratio, force_keep);
    inject_section_headers(content, &filtered)
}

/// For each line in `filtered` that appears in a markdown-sectioned `original`,
/// find and inject the nearest preceding `#`-header line if not already present.
fn inject_section_headers(original: &str, filtered: &str) -> String {
    let orig_lines: Vec<&str> = original.lines().collect();
    let filt_lines: Vec<&str> = filtered.lines().collect();

    let header_indices: Vec<usize> = orig_lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.starts_with('#'))
        .map(|(i, _)| i)
        .collect();

    if header_indices.is_empty() {
        return filtered.to_string();
    }

    let mut result_lines: Vec<&str> = Vec::with_capacity(filt_lines.len() + header_indices.len());
    let mut injected_headers: std::collections::HashSet<usize> = std::collections::HashSet::new();

    for filt_line in &filt_lines {
        let trimmed = filt_line.trim();
        if trimmed.is_empty() || trimmed.starts_with("[task:") {
            result_lines.push(filt_line);
            continue;
        }
        if let Some(orig_idx) = orig_lines.iter().position(|l| *l == *filt_line)
            && let Some(&h) = header_indices.iter().rev().find(|&&h| h <= orig_idx)
            && !injected_headers.contains(&h)
            && !filt_lines.contains(&orig_lines[h])
        {
            injected_headers.insert(h);
            result_lines.push(orig_lines[h]);
        }
        result_lines.push(filt_line);
    }

    result_lines.join("\n")
}

/// Task-type-aware IB filter. Uses `TaskType` to adjust structural weights.
/// `force_keep` lines (explicit `protect` tokens, #709) are kept verbatim on top
/// of the budget; `&[]` reproduces the pre-protect output byte-for-byte (#498).
pub fn information_bottleneck_filter_typed(
    content: &str,
    task_keywords: &[String],
    budget_ratio: f64,
    task_type: Option<super::intent_engine::TaskType>,
    force_keep: &[String],
) -> String {
    let selected = ib_select(content, task_keywords, budget_ratio, task_type, force_keep);
    if selected.is_empty() {
        return String::new();
    }
    let body: Vec<&str> = selected.iter().map(|(_, line)| *line).collect();
    let body = body.join("\n");
    if task_keywords.is_empty() {
        body
    } else {
        format!("[task: {}]\n{body}", task_keywords.join(", "))
    }
}

/// Ranked line selection behind every task-mode view: scores each line, applies
/// the MMR redundancy penalty, and hands back the winners **in source order**
/// as `(line_index, line)` pairs.
///
/// Two properties matter to callers and were both missing before (#1589):
///
/// * **Blank lines are never candidates.** A blank line scored 0.05 — above the
///   tail of a real ranking — and then bypassed the MMR similarity penalty
///   entirely, because an empty token set has no overlap to punish. Once the
///   ranked candidates fell below that floor, blanks won every remaining slot;
///   a field report saw ~35 consecutive blank lines where the body should have
///   been.
/// * **The result is ordered by line number, not by score.** A task view that
///   reorders a file's fragments is not a compressed file, it is a different
///   file — it reads as though the code executes in that order.
pub fn ib_select<'a>(
    content: &'a str,
    task_keywords: &[String],
    budget_ratio: f64,
    task_type: Option<super::intent_engine::TaskType>,
    force_keep: &[String],
) -> Vec<(usize, &'a str)> {
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        return Vec::new();
    }

    let n = lines.len();
    let kw_lower: Vec<String> = task_keywords.iter().map(|k| k.to_lowercase()).collect();

    let mut global_token_freq: HashMap<&str, usize> = HashMap::new();
    for line in &lines {
        for token in line.split_whitespace() {
            *global_token_freq.entry(token).or_insert(0) += 1;
        }
    }
    let total_unique = global_token_freq.len().max(1) as f64;
    let total_lines = n.max(1) as f64;

    let task_token_set: HashSet<String> = kw_lower
        .iter()
        .flat_map(|kw| kw.split(|c: char| !c.is_alphanumeric()).map(String::from))
        .filter(|t| t.len() >= 2)
        .collect();

    let effective_ratio = if task_token_set.is_empty() {
        budget_ratio
    } else {
        adaptive_ib_budget(content, budget_ratio)
    };

    let weights = StructuralWeights::for_task_type(task_type);

    let mut scored_lines: Vec<(usize, &str, f64)> = lines
        .iter()
        .enumerate()
        .filter_map(|(i, line)| {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                // #1589: a blank line carries no information to select *for*.
                // Only an explicit protect token can pull one into the view.
                return super::protect::line_is_protected(line, force_keep).then_some((
                    i,
                    *line,
                    f64::INFINITY,
                ));
            }

            let line_lower = trimmed.to_lowercase();
            let line_tokens: Vec<&str> = trimmed.split_whitespace().collect();
            let line_token_count = line_tokens.len().max(1) as f64;

            let mi_score = if task_token_set.is_empty() {
                0.0
            } else {
                let line_token_set: HashSet<String> =
                    line_tokens.iter().map(|t| t.to_lowercase()).collect();
                let overlap: f64 = line_token_set
                    .iter()
                    .filter(|t| task_token_set.iter().any(|kw| t.contains(kw.as_str())))
                    .map(|t| {
                        let freq = *global_token_freq.get(t.as_str()).unwrap_or(&1) as f64;
                        (total_lines / freq).ln().max(0.1)
                    })
                    .sum();
                overlap / line_token_count
            };

            let keyword_hits: f64 = kw_lower
                .iter()
                .filter(|kw| line_lower.contains(kw.as_str()))
                .count() as f64;

            let structural = if is_error_handling(trimmed) {
                weights.error_handling
            } else if is_definition_line(trimmed) {
                weights.definition
            } else if is_control_flow(trimmed) {
                weights.control_flow
            } else if is_closing_brace(trimmed) {
                weights.closing_brace
            } else {
                weights.other
            };
            let relevance = mi_score * 0.4 + keyword_hits * 0.3 + structural;

            let unique_in_line = line_tokens.iter().collect::<HashSet<_>>().len() as f64;
            let token_diversity = unique_in_line / line_token_count;

            let avg_idf: f64 = if line_tokens.is_empty() {
                0.0
            } else {
                line_tokens
                    .iter()
                    .map(|t| {
                        let freq = *global_token_freq.get(t).unwrap_or(&1) as f64;
                        (total_unique / freq).ln().max(0.0)
                    })
                    .sum::<f64>()
                    / line_token_count
            };
            let information = (token_diversity * 0.4 + (avg_idf.min(3.0) / 3.0) * 0.6).min(1.0);

            let pos = i as f64 / n.max(1) as f64;
            let attn_weight = 1.0 - (pos - 0.5).abs() * 0.5;

            let score = (relevance * 0.6 + 0.05)
                * (information * 0.25 + 0.05)
                * (attn_weight * 0.15 + 0.05);

            // Explicit protect tokens (#709) force the line to the top of the
            // ranking; INF survives the MMR lambda penalty so it is always kept.
            let score = if super::protect::line_is_protected(line, force_keep) {
                f64::INFINITY
            } else {
                score
            };

            Some((i, *line, score))
        })
        .collect();

    // Protected lines (#709) are kept on top of the ranked budget, so widen the
    // budget to hold them without displacing other selected content. With an
    // empty `force_keep` this adds zero and the budget is byte-identical (#498).
    let protected_count = lines
        .iter()
        .filter(|l| super::protect::line_is_protected(l, force_keep))
        .count();
    let budget = (((n as f64) * effective_ratio).ceil() as usize) + protected_count;

    scored_lines.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));

    let mut selected = mmr_select(&scored_lines, budget, 0.3);
    // Rank decided *what* survives; the file decides in which order it is read.
    selected.sort_by_key(|(i, _, _)| *i);
    selected.into_iter().map(|(i, line, _)| (i, line)).collect()
}

/// Maximum Marginal Relevance selection — greedy selection that penalizes
/// redundancy with already-selected lines using token-set Jaccard similarity.
///
/// MMR(i) = relevance(i) - lambda * max_{j in S} jaccard(i, j)
fn mmr_select<'a>(
    candidates: &[(usize, &'a str, f64)],
    budget: usize,
    lambda: f64,
) -> Vec<(usize, &'a str, f64)> {
    if candidates.is_empty() || budget == 0 {
        return Vec::new();
    }

    let mut selected: Vec<(usize, &'a str, f64)> = Vec::with_capacity(budget);
    let mut remaining: Vec<(usize, &'a str, f64)> = candidates.to_vec();

    // Always take the top-scored line first
    selected.push(remaining.remove(0));

    while selected.len() < budget && !remaining.is_empty() {
        let mut best_idx = 0;
        let mut best_mmr = f64::NEG_INFINITY;

        for (i, &(_, cand_line, cand_score)) in remaining.iter().enumerate() {
            let cand_tokens: HashSet<&str> = cand_line.split_whitespace().collect();
            if cand_tokens.is_empty() {
                if cand_score > best_mmr {
                    best_mmr = cand_score;
                    best_idx = i;
                }
                continue;
            }

            let max_sim = selected
                .iter()
                .map(|&(_, sel_line, _)| {
                    let sel_tokens: HashSet<&str> = sel_line.split_whitespace().collect();
                    if sel_tokens.is_empty() {
                        return 0.0;
                    }
                    let inter = cand_tokens.intersection(&sel_tokens).count();
                    let union = cand_tokens.union(&sel_tokens).count();
                    if union == 0 {
                        0.0
                    } else {
                        inter as f64 / union as f64
                    }
                })
                .fold(0.0_f64, f64::max);

            let mmr = cand_score - lambda * max_sim;
            if mmr > best_mmr {
                best_mmr = mmr;
                best_idx = i;
            }
        }

        selected.push(remaining.remove(best_idx));
    }

    selected
}

fn is_error_handling(line: &str) -> bool {
    line.starts_with("return Err(")
        || line.starts_with("Err(")
        || line.starts_with("bail!(")
        || line.starts_with("anyhow::bail!")
        || line.contains(".map_err(")
        || line.contains("unwrap()")
        || line.contains("expect(\"")
        || line.starts_with("raise ")
        || line.starts_with("throw ")
        || line.starts_with("catch ")
        || line.starts_with("except ")
        || line.starts_with("try ")
        || (line.contains("?;") && !line.starts_with("//"))
        || line.starts_with("panic!(")
        || line.contains("Error::")
        || line.contains("error!")
}

/// Compute an adaptive IB budget ratio based on content characteristics.
/// Highly repetitive content → more aggressive filtering (lower ratio).
/// High-entropy diverse content → more conservative (higher ratio).
pub fn adaptive_ib_budget(content: &str, base_ratio: f64) -> f64 {
    let lines: Vec<&str> = content.lines().collect();
    if lines.len() < 10 {
        return 1.0;
    }

    let mut token_freq: HashMap<&str, usize> = HashMap::new();
    let mut total_tokens = 0usize;
    for line in &lines {
        for token in line.split_whitespace() {
            *token_freq.entry(token).or_insert(0) += 1;
            total_tokens += 1;
        }
    }

    if total_tokens == 0 {
        return base_ratio;
    }

    let unique_ratio = token_freq.len() as f64 / total_tokens as f64;
    let repetition_factor = 1.0 - unique_ratio;

    (base_ratio * (1.0 - repetition_factor * 0.3)).clamp(0.2, 1.0)
}

fn is_definition_line(line: &str) -> bool {
    let prefixes = [
        "fn ",
        "pub fn ",
        "async fn ",
        "pub async fn ",
        "struct ",
        "pub struct ",
        "enum ",
        "pub enum ",
        "trait ",
        "pub trait ",
        "impl ",
        "type ",
        "pub type ",
        "const ",
        "pub const ",
        "static ",
        "pub static ",
        "class ",
        "export class ",
        "interface ",
        "export interface ",
        "function ",
        "export function ",
        "async function ",
        "def ",
        "async def ",
        "func ",
    ];
    prefixes
        .iter()
        .any(|p| line.starts_with(p) || line.trim_start().starts_with(p))
}

fn is_control_flow(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with("if ")
        || trimmed.starts_with("else ")
        || trimmed.starts_with("match ")
        || trimmed.starts_with("for ")
        || trimmed.starts_with("while ")
        || trimmed.starts_with("return ")
        || trimmed.starts_with("break")
        || trimmed.starts_with("continue")
        || trimmed.starts_with("yield")
        || trimmed.starts_with("await ")
}

fn is_closing_brace(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed == "}" || trimmed == "};" || trimmed == "})" || trimmed == "});"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_task_finds_files_and_keywords() {
        let (files, keywords) =
            parse_task_hints("Fix the authentication bug in src/auth.rs and update tests");
        assert!(files.iter().any(|f| f.contains("auth.rs")));
        assert!(
            keywords
                .iter()
                .any(|k| k.to_lowercase().contains("authentication"))
        );
    }

    #[test]
    fn recommend_mode_by_score() {
        assert_eq!(recommend_mode(1.0), "full");
        assert_eq!(recommend_mode(0.6), "signatures");
        assert_eq!(recommend_mode(0.3), "map");
        assert_eq!(recommend_mode(0.1), "reference");
    }

    #[test]
    fn info_bottleneck_preserves_definitions() {
        let content = "fn main() {\n    let x = 42;\n    // boring comment\n    println!(x);\n}\n";
        let result = information_bottleneck_filter(content, &["main".to_string()], 0.6, &[]);
        assert!(result.contains("fn main"), "definitions must be preserved");
        assert!(result.contains("[task: main]"), "should have task summary");
    }

    #[test]
    fn protect_force_keeps_line_in_ib() {
        let content = "fn main() {\n    let x = 1;\n    let unimportant = 2;\n    let y = 3;\n    let z = 4;\n}\n";
        // Tiny budget → the 'unimportant' line is normally filtered out.
        let kept = information_bottleneck_filter(
            content,
            &["main".to_string()],
            0.1,
            &["unimportant".to_string()],
        );
        assert!(
            kept.contains("let unimportant = 2;"),
            "protected line must survive the IB budget: {kept}"
        );
    }

    #[test]
    fn ib_empty_force_keep_is_byte_identical() {
        // Protect must not change the unprotected IB output (#498).
        let content = "fn main() {\n    let x = 1;\n    return Err(\"e\");\n    let y = 2;\n}\n";
        let a = information_bottleneck_filter(content, &["main".to_string()], 0.5, &[]);
        let b = information_bottleneck_filter_typed(content, &["main".to_string()], 0.5, None, &[]);
        assert_eq!(a, b);
    }

    #[test]
    fn info_bottleneck_error_handling_priority() {
        let content = "fn validate() {\n    let data = parse()?;\n    return Err(\"invalid\");\n    let x = 1;\n    let y = 2;\n}\n";
        let result = information_bottleneck_filter(content, &["validate".to_string()], 0.5, &[]);
        assert!(
            result.contains("return Err"),
            "error handling should survive filtering"
        );
    }

    #[test]
    fn info_bottleneck_preserves_source_order() {
        // #1589: selection is ranked, output is not. A task view that reorders
        // fragments reads as if the code ran in that order.
        let content = "fn important() {\n    let x = 1;\n    let y = 2;\n    let z = 3;\n}\n}\n";
        let result = information_bottleneck_filter(content, &[], 0.6, &[]);
        let lines: Vec<&str> = result.lines().collect();
        let def_pos = lines.iter().position(|l| l.contains("fn important"));
        let brace_pos = lines.iter().position(|l| l.trim() == "}");
        if let (Some(d), Some(b)) = (def_pos, brace_pos) {
            assert!(d < b, "the definition precedes its closing brace in source");
        }

        let numbered = "alpha_one\nbeta_two\ngamma_three\ndelta_four\nepsilon_five\nzeta_six\n";
        let selected = ib_select(numbered, &["gamma".to_string()], 0.6, None, &[]);
        let indices: Vec<usize> = selected.iter().map(|(i, _)| *i).collect();
        let mut sorted = indices.clone();
        sorted.sort_unstable();
        assert_eq!(
            indices, sorted,
            "selection must be returned in source order"
        );
    }

    #[test]
    fn info_bottleneck_never_selects_blank_lines() {
        // #1589: blanks scored 0.05 and skipped the MMR similarity penalty, so
        // they out-competed real content once the ranking tail fell below that
        // floor — a field report saw ~35 consecutive blank lines.
        let mut content = String::new();
        for i in 0..40 {
            content.push_str(&format!("fn handler_{i}(input: &str) -> usize {{\n"));
            content.push_str("\n\n\n");
            content.push_str("    input.len()\n}\n\n\n");
        }
        let result = information_bottleneck_filter(&content, &["handler".to_string()], 0.3, &[]);
        for line in result.lines().skip(1) {
            assert!(
                !line.trim().is_empty(),
                "no blank line may consume a slot in the task budget"
            );
        }
        assert!(result.contains("handler_"), "real content must survive");
    }

    #[test]
    fn protected_blank_line_still_survives() {
        // The blank-line ban is a ranking rule, not a censor: an explicit
        // protect token still pulls its line through (#709).
        let content = "fn a() {}\n   \nfn b() {}\n";
        let kept = ib_select(content, &["a".to_string()], 0.1, None, &["   ".to_string()]);
        assert!(
            kept.iter().any(|(_, l)| l.trim().is_empty()),
            "an explicitly protected line is kept even when blank"
        );
    }

    #[test]
    fn adaptive_budget_reduces_for_repetitive() {
        let repetitive = "let x = 1;\n".repeat(50);
        let diverse = (0..50)
            .map(|i| format!("let var_{i} = func_{i}(arg_{i});"))
            .collect::<Vec<_>>()
            .join("\n");
        let budget_rep = super::adaptive_ib_budget(&repetitive, 0.7);
        let budget_div = super::adaptive_ib_budget(&diverse, 0.7);
        assert!(
            budget_rep < budget_div,
            "repetitive content should get lower budget"
        );
    }
}
