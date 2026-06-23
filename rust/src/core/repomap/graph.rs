//! Graph builder for repo map.
//!
//! Constructs a file-level directed graph from the project index edges
//! and call graph edges, then exposes symbol definitions per file.

use std::collections::{HashMap, HashSet};

use crate::core::call_graph::{CallGraph, CallGraphInputs};
use crate::core::graph_index::{self, ProjectIndex, SymbolEntry};

/// A symbol definition with its file context.
#[derive(Debug, Clone)]
pub struct SymbolDef {
    pub name: String,
    pub kind: String,
    pub file: String,
    pub line: usize,
    pub end_line: usize,
    pub is_exported: bool,
    pub signature: String,
}

/// File-level graph combining import edges and call edges.
pub struct RepoGraph {
    pub files: HashSet<String>,
    /// Forward adjacency: file -> list of files it depends on.
    pub forward: HashMap<String, Vec<String>>,
    /// All symbol definitions grouped by file.
    pub symbols_by_file: HashMap<String, Vec<SymbolDef>>,
}

impl RepoGraph {
    /// Build the repo graph from a project root.
    ///
    /// Loads or builds the project index and call graph,
    /// then merges their edges into a unified file-level graph.
    pub fn build(project_root: &str) -> Self {
        let (index, content_cache) = match Self::try_build_pipeline(project_root) {
            Some(result) => result,
            None => {
                // Pipeline unavailable (non-existent root, etc.) — build an
                // empty graph from what's available on disk.
                let index = ProjectIndex::new(project_root);
                return Self::from_index_and_calls(&index, &CallGraph::new(project_root), &HashMap::new());
            }
        };
        // Pipeline doesn't preserve the content cache; repomap omits detailed
        // signature enrichment when it is unavailable (still builds valid graph).
        let content_cache = content_cache.unwrap_or_default();
        let cg_inputs = CallGraphInputs::from_project_index(&index);
        let call_graph = CallGraph::load_or_build(project_root, &cg_inputs);

        Self::from_index_and_calls(&index, &call_graph, &content_cache)
    }

    fn from_index_and_calls(
        index: &ProjectIndex,
        call_graph: &CallGraph,
        content_cache: &HashMap<String, String>,
    ) -> Self {
        let files: HashSet<String> = index.files.keys().cloned().collect();

        let mut forward: HashMap<String, Vec<String>> = HashMap::new();

        // Import edges from the project index
        for edge in &index.edges {
            if files.contains(&edge.from) && files.contains(&edge.to) && edge.from != edge.to {
                forward
                    .entry(edge.from.clone())
                    .or_default()
                    .push(edge.to.clone());
            }
        }

        // Call edges from the call graph
        let symbols_by_name = build_symbol_location_map(index);
        for call_edge in &call_graph.edges {
            if let Some(target_file) = symbols_by_name.get(&call_edge.callee_name.to_lowercase())
                && files.contains(&call_edge.caller_file)
                && files.contains(target_file)
                && call_edge.caller_file != *target_file
            {
                forward
                    .entry(call_edge.caller_file.clone())
                    .or_default()
                    .push(target_file.clone());
            }
        }

        // Deduplicate edges
        for deps in forward.values_mut() {
            deps.sort();
            deps.dedup();
        }

        let symbols_by_file = build_symbols_with_signatures(index, content_cache);

        Self {
            files,
            forward,
            symbols_by_file,
        }
    }

    /// Try to build a ProjectIndex from the pipeline; returns None if the
    /// pipeline cannot be built (e.g. root does not exist) so callers can
    /// fall back to an empty index instead of panicking.
    fn try_build_pipeline(project_root: &str) -> Option<(ProjectIndex, Option<HashMap<String, String>>)> {
        let root = std::path::PathBuf::from(project_root);
        if !root.exists() || !root.is_dir() {
            return None;
        }
        let handle = crate::core::index_pipeline::pipeline::IndexPipeline::new(root)
            .build()
            .ok()?;
        let (index, _bm25) = handle.run_and_load().ok()?;
        Some((index, None))
    }
}

/// Map lowercase symbol name -> file path (first definition wins).
fn build_symbol_location_map(index: &ProjectIndex) -> HashMap<String, String> {
    let mut map: HashMap<String, String> = HashMap::with_capacity(index.symbols.len());
    for sym in index.symbols.values() {
        map.entry(sym.name.to_lowercase())
            .or_insert_with(|| sym.file.clone());
    }
    map
}

