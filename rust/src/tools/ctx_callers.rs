use crate::core::call_graph::CallGraph;
use crate::core::graph_index;

pub fn handle(symbol: &str, file: Option<&str>, project_root: &str) -> String {
    let index = graph_index::load_or_build(project_root);
    let graph = CallGraph::load_or_build(project_root, &index);
    let _ = graph.save();

    let mut callers = graph.callers_of(symbol);

    if let Some(f) = file {
        let filter = make_relative(f, project_root);
        callers.retain(|e| e.caller_file.contains(&filter));
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

fn make_relative(path: &str, root: &str) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .trim_start_matches('/')
        .trim_start_matches('\\')
        .to_string()
}

#[cfg(test)]
mod tests {
    use crate::core::call_graph::{CallEdge, CallGraph};

    #[test]
    fn format_callers_output() {
        let mut graph = CallGraph::new("/tmp");
        graph.edges.push(CallEdge {
            caller_file: "src/main.rs".to_string(),
            caller_symbol: "main".to_string(),
            caller_line: 5,
            callee_name: "init".to_string(),
        });
        let callers = graph.callers_of("init");
        assert_eq!(callers.len(), 1);
        assert_eq!(callers[0].caller_symbol, "main");
    }
}
