use crate::core::cache::SessionCache;
use crate::core::task_relevance::{compute_relevance, parse_task_hints};
use crate::core::tokens::count_tokens;
use crate::tools::CrpMode;

/// Multi-resolution context overview.
///
/// Provides a compact map of the entire project, organized by task relevance.
/// Files are shown at different detail levels based on their relevance score:
/// - Level 0 (full): directly task-relevant files → full content (use ctx_read)
/// - Level 1 (signatures): graph neighbors → key signatures
/// - Level 2 (reference): distant files → name + line count only
///
/// This implements lazy evaluation for context: start with the overview,
/// then zoom into specific files as needed.
pub fn handle(
    cache: &SessionCache,
    task: Option<&str>,
    path: Option<&str>,
    _crp_mode: CrpMode,
) -> String {
    let project_root = path
        .map(|p| p.to_string())
        .unwrap_or_else(|| ".".to_string());

    let index = crate::core::graph_index::load_or_build(&project_root);

    let (task_files, task_keywords) = if let Some(task_desc) = task {
        parse_task_hints(task_desc)
    } else {
        (vec![], vec![])
    };

    let has_task = !task_files.is_empty() || !task_keywords.is_empty();

    let mut output = Vec::new();

    if has_task {
        let relevance = compute_relevance(&index, &task_files, &task_keywords);

        if let Some(task_desc) = task {
            let file_context: Vec<(String, usize)> = relevance
                .iter()
                .filter(|r| r.score >= 0.3)
                .take(8)
                .filter_map(|r| {
                    std::fs::read_to_string(&r.path)
                        .ok()
                        .map(|c| (r.path.clone(), c.lines().count()))
                })
                .collect();
            let briefing = crate::core::task_briefing::build_briefing(task_desc, &file_context);
            output.push(crate::core::task_briefing::format_briefing(&briefing));
        }

        let high: Vec<&_> = relevance.iter().filter(|r| r.score >= 0.8).collect();
        let medium: Vec<&_> = relevance
            .iter()
            .filter(|r| r.score >= 0.3 && r.score < 0.8)
            .collect();
        let low: Vec<&_> = relevance.iter().filter(|r| r.score < 0.3).collect();

        output.push(format!(
            "PROJECT OVERVIEW  {} files  task-filtered",
            index.files.len()
        ));
        output.push(String::new());

        if !high.is_empty() {
            output.push("▸ DIRECTLY RELEVANT (use ctx_read full):".to_string());
            for r in &high {
                let line_count = file_line_count(&r.path);
                let ref_id = cache.get_file_ref_readonly(&r.path);
                let ref_str = ref_id.map_or(String::new(), |r| format!("{r}="));
                output.push(format!(
                    "  {ref_str}{} {line_count}L  score={:.1}",
                    short_path(&r.path),
                    r.score
                ));
            }
            output.push(String::new());
        }

        if !medium.is_empty() {
            output.push("▸ CONTEXT (use ctx_read signatures/map):".to_string());
            for r in medium.iter().take(20) {
                let line_count = file_line_count(&r.path);
                output.push(format!(
                    "  {} {line_count}L  mode={}",
                    short_path(&r.path),
                    r.recommended_mode
                ));
            }
            if medium.len() > 20 {
                output.push(format!("  ... +{} more", medium.len() - 20));
            }
            output.push(String::new());
        }

        if !low.is_empty() {
            output.push(format!(
                "▸ DISTANT ({} files, not loaded unless needed)",
                low.len()
            ));
            for r in low.iter().take(10) {
                output.push(format!("  {}", short_path(&r.path)));
            }
            if low.len() > 10 {
                output.push(format!("  ... +{} more", low.len() - 10));
            }
        }
    } else {
        // No task context: show project structure overview
        output.push(format!(
            "PROJECT OVERVIEW  {} files  {} edges",
            index.files.len(),
            index.edges.len()
        ));
        output.push(String::new());

        // Group by directory
        let mut by_dir: std::collections::BTreeMap<String, Vec<String>> =
            std::collections::BTreeMap::new();

        for file_entry in index.files.values() {
            let dir = std::path::Path::new(&file_entry.path)
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| ".".to_string());
            by_dir
                .entry(dir)
                .or_default()
                .push(short_path(&file_entry.path));
        }

        for (dir, files) in &by_dir {
            let dir_display = if dir.len() > 50 {
                format!("...{}", &dir[dir.len() - 47..])
            } else {
                dir.clone()
            };

            if files.len() <= 5 {
                output.push(format!("{dir_display}/  {}", files.join(" ")));
            } else {
                output.push(format!(
                    "{dir_display}/  {} +{} more",
                    files[..3].join(" "),
                    files.len() - 3
                ));
            }
        }

        // Show top connected files (hub files)
        output.push(String::new());
        let mut connection_counts: std::collections::HashMap<&str, usize> =
            std::collections::HashMap::new();
        for edge in &index.edges {
            *connection_counts.entry(&edge.from).or_insert(0) += 1;
            *connection_counts.entry(&edge.to).or_insert(0) += 1;
        }
        let mut hubs: Vec<(&&str, &usize)> = connection_counts.iter().collect();
        hubs.sort_by(|a, b| b.1.cmp(a.1));

        if !hubs.is_empty() {
            output.push("HUB FILES (most connected):".to_string());
            for (path, count) in hubs.iter().take(8) {
                output.push(format!("  {} ({count} edges)", short_path(path)));
            }
        }
    }

    let original = count_tokens(&format!("{} files", index.files.len())) * index.files.len();
    let compressed = count_tokens(&output.join("\n"));
    output.push(String::new());
    output.push(crate::core::protocol::format_savings(original, compressed));

    output.join("\n")
}

fn short_path(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() <= 2 {
        return path.to_string();
    }
    parts[parts.len() - 2..].join("/")
}

fn file_line_count(path: &str) -> usize {
    std::fs::read_to_string(path)
        .map(|c| c.lines().count())
        .unwrap_or(0)
}
