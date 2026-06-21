//! Mirror a `graph_index` [`ProjectIndex`] into the SQLite property graph.
//!
//! The "one extractor → one store" path (#682.1): the mature graph_index
//! extractor produces the [`ProjectIndex`] (files, symbols, edges); this mirrors
//! it faithfully into the scalable SQLite store so the property graph carries
//! identical data — including a populated `file_catalog`, which the provider
//! facade's `pg_populated` gate requires. Feeding PG from the proven extractor
//! guarantees PG ⊇ graph_index, so a later backend flip cannot lose data.
//!
//! This is a pure replace of the *code graph*: nodes, edges and the file
//! catalog are cleared first, then rebuilt from the index, so re-running it is
//! idempotent. Provider `cross_source_edges` are deliberately preserved.

use super::{
    CodeGraph, Edge, EdgeKind, FileCatalogEntry, GRAPH_ENGINE_VERSION, Node, NodeKind,
    PropertyGraphMetaV1, write_meta,
};
use crate::core::graph_index::ProjectIndex;
use std::path::Path;
use std::time::{Duration, Instant};

/// Map a graph_index edge-kind string onto a property-graph [`EdgeKind`].
///
/// `import` → `Imports` and `reexport` → `Module` keep both inside
/// `STRUCTURAL_EDGE_KINDS` (so dependency/impact queries see them) while
/// preserving the distinction graph_index draws between the two. Other kinds
/// (`calls`, `exports`, `module`, `cochange`, `sibling`, …) round-trip through
/// [`EdgeKind::parse`].
fn map_edge_kind(kind: &str) -> EdgeKind {
    match kind {
        "import" => EdgeKind::Imports,
        "reexport" => EdgeKind::Module,
        other => EdgeKind::parse(other),
    }
}

/// Compact, deterministic JSON metadata preserving the symbol's source kind and
/// export flag (the property-graph `Node` only models a coarse `NodeKind`).
fn symbol_metadata(kind: &str, is_exported: bool) -> String {
    format!(
        r#"{{"kind":{},"exported":{}}}"#,
        json_str(kind),
        is_exported
    )
}

/// Inverse of `symbol_metadata`: recover the source `kind` and `exported`
/// flag from a symbol node's metadata JSON. The property-graph `Node` only
/// models a coarse `NodeKind`, so the precise graph_index kind (`function`,
/// `struct`, …) and export flag live in this metadata blob — the provider
/// facade must read them back to surface a lossless symbol (#696 C1). Returns
/// `(None, None)` for absent/malformed metadata so callers can fall back.
pub fn parse_symbol_metadata(meta: Option<&str>) -> (Option<String>, Option<bool>) {
    let Some(raw) = meta else {
        return (None, None);
    };
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(v) => {
            let kind = v
                .get("kind")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            let exported = v.get("exported").and_then(serde_json::Value::as_bool);
            (kind, exported)
        }
        Err(_) => (None, None),
    }
}

