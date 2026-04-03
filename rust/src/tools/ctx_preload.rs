use crate::core::cache::SessionCache;
use crate::core::protocol;
use crate::core::task_relevance::{compute_relevance, parse_task_hints};
use crate::core::tokens::count_tokens;
use crate::tools::CrpMode;

const MAX_PRELOAD_FILES: usize = 8;
const MAX_CRITICAL_LINES: usize = 15;
const SIGNATURES_BUDGET: usize = 10;
const TOTAL_TOKEN_BUDGET: usize = 4000;

pub fn handle(
    cache: &mut SessionCache,
    task: &str,
    path: Option<&str>,
    crp_mode: CrpMode,
) -> String {
    if task.trim().is_empty() {
        return "ERROR: ctx_preload requires a task description".to_string();
    }

    let project_root = path
        .map(|p| p.to_string())
        .unwrap_or_else(|| ".".to_string());

    let index = crate::core::graph_index::load_or_build(&project_root);

    let (task_files, task_keywords) = parse_task_hints(task);
    let relevance = compute_relevance(&index, &task_files, &task_keywords);

    let candidates: Vec<_> = relevance
        .iter()
        .filter(|r| r.score >= 0.1)
        .take(MAX_PRELOAD_FILES + 10)
        .collect();

    if candidates.is_empty() {
        return format!(
            "[task: {task}]\nNo directly relevant files found. Use ctx_overview for project map."
        );
    }

    // Boltzmann allocation: p(file_i) = exp(score_i / T) / Z
    // Temperature T is derived from task specificity:
    //   - Many keywords / specific file mentions → low T → concentrate budget
    //   - Few keywords / broad task → high T → spread budget evenly
    let task_specificity =
        (task_files.len() as f64 * 0.3 + task_keywords.len() as f64 * 0.1).clamp(0.0, 1.0);
    let temperature = 0.8 - task_specificity * 0.6; // range [0.2, 0.8]
    let temperature = temperature.max(0.1);

    let allocations = boltzmann_allocate(&candidates, TOTAL_TOKEN_BUDGET, temperature);

    let file_context: Vec<(String, usize)> = candidates
        .iter()
        .filter_map(|c| {
            std::fs::read_to_string(&c.path)
                .ok()
                .map(|content| (c.path.clone(), content.lines().count()))
        })
        .collect();
    let briefing = crate::core::task_briefing::build_briefing(task, &file_context);
    let briefing_block = crate::core::task_briefing::format_briefing(&briefing);

    let mut output = Vec::new();
    output.push(briefing_block);
    output.push(format!("[task: {task}]"));

    let mut total_estimated_saved = 0usize;
    let mut critical_count = 0usize;

    for (rel, token_budget) in candidates.iter().zip(allocations.iter()) {
        if *token_budget < 20 {
            continue;
        }
        critical_count += 1;
        if critical_count > MAX_PRELOAD_FILES {
            break;
        }

        let content = match std::fs::read_to_string(&rel.path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let file_ref = cache.get_file_ref(&rel.path);
        let short = protocol::shorten_path(&rel.path);
        let line_count = content.lines().count();
        let file_tokens = count_tokens(&content);

        let (entry, _) = cache.store(&rel.path, content.clone());
        let _ = entry;

        let mode = budget_to_mode(*token_budget, file_tokens);

        let critical_lines = extract_critical_lines(&content, &task_keywords, MAX_CRITICAL_LINES);
        let sigs = extract_key_signatures(&content, SIGNATURES_BUDGET);
        let imports = extract_imports(&content);

        output.push(format!(
            "\nCRITICAL: {file_ref}={short} {line_count}L score={:.1} budget={token_budget}tok mode={mode}",
            rel.score
        ));

        if !critical_lines.is_empty() {
            for (line_no, line) in &critical_lines {
                output.push(format!("  :{line_no} {line}"));
            }
        }

        if !imports.is_empty() {
            output.push(format!("  imports: {}", imports.join(", ")));
        }

        if !sigs.is_empty() {
            for sig in &sigs {
                output.push(format!("  {sig}"));
            }
        }

        total_estimated_saved += file_tokens;
    }

    let context_files: Vec<_> = relevance
        .iter()
        .filter(|r| r.score >= 0.1 && r.score < 0.3)
        .take(10)
        .collect();

    if !context_files.is_empty() {
        output.push("\nRELATED:".to_string());
        for rel in &context_files {
            let short = protocol::shorten_path(&rel.path);
            output.push(format!(
                "  {} mode={} score={:.1}",
                short, rel.recommended_mode, rel.score
            ));
        }
    }

    let graph_edges: Vec<_> = index
        .edges
        .iter()
        .filter(|e| {
            candidates
                .iter()
                .any(|c| c.path == e.from || c.path == e.to)
        })
        .take(10)
        .collect();

    if !graph_edges.is_empty() {
        output.push("\nGRAPH:".to_string());
        for edge in &graph_edges {
            let from_short = protocol::shorten_path(&edge.from);
            let to_short = protocol::shorten_path(&edge.to);
            output.push(format!("  {from_short} -> {to_short}"));
        }
    }

    let preload_result = output.join("\n");
    let preload_tokens = count_tokens(&preload_result);
    let savings = protocol::format_savings(total_estimated_saved, preload_tokens);

    if crp_mode.is_tdd() {
        format!("{preload_result}\n{savings}")
    } else {
        format!("{preload_result}\n\nNext: ctx_read(path, mode=\"full\") for any file above.\n{savings}")
    }
}

/// Boltzmann distribution for token budget allocation across files.
/// p(file_i) = exp(score_i / T) / Z, then budget_i = total * p(file_i)
fn boltzmann_allocate(
    candidates: &[&crate::core::task_relevance::RelevanceScore],
    total_budget: usize,
    temperature: f64,
) -> Vec<usize> {
    if candidates.is_empty() {
        return Vec::new();
    }

    let t = temperature.max(0.01);

    // Compute exp(score / T) for each candidate, using log-sum-exp for numerical stability
    let log_weights: Vec<f64> = candidates.iter().map(|c| c.score / t).collect();
    let max_log = log_weights
        .iter()
        .cloned()
        .fold(f64::NEG_INFINITY, f64::max);
    let exp_weights: Vec<f64> = log_weights.iter().map(|&lw| (lw - max_log).exp()).collect();
    let z: f64 = exp_weights.iter().sum();

    if z <= 0.0 {
        return vec![total_budget / candidates.len().max(1); candidates.len()];
    }

    let mut allocations: Vec<usize> = exp_weights
        .iter()
        .map(|&w| ((w / z) * total_budget as f64).round() as usize)
        .collect();

    // Ensure total doesn't exceed budget
    let sum: usize = allocations.iter().sum();
    if sum > total_budget {
        let overflow = sum - total_budget;
        if let Some(last) = allocations.last_mut() {
            *last = last.saturating_sub(overflow);
        }
    }

    allocations
}

/// Map a token budget to a recommended compression mode.
fn budget_to_mode(budget: usize, file_tokens: usize) -> &'static str {
    let ratio = budget as f64 / file_tokens.max(1) as f64;
    if ratio >= 0.8 {
        "full"
    } else if ratio >= 0.4 {
        "signatures"
    } else if ratio >= 0.15 {
        "map"
    } else {
        "reference"
    }
}