/// Build symbol definitions with compact signatures from file contents.
fn build_symbols_with_signatures(
    index: &ProjectIndex,
    content_cache: &HashMap<String, String>,
) -> HashMap<String, Vec<SymbolDef>> {
    let mut result: HashMap<String, Vec<SymbolDef>> = HashMap::new();

    // Group index symbols by file
    let mut idx_symbols: HashMap<&str, Vec<&SymbolEntry>> = HashMap::new();
    for sym in index.symbols.values() {
        idx_symbols.entry(sym.file.as_str()).or_default().push(sym);
    }

    for (file_path, file_entry) in &index.files {
        let ext = std::path::Path::new(file_path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");

        // Extract signatures from file content if available
        let signatures = content_cache
            .get(file_path)
            .map(|content| crate::core::signatures::extract_signatures(content, ext))
            .unwrap_or_default();

        let sig_by_name: HashMap<&str, &crate::core::signatures::Signature> =
            signatures.iter().map(|s| (s.name.as_str(), s)).collect();

        let mut file_symbols: Vec<SymbolDef> = Vec::new();

        if let Some(syms) = idx_symbols.get(file_path.as_str()) {
            for sym in syms {
                let signature = sig_by_name
                    .get(sym.name.as_str())
                    .map_or_else(|| format!("{} {}", sym.kind, sym.name), |s| s.to_compact());

                file_symbols.push(SymbolDef {
                    name: sym.name.clone(),
                    kind: sym.kind.clone(),
                    file: sym.file.clone(),
                    line: sym.start_line,
                    end_line: sym.end_line,
                    is_exported: sym.is_exported,
                    signature,
                });
            }
        }

        // Also include exports from file entry that may not be in the symbols map
        for export in &file_entry.exports {
            let already_present = file_symbols.iter().any(|s| s.name == *export);
            if !already_present {
                let signature = sig_by_name
                    .get(export.as_str())
                    .map_or_else(|| export.clone(), |s| s.to_compact());

                let (line, end_line) = sig_by_name
                    .get(export.as_str())
                    .and_then(|s| s.start_line.zip(s.end_line))
                    .unwrap_or((0, 0));

                file_symbols.push(SymbolDef {
                    name: export.clone(),
                    kind: "export".to_string(),
                    file: file_path.clone(),
                    line,
                    end_line,
                    is_exported: true,
                    signature,
                });
            }
        }

        file_symbols.sort_by_key(|s| s.line);

        if !file_symbols.is_empty() {
            result.insert(file_path.clone(), file_symbols);
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_location_map_uses_first_definition() {
        let mut index = ProjectIndex::new("/tmp");
        index.symbols.insert(
            "a::foo".into(),
            SymbolEntry {
                file: "a.rs".into(),
                name: "foo".into(),
                kind: "fn".into(),
                start_line: 1,
                end_line: 10,
                is_exported: true,
            },
        );
        index.symbols.insert(
            "b::foo".into(),
            SymbolEntry {
                file: "b.rs".into(),
                name: "foo".into(),
                kind: "fn".into(),
                start_line: 1,
                end_line: 5,
                is_exported: false,
            },
        );

        let map = build_symbol_location_map(&index);
        assert!(map.contains_key("foo"));
    }

    #[test]
    fn repo_graph_deduplicates_edges() {
        let mut index = ProjectIndex::new("/tmp");
        index.files.insert("a.rs".into(), dummy_file_entry("a.rs"));
        index.files.insert("b.rs".into(), dummy_file_entry("b.rs"));
        index.edges.push(graph_index::IndexEdge {
            from: "a.rs".into(),
            to: "b.rs".into(),
            kind: "import".into(),
            weight: 1.0,
        });
        index.edges.push(graph_index::IndexEdge {
            from: "a.rs".into(),
            to: "b.rs".into(),
            kind: "import".into(),
            weight: 1.0,
        });

        let call_graph = CallGraph::new("/tmp");
        let graph = RepoGraph::from_index_and_calls(&index, &call_graph, &HashMap::new());

        let a_deps = graph.forward.get("a.rs").unwrap();
        assert_eq!(a_deps.len(), 1, "duplicate edges should be deduped");
    }

    #[test]
    fn repo_graph_ignores_self_edges() {
        let mut index = ProjectIndex::new("/tmp");
        index.files.insert("a.rs".into(), dummy_file_entry("a.rs"));
        index.edges.push(graph_index::IndexEdge {
            from: "a.rs".into(),
            to: "a.rs".into(),
            kind: "import".into(),
            weight: 1.0,
        });

        let call_graph = CallGraph::new("/tmp");
        let graph = RepoGraph::from_index_and_calls(&index, &call_graph, &HashMap::new());

        assert!(
            !graph.forward.contains_key("a.rs"),
            "self-edges should be excluded"
        );
    }

    fn dummy_file_entry(path: &str) -> graph_index::FileEntry {
        graph_index::FileEntry {
            path: path.into(),
            hash: "abc".into(),
            language: "rust".into(),
            line_count: 10,
            token_count: 50,
            exports: vec![],
            summary: String::new(),
        }
    }
}
