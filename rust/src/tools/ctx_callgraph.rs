use crate::core::call_graph::{CallGraph, CallGraphInputs, RiskLevel};
use crate::core::index_paths;

const MAX_BFS_DEPTH: usize = 5;

pub fn handle(
    action: &str,
    symbol: Option<&str>,
    file: Option<&str>,
    project_root: &str,
    depth: usize,
    from: Option<&str>,
    to: Option<&str>,
) -> String {
    match action {
        "callers" | "callees" => {
            let Some(sym) = symbol else {
                return "symbol is required for callers/callees action".to_string();
            };
            handle_direction(sym, file, project_root, action, depth)
        }
        "trace" => handle_trace(from, to, project_root),
        "risk" => {
            let Some(sym) = symbol else {
                return "symbol is required for risk action".to_string();
            };
            handle_risk(sym, project_root)
        }
        _ => format!("Unknown action '{action}'. Use: callers|callees|trace|risk"),
    }
}

fn load_graph(project_root: &str) -> CallGraph {
    let inputs = CallGraphInputs::open(project_root);
    let graph = CallGraph::load_or_build(project_root, &inputs);
    let _ = graph.save();
    graph
}

fn handle_direction(
    symbol: &str,
    file: Option<&str>,
    project_root: &str,
    direction: &str,
    depth: usize,
) -> String {
    let graph = load_graph(project_root);
    let filter = file.map(|f| graph_file_filter(f, project_root));
    let clamped_depth = depth.clamp(1, MAX_BFS_DEPTH);

    if clamped_depth == 1 {
        match direction {
            "callers" => format_callers(symbol, &graph, filter.as_deref()),
            "callees" => format_callees(symbol, &graph, filter.as_deref()),
            _ => unreachable!(),
        }
    } else {
        match direction {
            "callers" => format_bfs_callers(symbol, &graph, clamped_depth, filter.as_deref()),
            "callees" => format_bfs_callees(symbol, &graph, clamped_depth, filter.as_deref()),
            _ => unreachable!(),
        }
    }
}

fn handle_trace(from: Option<&str>, to: Option<&str>, project_root: &str) -> String {
    let Some(from_sym) = from else {
        return "'from' is required for trace action".to_string();
    };
    let Some(to_sym) = to else {
        return "'to' is required for trace action".to_string();
    };

    let graph = load_graph(project_root);

    match graph.find_call_path(from_sym, to_sym) {
        Some(hops) => {
            let mut out = format!("Call path ({} hop(s)):\n", hops.len() - 1);
            for (i, hop) in hops.iter().enumerate() {
                let loc = if hop.file.is_empty() {
                    String::new()
                } else {
                    format!("  ({}:L{})", hop.file, hop.line)
                };
                if i == 0 {
                    out.push_str(&format!("  {}{loc}\n", hop.symbol));
                } else {
                    out.push_str(&format!("  → {}{loc}\n", hop.symbol));
                }
            }
            out
        }
        None => {
            format!("No call path found from '{from_sym}' to '{to_sym}' (searched up to depth 10)")
        }
    }
}