fn extract_critical_lines(content: &str, keywords: &[String], max: usize) -> Vec<(usize, String)> {
    let kw_lower: Vec<String> = keywords.iter().map(|k| k.to_lowercase()).collect();

    let mut hits: Vec<(usize, String, usize)> = content
        .lines()
        .enumerate()
        .filter_map(|(i, line)| {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                return None;
            }
            let line_lower = trimmed.to_lowercase();
            let hit_count = kw_lower
                .iter()
                .filter(|kw| line_lower.contains(kw.as_str()))
                .count();

            let is_error = trimmed.contains("Error")
                || trimmed.contains("Err(")
                || trimmed.contains("panic!")
                || trimmed.contains("unwrap()")
                || trimmed.starts_with("return Err");

            if hit_count > 0 || is_error {
                let priority = hit_count + if is_error { 2 } else { 0 };
                Some((i + 1, trimmed.to_string(), priority))
            } else {
                None
            }
        })
        .collect();

    hits.sort_by(|a, b| b.2.cmp(&a.2));
    hits.truncate(max);
    hits.iter().map(|(n, l, _)| (*n, l.clone())).collect()
}

fn extract_key_signatures(content: &str, max: usize) -> Vec<String> {
    let sig_starters = [
        "pub fn ",
        "pub async fn ",
        "pub struct ",
        "pub enum ",
        "pub trait ",
        "pub type ",
        "pub const ",
    ];

    content
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            sig_starters.iter().any(|s| trimmed.starts_with(s))
        })
        .take(max)
        .map(|line| {
            let trimmed = line.trim();
            if trimmed.len() > 120 {
                format!("{}...", &trimmed[..117])
            } else {
                trimmed.to_string()
            }
        })
        .collect()
}

fn extract_imports(content: &str) -> Vec<String> {
    content
        .lines()
        .filter(|line| {
            let t = line.trim();
            t.starts_with("use ") || t.starts_with("import ") || t.starts_with("from ")
        })
        .take(8)
        .map(|line| {
            let t = line.trim();
            if let Some(rest) = t.strip_prefix("use ") {
                rest.trim_end_matches(';').to_string()
            } else {
                t.to_string()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_critical_lines_finds_keywords() {
        let content = "fn main() {\n    let token = validate();\n    return Err(e);\n}\n";
        let result = extract_critical_lines(content, &["validate".to_string()], 5);
        assert!(!result.is_empty());
        assert!(result.iter().any(|(_, l)| l.contains("validate")));
    }

    #[test]
    fn extract_critical_lines_prioritizes_errors() {
        let content = "fn main() {\n    let x = 1;\n    return Err(\"bad\");\n    let token = validate();\n}\n";
        let result = extract_critical_lines(content, &["validate".to_string()], 5);
        assert!(result.len() >= 2);
        assert!(result[0].1.contains("Err"), "errors should be first");
    }

    #[test]
    fn extract_key_signatures_finds_pub() {
        let content = "use std::io;\nfn private() {}\npub fn public_one() {}\npub struct Foo {}\n";
        let sigs = extract_key_signatures(content, 10);
        assert_eq!(sigs.len(), 2);
        assert!(sigs[0].contains("pub fn public_one"));
        assert!(sigs[1].contains("pub struct Foo"));
    }

    #[test]
    fn extract_imports_works() {
        let content = "use std::io;\nuse crate::core::cache;\nfn main() {}\n";
        let imports = extract_imports(content);
        assert_eq!(imports.len(), 2);
        assert!(imports[0].contains("std::io"));
    }
}
