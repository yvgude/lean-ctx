//! Graph-driven context loading — automatically includes related files
//! based on Property Graph proximity and token budgeting.
//!
//! Used by `ctx_read` (task mode) to surface a small, budgeted set of
//! related files (deterministic ordering; no output spam).

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
