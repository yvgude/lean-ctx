use crate::core::call_graph::CallGraph;
use crate::core::graph_index;

const VALID_DIRECTIONS: &str = "callers|callees";

pub fn handle(symbol: &str, file: Option<&str>, project_root: &str, direction: &str) -> String {
    let normalized_direction = match direction.to_lowercase().as_str() {
        "callers" | "caller" => "callers",
        "callees" | "callee" => "callees",
        _ => {
            return format!("Unknown direction '{direction}'. Use: {VALID_DIRECTIONS}");
        }
    };

    let index = graph_index::load_or_build(project_root);
    let graph = CallGraph::load_or_build(project_root, &index);
    let _ = graph.save();

    let filter = file.map(|f| graph_file_filter(f, project_root));

    match normalized_direction {
        "callers" => format_callers(symbol, &graph, filter.as_deref()),
        "callees" => format_callees(symbol, &graph, filter.as_deref()),
        _ => unreachable!("direction normalized above"),
    }
}

fn format_callers(symbol: &str, graph: &CallGraph, filter: Option<&str>) -> String {
    let mut callers = graph.callers_of(symbol);
    if let Some(f) = filter {
        callers.retain(|e| graph_index::graph_match_key(&e.caller_file).contains(f));
    }

    if callers.is_empty() {
        return format!(
            "No callers found for '{}' ({} edges in graph)",
            symbol,
            graph.edges.len()
        );
    }

    let mut out = format!("{} caller(s) of '{symbol}':\n", callers.len());
    for edge in &callers {
        out.push_str(&format!(
            "  {} → {}  (L{})\n",
            edge.caller_file, edge.caller_symbol, edge.caller_line
        ));
    }
    out
}

fn format_callees(symbol: &str, graph: &CallGraph, filter: Option<&str>) -> String {
    let mut callees = graph.callees_of(symbol);
    if let Some(f) = filter {
        callees.retain(|e| graph_index::graph_match_key(&e.caller_file).contains(f));
    }

    if callees.is_empty() {
        return format!(
            "No callees found for '{}' ({} edges in graph)",
            symbol,
            graph.edges.len()
        );
    }

    let mut out = format!("{} callee(s) of '{symbol}':\n", callees.len());
    for edge in &callees {
        out.push_str(&format!(
            "  → {}  ({}:L{})\n",
            edge.callee_name, edge.caller_file, edge.caller_line
        ));
    }
    out
}

fn graph_file_filter(file: &str, project_root: &str) -> String {
    let rel = graph_index::graph_relative_key(file, project_root);
    let rel_key = graph_index::graph_match_key(&rel);
    if rel_key.is_empty() {
        graph_index::graph_match_key(file)
    } else {
        rel_key
    }
}

#[cfg(test)]
mod tests {
    use super::graph_file_filter;
    use super::handle;

    #[test]
    fn graph_file_filter_normalizes_windows_styles() {
        let filter = graph_file_filter(r"C:/repo/src/main/kotlin/Example.kt", r"C:\repo");
        let expected = if cfg!(windows) {
            "src/main/kotlin/Example.kt"
        } else {
            "C:/repo/src/main/kotlin/Example.kt"
        };
        assert_eq!(filter, expected);
    }

    #[test]
    fn invalid_direction_returns_helpful_error() {
        let output = handle("foo", None, "/tmp", "unknown");
        assert!(output.contains("Unknown direction"));
        assert!(output.contains("callers|callees"));
    }
}
