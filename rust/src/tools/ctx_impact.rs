//! `ctx_impact` — Graph-based impact analysis tool.
//!
//! Uses the SQLite-backed Property Graph to answer: "What breaks when file X changes?"
//! Performs BFS traversal of reverse import edges to find all transitively affected files.

use crate::core::property_graph::{CodeGraph, DependencyChain, Edge, EdgeKind, ImpactResult, Node};
use crate::core::tokens::count_tokens;
use std::path::Path;

pub fn handle(action: &str, path: Option<&str>, root: &str, depth: Option<usize>) -> String {
    match action {
        "analyze" => handle_analyze(path, root, depth.unwrap_or(5)),
        "chain" => handle_chain(path, root),
        "build" => handle_build(root),
        "status" => handle_status(root),
        _ => "Unknown action. Use: analyze, chain, build, status".to_string(),
    }
}

fn open_graph(root: &str) -> Result<CodeGraph, String> {
    CodeGraph::open(Path::new(root)).map_err(|e| format!("Failed to open graph: {e}"))
}

fn handle_analyze(path: Option<&str>, root: &str, max_depth: usize) -> String {
    let target = match path {
        Some(p) => p,
        None => return "path is required for 'analyze' action".to_string(),
    };

    let graph = match open_graph(root) {
        Ok(g) => g,
        Err(e) => return e,
    };

    let canon_root = std::fs::canonicalize(root)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| root.to_string());
    let canon_target = std::fs::canonicalize(target)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| target.to_string());
    let root_slash = if canon_root.ends_with('/') {
        canon_root.clone()
    } else {
        format!("{canon_root}/")
    };
    let rel_target = canon_target
        .strip_prefix(&root_slash)
        .or_else(|| canon_target.strip_prefix(&canon_root))
        .unwrap_or(&canon_target)
        .trim_start_matches('/');

    let node_count = graph.node_count().unwrap_or(0);
    if node_count == 0 {
        drop(graph);
        let build_result = handle_build(root);
        tracing::info!(
            "Auto-built graph for impact analysis: {}",
            &build_result[..build_result.len().min(100)]
        );
        let graph = match open_graph(root) {
            Ok(g) => g,
            Err(e) => return e,
        };
        if graph.node_count().unwrap_or(0) == 0 {
            return "Graph is empty after auto-build. No supported source files found.".to_string();
        }
        let impact = match graph.impact_analysis(rel_target, max_depth) {
            Ok(r) => r,
            Err(e) => return format!("Impact analysis failed: {e}"),
        };
        return format_impact(&impact, rel_target);
    }

    let impact = match graph.impact_analysis(rel_target, max_depth) {
        Ok(r) => r,
        Err(e) => return format!("Impact analysis failed: {e}"),
    };

    format_impact(&impact, rel_target)
}

fn format_impact(impact: &ImpactResult, target: &str) -> String {
    if impact.affected_files.is_empty() {
        let result = format!("No files depend on {target} (leaf node in the dependency graph).");
        let tokens = count_tokens(&result);
        return format!("{result}\n[ctx_impact: {tokens} tok]");
    }

    let mut result = format!(
        "Impact of changing {target}: {} affected files (depth: {}, edges traversed: {})\n",
        impact.affected_files.len(),
        impact.max_depth_reached,
        impact.edges_traversed
    );

    let mut sorted = impact.affected_files.clone();
    sorted.sort();

    for file in &sorted {
        result.push_str(&format!("  {file}\n"));
    }

    let tokens = count_tokens(&result);
    format!("{result}[ctx_impact: {tokens} tok]")
}

fn handle_chain(path: Option<&str>, root: &str) -> String {
    let spec = match path {
        Some(p) => p,
        None => {
            return "path is required for 'chain' action (format: from_file->to_file)".to_string()
        }
    };

    let (from, to) = match spec.split_once("->") {
        Some((f, t)) => (f.trim(), t.trim()),
        None => {
            return format!(
                "Invalid chain spec '{spec}'. Use format: from_file->to_file\n\
                 Example: src/server.rs->src/core/config.rs"
            )
        }
    };

    let graph = match open_graph(root) {
        Ok(g) => g,
        Err(e) => return e,
    };

    let canon_root = std::fs::canonicalize(root)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| root.to_string());
    let root_slash = if canon_root.ends_with('/') {
        canon_root.clone()
    } else {
        format!("{canon_root}/")
    };
    let canon_from = std::fs::canonicalize(from)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| from.to_string());
    let canon_to = std::fs::canonicalize(to)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| to.to_string());
    let rel_from = canon_from
        .strip_prefix(&root_slash)
        .or_else(|| canon_from.strip_prefix(&canon_root))
        .unwrap_or(&canon_from)
        .trim_start_matches('/');
    let rel_to = canon_to
        .strip_prefix(&root_slash)
        .or_else(|| canon_to.strip_prefix(&canon_root))
        .unwrap_or(&canon_to)
        .trim_start_matches('/');

    match graph.dependency_chain(rel_from, rel_to) {
        Ok(Some(chain)) => format_chain(&chain),
        Ok(None) => {
            let result = format!("No dependency path from {rel_from} to {rel_to}");
            let tokens = count_tokens(&result);
            format!("{result}\n[ctx_impact chain: {tokens} tok]")
        }
        Err(e) => format!("Chain analysis failed: {e}"),
    }
}

