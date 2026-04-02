use crate::core::cache::SessionCache;
use crate::core::graph_index::ProjectIndex;
use crate::core::protocol;
use crate::core::task_relevance::{compute_relevance, parse_task_hints};
use crate::core::tokens::count_tokens;
use crate::tools::CrpMode;

const MAX_PRELOAD_FILES: usize = 5;
const MAX_CRITICAL_LINES: usize = 15;
const SIGNATURES_BUDGET: usize = 10;

pub fn handle(
    cache: &mut SessionCache,
    task: &str,
    path: Option<&str>,
    crp_mode: CrpMode,
) -> String {
    if task.trim().is_empty() {
        return "ERROR: ctx_preload requires a task description".to_string();
    }

    let project_root = path.map(|p| p.to_string()).unwrap_or_else(|| {
        std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".to_string())
    });

    let mut index = ProjectIndex::load(&project_root).unwrap_or_else(|| {
        let new_index = ProjectIndex::new(&project_root);
        let _ = new_index.save();
        new_index
    });
    if index.files.is_empty() {
        index = ProjectIndex::new(&project_root);
        let _ = index.save();
    }

    let (task_files, task_keywords) = parse_task_hints(task);
    let relevance = compute_relevance(&index, &task_files, &task_keywords);

    let critical: Vec<_> = relevance
        .iter()
        .filter(|r| r.score >= 0.5)
        .take(MAX_PRELOAD_FILES)
        .collect();

    if critical.is_empty() {
        return format!(
            "[task: {task}]\nNo directly relevant files found. Use ctx_overview for project map."
        );
    }

    let mut output = Vec::new();
    output.push(format!("[task: {task}]"));

    let mut total_estimated_saved = 0usize;

    for rel in &critical {
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

        let critical_lines = extract_critical_lines(&content, &task_keywords, MAX_CRITICAL_LINES);
        let sigs = extract_key_signatures(&content, SIGNATURES_BUDGET);
        let imports = extract_imports(&content);

        output.push(format!(
            "\nCRITICAL: {file_ref}={short} {line_count}L score={:.1}",
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
        .filter(|r| r.score >= 0.2 && r.score < 0.5)
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
        .filter(|e| critical.iter().any(|c| c.path == e.from || c.path == e.to))
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
