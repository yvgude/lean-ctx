use std::path::Path;

use super::graph_index::ProjectIndex;
use super::property_graph::CodeGraph;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphProviderSource {
    PropertyGraph,
    GraphIndex,
}

pub enum GraphProvider {
    PropertyGraph(CodeGraph),
    GraphIndex(ProjectIndex),
}

pub struct OpenGraphProvider {
    pub source: GraphProviderSource,
    pub provider: GraphProvider,
}

impl GraphProvider {
    pub fn node_count(&self) -> Option<usize> {
        match self {
            GraphProvider::PropertyGraph(g) => g.node_count().ok(),
            GraphProvider::GraphIndex(i) => Some(i.file_count()),
        }
    }

    pub fn edge_count(&self) -> Option<usize> {
        match self {
            GraphProvider::PropertyGraph(g) => g.edge_count().ok(),
            GraphProvider::GraphIndex(i) => Some(i.edge_count()),
        }
    }

    pub fn dependencies(&self, file_path: &str) -> Vec<String> {
        match self {
            GraphProvider::PropertyGraph(g) => g.dependencies(file_path).unwrap_or_default(),
            GraphProvider::GraphIndex(i) => i
                .edges
                .iter()
                .filter(|e| e.kind == "import" && e.from == file_path)
                .map(|e| e.to.clone())
                .collect(),
        }
    }

    pub fn dependents(&self, file_path: &str) -> Vec<String> {
        match self {
            GraphProvider::PropertyGraph(g) => g.dependents(file_path).unwrap_or_default(),
            GraphProvider::GraphIndex(i) => i
                .edges
                .iter()
                .filter(|e| e.kind == "import" && e.to == file_path)
                .map(|e| e.from.clone())
                .collect(),
        }
    }

    pub fn related(&self, file_path: &str, depth: usize) -> Vec<String> {
        match self {
            GraphProvider::PropertyGraph(g) => g
                .impact_analysis(file_path, depth)
                .map(|r| r.affected_files)
                .unwrap_or_default(),
            GraphProvider::GraphIndex(i) => i.get_related(file_path, depth),
        }
    }
}

pub fn open_best_effort(project_root: &str) -> Option<OpenGraphProvider> {
    let root = Path::new(project_root);
    if let Ok(pg) = CodeGraph::open(root) {
        if let Ok(n) = pg.node_count() {
            if n > 0 {
                return Some(OpenGraphProvider {
                    source: GraphProviderSource::PropertyGraph,
                    provider: GraphProvider::PropertyGraph(pg),
                });
            }
        }
    }

    if let Some(idx) = super::index_orchestrator::try_load_graph_index(project_root) {
        if !idx.files.is_empty() {
            return Some(OpenGraphProvider {
                source: GraphProviderSource::GraphIndex,
                provider: GraphProvider::GraphIndex(idx),
            });
        }
    }

    None
}

pub fn open_or_build(project_root: &str) -> Option<OpenGraphProvider> {
    if let Some(p) = open_best_effort(project_root) {
        return Some(p);
    }
    let idx = super::graph_index::load_or_build(project_root);
    if idx.files.is_empty() {
        return None;
    }
    Some(OpenGraphProvider {
        source: GraphProviderSource::GraphIndex,
        provider: GraphProvider::GraphIndex(idx),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn best_effort_prefers_graph_index_when_property_graph_empty() {
        let _lock = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        let data = tmp.path().join("data");
        std::fs::create_dir_all(&data).expect("mkdir data");
        std::env::set_var("LEAN_CTX_DATA_DIR", data.to_string_lossy().to_string());

        let project_root = tmp.path().join("proj");
        std::fs::create_dir_all(&project_root).expect("mkdir proj");
        let root = project_root.to_string_lossy().to_string();

        let mut idx = ProjectIndex::new(&root);
        idx.files.insert(
            "src/main.rs".to_string(),
            super::super::graph_index::FileEntry {
                path: "src/main.rs".to_string(),
                hash: "h".to_string(),
                language: "rs".to_string(),
                line_count: 1,
                token_count: 1,
                exports: vec![],
                summary: String::new(),
            },
        );
        idx.save().expect("save index");

        let open = open_best_effort(&root).expect("open");
        assert_eq!(open.source, GraphProviderSource::GraphIndex);

        std::env::remove_var("LEAN_CTX_DATA_DIR");
    }

    #[test]
    fn best_effort_none_when_no_graphs() {
        let _lock = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        let data = tmp.path().join("data");
        std::fs::create_dir_all(&data).expect("mkdir data");
        std::env::set_var("LEAN_CTX_DATA_DIR", data.to_string_lossy().to_string());

        let project_root = tmp.path().join("proj");
        std::fs::create_dir_all(&project_root).expect("mkdir proj");
        let root = project_root.to_string_lossy().to_string();

        let open = open_best_effort(&root);
        assert!(open.is_none());

        std::env::remove_var("LEAN_CTX_DATA_DIR");
    }
}
