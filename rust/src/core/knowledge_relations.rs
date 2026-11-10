use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Average relations per current fact the graph may hold before its weakest
/// edges are pruned. Knowledge has no fact limit; this bounds edge *density*
/// (an edge-producing loop cannot grow the graph faster than the facts it
/// links), so the bound grows with the store instead of capping it.
pub const EDGES_PER_FACT: usize = 8;
/// Floor of [`edge_bound`], so a young store keeps room for relations (the
/// bound the old default of 200 facts gave).
pub const MIN_EDGE_BOUND: usize = 1_600;

/// The relation-edge bound for a store with `current_facts` current facts.
pub fn edge_bound(current_facts: usize) -> usize {
    current_facts
        .saturating_mul(EDGES_PER_FACT)
        .max(MIN_EDGE_BOUND)
}

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

/// The kinds this store writes. Every released reader of `relations.json`
/// deserializes the kind strictly, so a new kind is added only once the
/// oldest supported reader parses it; kinds a newer writer stored are kept
/// as written (see [`KnowledgeRelationGraph::load`]).
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
    /// Ephemeral incident-edge lookup rebuilt after JSON load and mutations.
    /// Skipping it preserves the existing relations.json format.
    #[serde(skip)]
    pub(crate) edge_positions: BTreeMap<String, Vec<usize>>,
    /// Edges this build cannot read (a kind or shape a newer writer stored),
    /// kept as written and saved back unchanged, so no reader drops what it
    /// does not understand.
    #[serde(skip)]
    foreign_edges: Vec<serde_json::Value>,
    /// The file exists but does not parse: [`Self::save`] refuses, so a
    /// writer never replaces relations it could not read with an empty set.
    #[serde(skip)]
    unreadable: bool,
}

impl Default for KnowledgeRelationGraph {
    fn default() -> Self {
        Self {
            project_hash: String::new(),
            edges: Vec::new(),
            updated_at: Utc::now(),
            edge_positions: BTreeMap::new(),
            foreign_edges: Vec::new(),
            unreadable: false,
        }
    }
}

impl KnowledgeRelationGraph {
    pub fn new(project_hash: &str) -> Self {
        Self {
            project_hash: project_hash.to_string(),
            edges: Vec::new(),
            updated_at: Utc::now(),
            edge_positions: BTreeMap::new(),
            foreign_edges: Vec::new(),
            unreadable: false,
        }
    }

    pub fn path(project_hash: &str) -> Result<PathBuf, String> {
        let dir = crate::core::data_dir::lean_ctx_data_dir()?
            .join("knowledge")
            .join(project_hash);
        Ok(dir.join("relations.json"))
    }

    pub fn load(project_hash: &str) -> Option<Self> {
        Self::load_checked(project_hash).ok().flatten()
    }

    /// `Ok(None)` when there is no file, `Err` when it exists but cannot be
    /// read or parsed.
    fn load_checked(project_hash: &str) -> Result<Option<Self>, String> {
        let path = Self::path(project_hash)?;
        let content = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let mut g = Self::from_json(&content)
            .ok_or_else(|| format!("{} does not parse", path.display()))?;
        if g.project_hash.trim().is_empty() {
            g.project_hash = project_hash.to_string();
        }
        g.rebuild_index();
        Ok(Some(g))
    }

    /// Parse a stored graph edge by edge: an edge this build cannot read is
    /// kept in [`Self::foreign_edges`] instead of failing the whole file.
    fn from_json(content: &str) -> Option<Self> {
        let mut doc: serde_json::Value = serde_json::from_str(content).ok()?;
        let raw = doc.as_object_mut()?.remove("edges");
        let mut g: Self = serde_json::from_value(doc).ok()?;
        match raw {
            None | Some(serde_json::Value::Null) => {}
            Some(serde_json::Value::Array(raw)) => {
                for edge in raw {
                    match serde_json::from_value::<KnowledgeEdge>(edge.clone()) {
                        Ok(edge) => g.edges.push(edge),
                        Err(_) => g.foreign_edges.push(edge),
                    }
                }
            }
            Some(_) => return None,
        }
        Some(g)
    }

    pub fn load_or_create(project_hash: &str) -> Self {
        match Self::load_checked(project_hash) {
            Ok(Some(g)) => g,
            Ok(None) => Self::new(project_hash),
            Err(error) => {
                tracing::warn!("[knowledge] relations not loaded, kept as they are: {error}");
                let mut g = Self::new(project_hash);
                g.unreadable = true;
                g
            }
        }
    }

