use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct KnowledgeNodeRef {
    pub category: String,
    pub key: String,
}

impl KnowledgeNodeRef {
    #[must_use]
    pub fn new(category: &str, key: &str) -> Self {
        Self {
            category: category.trim().to_string(),
            key: key.trim().to_string(),
        }
    }

    #[must_use]
    pub fn id(&self) -> String {
        format!("{}/{}", self.category, self.key)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeEdgeKind {
    DependsOn,
    RelatedTo,
    Supports,
    Contradicts,
    Supersedes,
}

impl KnowledgeEdgeKind {
    #[must_use]
    pub fn parse(input: &str) -> Option<Self> {
        match input.trim().to_lowercase().as_str() {
            "depends_on" | "depends" => Some(Self::DependsOn),
            "related_to" | "related" => Some(Self::RelatedTo),
            "supports" | "support" => Some(Self::Supports),
            "contradicts" | "contradict" => Some(Self::Contradicts),
            "supersedes" | "supersede" => Some(Self::Supersedes),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            KnowledgeEdgeKind::DependsOn => "depends_on",
            KnowledgeEdgeKind::RelatedTo => "related_to",
            KnowledgeEdgeKind::Supports => "supports",
            KnowledgeEdgeKind::Contradicts => "contradicts",
            KnowledgeEdgeKind::Supersedes => "supersedes",
        }
    }
}

fn default_strength() -> f64 {
    0.5
}
fn default_decay_rate() -> f64 {
    0.02
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeEdge {
    pub from: KnowledgeNodeRef,
    pub to: KnowledgeNodeRef,
    pub kind: KnowledgeEdgeKind,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub last_seen: Option<DateTime<Utc>>,
    #[serde(default)]
    pub count: u32,
    pub source_session: String,
    #[serde(default = "default_strength")]
    pub strength: f64,
    #[serde(default = "default_decay_rate")]
    pub decay_rate: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct KnowledgeRelationGraph {
    pub project_hash: String,
    pub edges: Vec<KnowledgeEdge>,
    pub updated_at: DateTime<Utc>,
}

impl Default for KnowledgeRelationGraph {
    fn default() -> Self {
        Self {
            project_hash: String::new(),
            edges: Vec::new(),
            updated_at: Utc::now(),
        }
    }
}

impl KnowledgeRelationGraph {
    #[must_use]
    pub fn new(project_hash: &str) -> Self {
        Self {
            project_hash: project_hash.to_string(),
            edges: Vec::new(),
            updated_at: Utc::now(),
        }
    }

    pub fn path(project_hash: &str) -> Result<PathBuf, String> {
        let dir = crate::core::data_dir::lean_ctx_data_dir()?
            .join("knowledge")
            .join(project_hash);
        Ok(dir.join("relations.json"))
    }

    #[must_use]
    pub fn load(project_hash: &str) -> Option<Self> {
        let path = Self::path(project_hash).ok()?;
        let content = std::fs::read_to_string(&path).ok()?;
        let mut g = serde_json::from_str::<Self>(&content).ok()?;
        if g.project_hash.trim().is_empty() {
            g.project_hash = project_hash.to_string();
        }
        Some(g)
    }

    #[must_use]
    pub fn load_or_create(project_hash: &str) -> Self {
        Self::load(project_hash).unwrap_or_else(|| Self::new(project_hash))
    }

    pub fn save(&mut self) -> Result<(), String> {
        let path = Self::path(&self.project_hash)?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }

        self.updated_at = Utc::now();
        self.edges.sort_by(|a, b| {
            a.from
                .category
                .cmp(&b.from.category)
                .then_with(|| a.from.key.cmp(&b.from.key))
                .then_with(|| a.kind.as_str().cmp(b.kind.as_str()))
                .then_with(|| a.to.category.cmp(&b.to.category))
                .then_with(|| a.to.key.cmp(&b.to.key))
                .then_with(|| b.count.cmp(&a.count))
                .then_with(|| b.last_seen.cmp(&a.last_seen))
                .then_with(|| b.created_at.cmp(&a.created_at))
        });

        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&path, json).map_err(|e| e.to_string())
    }

    pub fn upsert_edge(
        &mut self,
        from: KnowledgeNodeRef,
        to: KnowledgeNodeRef,
        kind: KnowledgeEdgeKind,
        session_id: &str,
    ) -> bool {
        let now = Utc::now();
        if let Some(e) = self
            .edges
            .iter_mut()
            .find(|e| e.from == from && e.to == to && e.kind == kind)
        {
            e.count = e.count.saturating_add(1).max(1);
            e.last_seen = Some(now);
            e.source_session = session_id.to_string();
            e.strength = (e.strength + 0.1 * (1.0 - e.strength)).min(1.0);
            self.updated_at = now;
            return false;
        }

        self.edges.push(KnowledgeEdge {
            from,
            to,
            kind,
            created_at: now,
            last_seen: Some(now),
            count: 1,
            source_session: session_id.to_string(),
            strength: default_strength(),
            decay_rate: default_decay_rate(),
        });
        self.updated_at = now;
        true
    }

    pub fn remove_edge(
        &mut self,
        from: &KnowledgeNodeRef,
        to: &KnowledgeNodeRef,
        kind: Option<KnowledgeEdgeKind>,
    ) -> usize {
        let before = self.edges.len();
        self.edges.retain(|e| {
            if &e.from != from || &e.to != to {
                return true;
            }
            if let Some(k) = kind {
                e.kind != k
            } else {
                false
            }
        });
        before.saturating_sub(self.edges.len())
    }

    pub fn enforce_cap(&mut self, max_edges: usize) -> bool {
        if max_edges == 0 || self.edges.len() <= max_edges {
            return false;
        }

        self.edges.sort_by(|a, b| {
            b.count
                .cmp(&a.count)
                .then_with(|| b.last_seen.cmp(&a.last_seen))
                .then_with(|| b.created_at.cmp(&a.created_at))
                .then_with(|| a.from.category.cmp(&b.from.category))
                .then_with(|| a.from.key.cmp(&b.from.key))
                .then_with(|| a.kind.as_str().cmp(b.kind.as_str()))
                .then_with(|| a.to.category.cmp(&b.to.category))
                .then_with(|| a.to.key.cmp(&b.to.key))
        });

        self.edges.truncate(max_edges);
        true
    }

    /// Hebbian strengthening: saturating formula so strength approaches but never exceeds 1.0
    pub fn strengthen_edge(
        &mut self,
        from: &KnowledgeNodeRef,
        to: &KnowledgeNodeRef,
        amount: f64,
    ) -> bool {
        if let Some(e) = self
            .edges
            .iter_mut()
            .find(|e| &e.from == from && &e.to == to)
        {
            e.strength = (e.strength + amount * (1.0 - e.strength)).min(1.0);
            e.last_seen = Some(Utc::now());
            e.count = e.count.saturating_add(1);
            return true;
        }
        if let Some(e) = self
            .edges
            .iter_mut()
            .find(|e| &e.from == to && &e.to == from)
        {
            e.strength = (e.strength + amount * (1.0 - e.strength)).min(1.0);
            e.last_seen = Some(Utc::now());
            e.count = e.count.saturating_add(1);
            return true;
        }
        false
    }

    /// Time-based exponential decay on all edge strengths
    pub fn decay_all_edges(&mut self, days_elapsed: f64) {
        for e in &mut self.edges {
            e.strength *= (1.0 - e.decay_rate).powf(days_elapsed);
            e.strength = e.strength.max(0.0);
        }
    }

    /// Remove edges whose strength has fallen below `threshold`
    pub fn prune_weak_edges(&mut self, threshold: f64) -> usize {
        let before = self.edges.len();
        self.edges.retain(|e| e.strength >= threshold);
        before - self.edges.len()
    }
}

#[must_use]
pub fn parse_node_ref(input: &str) -> Option<KnowledgeNodeRef> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }

    if let Some((cat, key)) = s.split_once('/') {
        let cat = cat.trim();
        let key = key.trim();
        if !cat.is_empty() && !key.is_empty() {
            return Some(KnowledgeNodeRef::new(cat, key));
        }
    }
    if let Some((cat, key)) = s.split_once(':') {
        let cat = cat.trim();
        let key = key.trim();
        if !cat.is_empty() && !key.is_empty() {
            return Some(KnowledgeNodeRef::new(cat, key));
        }
    }

    None
}

#[must_use]
pub fn format_mermaid(edges: &[KnowledgeEdge]) -> String {
    if edges.is_empty() {
        return "graph TD\n  %% no relations".to_string();
    }

    fn id_for(n: &KnowledgeNodeRef) -> String {
        let mut out = String::from("K_");
        for ch in n.id().chars() {
            if ch.is_ascii_alphanumeric() {
                out.push(ch);
            } else {
                out.push('_');
            }
        }
        out
    }

    let mut lines = Vec::new();
    lines.push("graph TD".to_string());
    for e in edges {
        let from = id_for(&e.from);
        let to = id_for(&e.to);
        let from_label = e.from.id();
        let to_label = e.to.id();
        lines.push(format!(
            "  {from}[\"{from_label}\"] -->|{}| {to}[\"{to_label}\"]",
            e.kind.as_str()
        ));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strengthen_edge_saturating() {
        let mut graph = KnowledgeRelationGraph::new("test");
        let from = KnowledgeNodeRef::new("a", "1");
        let to = KnowledgeNodeRef::new("b", "2");
        graph.upsert_edge(from.clone(), to.clone(), KnowledgeEdgeKind::RelatedTo, "s1");

        let initial = graph.edges[0].strength;
        assert!((initial - 0.5).abs() < 0.01);

        graph.strengthen_edge(&from, &to, 0.3);
        assert!(graph.edges[0].strength > initial);
        assert!(graph.edges[0].strength <= 1.0);

        for _ in 0..100 {
            graph.strengthen_edge(&from, &to, 0.5);
        }
        assert!(graph.edges[0].strength <= 1.0);
        assert!(graph.edges[0].strength > 0.99);
    }

    #[test]
    fn decay_reduces_strength() {
        let mut graph = KnowledgeRelationGraph::new("test");
        let from = KnowledgeNodeRef::new("a", "1");
        let to = KnowledgeNodeRef::new("b", "2");
        graph.upsert_edge(from, to, KnowledgeEdgeKind::RelatedTo, "s1");

        let initial = graph.edges[0].strength;
        graph.decay_all_edges(10.0);
        assert!(graph.edges[0].strength < initial);
        assert!(graph.edges[0].strength > 0.0);
    }

    #[test]
    fn prune_weak_edges_removes_below_threshold() {
        let mut graph = KnowledgeRelationGraph::new("test");
        graph.upsert_edge(
            KnowledgeNodeRef::new("a", "1"),
            KnowledgeNodeRef::new("b", "2"),
            KnowledgeEdgeKind::RelatedTo,
            "s1",
        );
        graph.upsert_edge(
            KnowledgeNodeRef::new("c", "3"),
            KnowledgeNodeRef::new("d", "4"),
            KnowledgeEdgeKind::RelatedTo,
            "s2",
        );

        graph.edges[1].strength = 0.01;

        let removed = graph.prune_weak_edges(0.05);
        assert_eq!(removed, 1);
        assert_eq!(graph.edges.len(), 1);
    }

    #[test]
    fn backward_compatible_edge_deserialization() {
        let json = r#"{
            "from": {"category": "a", "key": "1"},
            "to": {"category": "b", "key": "2"},
            "kind": "related_to",
            "created_at": "2024-01-01T00:00:00Z",
            "count": 1,
            "source_session": "s1"
        }"#;
        let edge: KnowledgeEdge = serde_json::from_str(json).unwrap();
        assert!((edge.strength - 0.5).abs() < 0.01);
        assert!((edge.decay_rate - 0.02).abs() < 0.001);
    }

    #[test]
    fn strengthen_edge_bidirectional() {
        let mut graph = KnowledgeRelationGraph::new("test");
        let from = KnowledgeNodeRef::new("a", "1");
        let to = KnowledgeNodeRef::new("b", "2");
        graph.upsert_edge(from.clone(), to.clone(), KnowledgeEdgeKind::RelatedTo, "s1");

        let found = graph.strengthen_edge(&to, &from, 0.2);
        assert!(found);
        assert!(graph.edges[0].strength > 0.5);
    }
}
