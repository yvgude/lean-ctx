use crate::core::knowledge::ProjectKnowledge;
use crate::core::memory_policy::MemoryPolicy;
use crate::core::property_graph::{CodeGraph, Edge, EdgeKind, Node, NodeKind};

use super::composition;
use super::content::{GraphLayer, KnowledgeLayer, PackageContent, PatternsLayer, SessionLayer};
use super::graph_model::ContextGraph;
use super::manifest::PackageManifest;

#[derive(Debug, Clone, Default)]
pub struct LoadReport {
    pub package_name: String,
    pub package_version: String,
    pub knowledge_facts_merged: u32,
    pub knowledge_facts_skipped: u32,
    pub knowledge_patterns_merged: u32,
    pub knowledge_insights_merged: u32,
    pub graph_nodes_imported: u32,
    pub graph_edges_imported: u32,
    pub gotchas_imported: u32,
    pub patterns_imported: u32,
    pub session_findings_merged: u32,
    pub session_decisions_merged: u32,
    pub v2_nodes_added: u32,
    pub v2_nodes_updated: u32,
    pub v2_edges_added: u32,
    pub v2_edges_merged: u32,
    pub v2_conflicts: Vec<String>,
    pub warnings: Vec<String>,
}

impl std::fmt::Display for LoadReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "Package: {} v{}",
            self.package_name, self.package_version
        )?;
        if self.knowledge_facts_merged > 0 || self.knowledge_facts_skipped > 0 {
            writeln!(
                f,
                "  Knowledge: {} facts merged, {} skipped (duplicates)",
                self.knowledge_facts_merged, self.knowledge_facts_skipped
            )?;
        }
        if self.knowledge_patterns_merged > 0 {
            writeln!(
                f,
                "  Patterns:  {} imported",
                self.knowledge_patterns_merged
            )?;
        }
        if self.knowledge_insights_merged > 0 {
            writeln!(
                f,
                "  Insights:  {} imported",
                self.knowledge_insights_merged
            )?;
        }
        if self.graph_nodes_imported > 0 || self.graph_edges_imported > 0 {
            writeln!(
                f,
                "  Graph:     {} nodes, {} edges imported",
                self.graph_nodes_imported, self.graph_edges_imported
            )?;
        }
        if self.patterns_imported > 0 {
            writeln!(
                f,
                "  Patterns:  {} imported (standalone)",
                self.patterns_imported
            )?;
        }
        if self.gotchas_imported > 0 {
            writeln!(f, "  Gotchas:   {} imported", self.gotchas_imported)?;
        }
        if self.session_findings_merged > 0 || self.session_decisions_merged > 0 {
            writeln!(
                f,
                "  Session:   {} findings, {} decisions imported",
                self.session_findings_merged, self.session_decisions_merged
            )?;
        }
        if self.v2_nodes_added > 0 || self.v2_nodes_updated > 0 {
            writeln!(
                f,
                "  Graph v2: {} nodes added, {} updated, {} edges added, {} merged",
                self.v2_nodes_added,
                self.v2_nodes_updated,
                self.v2_edges_added,
                self.v2_edges_merged,
            )?;
        }
        for c in &self.v2_conflicts {
            writeln!(f, "  CONFLICT: {c}")?;
        }
        for w in &self.warnings {
            writeln!(f, "  WARNING: {w}")?;
        }
        Ok(())
    }
}

pub fn load_package(
    manifest: &PackageManifest,
    content: &PackageContent,
    project_root: &str,
) -> Result<LoadReport, String> {
    let mut report = LoadReport {
        package_name: manifest.name.clone(),
        package_version: manifest.version.clone(),
        ..Default::default()
    };

    if let Some(ref min_ver) = manifest.compatibility.min_lean_ctx_version {
        let current = env!("CARGO_PKG_VERSION");
        if version_lt(current, min_ver) {
            report.warnings.push(format!(
                "package requires lean-ctx >= {min_ver}, current is {current}"
            ));
        }
    }

    if !manifest.dependencies.is_empty() {
        for dep in &manifest.dependencies {
            if !dep.optional {
                report.warnings.push(format!(
                    "unresolved dependency: {} {}",
                    dep.name, dep.version_req
                ));
            }
        }
    }

    if let Some(ref kl) = content.knowledge
        && let Err(e) = merge_knowledge(kl, project_root, manifest, &mut report)
    {
        report
            .warnings
            .push(format!("knowledge import failed: {e}"));
    }

    if let Some(ref gl) = content.graph
        && let Err(e) = import_graph(gl, project_root, &mut report)
    {
        report.warnings.push(format!("graph import failed: {e}"));
    }

    if let Some(ref patterns) = content.patterns
        && let Err(e) = import_patterns(patterns, project_root, manifest, &mut report)
    {
        report.warnings.push(format!("patterns import failed: {e}"));
    }

    if let Some(ref gotchas) = content.gotchas {
        import_gotchas(gotchas, project_root, &mut report);
    }

    if let Some(ref session) = content.session {
        import_session(session, project_root, manifest, &mut report);
    }

    if let Some(ref incoming_graph) = content.context_graph {
        import_v2_graph(incoming_graph, project_root, &mut report);
    }

    Ok(report)
}

