use crate::core::call_graph::{CallEdge, CallGraph, CallGraphInputs, RiskLevel};
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
            with_handle_hint(handle_direction(sym, file, project_root, action, depth))
        }
        "trace" => with_handle_hint(handle_trace(from, to, project_root)),
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
    load_graph_with_inputs(project_root).0
}

fn load_graph_with_inputs(project_root: &str) -> (CallGraph, CallGraphInputs) {
    let inputs = CallGraphInputs::open(project_root);
    let graph = CallGraph::load_or_build(project_root, &inputs);
    let _ = graph.save();
    (graph, inputs)
}

/// Where each listed call goes, with the evidence behind it:
/// `⇒ src/repo.rs [verified]`. Structurally uncertain edges are escalated to
/// a semantic backend on the spot (cached; `semantic_mode` rules apply).
/// Unknown callees (std, dependencies) get no annotation.
fn target_annotations(
    edges: &[&CallEdge],
    graph: &CallGraph,
    inputs: &CallGraphInputs,
    project_root: &str,
) -> Vec<String> {
    use crate::core::call_graph::{StructuralTarget, resolve_edge_callee_targets};
    use crate::core::semantic::{EvidenceGrade, SemanticVerdict, escalate_calls};

    let owned: Vec<CallEdge> = edges.iter().map(|e| (*e).clone()).collect();
    let structural = resolve_edge_callee_targets(inputs, &owned);
    let verdicts = match crate::core::property_graph::CodeGraph::open(project_root) {
        Ok(store) => {
            let mode = crate::core::config::SemanticMode::for_project(project_root);
            escalate_calls(
                &store,
                project_root,
                inputs,
                &owned,
                &structural,
                &graph.file_hashes,
                mode,
                crate::core::semantic::EscalationBudget::INTERACTIVE,
            )
            .verdicts
        }
        Err(_) => vec![None; owned.len()],
    };
    structural
        .iter()
        .zip(verdicts)
        .map(|(target, verdict)| match (verdict, target) {
            (Some(SemanticVerdict::Verified { file, .. }), _) => format!("  ⇒ {file} [verified]"),
            (Some(SemanticVerdict::NotInProject), _) => "  ⇒ external [verified]".to_string(),
            (None, StructuralTarget::Resolved { file, via }) => {
                format!("  ⇒ {file} [{}]", EvidenceGrade::from_scope(*via).as_str())
            }
            (None, StructuralTarget::Ambiguous) => "  ⇒ ? [ambiguous]".to_string(),
            (None, StructuralTarget::Unknown) => String::new(),
        })
        .collect()
}

fn handle_direction(
    symbol: &str,
    file: Option<&str>,
    project_root: &str,
    direction: &str,
    depth: usize,
) -> String {
    let clamped_depth = depth.clamp(1, MAX_BFS_DEPTH);

    if clamped_depth == 1 {
        let (graph, inputs) = load_graph_with_inputs(project_root);
        let filter = file.map(|f| graph_file_filter(f, project_root));
        let annotate =
            |edges: &[&CallEdge]| target_annotations(edges, &graph, &inputs, project_root);
        match direction {
            "callers" => format_callers(symbol, &graph, filter.as_deref(), annotate),
            "callees" => format_callees(symbol, &graph, filter.as_deref(), annotate),
            _ => unreachable!(),
        }
    } else {
        let graph = load_graph(project_root);
        let filter = file.map(|f| graph_file_filter(f, project_root));
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

fn format_callers(
    symbol: &str,
    graph: &CallGraph,
    filter: Option<&str>,
    annotate: impl FnOnce(&[&CallEdge]) -> Vec<String>,
) -> String {
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
    for (edge, target) in callers.iter().zip(annotate(&callers)) {
        out.push_str(&format!(
            "  {} → {}  (L{}){target}\n",
            edge.caller_file, edge.caller_symbol, edge.caller_line
        ));
    }
    out
}

fn format_callees(
    symbol: &str,
    graph: &CallGraph,
    filter: Option<&str>,
    annotate: impl FnOnce(&[&CallEdge]) -> Vec<String>,
) -> String {
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
    for (edge, target) in callees.iter().zip(annotate(&callees)) {
        out.push_str(&format!(
            "  → {}  ({}:L{}){target}\n",
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

/// Append the stable-handle usage hint (#607) to a non-empty result so the
/// agent can re-target any listed symbol via `ctx_search(action="symbol",
/// handle=…)`. Skips error/empty messages (which start with "No ") so failures
/// stay clean.
fn with_handle_hint(out: String) -> String {
    if out.starts_with("No ") || out.trim().is_empty() {
        out
    } else {
        format!("{out}{}\n", crate::core::handle::USAGE_HINT)
    }
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

    #[test]
    fn handle_hint_appended_to_results_only() {
        use super::with_handle_hint;
        let ok = with_handle_hint("1 caller(s) of 'x':\n  a → b  (L1)\n".to_string());
        assert!(
            ok.contains("handle=\""),
            "success output gets the hint: {ok}"
        );
        let empty = with_handle_hint("No callers found for 'x' (0 edges in graph)".to_string());
        assert!(
            !empty.contains("handle=\""),
            "empty result stays clean: {empty}"
        );
    }
}