/// Minimal JSON string escaper for the two metadata fields (avoids pulling a
/// serializer into this hot path; kinds are simple identifiers in practice).
fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Mirror `index` into `graph`: file nodes + file catalog, symbol nodes (with
/// line spans + kind/export metadata), and structural edges. Clears the code
/// graph first (preserving provider cross-source edges) so the result is a
/// faithful 1:1 representation of the index.
///
/// All writes run inside a single transaction — without it, SQLite fsyncs per
/// statement, which on a real repo (thousands of symbols) is pathologically
/// slow.
pub fn populate_from_project_index(graph: &CodeGraph, index: &ProjectIndex) -> anyhow::Result<()> {
    graph.clear_code_graph()?;

    let tx = graph.connection().unchecked_transaction()?;

    // 1) Files → file nodes + file_catalog (the `pg_populated` gate needs the
    //    catalog; the nodes anchor edges and symbol containment).
    for (path, fe) in &index.files {
        graph.upsert_node(&Node::file(path))?;
        graph.upsert_file_catalog(&FileCatalogEntry {
            path: fe.path.clone(),
            hash: fe.hash.clone(),
            language: fe.language.clone(),
            line_count: fe.line_count,
            token_count: fe.token_count,
            exports: fe.exports.clone(),
            summary: fe.summary.clone(),
        })?;
    }

    // 2) Symbols → symbol nodes carrying line span + kind/export metadata.
    for sym in index.symbols.values() {
        let mut node = Node::symbol(&sym.name, &sym.file, NodeKind::Symbol);
        node.line_start = Some(sym.start_line);
        node.line_end = Some(sym.end_line);
        node.metadata = Some(symbol_metadata(&sym.kind, sym.is_exported));
        graph.upsert_node(&node)?;
    }

    // 3) Edges → structural edges between file nodes. `upsert_node` is
    //    idempotent, so re-resolving endpoint ids is safe and cheap.
    for e in &index.edges {
        let from_id = graph.upsert_node(&Node::file(&e.from))?;
        let to_id = graph.upsert_node(&Node::file(&e.to))?;
        graph.upsert_edge(&Edge::new(from_id, to_id, map_edge_kind(&e.kind)))?;
    }

    tx.commit()?;
    Ok(())
}

/// Reliable, reusable PG build entry (#682.2): open the project's graph store,
/// [`populate_from_project_index`] from `index`, and stamp `graph.meta.json`.
///
/// Used by both the index orchestrator (with the index it just scanned) and the
/// `graph_provider` builder (after a load-or-scan), so the property graph is
/// built by the same worker that builds the JSON index — no dedicated
/// fire-and-forget thread that dies in short-lived processes.
pub fn mirror_index(project_root: &str, index: &ProjectIndex) -> anyhow::Result<()> {
    let t0 = Instant::now();
    let graph = CodeGraph::open(project_root)?;
    populate_from_project_index(&graph, index)?;

    let root_path = Path::new(project_root);
    let _ = write_meta(
        project_root,
        &PropertyGraphMetaV1 {
            schema_version: 1,
            engine_version: GRAPH_ENGINE_VERSION,
            built_with: env!("CARGO_PKG_VERSION").to_string(),
            project_root: crate::core::graph_index::normalize_project_root(project_root),
            built_at: chrono::Utc::now().to_rfc3339(),
            git_head: git_short_head(root_path),
            git_dirty: Some(git_is_dirty(root_path)),
            nodes: graph.node_count().ok(),
            edges: graph.edge_count().ok(),
            files_indexed: Some(index.files.len()),
            build_time_ms: Some(t0.elapsed().as_millis() as u64),
        },
    );
    Ok(())
}

fn git_short_head(root: &Path) -> Option<String> {
    crate::core::git::run_git(
        &["rev-parse", "--short", "HEAD"],
        root,
        Duration::from_secs(5),
        &[],
    )
    .ok()
    .and_then(|o| o.ok_stdout().ok())
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty())
}