    /// Load, change and save the graph under the project's knowledge lock,
    /// the in-process and cross-process lock fact edits and the cognition loop
    /// hold, so a concurrent writer (another process, the dashboard, the
    /// cognition loop) never loses an edge. `change` returns `None` when it
    /// changed nothing; the graph is then not written. Must not be called while
    /// that lock is held.
    pub fn mutate_locked<T>(
        project_root: &str,
        project_hash: &str,
        change: impl FnOnce(&mut Self) -> Option<T>,
    ) -> Result<Option<T>, String> {
        crate::core::knowledge::ProjectKnowledge::with_project_lock_checked(project_root, || {
            let mut graph = Self::load_or_create(project_hash);
            match change(&mut graph) {
                Some(output) => graph.save().map(|()| Some(output)),
                None => Ok(None),
            }
        })?
    }

    pub fn save(&mut self) -> Result<(), String> {
        if self.unreadable {
            return Err(
                "relations.json could not be read; it is not overwritten (fix or remove it)"
                    .to_string(),
            );
        }
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
        self.rebuild_index();

        let json = if self.foreign_edges.is_empty() {
            serde_json::to_string_pretty(self)
        } else {
            // Foreign edges follow the readable ones, in the order they were read.
            serde_json::to_value(&*self).and_then(|mut doc| {
                if let Some(serde_json::Value::Array(edges)) = doc.get_mut("edges") {
                    edges.extend(self.foreign_edges.iter().cloned());
                }
                serde_json::to_string_pretty(&doc)
            })
        }
        .map_err(|e| e.to_string())?;
        // `load` treats an unparsable file as absent; a torn write must not
        // turn into an empty graph that the next save persists.
        crate::core::atomic_fs::try_atomic_write(&path, json.as_bytes(), None)
            .map_err(|e| e.to_string())
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
        self.rebuild_index();
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
        let removed = before.saturating_sub(self.edges.len());
        if removed > 0 {
            self.rebuild_index();
        }
        removed
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
        self.rebuild_index();
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
        let removed = before - self.edges.len();
        if removed > 0 {
            self.rebuild_index();
        }
        removed
    }

    /// Rebuilds the in-memory incident-edge index from persisted edge positions.
    pub(crate) fn rebuild_index(&mut self) {
        self.edge_positions.clear();
        for (position, edge) in self.edges.iter().enumerate() {
            self.edge_positions
                .entry(edge.from.id())
                .or_default()
                .push(position);
            if edge.to != edge.from {
                self.edge_positions
                    .entry(edge.to.id())
                    .or_default()
                    .push(position);
            }
        }
    }

    /// Returns only the edges incident to `node`, using the load-time index.
    pub fn incident_edges(&self, node: &KnowledgeNodeRef) -> Vec<&KnowledgeEdge> {
        self.edge_positions
            .get(&node.id())
            .into_iter()
            .flatten()
            .filter_map(|&position| self.edges.get(position))
            .collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Parallel writers each add their own edge; under the knowledge lock no
    /// read-modify-write drops another writer's edge.
    #[test]
    fn concurrent_locked_writers_keep_every_edge() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let project = tempfile::tempdir().unwrap();
        let root = project.path().to_string_lossy().into_owned();
        let hash = crate::core::project_hash::hash_project_root(&root);
        // Six writers race as well as sixteen did, and stay inside the lock's
        // 10 s wait on a loaded CI host (sixteen serialized fsyncs did not).
        const WRITERS: usize = 6;
        std::thread::scope(|s| {
            for i in 0..WRITERS {
                let (root, hash) = (&root, &hash);
                s.spawn(move || {
                    KnowledgeRelationGraph::mutate_locked(root, hash, |graph| {
                        graph.upsert_edge(
                            KnowledgeNodeRef::new("a", &format!("{i}")),
                            KnowledgeNodeRef::new("b", "target"),
                            KnowledgeEdgeKind::RelatedTo,
                            "s1",
                        );
                        Some(())
                    })
                    .expect("locked write");
                });
            }
        });
        let graph = KnowledgeRelationGraph::load(&hash).expect("saved graph");
        assert_eq!(
            graph.edges.len(),
            WRITERS,
            "an edge was lost to a concurrent write"
        );

        // A change that changes nothing writes nothing.
        let untouched = KnowledgeRelationGraph::mutate_locked(&root, &hash, |_| None::<()>);
        assert_eq!(untouched, Ok(None));
    }

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

