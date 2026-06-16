use std::collections::{HashMap, HashSet};

use super::graph_model::{ContextGraph, ContextNode};

#[derive(Debug, Clone, Default)]
pub struct MergeReport {
    pub nodes_added: u32,
    pub nodes_updated: u32,
    pub nodes_superseded: u32,
    pub edges_added: u32,
    pub edges_merged: u32,
    pub conflicts: Vec<String>,
}

pub fn merge_graphs(base: &mut ContextGraph, incoming: &ContextGraph) -> MergeReport {
    let mut report = MergeReport::default();
    let mut existing_ids: HashSet<String> = base.nodes.iter().map(|n| n.id.clone()).collect();

    let superseded = collect_superseded(incoming);

    for node in &incoming.nodes {
        if superseded.contains(&node.id) {
            continue;
        }

        if existing_ids.contains(&node.id) {
            merge_existing_node(base, node, &mut report);
        } else {
            base.nodes.push(node.clone());
            existing_ids.insert(node.id.clone());
            report.nodes_added += 1;
        }
    }

    for id in &superseded {
        if let Some(n) = base.nodes.iter_mut().find(|n| n.id == *id) {
            n.activation = 0.0;
            report.nodes_superseded += 1;
        }
    }

    detect_conflicts(base, incoming, &mut report);

    let mut edge_index: HashMap<(String, String, String), usize> = HashMap::new();
    for (i, e) in base.edges.iter().enumerate() {
        edge_index.insert((e.from.clone(), e.to.clone(), e.edge_type.clone()), i);
    }

    for edge in &incoming.edges {
        if superseded.contains(&edge.from) || superseded.contains(&edge.to) {
            continue;
        }
        if !existing_ids.contains(&edge.from) || !existing_ids.contains(&edge.to) {
            continue;
        }

        let key = (edge.from.clone(), edge.to.clone(), edge.edge_type.clone());
        if let Some(&idx) = edge_index.get(&key) {
            let existing = &mut base.edges[idx];
            existing.weight = f64::midpoint(existing.weight, edge.weight);
            existing.coactivations += edge.coactivations;
            report.edges_merged += 1;
        } else {
            let new_idx = base.edges.len();
            base.edges.push(edge.clone());
            edge_index.insert(key, new_idx);
            report.edges_added += 1;
        }
    }

    report
}

fn collect_superseded(graph: &ContextGraph) -> HashSet<String> {
    let mut superseded = HashSet::new();
    for node in &graph.nodes {
        if let Some(ref s) = node.supersedes {
            superseded.insert(s.clone());
        }
    }
    superseded
}

fn merge_existing_node(base: &mut ContextGraph, incoming: &ContextNode, report: &mut MergeReport) {
    let Some(existing) = base.nodes.iter_mut().find(|n| n.id == incoming.id) else {
        return;
    };

    if incoming.activation > existing.activation {
        existing.activation = incoming.activation;
    }

    if let Some(ref inc_cat) = incoming.category
        && existing.category.is_none()
    {
        existing.category = Some(inc_cat.clone());
    }

    if let Some(inc_conf) = incoming.confidence {
        match existing.confidence {
            Some(ex_conf) if inc_conf > ex_conf => existing.confidence = Some(inc_conf),
            None => existing.confidence = Some(inc_conf),
            _ => {}
        }
    }

    report.nodes_updated += 1;
}

