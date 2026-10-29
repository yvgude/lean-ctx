//! Graph builder for repo map.
//!
//! Constructs a file-level directed graph from the project index edges
//! and call graph edges, then exposes symbol definitions per file.

use std::collections::{HashMap, HashSet};

use crate::core::call_graph::{CallGraph, CallGraphInputs};
use crate::core::graph_index::{self, ProjectIndex, SymbolEntry};

/// A symbol definition with its file context.
#[derive(Debug, Clone)]
pub(crate) struct SymbolDef {
    pub name: String,
    pub kind: String,
    pub file: String,
    pub line: usize,
    pub end_line: usize,
    pub is_exported: bool,
    pub signature: String,
}

/// File-level graph combining import edges and call edges.
pub(crate) struct RepoGraph {
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
    pub(crate) fn build(project_root: &str) -> Self {
        let (index, content_cache) = graph_index::scan_with_content_cache(project_root);
        let calls = graph_call_pairs(project_root).unwrap_or_else(|| {
            let cg_inputs = CallGraphInputs::from_project_index(&index);
            let call_graph = CallGraph::load_or_build(project_root, &cg_inputs);
            structural_call_pairs(&cg_inputs, &call_graph)
        });

        Self::from_index_and_calls(&index, &calls, &content_cache)
    }

    /// `calls`: caller file → callee file pairs (see [`graph_call_pairs`]).
    fn from_index_and_calls(
        index: &ProjectIndex,
        calls: &[(String, String)],
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

        // Call edges, resolved like the property graph's (never by bare name)
        for (from, to) in calls {
            if files.contains(from) && files.contains(to) && from != to {
                forward.entry(from.clone()).or_default().push(to.clone());
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
}

/// File-level `calls` edges of a current property graph: the same edges
/// ranking and impact analysis use — scope-resolved, verified by a semantic
/// backend where one answered, and without name-match guesses a backend
/// vetoed. `None` without a current graph whose calls were consolidated (a
/// freshly mirrored graph has none until enrichment runs). An empty edge set
/// counts once a backend has answered for the graph: then every candidate
/// was vetoed or ambiguous, and a fallback would resurrect the vetoed guess.
fn graph_call_pairs(project_root: &str) -> Option<Vec<(String, String)>> {
    use crate::core::property_graph::{CodeGraph, EdgeKind};
    if crate::core::property_graph::engine_outdated(project_root) {
        return None;
    }
    let graph = CodeGraph::open(project_root).ok()?;
    let pairs: Vec<(String, String)> = graph
        .file_edges_of_kind(&EdgeKind::Calls)
        .ok()?
        .into_iter()
        // Serve-time source check (G04): an edge of a withdrawn file neither
        // appears in the map nor ranks another file.
        .filter(|(from, to, _)| graph.serves_file(from) && graph.serves_file(to))
        .map(|(from, to, _)| (from, to))
        .collect();
    (!pairs.is_empty() || graph.has_semantic_answers().unwrap_or(false)).then_some(pairs)
}

/// Without a property graph: each call bound in the caller's own scope
/// (same file → unique import → unique in the project); an ambiguous name
/// yields no edge rather than an arbitrary one.
fn structural_call_pairs(
    inputs: &CallGraphInputs,
    call_graph: &CallGraph,
) -> Vec<(String, String)> {
    crate::core::call_graph::resolve_edge_callee_files(inputs, &call_graph.edges)
        .into_iter()
        .zip(&call_graph.edges)
        .filter_map(|(to, edge)| Some((edge.caller_file.clone(), to?)))
        .collect()
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

    /// Two files define `save`; the caller imports neither. The repo map
    /// used to link the call to whichever definition it saw first — now an
    /// ambiguous name yields no edge, while a scope-bound one still does.
    #[test]
    fn ambiguous_calls_are_not_linked_by_name() {
        use crate::core::call_graph::{CallEdge, SymbolSpan};
        let span = |file: &str, name: &str| SymbolSpan {
            file: file.into(),
            name: name.into(),
            start_line: 1,
            end_line: 3,
            kind: "fn".into(),
        };
        let inputs = CallGraphInputs {
            project_root: "/tmp".into(),
            file_paths: vec!["a.rs".into(), "b.rs".into(), "app.rs".into()],
            symbols: vec![
                span("a.rs", "save"),
                span("b.rs", "save"),
                span("b.rs", "load"),
            ],
            import_edges: Vec::new(),
        };
        let call = |callee: &str| CallEdge {
            caller_file: "app.rs".into(),
            caller_symbol: "run".into(),
            caller_line: 2,
            callee_name: callee.into(),
            ..Default::default()
        };
        let mut call_graph = CallGraph::new("/tmp");
        call_graph.edges = vec![call("save"), call("load")];

        assert_eq!(
            structural_call_pairs(&inputs, &call_graph),
            [("app.rs".to_string(), "b.rs".to_string())],
            "only the unique `load` binds"
        );
    }

    /// A current graph without call edges is authoritative once a backend
    /// answered for it (every guess was vetoed) — but not before enrichment.
    #[test]
    fn empty_graph_calls_count_only_after_a_semantic_answer() {
        use crate::core::property_graph::{
            CachedResolution, CodeGraph, GRAPH_ENGINE_VERSION, PropertyGraphMetaV1, write_meta,
        };
        let _iso = crate::core::data_dir::isolated_data_dir();
        let proj = tempfile::tempdir().unwrap();
        let root = proj.path().to_str().unwrap();
        let meta = PropertyGraphMetaV1 {
            built_at: "2026-01-01T00:00:00Z".to_string(),
            engine_version: GRAPH_ENGINE_VERSION,
            ..Default::default()
        };
        write_meta(root, &meta).unwrap();
        let graph = CodeGraph::open(root).unwrap();
        assert_eq!(graph_call_pairs(root), None, "not consolidated yet");

        let veto = CachedResolution {
            backend: "lsp:test@1".into(),
            outcome: "external".into(),
            target_file: None,
            target_line: None,
            target_symbol: None,
            context: String::new(),
        };
        let admission = crate::core::context_admission::stores::StoreAdmission::current();
        graph
            .semantic_store(("app.rs", 2, 4, "definition"), "h", &veto, &admission)
            .unwrap();
        assert_eq!(graph_call_pairs(root), Some(Vec::new()), "vetoed stays out");
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

        let graph = RepoGraph::from_index_and_calls(&index, &[], &HashMap::new());

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

        let graph = RepoGraph::from_index_and_calls(&index, &[], &HashMap::new());

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