    /// A file holding an old-shape edge and an edge of a kind this build does
    /// not know survives load → change → save: the old edge is read, the
    /// unknown one is written back as it was, and a second save of the
    /// same graph writes the same edges.
    #[test]
    fn unknown_kinds_survive_a_write() {
        let _isolated = crate::core::data_dir::isolated_data_dir();
        let project = tempfile::tempdir().unwrap();
        let root = project.path().to_string_lossy().into_owned();
        let hash = crate::core::project_hash::hash_project_root(&root);
        let future = serde_json::json!({
            "from": {"category": "a", "key": "1"},
            "to": {"category": "c", "key": "3"},
            "kind": "answers",
            "created_at": "2026-11-01T00:00:00Z",
            "count": 2,
            "source_session": "newer-build"
        });
        let doc = serde_json::json!({
            "project_hash": hash,
            "edges": [
                {"from": {"category": "a", "key": "1"}, "to": {"category": "b", "key": "2"},
                 "kind": "related_to", "created_at": "2024-01-01T00:00:00Z",
                 "count": 1, "source_session": "s1"},
                future
            ],
            "updated_at": "2024-01-01T00:00:00Z"
        });
        let path = KnowledgeRelationGraph::path(&hash).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, doc.to_string()).unwrap();

        KnowledgeRelationGraph::mutate_locked(&root, &hash, |g| {
            assert_eq!(g.edges.len(), 1, "the old-shape edge is read");
            g.upsert_edge(
                KnowledgeNodeRef::new("a", "1"),
                KnowledgeNodeRef::new("d", "4"),
                KnowledgeEdgeKind::DependsOn,
                "s2",
            );
            Some(())
        })
        .expect("write");
        let edges_on_disk = || -> Vec<serde_json::Value> {
            let saved: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            saved["edges"].as_array().unwrap().clone()
        };
        let first = edges_on_disk();
        assert_eq!(first.len(), 3);
        assert_eq!(first.last(), Some(&future), "kept exactly as written");

        let mut again = KnowledgeRelationGraph::load(&hash).unwrap();
        again.save().unwrap();
        assert_eq!(edges_on_disk(), first, "same graph, same edges on disk");
    }

    /// A file that does not parse is never replaced by an empty graph.
    #[test]
    fn an_unreadable_file_is_not_overwritten() {
        let _isolated = crate::core::data_dir::isolated_data_dir();
        let project = tempfile::tempdir().unwrap();
        let root = project.path().to_string_lossy().into_owned();
        let hash = crate::core::project_hash::hash_project_root(&root);
        let path = KnowledgeRelationGraph::path(&hash).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{\"edges\": [ torn").unwrap();

        let written = KnowledgeRelationGraph::mutate_locked(&root, &hash, |g| {
            g.upsert_edge(
                KnowledgeNodeRef::new("a", "1"),
                KnowledgeNodeRef::new("b", "2"),
                KnowledgeEdgeKind::DependsOn,
                "s1",
            );
            Some(())
        });
        assert!(written.is_err(), "{written:?}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "{\"edges\": [ torn"
        );
    }

    #[test]
    fn load_rebuilds_ephemeral_index_without_changing_json() {
        let _isolated = crate::core::data_dir::isolated_data_dir();
        let project_hash = "knowledge-relations-index";
        let from = KnowledgeNodeRef::new("architecture", "storage");
        let to = KnowledgeNodeRef::new("dependency", "sqlite");
        let mut graph = KnowledgeRelationGraph::new(project_hash);
        graph.upsert_edge(
            from.clone(),
            to,
            KnowledgeEdgeKind::DependsOn,
            "test-session",
        );
        graph.save().unwrap();

        let loaded = KnowledgeRelationGraph::load(project_hash).expect("saved graph should load");
        assert_eq!(loaded.edge_positions.get(&from.id()).map(Vec::len), Some(1));
        assert_eq!(loaded.incident_edges(&from).len(), 1);

        let json = serde_json::to_string(&loaded).unwrap();
        assert!(!json.contains("\"edge_positions\""));
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