fn detect_conflicts(base: &ContextGraph, incoming: &ContextGraph, report: &mut MergeReport) {
    let contradiction_targets: HashSet<&str> = incoming
        .edges
        .iter()
        .filter(|e| e.edge_type == "contradicts")
        .map(|e| e.to.as_str())
        .collect();

    let base_ids: HashSet<&str> = base.nodes.iter().map(|n| n.id.as_str()).collect();

    for target in contradiction_targets {
        if base_ids.contains(target) {
            report.conflicts.push(format!(
                "incoming graph contradicts existing node '{target}'"
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::graph_model::ContextEdge;
    use super::*;

    fn node(id: &str, node_type: &str, activation: f64) -> ContextNode {
        ContextNode {
            id: id.into(),
            node_type: node_type.into(),
            content: format!("content of {id}"),
            activation,
            category: None,
            source: None,
            created_at: None,
            decay_half_life_days: None,
            blob_ref: None,
            file_path: None,
            line_start: None,
            line_end: None,
            confidence: None,
            supersedes: None,
        }
    }

    fn edge(from: &str, to: &str, edge_type: &str, weight: f64) -> ContextEdge {
        ContextEdge {
            from: from.into(),
            to: to.into(),
            edge_type: edge_type.into(),
            weight,
            coactivations: 1,
            metadata: None,
        }
    }

    #[test]
    fn merge_adds_new_nodes() {
        let mut base = ContextGraph::new();
        base.add_node(node("a", "fact", 1.0));

        let mut incoming = ContextGraph::new();
        incoming.add_node(node("b", "gotcha", 0.9));

        let report = merge_graphs(&mut base, &incoming);
        assert_eq!(report.nodes_added, 1);
        assert_eq!(base.nodes.len(), 2);
    }

    #[test]
    fn merge_updates_existing_activation() {
        let mut base = ContextGraph::new();
        base.add_node(node("a", "fact", 0.5));

        let mut incoming = ContextGraph::new();
        incoming.add_node(node("a", "fact", 0.8));

        let report = merge_graphs(&mut base, &incoming);
        assert_eq!(report.nodes_updated, 1);
        assert!((base.nodes[0].activation - 0.8).abs() < 0.001);
    }

    #[test]
    fn merge_averages_shared_edge_weights() {
        let mut base = ContextGraph::new();
        base.add_node(node("a", "fact", 1.0));
        base.add_node(node("b", "fact", 1.0));
        base.add_edge(edge("a", "b", "supports", 0.6));

        let mut incoming = ContextGraph::new();
        incoming.add_node(node("a", "fact", 1.0));
        incoming.add_node(node("b", "fact", 1.0));
        incoming.add_edge(edge("a", "b", "supports", 1.0));

        let report = merge_graphs(&mut base, &incoming);
        assert_eq!(report.edges_merged, 1);
        assert!((base.edges[0].weight - 0.8).abs() < 0.001);
        assert_eq!(base.edges[0].coactivations, 2);
    }

    #[test]
    fn merge_adds_new_edges() {
        let mut base = ContextGraph::new();
        base.add_node(node("a", "fact", 1.0));
        base.add_node(node("b", "fact", 1.0));

        let mut incoming = ContextGraph::new();
        incoming.add_node(node("a", "fact", 1.0));
        incoming.add_node(node("b", "fact", 1.0));
        incoming.add_edge(edge("a", "b", "supports", 0.9));

        let report = merge_graphs(&mut base, &incoming);
        assert_eq!(report.edges_added, 1);
        assert_eq!(base.edges.len(), 1);
    }

    #[test]
    fn supersedes_deactivates_node() {
        let mut base = ContextGraph::new();
        base.add_node(node("old_fact", "fact", 1.0));

        let mut incoming = ContextGraph::new();
        let mut new_node = node("new_fact", "fact", 1.0);
        new_node.supersedes = Some("old_fact".into());
        incoming.add_node(new_node);

        let report = merge_graphs(&mut base, &incoming);
        assert_eq!(report.nodes_superseded, 1);
        assert_eq!(report.nodes_added, 1);
        assert!((base.node_by_id("old_fact").unwrap().activation).abs() < 0.001);
    }

    #[test]
    fn detects_contradictions() {
        let mut base = ContextGraph::new();
        base.add_node(node("existing", "fact", 1.0));

        let mut incoming = ContextGraph::new();
        incoming.add_node(node("new", "fact", 1.0));
        incoming.add_node(node("existing", "fact", 1.0));
        incoming.add_edge(ContextEdge {
            from: "new".into(),
            to: "existing".into(),
            edge_type: "contradicts".into(),
            weight: 1.0,
            coactivations: 0,
            metadata: None,
        });

        let report = merge_graphs(&mut base, &incoming);
        assert_eq!(report.conflicts.len(), 1);
        assert!(report.conflicts[0].contains("contradicts"));
    }

    #[test]
    fn edges_to_missing_nodes_skipped() {
        let mut base = ContextGraph::new();
        base.add_node(node("a", "fact", 1.0));

        let mut incoming = ContextGraph::new();
        incoming.add_edge(edge("a", "missing", "supports", 1.0));

        let report = merge_graphs(&mut base, &incoming);
        assert_eq!(report.edges_added, 0);
        assert!(base.edges.is_empty());
    }

    #[test]
    fn merge_is_idempotent() {
        let mut base = ContextGraph::new();
        base.add_node(node("a", "fact", 0.8));
        base.add_node(node("b", "gotcha", 0.9));
        base.add_edge(edge("a", "b", "has_gotcha", 0.7));

        let incoming = base.clone();
        let report = merge_graphs(&mut base, &incoming);

        assert_eq!(report.nodes_added, 0);
        assert_eq!(report.nodes_updated, 2);
        assert_eq!(report.edges_merged, 1);
        assert_eq!(report.edges_added, 0);
        assert_eq!(base.nodes.len(), 2);
        assert_eq!(base.edges.len(), 1);
    }

    #[test]
    fn multi_merge_three_packages() {
        let mut base = ContextGraph::new();
        base.add_node(node("shared", "fact", 0.5));

        let mut pkg_a = ContextGraph::new();
        pkg_a.add_node(node("shared", "fact", 0.7));
        pkg_a.add_node(node("from_a", "pattern", 0.9));
        pkg_a.add_edge(edge("shared", "from_a", "supports", 0.8));

        let mut pkg_b = ContextGraph::new();
        pkg_b.add_node(node("shared", "fact", 0.6));
        pkg_b.add_node(node("from_b", "gotcha", 1.0));
        pkg_b.add_edge(edge("shared", "from_b", "has_gotcha", 0.9));

        merge_graphs(&mut base, &pkg_a);
        let report_b = merge_graphs(&mut base, &pkg_b);

        assert_eq!(base.nodes.len(), 3);
        assert_eq!(base.edges.len(), 2);
        assert!(base.node_by_id("from_a").is_some());
        assert!(base.node_by_id("from_b").is_some());
        assert_eq!(report_b.nodes_added, 1);
    }

    #[test]
    fn lower_activation_does_not_downgrade() {
        let mut base = ContextGraph::new();
        base.add_node(node("a", "fact", 0.9));

        let mut incoming = ContextGraph::new();
        incoming.add_node(node("a", "fact", 0.3));

        merge_graphs(&mut base, &incoming);
        assert!((base.nodes[0].activation - 0.9).abs() < 0.001);
    }

    #[test]
    fn confidence_propagation() {
        let mut base = ContextGraph::new();
        let mut n = node("a", "fact", 1.0);
        n.confidence = Some(0.5);
        base.add_node(n);

        let mut incoming = ContextGraph::new();
        let mut n2 = node("a", "fact", 1.0);
        n2.confidence = Some(0.9);
        incoming.add_node(n2);

        merge_graphs(&mut base, &incoming);
        assert_eq!(base.nodes[0].confidence, Some(0.9));
    }

    #[test]
    fn confidence_not_downgraded() {
        let mut base = ContextGraph::new();
        let mut n = node("a", "fact", 1.0);
        n.confidence = Some(0.8);
        base.add_node(n);

        let mut incoming = ContextGraph::new();
        let mut n2 = node("a", "fact", 1.0);
        n2.confidence = Some(0.3);
        incoming.add_node(n2);

        merge_graphs(&mut base, &incoming);
        assert_eq!(base.nodes[0].confidence, Some(0.8));
    }

    #[test]
    fn category_filled_from_incoming() {
        let mut base = ContextGraph::new();
        base.add_node(node("a", "fact", 1.0));
        assert!(base.nodes[0].category.is_none());

        let mut incoming = ContextGraph::new();
        let mut n = node("a", "fact", 1.0);
        n.category = Some("security".into());
        incoming.add_node(n);

        merge_graphs(&mut base, &incoming);
        assert_eq!(base.nodes[0].category.as_deref(), Some("security"));
    }

    #[test]
    fn superseded_edges_are_dropped() {
        let mut base = ContextGraph::new();
        base.add_node(node("old", "fact", 1.0));
        base.add_node(node("other", "fact", 1.0));
        base.add_edge(edge("old", "other", "supports", 0.5));

        let mut incoming = ContextGraph::new();
        let mut new_node = node("replacement", "fact", 1.0);
        new_node.supersedes = Some("old".into());
        incoming.add_node(new_node);
        incoming.add_edge(edge("old", "other", "supports", 0.9));

        let report = merge_graphs(&mut base, &incoming);
        assert_eq!(report.nodes_superseded, 1);
        assert_eq!(base.edges.len(), 1);
    }

    #[test]
    fn different_edge_types_not_merged() {
        let mut base = ContextGraph::new();
        base.add_node(node("a", "fact", 1.0));
        base.add_node(node("b", "fact", 1.0));
        base.add_edge(edge("a", "b", "supports", 0.6));

        let mut incoming = ContextGraph::new();
        incoming.add_node(node("a", "fact", 1.0));
        incoming.add_node(node("b", "fact", 1.0));
        incoming.add_edge(edge("a", "b", "contradicts", 0.9));

        let report = merge_graphs(&mut base, &incoming);
        assert_eq!(report.edges_added, 1);
        assert_eq!(report.edges_merged, 0);
        assert_eq!(base.edges.len(), 2);
    }

    #[test]
    fn empty_graph_merge_is_noop() {
        let mut base = ContextGraph::new();
        base.add_node(node("a", "fact", 1.0));

        let incoming = ContextGraph::new();
        let report = merge_graphs(&mut base, &incoming);

        assert_eq!(report.nodes_added, 0);
        assert_eq!(report.nodes_updated, 0);
        assert_eq!(report.edges_added, 0);
        assert_eq!(base.nodes.len(), 1);
    }

    #[test]
    fn merge_into_empty_base() {
        let mut base = ContextGraph::new();

        let mut incoming = ContextGraph::new();
        incoming.add_node(node("x", "fact", 0.5));
        incoming.add_node(node("y", "gotcha", 0.8));
        incoming.add_edge(edge("x", "y", "has_gotcha", 0.7));

        let report = merge_graphs(&mut base, &incoming);
        assert_eq!(report.nodes_added, 2);
        assert_eq!(report.edges_added, 1);
        assert_eq!(base.nodes.len(), 2);
        assert_eq!(base.edges.len(), 1);
    }
}
