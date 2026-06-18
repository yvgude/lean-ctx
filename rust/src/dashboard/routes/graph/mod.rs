mod analysis;
mod analysis_cache;
mod architecture;
mod callgraph;
mod capability_matrix;
mod deps;
mod tree;

pub(super) fn handle(
    path: &str,
    query_str: &str,
    _method: &str,
    _body: &str,
) -> Option<(&'static str, &'static str, String)> {
    deps::get_route(path, query_str)
        .or_else(|| callgraph::get_route(path, query_str))
        .or_else(|| analysis::get_route(path, query_str))
        .or_else(|| architecture::get_route(path, query_str))
        .or_else(|| tree::get_route(path, query_str))
}

fn project_basename(abs_root: &str) -> String {
    std::path::Path::new(abs_root).file_name().map_or_else(
        || "project".to_string(),
        |n| n.to_string_lossy().to_string(),
    )
}

/// `202 Accepted` body for a graph/index route whose backing index is still
/// being built in the background (#452). The dashboard polls the same route and
/// renders the data once it returns `200`.
fn building_response(
    progress: &crate::core::graph_index::IndexBuildProgress,
) -> (&'static str, &'static str, String) {
    let json =
        serde_json::to_string(progress).unwrap_or_else(|_| "{\"status\":\"building\"}".to_string());
    ("202 Accepted", "application/json", json)
}
