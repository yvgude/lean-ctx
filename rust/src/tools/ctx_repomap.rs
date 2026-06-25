//! Tool handler for `ctx_repomap` — PageRank-based repo map.

use crate::core::repomap;

/// Handle a repo map request.
///
/// - `project_root`: project root path
/// - `max_tokens`: token budget (default 2048)
/// - `focus_files`: optional list of files to boost in ranking
/// - `session_files`: files from the active session (injected by MCP wrapper)
#[must_use]
pub fn handle(
    project_root: &str,
    max_tokens: usize,
    focus_files: &[String],
    session_files: &[String],
) -> String {
    let graph = repomap::RepoGraph::build(project_root);

    if graph.files.is_empty() {
        return format!(
            "No indexable files found in '{project_root}'. \
             Ensure it contains source files (.rs, .ts, .py, etc.)."
        );
    }

    let ranked = repomap::rank_symbols(&graph, session_files, focus_files);

    if ranked.is_empty() {
        return format!(
            "No symbols extracted from {} files in '{project_root}'.",
            graph.files.len()
        );
    }

    repomap::fit_to_budget(&ranked, max_tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonexistent_root_returns_helpful_message() {
        let result = handle("/tmp/nonexistent_repo_map_test_dir", 2048, &[], &[]);
        assert!(
            result.contains("No indexable files") || result.contains("No symbols"),
            "unexpected: {result}"
        );
    }
}
