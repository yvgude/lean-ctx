use crate::core::call_graph::CallGraph;
use crate::core::graph_index;

pub fn handle(symbol: &str, file: Option<&str>, project_root: &str) -> String {
    let index = graph_index::load_or_build(project_root);
    let graph = CallGraph::load_or_build(project_root, &index);
    let _ = graph.save();

    let mut callees = graph.callees_of(symbol);

    if let Some(f) = file {
        let filter = graph_file_filter(f, project_root);
        callees.retain(|e| graph_index::graph_match_key(&e.caller_file).contains(&filter));
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
    use crate::core::call_graph::{CallEdge, CallGraph};

    #[test]
    fn format_callees_output() {
        let mut graph = CallGraph::new("/tmp");
        graph.edges.push(CallEdge {
            caller_file: "src/main.rs".to_string(),
            caller_symbol: "main".to_string(),
            caller_line: 5,
            callee_name: "init".to_string(),
        });
        graph.edges.push(CallEdge {
            caller_file: "src/main.rs".to_string(),
            caller_symbol: "main".to_string(),
            caller_line: 8,
            callee_name: "run".to_string(),
        });
        let callees = graph.callees_of("main");
        assert_eq!(callees.len(), 2);
    }

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
}
