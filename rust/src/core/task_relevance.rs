use std::collections::{HashMap, HashSet};

use super::graph_index::ProjectIndex;

use super::neural::attention_learned::LearnedAttention;

#[derive(Debug, Clone)]
pub struct RelevanceScore {
    pub path: String,
    pub score: f64,
    pub recommended_mode: &'static str,
}

pub fn compute_relevance(
    index: &ProjectIndex,
    task_files: &[String],
    task_keywords: &[String],
) -> Vec<RelevanceScore> {
    let adj = build_adjacency_resolved(index);
    let all_nodes: Vec<String> = index.files.keys().cloned().collect();
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

    // Keyword boost
    if !task_keywords.is_empty() {
        let kw_lower: Vec<String> = task_keywords.iter().map(|k| k.to_lowercase()).collect();
        for (file_path, file_entry) in &index.files {
            let path_lower = file_path.to_lowercase();
            let mut keyword_hits = 0;
            for kw in &kw_lower {
                if path_lower.contains(kw) {
                    keyword_hits += 1;
                }
                for export in &file_entry.exports {
                    if export.to_lowercase().contains(kw) {
                        keyword_hits += 1;
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
    index: &ProjectIndex,
    intent: &super::intent_engine::StructuredIntent,
) -> Vec<RelevanceScore> {
    use super::intent_engine::IntentScope;

    let mut file_seeds: Vec<String> = Vec::new();
    let mut extra_keywords: Vec<String> = intent.keywords.clone();

    for target in &intent.targets {
        if target.contains('.') || target.contains('/') {
            let matched = resolve_target_to_files(index, target);
            if matched.is_empty() {
                extra_keywords.push(target.clone());
            } else {
                file_seeds.extend(matched);
            }
        } else {
            let from_symbol = resolve_symbol_to_files(index, target);
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
        if let Some(ext) = lang_ext {
            if file_seeds.is_empty() {
                for path in index.files.keys() {
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
    }

    let mut result = compute_relevance(index, &file_seeds, &extra_keywords);

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

fn resolve_target_to_files(index: &ProjectIndex, target: &str) -> Vec<String> {
    let mut matches = Vec::new();
    for path in index.files.keys() {
        if path.ends_with(target) || path.contains(target) {
            matches.push(path.clone());
        }
    }
    matches
}

fn resolve_symbol_to_files(index: &ProjectIndex, symbol: &str) -> Vec<String> {
    let sym_lower = symbol.to_lowercase();
    let mut matches = Vec::new();
    for entry in index.symbols.values() {
        let name_lower = entry.name.to_lowercase();
        if (name_lower == sym_lower || name_lower.contains(&sym_lower))
            && !matches.contains(&entry.file)
        {
            matches.push(entry.file.clone());
        }
    }
    if matches.is_empty() {
        for (path, file_entry) in &index.files {
            if file_entry
                .exports
                .iter()
                .any(|e| e.to_lowercase().contains(&sym_lower))
                && !matches.contains(path)
            {
                matches.push(path.clone());
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

/// Build adjacency with module-path → file-path resolution.
/// Graph edges store file paths as `from` and Rust module paths as `to`
/// (e.g. `crate::core::tokens::count_tokens`). We resolve `to` back to file
/// paths so heat diffusion and PageRank can propagate across the graph.
fn build_adjacency_resolved(index: &ProjectIndex) -> HashMap<String, Vec<String>> {
    let module_to_file = build_module_map(index);
    let mut adj: HashMap<String, Vec<String>> = HashMap::new();

    for edge in &index.edges {
        let from = &edge.from;
        let to_resolved = module_to_file
            .get(&edge.to)
            .cloned()
            .unwrap_or_else(|| edge.to.clone());

        if index.files.contains_key(from) && index.files.contains_key(&to_resolved) {
            adj.entry(from.clone())
                .or_default()
                .push(to_resolved.clone());
            adj.entry(to_resolved).or_default().push(from.clone());
        }
    }
    adj
}

/// Map module/import paths to file paths using heuristics.
/// e.g. `crate::core::tokens::count_tokens` → `rust/src/core/tokens.rs`
fn build_module_map(index: &ProjectIndex) -> HashMap<String, String> {
    let file_paths: Vec<&str> = index
        .files
        .keys()
        .map(std::string::String::as_str)
        .collect();
    let mut mapping: HashMap<String, String> = HashMap::new();

    let edge_targets: HashSet<String> = index.edges.iter().map(|e| e.to.clone()).collect();

    for target in &edge_targets {
        if index.files.contains_key(target) {
            mapping.insert(target.clone(), target.clone());
            continue;
        }

        if let Some(resolved) = resolve_module_to_file(target, &file_paths) {
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
) -> String {
    information_bottleneck_filter_typed(content, task_keywords, budget_ratio, None)
}

/// Task-type-aware IB filter. Uses `TaskType` to adjust structural weights.
pub fn information_bottleneck_filter_typed(
    content: &str,
    task_keywords: &[String],
    budget_ratio: f64,
    task_type: Option<super::intent_engine::TaskType>,
) -> String {
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        return String::new();
    }

    let n = lines.len();
    let kw_lower: Vec<String> = task_keywords.iter().map(|k| k.to_lowercase()).collect();
    let attention = LearnedAttention::with_defaults();

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
        .map(|(i, line)| {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                return (i, *line, 0.05);
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
            let attn_weight = attention.weight(pos);

            let score = (relevance * 0.6 + 0.05)
                * (information * 0.25 + 0.05)
                * (attn_weight * 0.15 + 0.05);

            (i, *line, score)
        })
        .collect();

    let budget = ((n as f64) * effective_ratio).ceil() as usize;

    scored_lines.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));

    let selected = mmr_select(&scored_lines, budget, 0.3);

    let mut output_lines: Vec<&str> = Vec::with_capacity(budget + 1);

    if !kw_lower.is_empty() {
        output_lines.push("");
    }

    for (_, line, _) in &selected {
        output_lines.push(line);
    }

    if !kw_lower.is_empty() {
        let summary = format!("[task: {}]", task_keywords.join(", "));
        let mut result = summary;
        result.push('\n');
        result.push_str(&output_lines[1..].to_vec().join("\n"));
        return result;
    }

    output_lines.join("\n")
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
        assert!(keywords
            .iter()
            .any(|k| k.to_lowercase().contains("authentication")));
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
        let result = information_bottleneck_filter(content, &["main".to_string()], 0.6);
        assert!(result.contains("fn main"), "definitions must be preserved");
        assert!(result.contains("[task: main]"), "should have task summary");
    }

    #[test]
    fn info_bottleneck_error_handling_priority() {
        let content = "fn validate() {\n    let data = parse()?;\n    return Err(\"invalid\");\n    let x = 1;\n    let y = 2;\n}\n";
        let result = information_bottleneck_filter(content, &["validate".to_string()], 0.5);
        assert!(
            result.contains("return Err"),
            "error handling should survive filtering"
        );
    }

    #[test]
    fn info_bottleneck_score_sorted() {
        let content = "fn important() {\n    let x = 1;\n    let y = 2;\n    let z = 3;\n}\n}\n";
        let result = information_bottleneck_filter(content, &[], 0.6);
        let lines: Vec<&str> = result.lines().collect();
        let def_pos = lines.iter().position(|l| l.contains("fn important"));
        let brace_pos = lines.iter().position(|l| l.trim() == "}");
        if let (Some(d), Some(b)) = (def_pos, brace_pos) {
            assert!(
                d < b,
                "definitions should appear before closing braces in score-sorted output"
            );
        }
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
