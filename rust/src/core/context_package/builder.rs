use chrono::Utc;
use sha2::{Digest, Sha256};

use super::content::{
    GotchaExport, GotchasLayer, GraphEdgeExport, GraphLayer, GraphNodeExport, KnowledgeLayer,
    PackageContent, PatternsLayer, SessionDecision, SessionFinding, SessionLayer,
};
use super::manifest::{
    CompatibilitySpec, PackageIntegrity, PackageLayer, PackageManifest, PackageProvenance,
    PackageStats,
};

pub struct PackageBuilder {
    name: String,
    version: String,
    description: String,
    author: Option<String>,
    scope: Option<String>,
    tags: Vec<String>,
    visibility: Option<String>,
    compatibility: CompatibilitySpec,
    content: PackageContent,
    project_hash: Option<String>,
    session_id: Option<String>,
    level: u32,
}

impl PackageBuilder {
    #[must_use]
    pub fn new(name: &str, version: &str) -> Self {
        Self {
            name: name.to_string(),
            version: version.to_string(),
            description: String::new(),
            author: None,
            scope: None,
            tags: Vec::new(),
            visibility: None,
            compatibility: CompatibilitySpec::default(),
            content: PackageContent::default(),
            project_hash: None,
            session_id: None,
            level: 1,
        }
    }

    #[must_use]
    pub fn description(mut self, desc: &str) -> Self {
        self.description = desc.to_string();
        self
    }

    #[must_use]
    pub fn author(mut self, author: &str) -> Self {
        self.author = Some(author.to_string());
        self
    }

    #[must_use]
    pub fn tags(mut self, tags: Vec<String>) -> Self {
        self.tags = tags;
        self
    }

    #[must_use]
    pub fn compatibility(mut self, spec: CompatibilitySpec) -> Self {
        self.compatibility = spec;
        self
    }

    #[must_use]
    pub fn project_hash(mut self, hash: &str) -> Self {
        self.project_hash = Some(hash.to_string());
        self
    }

    #[must_use]
    pub fn session_id(mut self, id: &str) -> Self {
        self.session_id = Some(id.to_string());
        self
    }

    #[must_use]
    pub fn scope(mut self, scope: &str) -> Self {
        self.scope = Some(scope.to_string());
        self
    }

    /// Mark the package `private` for the hosted registry (GL #524).
    #[must_use]
    pub fn private(mut self) -> Self {
        self.visibility = Some("private".to_string());
        self
    }

    #[must_use]
    pub fn level(mut self, level: u32) -> Self {
        self.level = level.clamp(1, 3);
        self
    }

    #[must_use]
    pub fn add_knowledge_from_project(mut self, project_root: &str) -> Self {
        let knowledge = crate::core::knowledge::ProjectKnowledge::load_or_create(project_root);

        if knowledge.facts.is_empty()
            && knowledge.patterns.is_empty()
            && knowledge.history.is_empty()
        {
            return self;
        }

        self.content.knowledge = Some(KnowledgeLayer {
            facts: knowledge.facts,
            patterns: knowledge.patterns,
            insights: knowledge.history,
            exported_at: Utc::now(),
        });

        self
    }

    #[must_use]
    pub fn add_graph_from_project(mut self, project_root: &str) -> Self {
        let Ok(graph) = crate::core::property_graph::CodeGraph::open(project_root) else {
            return self;
        };

        let nodes = export_graph_nodes(&graph);
        let edges = export_graph_edges(&graph);

        if nodes.is_empty() && edges.is_empty() {
            return self;
        }

        self.content.graph = Some(GraphLayer {
            nodes,
            edges,
            exported_at: Utc::now(),
        });

        self
    }

    #[must_use]
    pub fn add_session(mut self, session: &crate::core::session::SessionState) -> Self {
        let has_content = session.task.is_some()
            || !session.findings.is_empty()
            || !session.decisions.is_empty()
            || !session.next_steps.is_empty()
            || !session.files_touched.is_empty();

        if !has_content {
            return self;
        }

        let layer = SessionLayer {
            task_description: session.task.as_ref().map(|t| t.description.clone()),
            findings: session
                .findings
                .iter()
                .map(|f| SessionFinding {
                    summary: f.summary.clone(),
                    file: f.file.clone(),
                    line: f.line,
                })
                .collect(),
            decisions: session
                .decisions
                .iter()
                .map(|d| SessionDecision {
                    summary: d.summary.clone(),
                    rationale: d.rationale.clone(),
                })
                .collect(),
            next_steps: session.next_steps.clone(),
            files_touched: session
                .files_touched
                .iter()
                .map(|f| f.path.clone())
                .collect(),
            exported_at: Utc::now(),
        };

        self.content.session = Some(layer);
        self
    }