fn git_is_dirty(root: &Path) -> bool {
    crate::core::git::run_git(
        &["status", "--porcelain"],
        root,
        Duration::from_secs(5),
        &[],
    )
    .is_ok_and(|o| !o.stdout.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::graph_index::{FileEntry, IndexEdge, SymbolEntry};
    use crate::core::graph_provider::GraphProvider;

    fn file_entry(path: &str) -> FileEntry {
        FileEntry {
            path: path.to_string(),
            hash: "h".to_string(),
            language: "rs".to_string(),
            line_count: 10,
            token_count: 20,
            exports: vec![],
            summary: String::new(),
        }
    }

    fn fixture_index() -> ProjectIndex {
        let mut idx = ProjectIndex::new("/test");
        for f in ["src/a.rs", "src/b.rs", "src/c.rs"] {
            idx.files.insert(f.to_string(), file_entry(f));
        }
        idx.symbols.insert(
            "src/a.rs::run".to_string(),
            SymbolEntry {
                file: "src/a.rs".to_string(),
                name: "run".to_string(),
                kind: "function".to_string(),
                start_line: 1,
                end_line: 5,
                is_exported: true,
            },
        );
        idx.symbols.insert(
            "src/b.rs::Helper".to_string(),
            SymbolEntry {
                file: "src/b.rs".to_string(),
                name: "Helper".to_string(),
                kind: "struct".to_string(),
                start_line: 3,
                end_line: 9,
                is_exported: false,
            },
        );
        idx.edges.push(IndexEdge {
            from: "src/a.rs".to_string(),
            to: "src/b.rs".to_string(),
            kind: "import".to_string(),
            weight: 1.0,
        });
        idx.edges.push(IndexEdge {
            from: "src/a.rs".to_string(),
            to: "src/c.rs".to_string(),
            kind: "import".to_string(),
            weight: 1.0,
        });
        idx
    }

    #[test]
    fn mirror_populates_file_catalog_and_nodes() {
        let pg = CodeGraph::open_in_memory().unwrap();
        let idx = fixture_index();
        populate_from_project_index(&pg, &idx).unwrap();

        assert_eq!(pg.file_catalog_count().unwrap(), 3, "all files cataloged");
        assert_eq!(pg.symbol_count().unwrap(), 2, "both symbols mirrored");
        assert!(pg.node_count().unwrap() >= 3 + 2, "file + symbol nodes");
        assert!(pg.edge_count().unwrap() >= 2, "import edges mirrored");
    }

    #[test]
    fn facade_parity_property_graph_equals_graph_index() {
        let pg = CodeGraph::open_in_memory().unwrap();
        let idx = fixture_index();
        populate_from_project_index(&pg, &idx).unwrap();

        let gi = GraphProvider::GraphIndex(fixture_index());
        let pgp = GraphProvider::PropertyGraph(pg);

        // file inventory
        assert_eq!(pgp.file_count(), gi.file_count());
        assert_eq!(pgp.file_paths(), gi.file_paths());
        assert_eq!(pgp.symbol_count(), gi.symbol_count());

        // structural dependencies (import edges) must agree exactly
        let mut pg_dep = pgp.dependencies("src/a.rs");
        let mut gi_dep = gi.dependencies("src/a.rs");
        pg_dep.sort();
        gi_dep.sort();
        assert_eq!(pg_dep, gi_dep, "dependencies must match");

        let mut pg_rdep = pgp.dependents("src/b.rs");
        let mut gi_rdep = gi.dependents("src/b.rs");
        pg_rdep.sort();
        gi_rdep.sort();
        assert_eq!(pg_rdep, gi_rdep, "dependents must match");

        // symbol lookup by `file::name`
        let pg_sym = pgp.get_symbol("src/a.rs::run").expect("pg symbol");
        let gi_sym = gi.get_symbol("src/a.rs::run").expect("gi symbol");
        assert_eq!(pg_sym.name, gi_sym.name);
        assert_eq!(pg_sym.file, gi_sym.file);
        assert_eq!(pg_sym.start_line, gi_sym.start_line);
        assert_eq!(pg_sym.end_line, gi_sym.end_line);
    }

    #[test]
    fn mirror_preserves_cross_source_edges() {
        let pg = CodeGraph::open_in_memory().unwrap();
        pg.upsert_cross_source_edge("src/a.rs", "github:issue/42", "mentioned_in", 1.0)
            .unwrap();
        assert_eq!(pg.cross_source_edge_count().unwrap(), 1);

        populate_from_project_index(&pg, &fixture_index()).unwrap();

        assert_eq!(
            pg.cross_source_edge_count().unwrap(),
            1,
            "provider cross-source edges survive a code-graph rebuild"
        );
        assert_eq!(pg.file_catalog_count().unwrap(), 3, "code graph rebuilt");
    }

    #[test]
    fn mirror_is_idempotent() {
        let pg = CodeGraph::open_in_memory().unwrap();
        let idx = fixture_index();
        populate_from_project_index(&pg, &idx).unwrap();
        populate_from_project_index(&pg, &idx).unwrap();

        assert_eq!(pg.file_catalog_count().unwrap(), 3);
        assert_eq!(pg.symbol_count().unwrap(), 2);
        assert_eq!(pg.edge_count().unwrap(), 2, "no duplicate edges on rerun");
    }
}
