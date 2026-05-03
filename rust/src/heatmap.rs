use crate::core::graph_index::{self, ProjectIndex};
use std::collections::HashMap;

struct HeatEntry {
    path: String,
    token_count: usize,
    connections: usize,
    heat_score: f64,
}

pub fn cmd_heatmap(args: &[String]) {
    let project_root = std::env::current_dir()
        .ok()
        .and_then(|d| d.to_str().map(String::from))
        .unwrap_or_else(|| ".".to_string());

    let top_n: usize = args
        .iter()
        .find_map(|a| a.strip_prefix("--top="))
        .and_then(|v| v.parse().ok())
        .unwrap_or(20);

    let dir_filter: Option<&str> = args
        .iter()
        .find_map(|a| a.strip_prefix("--dir="))
        .map(|s| s.trim_end_matches('/'));

    let sort_by = if args.iter().any(|a| a == "--by=connections") {
        SortBy::Connections
    } else if args.iter().any(|a| a == "--by=tokens") {
        SortBy::Tokens
    } else {
        SortBy::Heat
    };

    let json_output = args.iter().any(|a| a == "--json");

    let index = graph_index::load_or_build(&project_root);

    let entries = build_heat_entries(&index, dir_filter);

    if entries.is_empty() {
        eprintln!("No files found in project graph.");
        eprintln!("  Run: lean-ctx setup  (to build the project graph)");
        return;
    }

    let mut sorted = entries;
    match sort_by {
        SortBy::Heat => sorted.sort_by(|a, b| {
            b.heat_score
                .partial_cmp(&a.heat_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        }),
        SortBy::Tokens => sorted.sort_by_key(|x| std::cmp::Reverse(x.token_count)),
        SortBy::Connections => sorted.sort_by_key(|x| std::cmp::Reverse(x.connections)),
    }

    let top = &sorted[..sorted.len().min(top_n)];

    if json_output {
        print_json(top);
    } else {
        print_heatmap(&project_root, top, &sorted);
    }
}

enum SortBy {
    Heat,
    Tokens,
    Connections,
}

fn build_heat_entries(index: &ProjectIndex, dir_filter: Option<&str>) -> Vec<HeatEntry> {
    let mut connection_counts: HashMap<String, usize> = HashMap::new();
    for edge in &index.edges {
        *connection_counts.entry(edge.from.clone()).or_default() += 1;
        *connection_counts.entry(edge.to.clone()).or_default() += 1;
    }

    let max_tokens = index
        .files
        .values()
        .map(|f| f.token_count)
        .max()
        .unwrap_or(1) as f64;
    let max_connections = connection_counts.values().max().copied().unwrap_or(1) as f64;

    index
        .files
        .values()
        .filter(|f| {
            if let Some(dir) = dir_filter {
                f.path.starts_with(dir) || f.path.starts_with(&format!("./{dir}"))
            } else {
                true
            }
        })
        .map(|f| {
            let connections = connection_counts.get(&f.path).copied().unwrap_or(0);
            let token_norm = f.token_count as f64 / max_tokens;
            let conn_norm = connections as f64 / max_connections;
            let heat_score = token_norm * 0.4 + conn_norm * 0.6;

            HeatEntry {
                path: f.path.clone(),
                token_count: f.token_count,
                connections,
                heat_score,
            }
        })
        .collect()
}

fn heat_color(score: f64) -> &'static str {
    if score > 0.8 {
        "\x1b[91m" // bright red
    } else if score > 0.6 {
        "\x1b[31m" // red
    } else if score > 0.4 {
        "\x1b[33m" // yellow
    } else if score > 0.2 {
        "\x1b[36m" // cyan
    } else {
        "\x1b[34m" // blue
    }
}

fn heat_bar(score: f64, width: usize) -> String {
    let filled = (score * width as f64).round() as usize;
    let blocks = "█".repeat(filled);
    let empty = "░".repeat(width.saturating_sub(filled));
    format!("{}{blocks}\x1b[38;5;239m{empty}\x1b[0m", heat_color(score))
}

fn print_heatmap(project_root: &str, entries: &[HeatEntry], all: &[HeatEntry]) {
    let total_files = all.len();
    let total_tokens: usize = all.iter().map(|e| e.token_count).sum();
    let total_connections: usize = all.iter().map(|e| e.connections).sum();

    let project_name = std::path::Path::new(project_root).file_name().map_or_else(
        || project_root.to_string(),
        |n| n.to_string_lossy().to_string(),
    );

    println!();
    println!("\x1b[1;37m  Context Heat Map\x1b[0m  \x1b[38;5;239m{project_name}\x1b[0m");
    println!(
        "\x1b[38;5;239m  {total_files} files · {total_tokens} tokens · {total_connections} connections\x1b[0m"
    );
    println!();

    let max_path_len = entries.iter().map(|e| e.path.len()).max().unwrap_or(30);
    let path_width = max_path_len.min(50);

    println!(
        "  \x1b[38;5;239m{:<width$}  {:>6}  {:>5}  HEAT\x1b[0m",
        "FILE",
        "TOKENS",
        "CONNS",
        width = path_width
    );
    println!("  \x1b[38;5;239m{}\x1b[0m", "─".repeat(path_width + 32));

    for entry in entries {
        let display_path = if entry.path.len() > path_width {
            let skip = entry.path.len() - path_width + 3;
            format!("...{}", &entry.path[skip..])
        } else {
            entry.path.clone()
        };

        let bar = heat_bar(entry.heat_score, 16);

        println!(
            "  {color}{:<width$}\x1b[0m  \x1b[38;5;245m{:>6}\x1b[0m  \x1b[38;5;245m{:>5}\x1b[0m  {bar}  {color}{:.0}%\x1b[0m",
            display_path,
            entry.token_count,
            entry.connections,
            entry.heat_score * 100.0,
            color = heat_color(entry.heat_score),
            width = path_width,
        );
    }

    println!();
    println!(
        "  \x1b[38;5;239mLegend: \x1b[91m█\x1b[38;5;239m hot  \x1b[33m█\x1b[38;5;239m warm  \x1b[36m█\x1b[38;5;239m cool  \x1b[34m█\x1b[38;5;239m cold\x1b[0m"
    );
    println!(
        "  \x1b[38;5;239mOptions: --top=N  --dir=path  --by=tokens|connections  --json\x1b[0m"
    );
    println!();
}

fn print_json(entries: &[HeatEntry]) {
    let items: Vec<serde_json::Value> = entries
        .iter()
        .map(|e| {
            serde_json::json!({
                "path": e.path,
                "token_count": e.token_count,
                "connections": e.connections,
                "heat_score": (e.heat_score * 100.0).round() / 100.0,
            })
        })
        .collect();

    println!(
        "{}",
        serde_json::to_string_pretty(&items).unwrap_or_else(|_| "[]".to_string())
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_heat_color_ranges() {
        assert_eq!(heat_color(0.9), "\x1b[91m");
        assert_eq!(heat_color(0.7), "\x1b[31m");
        assert_eq!(heat_color(0.5), "\x1b[33m");
        assert_eq!(heat_color(0.3), "\x1b[36m");
        assert_eq!(heat_color(0.1), "\x1b[34m");
    }

    #[test]
    fn test_heat_bar_length() {
        let bar = heat_bar(0.5, 10);
        assert!(bar.contains("█████"));
    }

    #[test]
    fn test_build_heat_entries_empty() {
        let index = ProjectIndex::new(".");
        let entries = build_heat_entries(&index, None);
        assert!(entries.is_empty());
    }
}