    #[must_use]
    pub fn add_patterns_from_project(mut self, project_root: &str) -> Self {
        let knowledge = crate::core::knowledge::ProjectKnowledge::load_or_create(project_root);

        if knowledge.patterns.is_empty() {
            return self;
        }

        self.content.patterns = Some(PatternsLayer {
            patterns: knowledge.patterns,
            exported_at: Utc::now(),
        });

        self
    }

    pub fn build_context_graph(&mut self, project_root: &str) {
        use super::graph_model::{ContextEdge, ContextGraph, ContextNode};

        let mut graph = ContextGraph::new();
        let mut node_count: u32 = 0;

        let mut next_id = || -> String {
            node_count += 1;
            format!("N{node_count}")
        };

        let knowledge = crate::core::knowledge::ProjectKnowledge::load_or_create(project_root);
        let mut fact_id_map: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();

        for fact in &knowledge.facts {
            let id = next_id();
            let mut node = ContextNode::fact(&id, &fact.value, &fact.category);
            node.confidence = Some(fact.confidence);
            node.source = Some(fact.source_session.clone());
            node.created_at = Some(fact.created_at);
            if let Some(ref s) = fact.supersedes {
                node.supersedes = fact_id_map.get(s).cloned();
            }
            let map_key = format!("{}/{}", fact.category, fact.key);
            fact_id_map.insert(map_key, id.clone());
            graph.add_node(node);
        }

        for pattern in &knowledge.patterns {
            let id = next_id();
            let mut node = ContextNode::fact(&id, &pattern.description, "pattern");
            node.node_type = "pattern".into();
            node.created_at = Some(pattern.created_at);
            graph.add_node(node);
        }

        for insight in &knowledge.history {
            let id = next_id();
            let mut node = ContextNode::fact(&id, &insight.summary, "insight");
            node.node_type = "insight".into();
            node.created_at = Some(insight.timestamp);
            graph.add_node(node);
        }

        let gotcha_store = crate::core::gotcha_tracker::GotchaStore::load(project_root);
        for g in &gotcha_store.gotchas {
            let id = next_id();
            let mut node = ContextNode::gotcha(&id, &g.trigger, &g.resolution);
            node.category = Some(g.category.short_label().to_string());
            node.confidence = Some(g.confidence);
            node.created_at = Some(g.first_seen);
            graph.add_node(node);
        }

        if self.level >= 2 {
            if let Ok(code_graph) = crate::core::property_graph::CodeGraph::open(project_root) {
                let v1_nodes = export_graph_nodes(&code_graph);
                let v1_edges = export_graph_edges(&code_graph);

                let mut code_node_map: std::collections::HashMap<(String, String), String> =
                    std::collections::HashMap::new();

                for n in &v1_nodes {
                    let id = next_id();
                    let node = ContextNode::code_symbol(&id, &n.kind, &n.name, &n.file_path);
                    code_node_map.insert((n.file_path.clone(), n.name.clone()), id.clone());
                    graph.add_node(node);
                }

                for e in &v1_edges {
                    let src_key = (e.source_path.clone(), e.source_name.clone());
                    let tgt_key = (e.target_path.clone(), e.target_name.clone());
                    if let (Some(from), Some(to)) =
                        (code_node_map.get(&src_key), code_node_map.get(&tgt_key))
                    {
                        graph.add_edge(ContextEdge {
                            from: from.clone(),
                            to: to.clone(),
                            edge_type: e.kind.clone(),
                            weight: 1.0,
                            coactivations: 0,
                            metadata: e.metadata.clone(),
                        });
                    }
                }
            }

            let phash = crate::core::project_hash::hash_project_root(project_root);
            if let Some(rel_graph) =
                crate::core::knowledge_relations::KnowledgeRelationGraph::load(&phash)
            {
                for edge in &rel_graph.edges {
                    let from_key = edge.from.id();
                    let to_key = edge.to.id();
                    if let (Some(from_id), Some(to_id)) =
                        (fact_id_map.get(&from_key), fact_id_map.get(&to_key))
                    {
                        let edge_type = match edge.kind {
                            crate::core::knowledge_relations::KnowledgeEdgeKind::DependsOn => {
                                "depends_on"
                            }
                            crate::core::knowledge_relations::KnowledgeEdgeKind::RelatedTo => {
                                "related_to"
                            }
                            crate::core::knowledge_relations::KnowledgeEdgeKind::Supports => {
                                "supports"
                            }
                            crate::core::knowledge_relations::KnowledgeEdgeKind::Contradicts => {
                                "contradicts"
                            }
                            crate::core::knowledge_relations::KnowledgeEdgeKind::Supersedes => {
                                "supersedes"
                            }
                        };
                        graph.add_edge(ContextEdge {
                            from: from_id.clone(),
                            to: to_id.clone(),
                            edge_type: edge_type.into(),
                            weight: edge.strength,
                            coactivations: edge.count,
                            metadata: None,
                        });
                    }
                }
            }
        }

        if !graph.nodes.is_empty() {
            self.content.context_graph = Some(graph);
        }
    }

