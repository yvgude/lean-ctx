//! code-graph adapter (#1098): Graphify-style node/edge output → property-graph
//! cross-source edges, so `ctx_callgraph` and `ctx_read` hints benefit from an
//! externally-computed code graph.
//!
//! Defensive JSON parsing: accepts the common shapes
//!   - `{ "edges": [ {from|source, to|target, type|kind|relation}, … ] }`
//!   - a top-level array of edge objects
//! and skips anything it does not recognize (best-effort, never panics). Runs as
//! a side-channel (background thread), so it cannot affect output determinism.

use crate::core::property_graph::CodeGraph;

struct ParsedEdge {
    from: String,
    to: String,
    kind: String,
}

/// Parse `text` for graph edges and merge them into the project's property graph
/// as cross-source edges. No-op on unparseable input or an empty edge set.
pub fn ingest(server: &str, _tool: &str, text: &str, project_root: &str) {
    let edges = parse_edges(text);
    if edges.is_empty() {
        return;
    }
    let Ok(pg) = CodeGraph::open(project_root) else {
        tracing::warn!("[code-graph adapter] property graph open failed for `{server}`");
        return;
    };
    for e in &edges {
        let kind = if e.kind.is_empty() {
            format!("{server}_edge")
        } else {
            e.kind.clone()
        };
        let _ = pg.upsert_cross_source_edge(&e.from, &e.to, &kind, 1.0);
    }
}

fn parse_edges(text: &str) -> Vec<ParsedEdge> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text.trim()) else {
        return Vec::new();
    };
    let array = v
        .get("edges")
        .and_then(serde_json::Value::as_array)
        .or_else(|| v.as_array());
    let Some(items) = array else {
        return Vec::new();
    };
    items.iter().filter_map(parse_one).collect()
}

fn parse_one(item: &serde_json::Value) -> Option<ParsedEdge> {
    let obj = item.as_object()?;
    let from = str_field(obj, &["from", "source", "src", "caller"])?;
    let to = str_field(obj, &["to", "target", "dst", "callee"])?;
    let kind = str_field(obj, &["type", "kind", "relation", "label"]).unwrap_or_default();
    Some(ParsedEdge { from, to, kind })
}

/// First present, non-empty string field among `keys`.
fn str_field(obj: &serde_json::Map<String, serde_json::Value>, keys: &[&str]) -> Option<String> {
    keys.iter()
        .filter_map(|k| obj.get(*k).and_then(serde_json::Value::as_str))
        .map(str::trim)
        .find(|s| !s.is_empty())
        .map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_edges_object_and_array_shapes() {
        let obj = r#"{"edges":[{"from":"a.rs","to":"b.rs","type":"calls"}]}"#;
        assert_eq!(parse_edges(obj).len(), 1);
        let arr = r#"[{"source":"a.rs","target":"b.rs","relation":"imports"}]"#;
        let e = parse_edges(arr);
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].kind, "imports");
        assert!(parse_edges("garbage").is_empty());
        assert!(parse_edges(r#"{"nodes":[]}"#).is_empty());
    }

    #[test]
    fn ingest_writes_cross_source_edges() {
        let _lock = crate::core::data_dir::test_env_lock();
        let proj = tempfile::tempdir().unwrap();
        let root = proj.path().to_str().unwrap();

        let payload = r#"{"edges":[
            {"from":"src/auth.rs","to":"src/db.rs","type":"calls"},
            {"source":"src/api.rs","target":"src/auth.rs","kind":"imports"}
        ]}"#;
        ingest("graphify", "query_graph", payload, root);

        let pg = CodeGraph::open(root).expect("open graph");
        assert_eq!(pg.cross_source_edge_count().unwrap(), 2);
        let edges = pg.all_cross_source_edges();
        assert!(
            edges
                .iter()
                .any(|e| e.from == "src/auth.rs" && e.to == "src/db.rs")
        );
    }
}