fn import_v2_graph(incoming: &ContextGraph, project_root: &str, report: &mut LoadReport) {
    let project_hash = crate::core::project_hash::hash_project_root(project_root);
    let data_dir = match crate::core::data_dir::lean_ctx_data_dir() {
        Ok(d) => d,
        Err(e) => {
            report
                .warnings
                .push(format!("v2 graph: data dir unavailable: {e}"));
            return;
        }
    };
    let graph_path = data_dir
        .join("context_graph")
        .join(format!("{project_hash}.json"));

    let graph_path_str = graph_path.to_string_lossy().to_string();
    let mut local_graph = if let Ok(data) = std::fs::read_to_string(&graph_path_str) {
        serde_json::from_str::<ContextGraph>(&data).unwrap_or_default()
    } else {
        ContextGraph::default()
    };

    let merge_report = composition::merge_graphs(&mut local_graph, incoming);

    report.v2_nodes_added = merge_report.nodes_added;
    report.v2_nodes_updated = merge_report.nodes_updated;
    report.v2_edges_added = merge_report.edges_added;
    report.v2_edges_merged = merge_report.edges_merged;
    report.v2_conflicts = merge_report.conflicts;

    if merge_report.nodes_added > 0
        || merge_report.nodes_updated > 0
        || merge_report.edges_added > 0
    {
        match serde_json::to_string_pretty(&local_graph) {
            Ok(json) => {
                if let Some(parent) = graph_path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if let Err(e) = std::fs::write(&graph_path_str, json) {
                    report.warnings.push(format!("v2 graph save failed: {e}"));
                }
            }
            Err(e) => {
                report
                    .warnings
                    .push(format!("v2 graph serialize failed: {e}"));
            }
        }
    }
}

fn merge_knowledge(
    layer: &KnowledgeLayer,
    project_root: &str,
    manifest: &PackageManifest,
    report: &mut LoadReport,
) -> Result<(), String> {
    let mut knowledge = ProjectKnowledge::load_or_create(project_root);
    let policy = MemoryPolicy::default();
    let source_tag = format!("{}@{}", manifest.name, manifest.version);

    for fact in &layer.facts {
        let exists = knowledge
            .facts
            .iter()
            .any(|f| f.category == fact.category && f.key == fact.key && f.value == fact.value);

        if exists {
            report.knowledge_facts_skipped += 1;
            continue;
        }

        knowledge.remember(
            &fact.category,
            &fact.key,
            &fact.value,
            &fact.source_session,
            fact.confidence.min(0.8),
            &policy,
        );
        if let Some(last) = knowledge.facts.last_mut() {
            last.imported_from = Some(source_tag.clone());
        }
        report.knowledge_facts_merged += 1;
    }

    for pattern in &layer.patterns {
        let exists = knowledge.patterns.iter().any(|p| {
            p.pattern_type == pattern.pattern_type && p.description == pattern.description
        });

        if !exists {
            knowledge.patterns.push(pattern.clone());
            report.knowledge_patterns_merged += 1;
        }
    }

    for insight in &layer.insights {
        let exists = knowledge
            .history
            .iter()
            .any(|h| h.summary == insight.summary);

        if !exists {
            knowledge.history.push(insight.clone());
            report.knowledge_insights_merged += 1;
        }
    }

    knowledge.save()?;
    Ok(())
}

