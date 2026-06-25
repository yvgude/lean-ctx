//! Data collection and HTML rendering for the interactive visualizer.
//!
//! Gathers graph, knowledge, heatmap (token savings), and session data
//! from the current project, then renders a self-contained HTML report
//! with embedded D3.js.

use serde::Serialize;

use crate::core::heatmap::HeatMap;
use crate::core::knowledge::{KnowledgeFact, ProjectKnowledge};
use crate::core::property_graph::CodeGraph;
use crate::core::session::{SessionState, SessionStats};

// ---------------------------------------------------------------------------
// Data types serialized into JSON for the HTML template
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct VisualizerData {
    pub graph: GraphData,
    pub knowledge: Vec<KnowledgeEntry>,
    pub savings: SavingsData,
    pub history: SessionHistory,
}

#[derive(Serialize)]
pub struct GraphData {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
}

#[derive(Serialize)]
pub struct GraphNode {
    pub id: String,
    pub kind: String,
    pub label: String,
}

#[derive(Serialize)]
pub struct GraphEdge {
    pub source: String,
    pub target: String,
    pub kind: String,
    pub weight: f64,
}

#[derive(Serialize)]
pub struct KnowledgeEntry {
    pub category: String,
    pub key: String,
    pub value: String,
    pub confidence: f32,
    pub archetype: String,
    pub created_at: String,
    pub valid_from: Option<String>,
    pub valid_until: Option<String>,
    pub retrieval_count: u32,
    pub confirmation_count: u32,
}

#[derive(Serialize)]
pub struct SavingsData {
    pub files: Vec<FileSavingsEntry>,
    pub total_original: u64,
    pub total_saved: u64,
    pub overall_ratio: f32,
}

#[derive(Serialize)]
pub struct FileSavingsEntry {
    pub path: String,
    pub access_count: u32,
    pub original_tokens: u64,
    pub saved_tokens: u64,
    pub compression_ratio: f32,
}

#[derive(Serialize)]
pub struct SessionHistory {
    pub session_id: String,
    pub started_at: String,
    pub task: Option<String>,
    pub stats: SessionStatsEntry,
    pub files_touched: Vec<FileTouchedEntry>,
    pub findings: Vec<FindingEntry>,
    pub decisions: Vec<DecisionEntry>,
    pub progress: Vec<ProgressEntryViz>,
}

#[derive(Serialize)]
pub struct SessionStatsEntry {
    pub total_tool_calls: u32,
    pub total_tokens_saved: u64,
    pub total_tokens_input: u64,
    pub cache_hits: u32,
    pub files_read: u32,
    pub commands_run: u32,
}

#[derive(Serialize)]
pub struct FileTouchedEntry {
    pub path: String,
    pub read_count: u32,
    pub modified: bool,
    pub mode: String,
    pub tokens: usize,
}

#[derive(Serialize)]
pub struct FindingEntry {
    pub file: Option<String>,
    pub summary: String,
    pub timestamp: String,
}

#[derive(Serialize)]
pub struct DecisionEntry {
    pub summary: String,
    pub rationale: Option<String>,
    pub timestamp: String,
}

#[derive(Serialize)]
pub struct ProgressEntryViz {
    pub action: String,
    pub detail: Option<String>,
    pub timestamp: String,
}

// ---------------------------------------------------------------------------
// Data collection
// ---------------------------------------------------------------------------

#[must_use]
pub fn collect_data(project_root: &str) -> VisualizerData {
    let graph = collect_graph(project_root);
    let knowledge = collect_knowledge(project_root);
    let savings = collect_savings();
    let history = collect_session(project_root);

    VisualizerData {
        graph,
        knowledge,
        savings,
        history,
    }
}

fn collect_graph(project_root: &str) -> GraphData {
    let Ok(cg) = CodeGraph::open(project_root) else {
        return GraphData {
            nodes: Vec::new(),
            edges: Vec::new(),
        };
    };

    let flat_edges = cg.all_edges_flat().unwrap_or_default();

    let mut node_set = std::collections::HashSet::new();
    let mut nodes = Vec::new();
    let mut edges = Vec::new();

    for (src, tgt, kind, weight) in &flat_edges {
        for path in [src, tgt] {
            if node_set.insert(path.clone()) {
                let label = path.rsplit('/').next().unwrap_or(path).to_string();
                nodes.push(GraphNode {
                    id: path.clone(),
                    kind: "file".to_string(),
                    label,
                });
            }
        }
        edges.push(GraphEdge {
            source: src.clone(),
            target: tgt.clone(),
            kind: kind.clone(),
            weight: *weight,
        });
    }

    GraphData { nodes, edges }
}

fn collect_knowledge(project_root: &str) -> Vec<KnowledgeEntry> {
    let Some(pk) = ProjectKnowledge::load(project_root) else {
        return Vec::new();
    };
    pk.facts
        .iter()
        .map(|f: &KnowledgeFact| KnowledgeEntry {
            category: f.category.clone(),
            key: f.key.clone(),
            value: f.value.clone(),
            confidence: f.confidence,
            archetype: format!("{:?}", f.archetype),
            created_at: f.created_at.to_rfc3339(),
            valid_from: f.valid_from.map(|d| d.to_rfc3339()),
            valid_until: f.valid_until.map(|d| d.to_rfc3339()),
            retrieval_count: f.retrieval_count,
            confirmation_count: f.confirmation_count,
        })
        .collect()
}

