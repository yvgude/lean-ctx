//! Property Graph Engine — SQLite-backed code knowledge graph.
//!
//! Stores nodes (File, Symbol, Module) and edges (imports, calls, defines,
//! exports) extracted by `deep_queries` + `import_resolver`.  Provides
//! efficient traversal queries for impact analysis, architecture discovery,
//! and graph-driven context loading.

mod edge;
mod node;
mod queries;
mod schema;

pub use edge::{Edge, EdgeKind};
pub use node::{Node, NodeKind};
pub use queries::{DependencyChain, GraphQuery, ImpactResult};

use rusqlite::Connection;
use std::path::{Path, PathBuf};

pub struct CodeGraph {
    conn: Connection,
    db_path: PathBuf,
}

impl CodeGraph {
    pub fn open(project_root: &Path) -> anyhow::Result<Self> {
        let db_dir = project_root.join(".lean-ctx");
        std::fs::create_dir_all(&db_dir)?;
        let db_path = db_dir.join("graph.db");
        let conn = Connection::open(&db_path)?;
        schema::initialize(&conn)?;
        Ok(Self { conn, db_path })
    }

    pub fn open_in_memory() -> anyhow::Result<Self> {
        let conn = Connection::open_in_memory()?;
        schema::initialize(&conn)?;
        Ok(Self {
            conn,
            db_path: PathBuf::from(":memory:"),
        })
    }

    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    pub fn upsert_node(&self, node: &Node) -> anyhow::Result<i64> {
        node::upsert(&self.conn, node)
    }

    pub fn upsert_edge(&self, edge: &Edge) -> anyhow::Result<()> {
        edge::upsert(&self.conn, edge)
    }

    pub fn get_node_by_path(&self, file_path: &str) -> anyhow::Result<Option<Node>> {
        node::get_by_path(&self.conn, file_path)
    }

    pub fn get_node_by_symbol(&self, name: &str, file_path: &str) -> anyhow::Result<Option<Node>> {
        node::get_by_symbol(&self.conn, name, file_path)
    }

    pub fn remove_file_nodes(&self, file_path: &str) -> anyhow::Result<()> {
        node::remove_by_file(&self.conn, file_path)
    }

    pub fn edges_from(&self, node_id: i64) -> anyhow::Result<Vec<Edge>> {
        edge::from_node(&self.conn, node_id)
    }

    pub fn edges_to(&self, node_id: i64) -> anyhow::Result<Vec<Edge>> {
        edge::to_node(&self.conn, node_id)
    }

    pub fn dependents(&self, file_path: &str) -> anyhow::Result<Vec<String>> {
        queries::dependents(&self.conn, file_path)
    }

    pub fn dependencies(&self, file_path: &str) -> anyhow::Result<Vec<String>> {
        queries::dependencies(&self.conn, file_path)
    }

    pub fn impact_analysis(
        &self,
        file_path: &str,
        max_depth: usize,
    ) -> anyhow::Result<ImpactResult> {
        queries::impact_analysis(&self.conn, file_path, max_depth)
    }

    pub fn dependency_chain(
        &self,
        from: &str,
        to: &str,
    ) -> anyhow::Result<Option<DependencyChain>> {
        queries::dependency_chain(&self.conn, from, to)
    }

    pub fn node_count(&self) -> anyhow::Result<usize> {
        node::count(&self.conn)
    }

    pub fn edge_count(&self) -> anyhow::Result<usize> {
        edge::count(&self.conn)
    }

    pub fn clear(&self) -> anyhow::Result<()> {
        self.conn
            .execute_batch("DELETE FROM edges; DELETE FROM nodes;")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_graph() -> CodeGraph {
        CodeGraph::open_in_memory().unwrap()
    }

    #[test]
    fn create_and_query_nodes() {
        let g = test_graph();

        let id = g.upsert_node(&Node::file("src/main.rs")).unwrap();
        assert!(id > 0);

        let found = g.get_node_by_path("src/main.rs").unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().file_path, "src/main.rs");
    }

    #[test]
    fn create_and_query_edges() {
        let g = test_graph();

        let a = g.upsert_node(&Node::file("src/a.rs")).unwrap();
        let b = g.upsert_node(&Node::file("src/b.rs")).unwrap();

        g.upsert_edge(&Edge::new(a, b, EdgeKind::Imports)).unwrap();

        let from_a = g.edges_from(a).unwrap();
        assert_eq!(from_a.len(), 1);
        assert_eq!(from_a[0].target_id, b);

        let to_b = g.edges_to(b).unwrap();
        assert_eq!(to_b.len(), 1);
        assert_eq!(to_b[0].source_id, a);
    }

