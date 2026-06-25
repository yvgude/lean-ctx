use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextGraph {
    pub format: String,
    pub nodes: Vec<ContextNode>,
    pub edges: Vec<ContextEdge>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextNode {
    pub id: String,
    #[serde(rename = "type")]
    pub node_type: String,
    pub content: String,
    #[serde(default = "default_activation")]
    pub activation: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decay_half_life_days: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_start: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_end: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextEdge {
    pub from: String,
    pub to: String,
    #[serde(rename = "type")]
    pub edge_type: String,
    #[serde(default = "default_weight")]
    pub weight: f64,
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub coactivations: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<String>,
}

fn default_activation() -> f64 {
    1.0
}

fn default_weight() -> f64 {
    1.0
}

fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GraphSummary {
    pub node_count: u32,
    pub edge_count: u32,
    #[serde(default)]
    pub node_types: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activation_mean: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freshness: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MarketplaceMeta {
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default)]
    pub badges: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
}

pub const GRAPH_FORMAT_V2: &str = "ctxpkg-graph-v2";

impl ContextGraph {
    #[must_use]
    pub fn new() -> Self {
        Self {
            format: GRAPH_FORMAT_V2.into(),
            nodes: Vec::new(),
            edges: Vec::new(),
        }
    }

    pub fn add_node(&mut self, node: ContextNode) {
        self.nodes.push(node);
    }

    pub fn add_edge(&mut self, edge: ContextEdge) {
        self.edges.push(edge);
    }

    #[must_use]
    pub fn node_by_id(&self, id: &str) -> Option<&ContextNode> {
        self.nodes.iter().find(|n| n.id == id)
    }

    #[must_use]
    pub fn node_types(&self) -> Vec<String> {
        let mut types: Vec<String> = self
            .nodes
            .iter()
            .map(|n| n.node_type.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        types.sort();
        types
    }

    #[must_use]
    pub fn activation_mean(&self) -> f64 {
        if self.nodes.is_empty() {
            return 0.0;
        }
        let sum: f64 = self.nodes.iter().map(|n| n.activation).sum();
        sum / self.nodes.len() as f64
    }

    #[must_use]
    pub fn summary(&self) -> GraphSummary {
        GraphSummary {
            node_count: self.nodes.len() as u32,
            edge_count: self.edges.len() as u32,
            node_types: self.node_types(),
            activation_mean: Some(self.activation_mean()),
            freshness: self.nodes.iter().filter_map(|n| n.created_at).max(),
        }
    }

    pub fn apply_temporal_decay(&mut self, now: DateTime<Utc>) {
        for node in &mut self.nodes {
            let Some(half_life) = node.decay_half_life_days else {
                continue;
            };
            let Some(created) = node.created_at else {
                continue;
            };
            if half_life == 0 {
                continue;
            }
            let age_days = (now - created).num_days().max(0) as f64;
            let decay = 0.5_f64.powf(age_days / f64::from(half_life));
            node.activation *= decay;
        }
    }
}

impl Default for ContextGraph {
    fn default() -> Self {
        Self::new()
    }
}

impl ContextNode {
    #[must_use]
    pub fn fact(id: &str, content: &str, category: &str) -> Self {
        Self {
            id: id.into(),
            node_type: "fact".into(),
            content: content.into(),
            activation: 1.0,
            category: Some(category.into()),
            source: None,
            created_at: Some(Utc::now()),
            decay_half_life_days: Some(90),
            blob_ref: None,
            file_path: None,
            line_start: None,
            line_end: None,
            confidence: None,
            supersedes: None,
        }
    }

    #[must_use]
    pub fn gotcha(id: &str, trigger: &str, resolution: &str) -> Self {
        Self {
            id: id.into(),
            node_type: "gotcha".into(),
            content: format!("{trigger}\n---\n{resolution}"),
            activation: 1.0,
            category: None,
            source: None,
            created_at: Some(Utc::now()),
            decay_half_life_days: None,
            blob_ref: None,
            file_path: None,
            line_start: None,
            line_end: None,
            confidence: None,
            supersedes: None,
        }
    }

    #[must_use]
    pub fn code_symbol(id: &str, kind: &str, name: &str, file_path: &str) -> Self {
        Self {
            id: id.into(),
            node_type: format!("code_{kind}"),
            content: name.into(),
            activation: 1.0,
            category: Some(kind.into()),
            source: None,
            created_at: None,
            decay_half_life_days: None,
            blob_ref: None,
            file_path: Some(file_path.into()),
            line_start: None,
            line_end: None,
            confidence: None,
            supersedes: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_graph_has_correct_format() {
        let g = ContextGraph::new();
        assert_eq!(g.format, GRAPH_FORMAT_V2);
        assert!(g.nodes.is_empty());
        assert!(g.edges.is_empty());
    }

    #[test]
    fn summary_counts() {
        let mut g = ContextGraph::new();
        g.add_node(ContextNode::fact("n1", "hello", "arch"));
        g.add_node(ContextNode::gotcha("n2", "trig", "res"));
        g.add_edge(ContextEdge {
            from: "n1".into(),
            to: "n2".into(),
            edge_type: "has_gotcha".into(),
            weight: 0.9,
            coactivations: 5,
            metadata: None,
        });
        let s = g.summary();
        assert_eq!(s.node_count, 2);
        assert_eq!(s.edge_count, 1);
        assert_eq!(s.node_types, vec!["fact", "gotcha"]);
    }

    #[test]
    fn activation_mean_calculation() {
        let mut g = ContextGraph::new();
        let mut n1 = ContextNode::fact("n1", "a", "x");
        n1.activation = 0.8;
        let mut n2 = ContextNode::fact("n2", "b", "x");
        n2.activation = 0.6;
        g.add_node(n1);
        g.add_node(n2);
        let mean = g.activation_mean();
        assert!((mean - 0.7).abs() < 0.001);
    }

    #[test]
    fn temporal_decay_halves_at_half_life() {
        let mut g = ContextGraph::new();
        let mut n = ContextNode::fact("n1", "test", "x");
        n.activation = 1.0;
        n.decay_half_life_days = Some(30);
        n.created_at = Some(Utc::now() - chrono::Duration::days(30));
        g.add_node(n);

        g.apply_temporal_decay(Utc::now());
        assert!((g.nodes[0].activation - 0.5).abs() < 0.01);
    }

    #[test]
    fn node_by_id_lookup() {
        let mut g = ContextGraph::new();
        g.add_node(ContextNode::fact("alpha", "content a", "cat"));
        g.add_node(ContextNode::fact("beta", "content b", "cat"));
        assert_eq!(g.node_by_id("alpha").unwrap().content, "content a");
        assert!(g.node_by_id("gamma").is_none());
    }

    #[test]
    fn serde_roundtrip() {
        let mut g = ContextGraph::new();
        g.add_node(ContextNode::fact("n1", "test fact", "arch"));
        g.add_edge(ContextEdge {
            from: "n1".into(),
            to: "n1".into(),
            edge_type: "self_ref".into(),
            weight: 1.0,
            coactivations: 0,
            metadata: None,
        });
        let json = serde_json::to_string(&g).unwrap();
        let decoded: ContextGraph = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.nodes.len(), 1);
        assert_eq!(decoded.edges.len(), 1);
        assert_eq!(decoded.nodes[0].node_type, "fact");
    }

    #[test]
    fn empty_graph_activation_mean_is_zero() {
        let g = ContextGraph::new();
        assert_eq!(g.activation_mean(), 0.0);
    }

    #[test]
    fn decay_without_created_at_is_noop() {
        let mut g = ContextGraph::new();
        let mut n = ContextNode::fact("n1", "test", "x");
        n.activation = 1.0;
        n.decay_half_life_days = Some(30);
        n.created_at = None;
        g.add_node(n);
        g.apply_temporal_decay(Utc::now());
        assert!((g.nodes[0].activation - 1.0).abs() < 0.001);
    }

    #[test]
    fn decay_with_zero_half_life_is_noop() {
        let mut g = ContextGraph::new();
        let mut n = ContextNode::fact("n1", "test", "x");
        n.activation = 1.0;
        n.decay_half_life_days = Some(0);
        n.created_at = Some(Utc::now() - chrono::Duration::days(100));
        g.add_node(n);
        g.apply_temporal_decay(Utc::now());
        assert!((g.nodes[0].activation - 1.0).abs() < 0.001);
    }

    #[test]
    fn decay_without_half_life_is_noop() {
        let mut g = ContextGraph::new();
        let mut n = ContextNode::fact("n1", "test", "x");
        n.activation = 0.9;
        n.decay_half_life_days = None;
        n.created_at = Some(Utc::now() - chrono::Duration::days(365));
        g.add_node(n);
        g.apply_temporal_decay(Utc::now());
        assert!((g.nodes[0].activation - 0.9).abs() < 0.001);
    }

    #[test]
    fn decay_two_half_lives_quarters() {
        let mut g = ContextGraph::new();
        let mut n = ContextNode::fact("n1", "test", "x");
        n.activation = 1.0;
        n.decay_half_life_days = Some(30);
        n.created_at = Some(Utc::now() - chrono::Duration::days(60));
        g.add_node(n);
        g.apply_temporal_decay(Utc::now());
        assert!((g.nodes[0].activation - 0.25).abs() < 0.01);
    }

    #[test]
    fn code_symbol_factory() {
        let n = ContextNode::code_symbol("s1", "function", "main", "src/main.rs");
        assert_eq!(n.node_type, "code_function");
        assert_eq!(n.content, "main");
        assert_eq!(n.file_path.as_deref(), Some("src/main.rs"));
        assert_eq!(n.category.as_deref(), Some("function"));
    }

    #[test]
    fn gotcha_factory_content_format() {
        let n = ContextNode::gotcha("g1", "race condition", "use mutex");
        assert!(n.content.contains("race condition"));
        assert!(n.content.contains("---"));
        assert!(n.content.contains("use mutex"));
        assert_eq!(n.node_type, "gotcha");
    }

    #[test]
    fn node_types_deduplicates_and_sorts() {
        let mut g = ContextGraph::new();
        g.add_node(ContextNode::fact("a", "x", "c"));
        g.add_node(ContextNode::fact("b", "y", "c"));
        g.add_node(ContextNode::gotcha("c", "t", "r"));
        g.add_node(ContextNode::fact("d", "z", "c"));
        let types = g.node_types();
        assert_eq!(types, vec!["fact", "gotcha"]);
    }

    #[test]
    fn summary_freshness_is_most_recent() {
        let mut g = ContextGraph::new();
        let mut n1 = ContextNode::fact("n1", "old", "c");
        n1.created_at = Some(Utc::now() - chrono::Duration::days(10));
        let mut n2 = ContextNode::fact("n2", "new", "c");
        n2.created_at = Some(Utc::now());
        g.add_node(n1);
        g.add_node(n2);
        let s = g.summary();
        let freshness = s.freshness.unwrap();
        let age_secs = (Utc::now() - freshness).num_seconds().abs();
        assert!(age_secs < 5);
    }

    #[test]
    fn serde_preserves_type_field_name() {
        let n = ContextNode::fact("n1", "test", "arch");
        let json = serde_json::to_value(&n).unwrap();
        assert!(json.get("type").is_some());
        assert!(json.get("node_type").is_none());
    }

    #[test]
    fn serde_edge_preserves_type_field_name() {
        let e = ContextEdge {
            from: "a".into(),
            to: "b".into(),
            edge_type: "supports".into(),
            weight: 1.0,
            coactivations: 0,
            metadata: None,
        };
        let json = serde_json::to_value(&e).unwrap();
        assert!(json.get("type").is_some());
        assert!(json.get("edge_type").is_none());
    }

    #[test]
    fn default_values_in_deserialization() {
        let json = r#"{"id":"n1","type":"fact","content":"hello"}"#;
        let node: ContextNode = serde_json::from_str(json).unwrap();
        assert_eq!(node.activation, 1.0);
        assert!(node.category.is_none());
        assert!(node.supersedes.is_none());
    }

    #[test]
    fn edge_default_weight_in_deserialization() {
        let json = r#"{"from":"a","to":"b","type":"supports"}"#;
        let edge: ContextEdge = serde_json::from_str(json).unwrap();
        assert_eq!(edge.weight, 1.0);
        assert_eq!(edge.coactivations, 0);
    }
}
