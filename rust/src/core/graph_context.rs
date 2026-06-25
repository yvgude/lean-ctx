//! Graph-driven context loading — automatically includes related files
//! based on Property Graph proximity and token budgeting.
//!
//! Used by `ctx_read` (task mode) to surface a small, budgeted set of
//! related files (deterministic ordering; no output spam).

use std::collections::{HashMap, HashSet};

use super::graph_provider::{self, GraphProviderSource};
use super::tokens::count_tokens;

#[derive(Debug)]
pub struct GraphContext {
    pub source: GraphProviderSource,
    pub primary_file: String,
    pub related_files: Vec<RelatedFile>,
    pub total_tokens: usize,
    pub budget_remaining: usize,
}

#[derive(Debug)]
pub struct RelatedFile {
    pub path: String,
    pub relationship: Relationship,
    pub token_count: usize,
}

#[derive(Debug, Clone)]
pub enum Relationship {
    DirectDependency,
    DirectDependent,
    TransitiveDependency,
    TypeProvider,
}

impl Relationship {
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Relationship::DirectDependency => "imports",
            Relationship::DirectDependent => "imported-by",
            Relationship::TransitiveDependency => "transitive-dep",
            Relationship::TypeProvider => "type-provider",
        }
    }

    fn priority(&self) -> usize {
        match self {
            Relationship::DirectDependency => 0,
            Relationship::TypeProvider => 1,
            Relationship::DirectDependent => 2,
            Relationship::TransitiveDependency => 3,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct GraphContextOptions {
    pub token_budget: usize,
    pub max_files: usize,
    pub max_edges: usize,
    pub max_depth: usize,
    pub allow_build: bool,
}

impl Default for GraphContextOptions {
    fn default() -> Self {
        Self {
            token_budget: crate::core::budgets::GRAPH_CONTEXT_TOKEN_BUDGET,
            max_files: crate::core::budgets::GRAPH_CONTEXT_MAX_FILES,
            max_edges: crate::core::budgets::GRAPH_CONTEXT_MAX_EDGES,
            max_depth: crate::core::budgets::GRAPH_CONTEXT_MAX_DEPTH,
            allow_build: false,
        }
    }
}

#[must_use]
pub fn build_graph_context(
    file_path: &str,
    project_root: &str,
    options: Option<GraphContextOptions>,
) -> Option<GraphContext> {
    let opts = options.unwrap_or_default();

    let rel_path = file_path
        .strip_prefix(project_root)
        .unwrap_or(file_path)
        .trim_start_matches('/');

    let provider_open = if opts.allow_build {
        graph_provider::open_or_build(project_root)
    } else {
        graph_provider::open_best_effort(project_root)
    }?;

    let primary_content = std::fs::read_to_string(file_path).ok()?;
    let primary_tokens = count_tokens(&primary_content);

    let remaining = opts.token_budget.saturating_sub(primary_tokens);
    if remaining < 200 {
        return Some(GraphContext {
            source: provider_open.source,
            primary_file: rel_path.to_string(),
            related_files: Vec::new(),
            total_tokens: primary_tokens,
            budget_remaining: 0,
        });
    }

    let mut candidates = collect_candidates(&provider_open, rel_path, opts.max_depth);
    candidates.sort_by(|a, b| {
        a.relationship
            .priority()
            .cmp(&b.relationship.priority())
            .then_with(|| a.path.cmp(&b.path))
    });
    if candidates.len() > opts.max_edges {
        candidates.truncate(opts.max_edges);
    }

    let mut related: Vec<RelatedFile> = Vec::new();
    let mut tokens_used = primary_tokens;
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    seen.insert(rel_path.to_string());

    for candidate in candidates {
        if related.len() >= opts.max_files {
            break;
        }
        if seen.contains(&candidate.path) {
            continue;
        }

        let abs_path = format!("{project_root}/{}", candidate.path);
        if let Ok(content) = std::fs::read_to_string(&abs_path) {
            let tokens = count_tokens(&content);
            if tokens_used + tokens > opts.token_budget {
                continue;
            }
            tokens_used += tokens;
            seen.insert(candidate.path.clone());
            related.push(RelatedFile {
                path: candidate.path,
                relationship: candidate.relationship,
                token_count: tokens,
            });
        }
    }

    Some(GraphContext {
        source: provider_open.source,
        primary_file: rel_path.to_string(),
        related_files: related,
        total_tokens: tokens_used,
        budget_remaining: opts.token_budget.saturating_sub(tokens_used),
    })
}

struct Candidate {
    path: String,
    relationship: Relationship,
}

fn classify_dep(file: &str) -> Relationship {
    if file.ends_with(".d.ts") {
        Relationship::TypeProvider
    } else {
        Relationship::DirectDependency
    }
}

fn collect_candidates(
    open: &graph_provider::OpenGraphProvider,
    file_path: &str,
    max_depth: usize,
) -> Vec<Candidate> {
    let mut candidates: Vec<Candidate> = Vec::new();

    for dep in open.provider.dependencies(file_path) {
        let rel = classify_dep(&dep);
        candidates.push(Candidate {
            path: dep,
            relationship: rel,
        });
    }

    for dep in open.provider.dependents(file_path) {
        candidates.push(Candidate {
            path: dep,
            relationship: Relationship::DirectDependent,
        });
    }

    for affected in open.provider.related(file_path, max_depth.max(1)) {
        let already = candidates.iter().any(|c| c.path == affected);
        if !already {
            candidates.push(Candidate {
                path: affected,
                relationship: Relationship::TransitiveDependency,
            });
        }
    }

    candidates
}

fn related_files_scored_for_path(
    file_path: &str,
    project_root: &str,
    limit: usize,
) -> Option<Vec<(String, f64)>> {
    let provider = graph_provider::open_best_effort(project_root)?;
    let rel_path = file_path
        .strip_prefix(project_root)
        .unwrap_or(file_path)
        .trim_start_matches('/');
    let scored = provider.provider.related_files_scored(rel_path, limit);
    if scored.is_empty() {
        return None;
    }
    Some(scored)
}

/// Comma-separated repo-relative paths for dependency-cluster hints (e.g. CCP XML).
#[must_use]
pub fn build_related_paths_csv(
    file_path: &str,
    project_root: &str,
    limit: usize,
) -> Option<String> {
    let scored = related_files_scored_for_path(file_path, project_root, limit)?;
    Some(
        scored
            .into_iter()
            .map(|(path, _)| path)
            .collect::<Vec<_>>()
            .join(","),
    )
}

/// Lightweight one-line hint of the top related files from the Property Graph.
/// Returns `None` if no graph is available or no neighbors found.
#[must_use]
pub fn build_related_hint(file_path: &str, project_root: &str, limit: usize) -> Option<String> {
    let scored = related_files_scored_for_path(file_path, project_root, limit)?;

    let entries: Vec<String> = scored
        .iter()
        .map(|(path, score)| {
            let short = path.rsplit('/').next().unwrap_or(path);
            if *score >= 0.9 {
                short.to_string()
            } else {
                format!("{short} ({:.0}%)", score * 100.0)
            }
        })
        .collect();

    Some(format!("[related: {}]", entries.join(", ")))
}

/// Repo-relative paths (same style as BM25 chunks / property graph) ranked for RRF graph proximity.
///
/// `recent_repo_paths` should be ordered **most recently touched first**. Neighbors from earlier
/// seeds are ranked before those from later seeds; within a seed, graph `related_files_scored` order is kept.
#[must_use]
pub fn graph_neighbor_ranks_for_recent_files(
    project_root: &str,
    recent_repo_paths: &[String],
    per_seed_limit: usize,
    max_ranked: usize,
) -> Option<HashMap<String, usize>> {
    // Two neighbour sources, blended per seed:
    //   1. Traversal (co-access) — files the agent actually opened *together*
    //      (behavioural, the strongest "what next" signal). Listed first so it
    //      gets the best RRF rank. Works even without a static graph (#289).
    //   2. Static graph — import/call/type proximity.
    let coaccess = if crate::core::cooccurrence::traversal_enabled() {
        Some(crate::core::cooccurrence::load(project_root))
    } else {
        None
    };
    let open = graph_provider::open_best_effort(project_root);
    if open.is_none() && coaccess.is_none() {
        return None;
    }

    let mut seen = HashSet::<String>::new();
    let mut ranked: Vec<String> = Vec::new();

    'seeds: for seed in recent_repo_paths {
        let rel_path = normalize_repo_rel_path(seed, project_root);
        if rel_path.is_empty() {
            continue;
        }

        if let Some(co) = &coaccess {
            for (path, _) in co.related(&rel_path, per_seed_limit) {
                if seen.insert(path.clone()) {
                    ranked.push(path);
                    if ranked.len() >= max_ranked {
                        break 'seeds;
                    }
                }
            }
        }

        if let Some(open) = &open {
            for (path, _) in open
                .provider
                .related_files_scored(&rel_path, per_seed_limit)
            {
                if seen.insert(path.clone()) {
                    ranked.push(path);
                    if ranked.len() >= max_ranked {
                        break 'seeds;
                    }
                }
            }
        }
    }

    if ranked.is_empty() {
        None
    } else {
        Some(
            ranked
                .into_iter()
                .enumerate()
                .map(|(i, p)| (p, i))
                .collect(),
        )
    }
}

fn normalize_repo_rel_path(path: &str, project_root: &str) -> String {
    let p = path.replace('\\', "/");
    let root = project_root.trim_end_matches('/').replace('\\', "/");
    let prefix = format!("{root}/");
    if let Some(rest) = p.strip_prefix(&prefix) {
        return rest.to_string();
    }
    p.trim_start_matches('/').to_string()
}

#[must_use]
pub fn format_graph_context(ctx: &GraphContext) -> String {
    if ctx.related_files.is_empty() {
        return String::new();
    }

    let source = match ctx.source {
        GraphProviderSource::PropertyGraph => "property_graph",
        GraphProviderSource::GraphIndex => "graph_index",
    };
    let mut result = format!(
        "\n--- GRAPH CONTEXT (source={source}, {} related files, {} tok) ---\n",
        ctx.related_files.len(),
        ctx.total_tokens
    );

    for rf in &ctx.related_files {
        result.push_str(&format!(
            "  {} [{}] ({} tok)\n",
            rf.path,
            rf.relationship.label(),
            rf.token_count
        ));
    }

    result.push_str("--- END GRAPH CONTEXT ---");
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relationship_priorities() {
        assert!(
            Relationship::DirectDependency.priority() < Relationship::DirectDependent.priority()
        );
        assert!(
            Relationship::DirectDependent.priority()
                < Relationship::TransitiveDependency.priority()
        );
    }

    #[test]
    fn relationship_labels() {
        assert_eq!(Relationship::DirectDependency.label(), "imports");
        assert_eq!(Relationship::DirectDependent.label(), "imported-by");
        assert_eq!(Relationship::TransitiveDependency.label(), "transitive-dep");
        assert_eq!(Relationship::TypeProvider.label(), "type-provider");
    }

    #[test]
    fn format_empty_context() {
        let ctx = GraphContext {
            source: GraphProviderSource::GraphIndex,
            primary_file: "main.rs".to_string(),
            related_files: vec![],
            total_tokens: 100,
            budget_remaining: 7900,
        };
        assert!(format_graph_context(&ctx).is_empty());
    }

    #[test]
    fn format_with_related() {
        let ctx = GraphContext {
            source: GraphProviderSource::GraphIndex,
            primary_file: "main.rs".to_string(),
            related_files: vec![
                RelatedFile {
                    path: "lib.rs".to_string(),
                    relationship: Relationship::DirectDependency,
                    token_count: 500,
                },
                RelatedFile {
                    path: "utils.rs".to_string(),
                    relationship: Relationship::DirectDependent,
                    token_count: 300,
                },
            ],
            total_tokens: 900,
            budget_remaining: 7100,
        };
        let output = format_graph_context(&ctx);
        assert!(output.contains("2 related files"));
        assert!(output.contains("lib.rs [imports]"));
        assert!(output.contains("utils.rs [imported-by]"));
    }

    #[test]
    fn nonexistent_root_returns_none() {
        let result = build_graph_context("/nonexistent/file.rs", "/nonexistent", None);
        assert!(result.is_none());
    }
}
