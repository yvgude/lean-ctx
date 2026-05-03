use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct KnowledgeNodeRef {
    pub category: String,
    pub key: String,
}

impl KnowledgeNodeRef {
    pub fn new(category: &str, key: &str) -> Self {
        Self {
            category: category.trim().to_string(),
            key: key.trim().to_string(),
        }
    }

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

    pub fn load(project_hash: &str) -> Option<Self> {
        let path = Self::path(project_hash).ok()?;
        let content = std::fs::read_to_string(&path).ok()?;
        let mut g = serde_json::from_str::<Self>(&content).ok()?;
        if g.project_hash.trim().is_empty() {
            g.project_hash = project_hash.to_string();
        }
        Some(g)
    }

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
}

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
