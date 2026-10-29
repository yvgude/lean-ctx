//! `ctx_impact` — Graph-based impact analysis tool.
//!
//! Uses the SQLite-backed Property Graph to answer: "What breaks when file X changes?"
//! Performs BFS traversal of reverse import edges to find all transitively affected files.

use crate::core::property_graph::{CodeGraph, DependencyChain, ImpactResult};

/// Extensions whose files become Property Graph source nodes. Must stay a subset
/// of `language_capabilities::is_indexable_ext` and align with the deep-query
/// extractors (`deep_queries::{type_defs, calls}`) so each language contributes
/// real symbol/import/call structure rather than bare file nodes.
const GRAPH_SOURCE_EXTS: &[&str] = &[
    "rs", "ts", "tsx", "js", "jsx", "py", "go", "java", "gd", "cs", "kt", "kts",
];

fn open_graph(root: &str) -> Result<CodeGraph, String> {
    CodeGraph::open(root).map_err(|e| format!("Failed to open graph: {e}"))
}

// The graph rows this tool answers with, as they may be served now (G04).
// Each row is asked again (`CodeGraph::serves_file` / `serves_symbol`) before
// anything is listed or counted: a file that is no longer served is skipped
// as if it were not in the graph.

/// Whether `path` is a file node of the graph that is served now.
fn served_file_node(graph: &CodeGraph, path: &str) -> bool {
    graph.get_node_by_path(path).ok().flatten().is_some() && graph.serves_file(path)
}

/// `CodeGraph::impact_analysis` without the files that are not served now.
fn served_impact(graph: &CodeGraph, file: &str, max_depth: usize) -> anyhow::Result<ImpactResult> {
    let mut impact = graph.impact_analysis(file, max_depth)?;
    impact.affected_files.retain(|f| graph.serves_file(f));
    impact.weak_files.retain(|f| graph.serves_file(f));
    Ok(impact)
}

/// The files that define `symbol` and still serve its name.
fn served_def_files(graph: &CodeGraph, symbol: &str) -> anyhow::Result<Vec<String>> {
    let mut files = graph.resolve_symbol_def_files(symbol)?;
    files.retain(|f| graph.serves_symbol(f, symbol));
    Ok(files)
}

/// `CodeGraph::dependency_chain`, when every step of the path is served now;
/// a chain through a withdrawn file is no chain.
fn served_chain(
    graph: &CodeGraph,
    from: &str,
    to: &str,
) -> anyhow::Result<Option<DependencyChain>> {
    let chain = graph.dependency_chain(from, to)?;
    Ok(chain.filter(|chain| chain.path.iter().all(|step| graph.serves_file(step))))
}

mod analyze;
mod build;

pub use analyze::*;
#[allow(unused_imports)]
pub(crate) use build::*;
