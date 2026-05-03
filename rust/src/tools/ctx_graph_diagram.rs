use std::collections::HashMap;

use crate::core::call_graph::CallGraph;
use crate::core::graph_index;

const DEFAULT_MAX_NODES: usize = 30;
const DEFAULT_DEPTH: usize = 2;

pub fn handle(
    file: Option<&str>,
    depth: Option<usize>,
    kind: Option<&str>,
    project_root: &str,
) -> String {
    let max_depth = depth.unwrap_or(DEFAULT_DEPTH);
    let graph_kind = kind.unwrap_or("deps");

    match graph_kind {
        "calls" => render_call_graph(file, max_depth, project_root),
        _ => render_dep_graph(file, max_depth, project_root),
    }
}

fn render_dep_graph(file: Option<&str>, depth: usize, project_root: &str) -> String {
    let index = graph_index::load_or_build(project_root);

    if index.edges.is_empty() {
        return "No dependency edges found in project index.".to_string();
    }

    let edges: Vec<_> = if let Some(focus) = file {
        let reachable = bfs_reachable_files(focus, &index.edges, depth);
        index
            .edges
            .iter()
            .filter(|e| reachable.contains(e.from.as_str()) || reachable.contains(e.to.as_str()))
            .collect()
    } else {
        index.edges.iter().collect()
    };

    if edges.is_empty() {
        return format!(
            "No dependency edges found{}",
            file.map(|f| format!(" for '{f}'")).unwrap_or_default()
        );
    }

    let top_edges = select_top_edges(&edges, DEFAULT_MAX_NODES);

    let mut mermaid = String::from("```mermaid\nflowchart TD\n");
    for edge in &top_edges {
        let from_id = sanitize_node_id(&edge.from);
        let to_id = sanitize_node_id(&edge.to);
        let from_label = shorten_path(&edge.from);
        let to_label = shorten_path(&edge.to);
        mermaid.push_str(&format!(
            "    {from_id}[\"{from_label}\"] -->|{}| {to_id}[\"{to_label}\"]\n",
            edge.kind
        ));
    }
    mermaid.push_str("```");

    let total = index.edges.len();
    let shown = top_edges.len();
    if shown < total {
        format!("{mermaid}\n\n({shown}/{total} edges shown, top by connectivity)")
    } else {
        mermaid
    }
}

fn bfs_reachable_files(
    start: &str,
    edges: &[graph_index::IndexEdge],
    max_depth: usize,
) -> std::collections::HashSet<String> {
    let mut visited = std::collections::HashSet::new();
    let mut queue: std::collections::VecDeque<(String, usize)> = std::collections::VecDeque::new();

    for edge in edges {
        if edge.from.contains(start) || edge.to.contains(start) {
            if edge.from.contains(start) {
                visited.insert(edge.from.clone());
                queue.push_back((edge.from.clone(), 0));
            }
            if edge.to.contains(start) {
                visited.insert(edge.to.clone());
                queue.push_back((edge.to.clone(), 0));
            }
        }
    }

    while let Some((node, d)) = queue.pop_front() {
        if d >= max_depth {
            continue;
        }
        for edge in edges {
            let neighbor = if edge.from == node {
                &edge.to
            } else if edge.to == node {
                &edge.from
            } else {
                continue;
            };
            if visited.insert(neighbor.clone()) {
                queue.push_back((neighbor.clone(), d + 1));
            }
        }
    }

    visited
}

