use crate::core::call_graph::{BfsNode, CallGraph, CallGraphInputs, RiskLevel};
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

    match direction {
        "callers" => format_bfs_callers(symbol, &graph, clamped_depth, filter.as_deref()),
        "callees" => format_bfs_callees(symbol, &graph, clamped_depth, filter.as_deref()),
        _ => unreachable!(),
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

    format!(
        "Risk: {} — {} transitive caller(s) of '{}' (depth≤{}, {} direct)\n\
         Thresholds: CRITICAL >10 | HIGH 5–10 | MEDIUM 2–4 | LOW 0–1",
        level.label(),
        count,
        symbol,
        MAX_BFS_DEPTH,
        direct,
    )
}

// ---------------------------------------------------------------------------
// BFS hop-grouped formatters
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
    fmt_bfs_grouped(&nodes, symbol, depth, "caller")
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
    fmt_bfs_grouped(&nodes, symbol, depth, "callee")
}

/// Format BFS nodes — one compact line per hop entry.
fn fmt_bfs_grouped(
    nodes: &[BfsNode],
    symbol: &str,
    depth: usize,
    edge_type: &str,
) -> String {
    if nodes.is_empty() {
        return format!("No {}s found for '{symbol}' (depth≤{depth})", edge_type);
    }

    let label = format!("{}s", edge_type);
    let mut out = format!("{} {label} of '{symbol}' (depth≤{depth}):\n", nodes.len());
    for node in nodes {
        out.push_str(&format!(
            "  hop {}: {}:{}  {}  {}\n",
            node.depth, node.file, node.line, node.symbol, edge_type,
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