    #[must_use]
    pub fn add_gotchas_from_project(mut self, project_root: &str) -> Self {
        let store = crate::core::gotcha_tracker::GotchaStore::load(project_root);
        if store.gotchas.is_empty() {
            return self;
        }

        self.content.gotchas = Some(GotchasLayer {
            gotchas: store
                .gotchas
                .iter()
                .map(|g| GotchaExport {
                    id: g.id.clone(),
                    category: g.category.short_label().to_string(),
                    severity: match g.severity {
                        crate::core::gotcha_tracker::GotchaSeverity::Critical => "critical".into(),
                        crate::core::gotcha_tracker::GotchaSeverity::Warning => "warning".into(),
                        crate::core::gotcha_tracker::GotchaSeverity::Info => "info".into(),
                    },
                    trigger: g.trigger.clone(),
                    resolution: g.resolution.clone(),
                    file_patterns: g.file_patterns.clone(),
                    confidence: g.confidence,
                })
                .collect(),
            exported_at: Utc::now(),
        });

        self
    }

    pub fn build(self) -> Result<(PackageManifest, PackageContent), String> {
        if self.name.is_empty() {
            return Err("package name is required".into());
        }
        if self.version.is_empty() {
            return Err("package version is required".into());
        }
        if self.content.is_empty() {
            return Err("package has no content — add at least one layer".into());
        }

        let is_v2 = self.content.context_graph.is_some();

        let mut layers = Vec::new();
        if self.content.knowledge.is_some() {
            layers.push(PackageLayer::Knowledge);
        }
        if self.content.graph.is_some() {
            layers.push(PackageLayer::Graph);
        }
        if self.content.session.is_some() {
            layers.push(PackageLayer::Session);
        }
        if self.content.patterns.is_some() {
            layers.push(PackageLayer::Patterns);
        }
        if self.content.gotchas.is_some() {
            layers.push(PackageLayer::Gotchas);
        }

        let content_json = serde_json::to_string(&self.content).map_err(|e| e.to_string())?;
        let content_bytes = content_json.as_bytes();

        let content_hash = sha256_hex(content_bytes);
        let sha256 =
            sha256_hex(format!("{}:{}:{}", self.name, self.version, content_hash).as_bytes());

        let stats = compute_stats(&self.content);

        let schema_version = if is_v2 {
            crate::core::contracts::CONTEXT_PACKAGE_V2_SCHEMA_VERSION
        } else {
            crate::core::contracts::CONTEXT_PACKAGE_V1_SCHEMA_VERSION
        };

        let graph_summary = self
            .content
            .context_graph
            .as_ref()
            .map(super::graph_model::ContextGraph::summary);

        let manifest = PackageManifest {
            schema_version,
            conformance_level: if is_v2 { Some(self.level) } else { None },
            name: self.name,
            version: self.version,
            description: self.description,
            author: self.author,
            scope: self.scope,
            created_at: Utc::now(),
            updated_at: None,
            layers,
            dependencies: Vec::new(),
            tags: self.tags,
            visibility: self.visibility,
            integrity: PackageIntegrity {
                sha256,
                content_hash,
                byte_size: content_bytes.len() as u64,
            },
            provenance: PackageProvenance {
                tool: "lean-ctx".into(),
                tool_version: env!("CARGO_PKG_VERSION").into(),
                project_hash: self.project_hash,
                source_session_id: self.session_id,
            },
            compatibility: self.compatibility,
            stats,
            signature: None,
            graph_summary,
            marketplace: None,
        };

        manifest.validate().map_err(|errs| errs.join("; "))?;

        Ok((manifest, self.content))
    }
}