fn handle_risk(symbol: &str, project_root: &str) -> String {
    let graph = load_graph(project_root);
    let count = graph.transitive_caller_count(symbol, MAX_BFS_DEPTH);
    let level = RiskLevel::from_caller_count(count);
    let direct = graph.callers_of(symbol).len();

    let mut out = format!(
        "Risk: {} — {} transitive caller(s) of '{}' (depth≤{}, {} direct)\n\
         Thresholds: CRITICAL >10 | HIGH 5–10 | MEDIUM 2–4 | LOW 0–1",
        level.label(),
        count,
        symbol,
        MAX_BFS_DEPTH,
        direct,
    );

    // Fold in the code-health dimension (#1084): a symbol that is both
    // widely-called AND cognitively complex is the highest-leverage refactor.
    // Sourced from the persisted health fabric (best-effort, no parsing).
    if let Some(cc) = crate::core::code_health::fabric::hotspot_cc(project_root, symbol) {
        out.push_str(&format!(
            "\nComplexity: cc={cc} (over navigability threshold) — high blast-radius and hard to read."
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// Single-hop formatters (existing behavior)
// ---------------------------------------------------------------------------

fn format_callers(symbol: &str, graph: &CallGraph, filter: Option<&str>) -> String {
    let mut callers = graph.callers_of(symbol);
    if let Some(f) = filter {
        callers.retain(|e| index_paths::graph_match_key(&e.caller_file).contains(f));
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
        callees.retain(|e| index_paths::graph_match_key(&e.caller_file).contains(f));
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

// ---------------------------------------------------------------------------
// Multi-hop BFS formatters
// ---------------------------------------------------------------------------

fn format_bfs_callers(
    symbol: &str,
    graph: &CallGraph,
    depth: usize,
    filter: Option<&str>,
) -> String {
    let mut nodes = graph.bfs_callers(symbol, depth);
    if let Some(f) = filter {
        nodes.retain(|n| index_paths::graph_match_key(&n.file).contains(f));
    }

    if nodes.is_empty() {
        return format!(
            "No callers found for '{}' (depth≤{}, {} edges in graph)",
            symbol,
            depth,
            graph.edges.len()
        );
    }

    let mut out = format!(
        "{} caller(s) of '{}' (depth≤{}):\n",
        nodes.len(),
        symbol,
        depth
    );
    for node in &nodes {
        let indent = "  ".repeat(node.depth);
        out.push_str(&format!(
            "{indent}{} ← {}  ({}:L{})\n",
            node.from_symbol, node.symbol, node.file, node.line
        ));
    }
    out
}

fn format_bfs_callees(
    symbol: &str,
    graph: &CallGraph,
    depth: usize,
    filter: Option<&str>,
) -> String {
    let mut nodes = graph.bfs_callees(symbol, depth);
    if let Some(f) = filter {
        nodes.retain(|n| index_paths::graph_match_key(&n.file).contains(f));
    }

    if nodes.is_empty() {
        return format!(
            "No callees found for '{}' (depth≤{}, {} edges in graph)",
            symbol,
            depth,
            graph.edges.len()
        );
    }

    let mut out = format!(
        "{} callee(s) of '{}' (depth≤{}):\n",
        nodes.len(),
        symbol,
        depth
    );
    for node in &nodes {
        let indent = "  ".repeat(node.depth);
        out.push_str(&format!(
            "{indent}{} → {}  ({}:L{})\n",
            node.from_symbol, node.symbol, node.file, node.line
        ));
    }
    out
}

fn graph_file_filter(file: &str, project_root: &str) -> String {
    let rel = index_paths::graph_relative_key(file, project_root);
    let rel_key = index_paths::graph_match_key(&rel);
    if rel_key.is_empty() {
        index_paths::graph_match_key(file)
    } else {
        rel_key
    }
}

#[cfg(test)]
mod tests {
    use super::graph_file_filter;

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
    fn invalid_action_returns_helpful_error() {
        let output = super::handle("unknown", Some("foo"), None, "/tmp", 1, None, None);
        assert!(output.contains("Unknown action"));
        assert!(output.contains("callers|callees|trace|risk"));
    }

    #[test]
    fn callers_action_without_symbol_returns_error() {
        let output = super::handle("callers", None, None, "/tmp", 1, None, None);
        assert!(output.contains("symbol is required"));
    }

    #[test]
    fn trace_without_from_returns_error() {
        let output = super::handle("trace", None, None, "/tmp", 1, None, Some("b"));
        assert!(output.contains("'from' is required"));
    }

    #[test]
    fn trace_without_to_returns_error() {
        let output = super::handle("trace", None, None, "/tmp", 1, Some("a"), None);
        assert!(output.contains("'to' is required"));
    }

    #[test]
    fn risk_without_symbol_returns_error() {
        let output = super::handle("risk", None, None, "/tmp", 1, None, None);
        assert!(output.contains("symbol is required"));
    }
}