fn render_call_graph(file: Option<&str>, _depth: usize, project_root: &str) -> String {
    let index = graph_index::load_or_build(project_root);
    let call_graph = CallGraph::load_or_build(project_root, &index);
    let _ = call_graph.save();

    if call_graph.edges.is_empty() {
        return "No call edges found. Run ctx_callgraph first to build the call graph.".to_string();
    }

    let edges: Vec<_> = if let Some(focus) = file {
        call_graph
            .edges
            .iter()
            .filter(|e| {
                e.caller_file.contains(focus)
                    || e.caller_symbol.contains(focus)
                    || e.callee_name.contains(focus)
            })
            .collect()
    } else {
        call_graph.edges.iter().collect()
    };

    if edges.is_empty() {
        return format!(
            "No call edges found{}",
            file.map(|f| format!(" matching '{f}'")).unwrap_or_default()
        );
    }

    let top_nodes = select_top_call_nodes(&edges, DEFAULT_MAX_NODES);

    let mut mermaid = String::from("```mermaid\nflowchart LR\n");
    let mut seen = std::collections::HashSet::new();

    for edge in &edges {
        if !top_nodes.contains(&edge.caller_symbol.as_str())
            && !top_nodes.contains(&edge.callee_name.as_str())
        {
            continue;
        }
        let key = format!("{}→{}", edge.caller_symbol, edge.callee_name);
        if !seen.insert(key) {
            continue;
        }
        let from_id = sanitize_node_id(&edge.caller_symbol);
        let to_id = sanitize_node_id(&edge.callee_name);
        mermaid.push_str(&format!("    {from_id} --> {to_id}\n"));
    }
    mermaid.push_str("```");

    let total = call_graph.edges.len();
    let shown = seen.len();
    if shown < total {
        format!("{mermaid}\n\n({shown}/{total} call edges shown, top by connectivity)")
    } else {
        mermaid
    }
}

fn select_top_edges<'a>(
    edges: &'a [&'a graph_index::IndexEdge],
    max_nodes: usize,
) -> Vec<&'a graph_index::IndexEdge> {
    let mut node_counts: HashMap<&str, usize> = HashMap::new();
    for edge in edges {
        *node_counts.entry(&edge.from).or_insert(0) += 1;
        *node_counts.entry(&edge.to).or_insert(0) += 1;
    }

    let mut nodes_sorted: Vec<_> = node_counts.into_iter().collect();
    nodes_sorted.sort_by_key(|x| std::cmp::Reverse(x.1));
    let top: std::collections::HashSet<&str> = nodes_sorted
        .iter()
        .take(max_nodes)
        .map(|(n, _)| *n)
        .collect();

    edges
        .iter()
        .filter(|e| top.contains(e.from.as_str()) || top.contains(e.to.as_str()))
        .copied()
        .collect()
}

fn select_top_call_nodes<'a>(
    edges: &[&'a crate::core::call_graph::CallEdge],
    max_nodes: usize,
) -> std::collections::HashSet<&'a str> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for edge in edges {
        *counts.entry(&edge.caller_symbol).or_insert(0) += 1;
        *counts.entry(&edge.callee_name).or_insert(0) += 1;
    }

    let mut sorted: Vec<_> = counts.into_iter().collect();
    sorted.sort_by_key(|x| std::cmp::Reverse(x.1));
    sorted.into_iter().take(max_nodes).map(|(n, _)| n).collect()
}

fn sanitize_node_id(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn shorten_path(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() <= 2 {
        return path.to_string();
    }
    let last_two = &parts[parts.len() - 2..];
    format!("…/{}", last_two.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_node_id_removes_special_chars() {
        assert_eq!(sanitize_node_id("src/main.rs"), "src_main_rs");
        assert_eq!(sanitize_node_id("foo::bar"), "foo__bar");
    }

    #[test]
    fn shorten_path_keeps_short_paths() {
        assert_eq!(shorten_path("main.rs"), "main.rs");
        assert_eq!(shorten_path("src/main.rs"), "src/main.rs");
    }

    #[test]
    fn shorten_path_truncates_long_paths() {
        assert_eq!(shorten_path("a/b/c/main.rs"), "…/c/main.rs");
    }

    #[test]
    fn render_dep_graph_empty_index() {
        let result = render_dep_graph(None, 2, "/nonexistent/path");
        assert!(result.contains("No dependency edges") || result.contains("flowchart"));
    }

    #[test]
    fn render_call_graph_empty() {
        let result = render_call_graph(None, 2, "/nonexistent/path");
        assert!(result.contains("No call edges") || result.contains("flowchart"));
    }
}
