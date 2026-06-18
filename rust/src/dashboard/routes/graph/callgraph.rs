use crate::dashboard::routes::helpers::{detect_project_root_for_dashboard, extract_query_param};

pub(super) fn get_route(
    path: &str,
    query_str: &str,
) -> Option<(&'static str, &'static str, String)> {
    match path {
        "/api/call-graph" => Some(call_graph()),
        "/api/call-graph/status" => Some(call_graph_status()),
        "/api/symbols" => Some(symbols(query_str)),
        _ => None,
    }
}

fn call_graph() -> (&'static str, &'static str, String) {
    let root = detect_project_root_for_dashboard();
    let index = match crate::core::graph_index::get_or_start_build(&root) {
        Ok(index) => index,
        Err(progress) => return super::building_response(&progress),
    };
    match crate::core::call_graph::CallGraph::get_or_start_build(&root, index.clone()) {
        Ok(graph) => {
            // file_path → community_id, shared with the deps tab for a consistent
            // colour palette across both graph views.
            let communities = crate::core::graph_provider::open_best_effort(&root)
                .map(|open| {
                    crate::core::community::detect_communities_for_provider(&open.provider, &root)
                        .assignment_min_size(2)
                })
                .unwrap_or_default();
            let payload = serde_json::json!({
                "status": "ready",
                "project_root": super::project_basename(&graph.project_root),
                "edges": graph.edges,
                "file_hashes": graph.file_hashes,
                "indexed_file_count": index.files.len(),
                "indexed_symbol_count": index.symbols.len(),
                "analyzed_file_count": graph.file_hashes.len(),
                "call_graph_support": call_graph_support(&index),
                "language_matrix":
                    super::capability_matrix::realized_from_index(&index, Some(graph.edges.as_slice())),
                "communities": communities,
                "symbol_files": crate::core::call_graph::resolve_callee_files(&index, &graph.edges),
            });
            let json = serde_json::to_string(&payload)
                .unwrap_or_else(|_| "{\"error\":\"failed to serialize call graph\"}".to_string());
            ("200 OK", "application/json", json)
        }
        Err(progress) => {
            let json = serde_json::to_string(&progress)
                .unwrap_or_else(|_| "{\"status\":\"building\"}".to_string());
            ("202 Accepted", "application/json", json)
        }
    }
}

/// Describes whether the call graph can be populated for the project's languages.
/// Mirrors the dependency graph's `graph_support` so the dashboard can show a
/// truthful "call graph not supported for <language>" message instead of an
/// index-rebuild hint that produces zero edges (e.g. for a pure C#/Ruby project).
fn call_graph_support(index: &crate::core::graph_index::ProjectIndex) -> serde_json::Value {
    use crate::core::language_capabilities as lc;

    let mut unsupported_counts: std::collections::HashMap<&'static str, usize> =
        std::collections::HashMap::new();
    let mut has_supported = false;
    for path in index.files.keys() {
        let Some(lang) = lc::language_for_path(path) else {
            continue;
        };
        if lc::supports_call_graph(lang) {
            has_supported = true;
        } else {
            *unsupported_counts.entry(lang.id_str()).or_default() += 1;
        }
    }

    let mut ranked: Vec<(&'static str, usize)> = unsupported_counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    ranked.truncate(5);
    let unsupported_present: Vec<serde_json::Value> = ranked
        .into_iter()
        .map(|(language, files)| serde_json::json!({ "language": language, "files": files }))
        .collect();

    serde_json::json!({
        "supported_languages": lc::callgraph_supported_language_names(),
        "unsupported_present": unsupported_present,
        "has_supported": has_supported,
    })
}

fn call_graph_status() -> (&'static str, &'static str, String) {
    let progress = crate::core::call_graph::CallGraph::build_status();
    let json =
        serde_json::to_string(&progress).unwrap_or_else(|_| "{\"status\":\"idle\"}".to_string());
    ("200 OK", "application/json", json)
}

fn symbols(query_str: &str) -> (&'static str, &'static str, String) {
    let root = detect_project_root_for_dashboard();
    let index = match crate::core::graph_index::get_or_start_build(&root) {
        Ok(index) => index,
        Err(progress) => return super::building_response(&progress),
    };
    let q = extract_query_param(query_str, "q");
    let kind = extract_query_param(query_str, "kind");
    let json = build_symbols_json(&index, q.as_deref(), kind.as_deref());
    ("200 OK", "application/json", json)
}

fn build_symbols_json(
    index: &crate::core::graph_index::ProjectIndex,
    query: Option<&str>,
    kind: Option<&str>,
) -> String {
    let query = query
        .map(|q| q.trim().to_lowercase())
        .filter(|q| !q.is_empty());
    let kind = kind
        .map(|k| k.trim().to_lowercase())
        .filter(|k| !k.is_empty());

    let mut symbols: Vec<&crate::core::graph_index::SymbolEntry> = index
        .symbols
        .values()
        .filter(|sym| {
            let kind_match = match kind.as_ref() {
                Some(k) => sym.kind.eq_ignore_ascii_case(k),
                None => true,
            };
            let query_match = match query.as_ref() {
                Some(q) => {
                    let name = sym.name.to_lowercase();
                    let file = sym.file.to_lowercase();
                    let symbol_kind = sym.kind.to_lowercase();
                    name.contains(q) || file.contains(q) || symbol_kind.contains(q)
                }
                None => true,
            };
            kind_match && query_match
        })
        .collect();

    symbols.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then_with(|| a.start_line.cmp(&b.start_line))
            .then_with(|| a.name.cmp(&b.name))
    });
    symbols.truncate(500);

    serde_json::to_string(
        &symbols
            .into_iter()
            .map(|sym| {
                serde_json::json!({
                    "name": sym.name,
                    "kind": sym.kind,
                    "file": sym.file,
                    "start_line": sym.start_line,
                    "end_line": sym.end_line,
                    "is_exported": sym.is_exported,
                })
            })
            .collect::<Vec<_>>(),
    )
    .unwrap_or_else(|_| "[]".to_string())
}