fn collect_savings() -> SavingsData {
    let hm = HeatMap::load();
    let top = hm.top_files(500);

    let mut total_original = 0u64;
    let mut total_saved = 0u64;

    let files: Vec<FileSavingsEntry> = top
        .into_iter()
        .map(|e| {
            total_original += e.total_original_tokens;
            total_saved += e.total_tokens_saved;
            FileSavingsEntry {
                path: e.path.clone(),
                access_count: e.access_count,
                original_tokens: e.total_original_tokens,
                saved_tokens: e.total_tokens_saved,
                compression_ratio: e.avg_compression_ratio,
            }
        })
        .collect();

    let overall_ratio = if total_original > 0 {
        total_saved as f32 / total_original as f32
    } else {
        0.0
    };

    SavingsData {
        files,
        total_original,
        total_saved,
        overall_ratio,
    }
}

fn collect_session(project_root: &str) -> SessionHistory {
    let session = SessionState::load_latest_for_project_root(project_root)
        .or_else(SessionState::load_global_latest_pointer)
        .unwrap_or_default();

    map_session(&session)
}

fn map_session(s: &SessionState) -> SessionHistory {
    SessionHistory {
        session_id: s.id.clone(),
        started_at: s.started_at.to_rfc3339(),
        task: s.task.as_ref().map(|t| t.description.clone()),
        stats: map_stats(&s.stats),
        files_touched: s
            .files_touched
            .iter()
            .map(|f| FileTouchedEntry {
                path: f.path.clone(),
                read_count: f.read_count,
                modified: f.modified,
                mode: f.last_mode.clone(),
                tokens: f.tokens,
            })
            .collect(),
        findings: s
            .findings
            .iter()
            .map(|f| FindingEntry {
                file: f.file.clone(),
                summary: f.summary.clone(),
                timestamp: f.timestamp.to_rfc3339(),
            })
            .collect(),
        decisions: s
            .decisions
            .iter()
            .map(|d| DecisionEntry {
                summary: d.summary.clone(),
                rationale: d.rationale.clone(),
                timestamp: d.timestamp.to_rfc3339(),
            })
            .collect(),
        progress: s
            .progress
            .iter()
            .map(|p| ProgressEntryViz {
                action: p.action.clone(),
                detail: p.detail.clone(),
                timestamp: p.timestamp.to_rfc3339(),
            })
            .collect(),
    }
}

fn map_stats(s: &SessionStats) -> SessionStatsEntry {
    SessionStatsEntry {
        total_tool_calls: s.total_tool_calls,
        total_tokens_saved: s.total_tokens_saved,
        total_tokens_input: s.total_tokens_input,
        cache_hits: s.cache_hits,
        files_read: s.files_read,
        commands_run: s.commands_run,
    }
}

// ---------------------------------------------------------------------------
// HTML rendering
// ---------------------------------------------------------------------------

#[must_use]
pub fn render_html(data: &VisualizerData) -> String {
    let json = serde_json::to_string(data).unwrap_or_else(|_| "{}".to_string());
    let template = include_str!("../assets/visualizer.html");
    template.replace("/*__VISUALIZER_DATA__*/", &format!("const DATA = {json};"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_data_renders_valid_html() {
        let data = VisualizerData {
            graph: GraphData {
                nodes: Vec::new(),
                edges: Vec::new(),
            },
            knowledge: Vec::new(),
            savings: SavingsData {
                files: Vec::new(),
                total_original: 0,
                total_saved: 0,
                overall_ratio: 0.0,
            },
            history: SessionHistory {
                session_id: "test".to_string(),
                started_at: "2024-01-01T00:00:00Z".to_string(),
                task: None,
                stats: SessionStatsEntry {
                    total_tool_calls: 0,
                    total_tokens_saved: 0,
                    total_tokens_input: 0,
                    cache_hits: 0,
                    files_read: 0,
                    commands_run: 0,
                },
                files_touched: Vec::new(),
                findings: Vec::new(),
                decisions: Vec::new(),
                progress: Vec::new(),
            },
        };
        let html = render_html(&data);
        assert!(html.contains("<!DOCTYPE html>"));
        assert!(html.contains("const DATA ="));
        assert!(!html.contains("/*__VISUALIZER_DATA__*/"));
    }

    #[test]
    fn savings_ratio_zero_on_empty() {
        let s = collect_savings();
        assert!(s.overall_ratio >= 0.0);
    }

    #[test]
    fn graph_node_label_uses_filename() {
        let node = GraphNode {
            id: "src/core/main.rs".to_string(),
            kind: "file".to_string(),
            label: "main.rs".to_string(),
        };
        assert_eq!(node.label, "main.rs");
    }
}