fn import_graph(
    layer: &GraphLayer,
    project_root: &str,
    report: &mut LoadReport,
) -> Result<(), String> {
    let graph = CodeGraph::open(project_root).map_err(|e| format!("graph open: {e}"))?;

    for node_export in &layer.nodes {
        let node = Node {
            id: None,
            kind: NodeKind::parse(&node_export.kind),
            name: node_export.name.clone(),
            file_path: node_export.file_path.clone(),
            line_start: node_export.line_start,
            line_end: node_export.line_end,
            metadata: node_export.metadata.clone(),
        };

        match graph.upsert_node(&node) {
            Ok(_) => report.graph_nodes_imported += 1,
            Err(e) => {
                report
                    .warnings
                    .push(format!("node import failed ({}): {e}", node_export.name));
            }
        }
    }

    for edge_export in &layer.edges {
        let source = find_node_for_edge(&graph, &edge_export.source_path, &edge_export.source_name);
        let target = find_node_for_edge(&graph, &edge_export.target_path, &edge_export.target_name);

        match (source, target) {
            (Some(src), Some(tgt)) => {
                let Some(src_id) = src.id else {
                    report.warnings.push(format!(
                        "edge skipped: source node has no id ({}:{})",
                        edge_export.source_path, edge_export.source_name
                    ));
                    continue;
                };
                let Some(tgt_id) = tgt.id else {
                    report.warnings.push(format!(
                        "edge skipped: target node has no id ({}:{})",
                        edge_export.target_path, edge_export.target_name
                    ));
                    continue;
                };

                let edge = Edge {
                    id: None,
                    source_id: src_id,
                    target_id: tgt_id,
                    kind: EdgeKind::parse(&edge_export.kind),
                    metadata: edge_export.metadata.clone(),
                };

                match graph.upsert_edge(&edge) {
                    Ok(()) => report.graph_edges_imported += 1,
                    Err(e) => {
                        report.warnings.push(format!(
                            "edge import failed ({} -> {}): {e}",
                            edge_export.source_name, edge_export.target_name
                        ));
                    }
                }
            }
            _ => {
                report.warnings.push(format!(
                    "edge skipped: node not found ({} -> {})",
                    edge_export.source_name, edge_export.target_name
                ));
            }
        }
    }

    Ok(())
}

/// Find a node by symbol name+path first, then fall back to path-only lookup.
fn find_node_for_edge(graph: &CodeGraph, file_path: &str, name: &str) -> Option<Node> {
    if let Ok(Some(node)) = graph.get_node_by_symbol(name, file_path) {
        return Some(node);
    }
    if let Ok(Some(node)) = graph.get_node_by_path(file_path) {
        return Some(node);
    }
    None
}

fn import_patterns(
    layer: &PatternsLayer,
    project_root: &str,
    _manifest: &PackageManifest,
    report: &mut LoadReport,
) -> Result<(), String> {
    let mut knowledge = ProjectKnowledge::load_or_create(project_root);

    for pattern in &layer.patterns {
        let exists = knowledge.patterns.iter().any(|p| {
            p.pattern_type == pattern.pattern_type && p.description == pattern.description
        });

        if !exists {
            knowledge.patterns.push(pattern.clone());
            report.patterns_imported += 1;
        }
    }

    if report.patterns_imported > 0 {
        knowledge.save()?;
    }
    Ok(())
}

fn import_gotchas(
    layer: &super::content::GotchasLayer,
    project_root: &str,
    report: &mut LoadReport,
) {
    use crate::core::gotcha_tracker::{
        Gotcha, GotchaCategory, GotchaSeverity, GotchaSource, GotchaStore,
    };

    let mut store = GotchaStore::load(project_root);
    let before = store.gotchas.len();

    for g in &layer.gotchas {
        let dup = store.gotchas.iter().any(|e| e.id == g.id);
        if dup {
            continue;
        }

        let category = GotchaCategory::from_str_loose(&g.category);
        let severity = match g.severity.as_str() {
            "critical" => GotchaSeverity::Critical,
            "warning" => GotchaSeverity::Warning,
            _ => GotchaSeverity::Info,
        };

        let mut gotcha = Gotcha::new(
            category,
            severity,
            &g.trigger,
            &g.resolution,
            GotchaSource::AgentReported {
                session_id: "package-import".into(),
            },
            "package-import",
        );
        g.id.clone_into(&mut gotcha.id);
        g.file_patterns.clone_into(&mut gotcha.file_patterns);
        gotcha.confidence = g.confidence.min(0.8);

        store.gotchas.push(gotcha);
    }

    report.gotchas_imported = (store.gotchas.len() - before) as u32;
    if let Err(e) = store.save(project_root) {
        report.warnings.push(format!("gotcha save failed: {e}"));
    }
}

fn version_lt(current: &str, required: &str) -> bool {
    let parse = |v: &str| -> Vec<u32> {
        v.split('.')
            .map(|s| s.parse::<u32>().unwrap_or(0))
            .collect()
    };
    let c = parse(current);
    let r = parse(required);
    for i in 0..c.len().max(r.len()) {
        let cv = c.get(i).copied().unwrap_or(0);
        let rv = r.get(i).copied().unwrap_or(0);
        if cv < rv {
            return true;
        }
        if cv > rv {
            return false;
        }
    }
    false
}

