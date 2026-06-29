//! code-symbols adapter (#1099): Serena-style symbol output → property-graph
//! reference edges. Serena resolves references with an LSP, so these edges are
//! precise where tree-sitter is heuristic — they *complement* the built-in
//! graph rather than replace it.
//!
//! Defensive JSON parsing handles two shapes:
//!   - explicit edges: `[{from|source, to|target, …}]` (same as code-graph)
//!   - `find_referencing_symbols`: a queried symbol (`name_path`) plus a list of
//!     referencing locations (`relative_path`), turned into
//!     `relative_path --references--> symbol://name_path` edges.
//! Runs as a side-channel; never panics, never touches returned text.

use crate::core::property_graph::CodeGraph;

struct RefEdge {
    from: String,
    to: String,
    kind: String,
}

/// Parse `text` for symbol references and merge them into the property graph.
pub fn ingest(server: &str, _tool: &str, text: &str, project_root: &str) {
    let edges = parse(text);
    if edges.is_empty() {
        return;
    }
    let Ok(pg) = CodeGraph::open(project_root) else {
        tracing::warn!("[code-symbols adapter] property graph open failed for `{server}`");
        return;
    };
    for e in &edges {
        let _ = pg.upsert_cross_source_edge(&e.from, &e.to, &e.kind, 1.0);
    }
}

fn parse(text: &str) -> Vec<RefEdge> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text.trim()) else {
        return Vec::new();
    };
    // The queried symbol, when echoed at the top level.
    let target = str_field(
        v.as_object(),
        &["name_path", "symbol", "symbolName", "name"],
    )
    .map(|s| format!("symbol://{s}"));

    let items = v
        .get("references")
        .or_else(|| v.get("referencing_symbols"))
        .or_else(|| v.get("referencingSymbols"))
        .or_else(|| v.get("results"))
        .and_then(serde_json::Value::as_array)
        .or_else(|| v.as_array());
    let Some(items) = items else {
        return Vec::new();
    };

    items
        .iter()
        .filter_map(|item| parse_item(item.as_object()?, target.as_deref()))
        .collect()
}

fn parse_item(
    obj: &serde_json::Map<String, serde_json::Value>,
    target: Option<&str>,
) -> Option<RefEdge> {
    // 1) explicit edge object.
    if let (Some(from), Some(to)) = (
        str_field(Some(obj), &["from", "source", "src", "caller"]),
        str_field(Some(obj), &["to", "target", "dst", "callee"]),
    ) {
        let kind = str_field(Some(obj), &["type", "kind", "relation"])
            .unwrap_or_else(|| "references".into());
        return Some(RefEdge { from, to, kind });
    }
    // 2) a referencing location pointing at the queried symbol.
    let from = str_field(
        Some(obj),
        &["relative_path", "relativePath", "file", "path", "name_path"],
    )?;
    let to = target.map(String::from)?;
    Some(RefEdge {
        from,
        to,
        kind: "references".into(),
    })
}

fn str_field(
    obj: Option<&serde_json::Map<String, serde_json::Value>>,
    keys: &[&str],
) -> Option<String> {
    let obj = obj?;
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
    fn parses_referencing_symbols_shape() {
        let payload = r#"{
            "name_path": "AuthService/login",
            "references": [
                {"relative_path": "src/api/handler.rs", "line": 42},
                {"relative_path": "src/jobs/worker.rs", "line": 7}
            ]
        }"#;
        let edges = parse(payload);
        assert_eq!(edges.len(), 2);
        assert!(edges.iter().all(|e| e.to == "symbol://AuthService/login"));
        assert!(edges.iter().any(|e| e.from == "src/api/handler.rs"));
    }

    #[test]
    fn parses_explicit_edges() {
        let payload = r#"[{"from":"src/a.rs","to":"src/b.rs","kind":"calls"}]"#;
        let edges = parse(payload);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].kind, "calls");
    }

    #[test]
    fn ingest_persists_reference_edges() {
        let _lock = crate::core::data_dir::test_env_lock();
        let proj = tempfile::tempdir().unwrap();
        let root = proj.path().to_str().unwrap();

        let payload = r#"{
            "name_path": "Config/load",
            "references": [{"relative_path": "src/main.rs"}]
        }"#;
        ingest("serena", "find_referencing_symbols", payload, root);

        let pg = CodeGraph::open(root).expect("open graph");
        let edges = pg.all_cross_source_edges();
        assert!(
            edges
                .iter()
                .any(|e| e.from == "src/main.rs" && e.to == "symbol://Config/load")
        );
    }
}