fn format_chain(chain: &DependencyChain) -> String {
    let mut result = format!("Dependency chain (depth {}):\n", chain.depth);
    for (i, step) in chain.path.iter().enumerate() {
        if i > 0 {
            result.push_str("  -> ");
        } else {
            result.push_str("  ");
        }
        result.push_str(step);
        result.push('\n');
    }
    let tokens = count_tokens(&result);
    format!("{result}[ctx_impact chain: {tokens} tok]")
}

fn handle_build(root: &str) -> String {
    let graph = match open_graph(root) {
        Ok(g) => g,
        Err(e) => return e,
    };

    if let Err(e) = graph.clear() {
        return format!("Failed to clear graph: {e}");
    }

    let root_path = Path::new(root);
    let walker = ignore::WalkBuilder::new(root_path)
        .hidden(true)
        .git_ignore(true)
        .build();

    let supported_exts = ["rs", "ts", "tsx", "js", "jsx", "py", "go", "java"];
    let mut file_paths: Vec<String> = Vec::new();
    let mut file_contents: Vec<(String, String, String)> = Vec::new();

    for entry in walker.flatten() {
        if !entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
            continue;
        }

        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

        if !supported_exts.contains(&ext) {
            continue;
        }

        let rel_path = path
            .strip_prefix(root_path)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();

        file_paths.push(rel_path.clone());

        if let Ok(content) = std::fs::read_to_string(path) {
            file_contents.push((rel_path, content, ext.to_string()));
        }
    }

    let resolver_ctx =
        crate::core::import_resolver::ResolverContext::new(root_path, file_paths.clone());

    let mut total_nodes = 0usize;
    let mut total_edges = 0usize;

    for (rel_path, content, ext) in &file_contents {
        let file_node_id = match graph.upsert_node(&Node::file(rel_path)) {
            Ok(id) => id,
            Err(_) => continue,
        };
        total_nodes += 1;

        #[cfg(feature = "embeddings")]
        {
            let analysis = crate::core::deep_queries::analyze(content, ext);

            for type_def in &analysis.types {
                let kind = crate::core::property_graph::NodeKind::Symbol;
                let sym_node = Node::symbol(&type_def.name, rel_path, kind)
                    .with_lines(type_def.line, type_def.end_line);
                if let Ok(sym_id) = graph.upsert_node(&sym_node) {
                    total_nodes += 1;
                    let _ = graph.upsert_edge(&Edge::new(file_node_id, sym_id, EdgeKind::Defines));
                    total_edges += 1;
                }
            }

            let resolved = crate::core::import_resolver::resolve_imports(
                &analysis.imports,
                rel_path,
                ext,
                &resolver_ctx,
            );

            for imp in &resolved {
                if imp.is_external {
                    continue;
                }
                if let Some(ref target_path) = imp.resolved_path {
                    let target_id = match graph.upsert_node(&Node::file(target_path)) {
                        Ok(id) => id,
                        Err(_) => continue,
                    };
                    let _ =
                        graph.upsert_edge(&Edge::new(file_node_id, target_id, EdgeKind::Imports));
                    total_edges += 1;
                }
            }
        }

        #[cfg(not(feature = "embeddings"))]
        {
            let _ = (&content, &ext, file_node_id);
        }
    }

    let result = format!(
        "Graph built: {total_nodes} nodes, {total_edges} edges from {} files\n\
         Stored at: {}",
        file_contents.len(),
        graph.db_path().display()
    );
    let tokens = count_tokens(&result);
    format!("{result}\n[ctx_impact build: {tokens} tok]")
}

fn handle_status(root: &str) -> String {
    let graph = match open_graph(root) {
        Ok(g) => g,
        Err(e) => return e,
    };

    let nodes = graph.node_count().unwrap_or(0);
    let edges = graph.edge_count().unwrap_or(0);

    if nodes == 0 {
        return "Graph is empty. Run ctx_impact action='build' to index.".to_string();
    }

    format!(
        "Property Graph: {nodes} nodes, {edges} edges\nStored: {}",
        graph.db_path().display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_impact_empty() {
        let impact = ImpactResult {
            root_file: "a.rs".to_string(),
            affected_files: vec![],
            max_depth_reached: 0,
            edges_traversed: 0,
        };
        let result = format_impact(&impact, "a.rs");
        assert!(result.contains("No files depend on"));
    }

    #[test]
    fn format_impact_with_files() {
        let impact = ImpactResult {
            root_file: "a.rs".to_string(),
            affected_files: vec!["b.rs".to_string(), "c.rs".to_string()],
            max_depth_reached: 2,
            edges_traversed: 3,
        };
        let result = format_impact(&impact, "a.rs");
        assert!(result.contains("2 affected files"));
        assert!(result.contains("b.rs"));
        assert!(result.contains("c.rs"));
    }

    #[test]
    fn format_chain_display() {
        let chain = DependencyChain {
            path: vec!["a.rs".to_string(), "b.rs".to_string(), "c.rs".to_string()],
            depth: 2,
        };
        let result = format_chain(&chain);
        assert!(result.contains("depth 2"));
        assert!(result.contains("a.rs"));
        assert!(result.contains("-> b.rs"));
        assert!(result.contains("-> c.rs"));
    }

    #[test]
    fn handle_missing_path() {
        let result = handle("analyze", None, "/tmp", None);
        assert!(result.contains("path is required"));
    }

    #[test]
    fn handle_invalid_chain_spec() {
        let result = handle("chain", Some("no_arrow_here"), "/tmp", None);
        assert!(result.contains("Invalid chain spec"));
    }

    #[test]
    fn handle_unknown_action() {
        let result = handle("invalid", None, "/tmp", None);
        assert!(result.contains("Unknown action"));
    }
}
