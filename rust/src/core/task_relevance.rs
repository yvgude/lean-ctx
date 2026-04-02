use std::collections::{HashMap, HashSet, VecDeque};

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
    let mut scores: HashMap<String, f64> = HashMap::new();

    // Seed: task files get score 1.0
    for f in task_files {
        scores.insert(f.clone(), 1.0);
    }

    // BFS from task files through import graph, decaying by distance
    let adj = build_adjacency(index);
    for seed in task_files {
        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<(String, usize)> = VecDeque::new();
        queue.push_back((seed.clone(), 0));
        visited.insert(seed.clone());

        while let Some((node, depth)) = queue.pop_front() {
            if depth > 4 {
                continue;
            }
            let decay = 1.0 / (1.0 + depth as f64).powi(2); // quadratic decay
            let entry = scores.entry(node.clone()).or_insert(0.0);
            *entry = entry.max(decay);

            if let Some(neighbors) = adj.get(&node) {
                for neighbor in neighbors {
                    if !visited.contains(neighbor) {
                        visited.insert(neighbor.clone());
                        queue.push_back((neighbor.clone(), depth + 1));
                    }
                }
            }
        }
    }

    // Keyword boost: files containing task keywords get a relevance boost
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

fn build_adjacency(index: &ProjectIndex) -> HashMap<String, Vec<String>> {
    let mut adj: HashMap<String, Vec<String>> = HashMap::new();
    for edge in &index.edges {
        adj.entry(edge.from.clone())
            .or_default()
            .push(edge.to.clone());
        adj.entry(edge.to.clone())
            .or_default()
            .push(edge.from.clone());
    }
    adj
}

/// Extract likely task-relevant file paths and keywords from a task description.
pub fn parse_task_hints(task_description: &str) -> (Vec<String>, Vec<String>) {
    let mut files = Vec::new();
    let mut keywords = Vec::new();

    for word in task_description.split_whitespace() {
        let clean = word.trim_matches(|c: char| {
            !c.is_alphanumeric() && c != '.' && c != '/' && c != '_' && c != '-'
        });
        if clean.contains('.')
            && (clean.contains('/')
                || clean.ends_with(".rs")
                || clean.ends_with(".ts")
                || clean.ends_with(".py")
                || clean.ends_with(".go")
                || clean.ends_with(".js"))
        {
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

/// Information Bottleneck filter v2 — L-Curve aware, score-sorted output.
///
/// IB principle: maximize I(T;Y) (task relevance) while minimizing I(T;X) (input redundancy).
/// Each line is scored by: relevance_to_task * information_density * attention_weight.
///
/// v2 changes (based on Lab Experiments A-C):
///   - Uses empirical L-curve attention from attention_learned.rs instead of heuristic U-curve
///   - Output is sorted by score DESC (most important first), not by line number
///   - Error-handling lines get a priority boost (fragile under compression)
///   - Emits a one-line task summary as the first line when keywords are present
pub fn information_bottleneck_filter(
    content: &str,
    task_keywords: &[String],
    budget_ratio: f64,
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

    let mut scored_lines: Vec<(usize, &str, f64)> = lines
        .iter()
        .enumerate()
        .map(|(i, line)| {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                return (i, *line, 0.05);
            }

            let line_lower = trimmed.to_lowercase();
            let keyword_hits: f64 = kw_lower
                .iter()
                .filter(|kw| line_lower.contains(kw.as_str()))
                .count() as f64;

            let structural = if is_error_handling(trimmed) {
                1.5
            } else if is_definition_line(trimmed) {
                1.0
            } else if is_control_flow(trimmed) {
                0.5
            } else if is_closing_brace(trimmed) {
                0.15
            } else {
                0.3
            };
            let relevance = keyword_hits * 0.5 + structural;

            let line_tokens: Vec<&str> = trimmed.split_whitespace().collect();
            let unique_in_line = line_tokens.iter().collect::<HashSet<_>>().len() as f64;
            let line_token_count = line_tokens.len().max(1) as f64;
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

    let budget = ((n as f64) * budget_ratio).ceil() as usize;

    scored_lines.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));

    scored_lines.truncate(budget);

    let mut output_lines: Vec<&str> = Vec::with_capacity(budget + 1);

    if !kw_lower.is_empty() {
        output_lines.push(""); // placeholder for summary
    }

    for (_, line, _) in &scored_lines {
        output_lines.push(line);
    }

    if !kw_lower.is_empty() {
        let summary = format!("[task: {}]", task_keywords.join(", "));
        let mut result = summary;
        result.push('\n');
        result.push_str(
            &output_lines[1..].to_vec().join("\n"),
        );
        return result;
    }

    output_lines.join("\n")
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