fn export_graph_nodes(graph: &crate::core::property_graph::CodeGraph) -> Vec<GraphNodeExport> {
    let conn = graph.connection();
    let Ok(mut stmt) =
        conn.prepare("SELECT kind, name, file_path, line_start, line_end, metadata FROM nodes")
    else {
        tracing::warn!("ctxpkg: failed to prepare graph nodes query");
        return Vec::new();
    };

    let Ok(rows) = stmt.query_map([], |row| {
        let line_start: Option<i64> = row.get(3)?;
        let line_end: Option<i64> = row.get(4)?;
        Ok(GraphNodeExport {
            kind: row.get(0)?,
            name: row.get(1)?,
            file_path: row.get(2)?,
            line_start: line_start.map(|v| v as usize),
            line_end: line_end.map(|v| v as usize),
            metadata: row.get(5)?,
        })
    }) else {
        tracing::warn!("ctxpkg: failed to query graph nodes");
        return Vec::new();
    };

    let mut nodes = Vec::new();
    for row in rows {
        match row {
            Ok(n) => nodes.push(n),
            Err(e) => tracing::warn!("ctxpkg: skipping graph node: {e}"),
        }
    }
    nodes
}

fn export_graph_edges(graph: &crate::core::property_graph::CodeGraph) -> Vec<GraphEdgeExport> {
    let conn = graph.connection();
    let sql = "
        SELECT n1.file_path, n1.name, n2.file_path, n2.name, e.kind, e.metadata
        FROM edges e
        JOIN nodes n1 ON e.source_id = n1.id
        JOIN nodes n2 ON e.target_id = n2.id
    ";
    let Ok(mut stmt) = conn.prepare(sql) else {
        tracing::warn!("ctxpkg: failed to prepare graph edges query");
        return Vec::new();
    };

    let Ok(rows) = stmt.query_map([], |row| {
        Ok(GraphEdgeExport {
            source_path: row.get(0)?,
            source_name: row.get(1)?,
            target_path: row.get(2)?,
            target_name: row.get(3)?,
            kind: row.get(4)?,
            metadata: row.get(5)?,
        })
    }) else {
        tracing::warn!("ctxpkg: failed to query graph edges");
        return Vec::new();
    };

    let mut edges = Vec::new();
    for row in rows {
        match row {
            Ok(e) => edges.push(e),
            Err(e) => tracing::warn!("ctxpkg: skipping graph edge: {e}"),
        }
    }
    edges
}

fn compute_stats(content: &PackageContent) -> PackageStats {
    let knowledge_facts = content
        .knowledge
        .as_ref()
        .map_or(0, |k| k.facts.len() as u32);
    let graph_nodes = content.graph.as_ref().map_or(0, |g| g.nodes.len() as u32);
    let graph_edges = content.graph.as_ref().map_or(0, |g| g.edges.len() as u32);
    let pattern_count = content
        .patterns
        .as_ref()
        .map_or(0, |p| p.patterns.len() as u32);
    let gotcha_count = content
        .gotchas
        .as_ref()
        .map_or(0, |g| g.gotchas.len() as u32);

    let raw_json = serde_json::to_string(content).unwrap_or_default();
    let compression_ratio = {
        use flate2::Compression;
        use flate2::write::GzEncoder;
        use std::io::Write;
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        let _ = encoder.write_all(raw_json.as_bytes());
        let compressed = encoder.finish().unwrap_or_default();
        if raw_json.is_empty() {
            1.0
        } else {
            compressed.len() as f64 / raw_json.len() as f64
        }
    };

    PackageStats {
        knowledge_facts,
        graph_nodes,
        graph_edges,
        pattern_count,
        gotcha_count,
        compression_ratio,
    }
}

fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    crate::core::agent_identity::hex_encode(&hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::context_package::graph_model::{ContextEdge, ContextGraph, ContextNode};

    #[test]
    fn empty_builder_fails() {
        let result = PackageBuilder::new("test", "1.0.0").build();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("no content"));
    }

    #[test]
    fn sha256_is_deterministic() {
        let a = sha256_hex(b"hello world");
        let b = sha256_hex(b"hello world");
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
    }

    #[test]
    fn v2_build_with_context_graph() {
        let mut graph = ContextGraph::new();
        graph.add_node(ContextNode::fact("n1", "test fact", "architecture"));
        graph.add_node(ContextNode::gotcha("n2", "trigger", "resolution"));
        graph.add_edge(ContextEdge {
            from: "n1".into(),
            to: "n2".into(),
            edge_type: "has_gotcha".into(),
            weight: 0.9,
            coactivations: 3,
            metadata: None,
        });

        let mut builder = PackageBuilder::new("v2-test", "1.0.0")
            .description("v2 test package")
            .level(2)
            .scope("@test");

        builder.content.context_graph = Some(graph);

        let (manifest, content) = builder.build().unwrap();

        assert_eq!(
            manifest.schema_version,
            crate::core::contracts::CONTEXT_PACKAGE_V2_SCHEMA_VERSION
        );
        assert_eq!(manifest.conformance_level, Some(2));
        assert_eq!(manifest.scope.as_deref(), Some("@test"));
        assert!(content.context_graph.is_some());

        let gs = manifest.graph_summary.unwrap();
        assert_eq!(gs.node_count, 2);
        assert_eq!(gs.edge_count, 1);
        assert!(gs.activation_mean.is_some());
        assert_eq!(gs.node_types, vec!["fact", "gotcha"]);
    }

    #[test]
    fn v1_build_without_context_graph() {
        let mut builder = PackageBuilder::new("v1-test", "1.0.0").description("v1 test");

        builder.content.knowledge = Some(KnowledgeLayer {
            facts: vec![],
            patterns: vec![],
            insights: vec![],
            exported_at: chrono::Utc::now(),
        });

        let (manifest, _content) = builder.build().unwrap();
        assert_eq!(
            manifest.schema_version,
            crate::core::contracts::CONTEXT_PACKAGE_V1_SCHEMA_VERSION
        );
        assert!(manifest.conformance_level.is_none());
        assert!(manifest.graph_summary.is_none());
    }

    #[test]
    fn level_clamped_to_valid_range() {
        let b = PackageBuilder::new("t", "1.0.0").level(99);
        assert_eq!(b.level, 3);
        let b = PackageBuilder::new("t", "1.0.0").level(0);
        assert_eq!(b.level, 1);
    }

    #[test]
    fn scoped_name_in_v2_build() {
        let mut builder = PackageBuilder::new("@company/auth", "2.0.0")
            .level(2)
            .scope("@company");

        let mut graph = ContextGraph::new();
        graph.add_node(ContextNode::fact("n1", "jwt auth", "security"));
        builder.content.context_graph = Some(graph);

        let (manifest, _) = builder.build().unwrap();
        assert_eq!(manifest.name, "@company/auth");
        assert_eq!(manifest.scope.as_deref(), Some("@company"));
    }

    #[test]
    fn v2_manifest_roundtrip_json() {
        let mut graph = ContextGraph::new();
        graph.add_node(ContextNode::fact("n1", "fact", "cat"));
        graph.add_edge(ContextEdge {
            from: "n1".into(),
            to: "n1".into(),
            edge_type: "self".into(),
            weight: 1.0,
            coactivations: 0,
            metadata: None,
        });

        let mut builder = PackageBuilder::new("roundtrip-test", "1.0.0")
            .description("round trip")
            .level(3)
            .scope("@local");

        builder.content.context_graph = Some(graph);

        let (manifest, content) = builder.build().unwrap();

        let manifest_json = serde_json::to_string(&manifest).unwrap();
        let content_json = serde_json::to_string(&content).unwrap();

        let decoded_manifest: crate::core::context_package::manifest::PackageManifest =
            serde_json::from_str(&manifest_json).unwrap();
        let decoded_content: PackageContent = serde_json::from_str(&content_json).unwrap();

        assert_eq!(decoded_manifest.schema_version, 2);
        assert_eq!(decoded_manifest.conformance_level, Some(3));
        assert!(decoded_content.context_graph.is_some());
        let dg = decoded_content.context_graph.unwrap();
        assert_eq!(dg.nodes.len(), 1);
        assert_eq!(dg.edges.len(), 1);
    }
}