fn import_session(
    layer: &SessionLayer,
    project_root: &str,
    manifest: &PackageManifest,
    report: &mut LoadReport,
) {
    let mut knowledge = ProjectKnowledge::load_or_create(project_root);
    let policy = MemoryPolicy::default();
    let source_tag = format!("{}@{} (session)", manifest.name, manifest.version);

    for finding in &layer.findings {
        let key = finding.file.as_deref().unwrap_or("general");
        let exists = knowledge
            .facts
            .iter()
            .any(|f| f.category == "session_finding" && f.value == finding.summary);
        if !exists {
            knowledge.remember(
                "session_finding",
                key,
                &finding.summary,
                &source_tag,
                0.6,
                &policy,
            );
            report.session_findings_merged += 1;
        }
    }

    for decision in &layer.decisions {
        let value = if let Some(ref rationale) = decision.rationale {
            format!("{} (rationale: {})", decision.summary, rationale)
        } else {
            decision.summary.clone()
        };
        let exists = knowledge
            .facts
            .iter()
            .any(|f| f.category == "session_decision" && f.value == decision.summary);
        if !exists {
            knowledge.remember(
                "session_decision",
                "decision",
                &value,
                &source_tag,
                0.7,
                &policy,
            );
            report.session_decisions_merged += 1;
        }
    }

    if (report.session_findings_merged > 0 || report.session_decisions_merged > 0)
        && let Err(e) = knowledge.save()
    {
        report
            .warnings
            .push(format!("session knowledge save failed: {e}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::context_package::content::*;
    use crate::core::context_package::manifest::*;
    use chrono::Utc;

    fn test_manifest(layers: Vec<PackageLayer>) -> PackageManifest {
        PackageManifest {
            schema_version: 1,
            conformance_level: None,
            name: "test-pkg".into(),
            version: "1.0.0".into(),
            description: "test".into(),
            author: None,
            scope: None,
            created_at: Utc::now(),
            updated_at: None,
            layers,
            dependencies: vec![],
            tags: vec![],
            visibility: None,
            integrity: PackageIntegrity {
                sha256: "a".repeat(64),
                content_hash: "b".repeat(64),
                byte_size: 100,
            },
            provenance: PackageProvenance {
                tool: "test".into(),
                tool_version: "0.0.1".into(),
                project_hash: None,
                source_session_id: None,
            },
            compatibility: CompatibilitySpec::default(),
            stats: PackageStats::default(),
            signature: None,
            graph_summary: None,
            marketplace: None,
        }
    }

    #[test]
    fn version_lt_basic_comparisons() {
        assert!(version_lt("3.5.0", "3.6.0"));
        assert!(!version_lt("3.6.0", "3.5.0"));
        assert!(!version_lt("3.6.0", "3.6.0"));
        assert!(version_lt("3.6.14", "3.6.15"));
        assert!(version_lt("2.0.0", "3.0.0"));
    }

    #[test]
    fn compatibility_warning_when_version_too_low() {
        let mut manifest = test_manifest(vec![PackageLayer::Knowledge]);
        manifest.compatibility.min_lean_ctx_version = Some("99.0.0".into());

        let content = PackageContent {
            knowledge: Some(KnowledgeLayer {
                facts: vec![],
                patterns: vec![],
                insights: vec![],
                exported_at: Utc::now(),
            }),
            ..Default::default()
        };

        let dir = tempfile::tempdir().unwrap();
        let report = load_package(&manifest, &content, dir.path().to_str().unwrap()).unwrap();
        assert!(
            report
                .warnings
                .iter()
                .any(|w| w.contains("requires lean-ctx >= 99.0.0"))
        );
    }

    #[test]
    fn dependency_warning_for_required_deps() {
        let mut manifest = test_manifest(vec![PackageLayer::Knowledge]);
        manifest.dependencies.push(PackageDependency {
            name: "missing-pkg".into(),
            version_req: "^1.0".into(),
            optional: false,
        });

        let content = PackageContent {
            knowledge: Some(KnowledgeLayer {
                facts: vec![],
                patterns: vec![],
                insights: vec![],
                exported_at: Utc::now(),
            }),
            ..Default::default()
        };

        let dir = tempfile::tempdir().unwrap();
        let report = load_package(&manifest, &content, dir.path().to_str().unwrap()).unwrap();
        assert!(
            report
                .warnings
                .iter()
                .any(|w| w.contains("unresolved dependency: missing-pkg"))
        );
    }

    #[test]
    fn optional_dependency_no_warning() {
        let mut manifest = test_manifest(vec![PackageLayer::Knowledge]);
        manifest.dependencies.push(PackageDependency {
            name: "optional-pkg".into(),
            version_req: "^1.0".into(),
            optional: true,
        });

        let content = PackageContent {
            knowledge: Some(KnowledgeLayer {
                facts: vec![],
                patterns: vec![],
                insights: vec![],
                exported_at: Utc::now(),
            }),
            ..Default::default()
        };

        let dir = tempfile::tempdir().unwrap();
        let report = load_package(&manifest, &content, dir.path().to_str().unwrap()).unwrap();
        assert!(!report.warnings.iter().any(|w| w.contains("optional-pkg")));
    }

    #[test]
    fn load_report_display_format() {
        let report = LoadReport {
            package_name: "my-pkg".into(),
            package_version: "1.0.0".into(),
            knowledge_facts_merged: 5,
            knowledge_facts_skipped: 2,
            knowledge_patterns_merged: 3,
            knowledge_insights_merged: 1,
            graph_nodes_imported: 10,
            graph_edges_imported: 8,
            gotchas_imported: 4,
            patterns_imported: 2,
            session_findings_merged: 3,
            session_decisions_merged: 1,
            v2_nodes_added: 0,
            v2_nodes_updated: 0,
            v2_edges_added: 0,
            v2_edges_merged: 0,
            v2_conflicts: vec![],
            warnings: vec!["test warning".into()],
        };

        let display = format!("{report}");
        assert!(display.contains("my-pkg v1.0.0"));
        assert!(display.contains("5 facts merged"));
        assert!(display.contains("10 nodes"));
        assert!(display.contains("2 imported (standalone)"));
        assert!(display.contains("WARNING: test warning"));
    }

    #[test]
    fn v2_graph_import_creates_local_graph() {
        use crate::core::context_package::graph_model::{ContextEdge, ContextGraph, ContextNode};

        let mut graph = ContextGraph::new();
        graph.add_node(ContextNode::fact("n1", "test fact", "arch"));
        graph.add_node(ContextNode::gotcha("n2", "trigger", "resolution"));
        graph.add_edge(ContextEdge {
            from: "n1".into(),
            to: "n2".into(),
            edge_type: "has_gotcha".into(),
            weight: 0.9,
            coactivations: 5,
            metadata: None,
        });

        let content = PackageContent {
            context_graph: Some(graph),
            ..Default::default()
        };

        let mut manifest = test_manifest(vec![]);
        manifest.schema_version = 2;
        manifest.conformance_level = Some(2);

        let dir = tempfile::tempdir().unwrap();
        let report = load_package(&manifest, &content, dir.path().to_str().unwrap()).unwrap();

        assert_eq!(report.v2_nodes_added, 2);
        assert_eq!(report.v2_edges_added, 1);
    }

    #[test]
    fn v2_load_report_display_includes_graph() {
        let report = LoadReport {
            package_name: "v2-pkg".into(),
            package_version: "2.0.0".into(),
            v2_nodes_added: 15,
            v2_nodes_updated: 3,
            v2_edges_added: 20,
            v2_edges_merged: 5,
            v2_conflicts: vec!["conflict A".into()],
            ..Default::default()
        };

        let display = format!("{report}");
        assert!(display.contains("15 nodes added"));
        assert!(display.contains("3 updated"));
        assert!(display.contains("20 edges added"));
        assert!(display.contains("5 merged"));
        assert!(display.contains("CONFLICT: conflict A"));
    }

    #[test]
    fn v2_graph_import_merges_with_existing() {
        use crate::core::context_package::graph_model::{ContextGraph, ContextNode};

        let mut first_graph = ContextGraph::new();
        first_graph.add_node(ContextNode::fact("shared", "original", "cat"));

        let first_content = PackageContent {
            context_graph: Some(first_graph),
            ..Default::default()
        };

        let mut manifest = test_manifest(vec![]);
        manifest.schema_version = 2;

        let dir = tempfile::tempdir().unwrap();
        let r1 = load_package(&manifest, &first_content, dir.path().to_str().unwrap()).unwrap();
        assert_eq!(r1.v2_nodes_added, 1);

        let mut second_graph = ContextGraph::new();
        let mut node = ContextNode::fact("shared", "updated", "cat");
        node.activation = 0.9;
        second_graph.add_node(node);
        second_graph.add_node(ContextNode::fact("new_node", "new content", "cat"));

        let second_content = PackageContent {
            context_graph: Some(second_graph),
            ..Default::default()
        };

        let r2 = load_package(&manifest, &second_content, dir.path().to_str().unwrap()).unwrap();
        assert_eq!(r2.v2_nodes_added, 1);
        assert_eq!(r2.v2_nodes_updated, 1);
    }
}