    #[test]
    fn dependents_query() {
        let g = test_graph();

        let main = g.upsert_node(&Node::file("src/main.rs")).unwrap();
        let lib = g.upsert_node(&Node::file("src/lib.rs")).unwrap();
        let utils = g.upsert_node(&Node::file("src/utils.rs")).unwrap();

        g.upsert_edge(&Edge::new(main, lib, EdgeKind::Imports))
            .unwrap();
        g.upsert_edge(&Edge::new(utils, lib, EdgeKind::Imports))
            .unwrap();

        let deps = g.dependents("src/lib.rs").unwrap();
        assert_eq!(deps.len(), 2);
        assert!(deps.contains(&"src/main.rs".to_string()));
        assert!(deps.contains(&"src/utils.rs".to_string()));
    }

    #[test]
    fn dependencies_query() {
        let g = test_graph();

        let main = g.upsert_node(&Node::file("src/main.rs")).unwrap();
        let lib = g.upsert_node(&Node::file("src/lib.rs")).unwrap();
        let config = g.upsert_node(&Node::file("src/config.rs")).unwrap();

        g.upsert_edge(&Edge::new(main, lib, EdgeKind::Imports))
            .unwrap();
        g.upsert_edge(&Edge::new(main, config, EdgeKind::Imports))
            .unwrap();

        let deps = g.dependencies("src/main.rs").unwrap();
        assert_eq!(deps.len(), 2);
    }

    #[test]
    #[allow(clippy::many_single_char_names)] // graph test nodes: a, b, c, d, e
    fn impact_analysis_depth() {
        let g = test_graph();

        let a = g.upsert_node(&Node::file("a.rs")).unwrap();
        let b = g.upsert_node(&Node::file("b.rs")).unwrap();
        let c = g.upsert_node(&Node::file("c.rs")).unwrap();
        let d = g.upsert_node(&Node::file("d.rs")).unwrap();

        g.upsert_edge(&Edge::new(b, a, EdgeKind::Imports)).unwrap();
        g.upsert_edge(&Edge::new(c, b, EdgeKind::Imports)).unwrap();
        g.upsert_edge(&Edge::new(d, c, EdgeKind::Imports)).unwrap();

        let impact = g.impact_analysis("a.rs", 2).unwrap();
        assert!(impact.affected_files.contains(&"b.rs".to_string()));
        assert!(impact.affected_files.contains(&"c.rs".to_string()));
        assert!(!impact.affected_files.contains(&"d.rs".to_string()));

        let deep = g.impact_analysis("a.rs", 10).unwrap();
        assert!(deep.affected_files.contains(&"d.rs".to_string()));
    }

    #[test]
    fn upsert_idempotent() {
        let g = test_graph();

        let id1 = g.upsert_node(&Node::file("src/main.rs")).unwrap();
        let id2 = g.upsert_node(&Node::file("src/main.rs")).unwrap();
        assert_eq!(id1, id2);
        assert_eq!(g.node_count().unwrap(), 1);
    }

    #[test]
    fn remove_file_cascades() {
        let g = test_graph();

        let a = g.upsert_node(&Node::file("src/a.rs")).unwrap();
        let b = g.upsert_node(&Node::file("src/b.rs")).unwrap();
        let sym = g
            .upsert_node(&Node::symbol("MyStruct", "src/a.rs", NodeKind::Symbol))
            .unwrap();

        g.upsert_edge(&Edge::new(a, b, EdgeKind::Imports)).unwrap();
        g.upsert_edge(&Edge::new(sym, b, EdgeKind::Calls)).unwrap();

        g.remove_file_nodes("src/a.rs").unwrap();

        assert!(g.get_node_by_path("src/a.rs").unwrap().is_none());
        assert_eq!(g.edge_count().unwrap(), 0);
    }

    #[test]
    fn dependency_chain_found() {
        let g = test_graph();

        let a = g.upsert_node(&Node::file("a.rs")).unwrap();
        let b = g.upsert_node(&Node::file("b.rs")).unwrap();
        let c = g.upsert_node(&Node::file("c.rs")).unwrap();

        g.upsert_edge(&Edge::new(a, b, EdgeKind::Imports)).unwrap();
        g.upsert_edge(&Edge::new(b, c, EdgeKind::Imports)).unwrap();

        let chain = g.dependency_chain("a.rs", "c.rs").unwrap();
        assert!(chain.is_some());
        let chain = chain.unwrap();
        assert_eq!(chain.path, vec!["a.rs", "b.rs", "c.rs"]);
    }

    #[test]
    fn counts() {
        let g = test_graph();
        assert_eq!(g.node_count().unwrap(), 0);
        assert_eq!(g.edge_count().unwrap(), 0);

        let a = g.upsert_node(&Node::file("a.rs")).unwrap();
        let b = g.upsert_node(&Node::file("b.rs")).unwrap();
        g.upsert_edge(&Edge::new(a, b, EdgeKind::Imports)).unwrap();

        assert_eq!(g.node_count().unwrap(), 2);
        assert_eq!(g.edge_count().unwrap(), 1);
    }
}
