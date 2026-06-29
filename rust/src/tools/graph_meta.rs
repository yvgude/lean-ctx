//! Shared project + property-graph metadata summaries embedded in the JSON
//! output of `ctx_impact` and `ctx_architecture`.

use serde_json::{Value, json};
use std::path::Path;

use crate::core::git_util::{git_dirty, git_out};

/// Stable project identity + current git state, used as the `"project"` block
/// of a tool's JSON envelope.
pub(crate) fn project_meta(root: &str) -> Value {
    let root_hash = crate::core::project_hash::hash_project_root(root);
    let identity_hash = crate::core::project_hash::project_identity(root)
        .as_deref()
        .map(crate::core::hasher::hash_str);

    let root_path = Path::new(root);
    json!({
        "project_root_hash": root_hash,
        "project_identity_hash": identity_hash,
        "git": {
            "head": git_out(root_path, &["rev-parse", "--short", "HEAD"]),
            "branch": git_out(root_path, &["rev-parse", "--abbrev-ref", "HEAD"]),
            "dirty": git_dirty(root_path)
        }
    })
}

/// Property-graph existence + node/edge counts, used as the `"graph"` block of
/// a tool's JSON envelope. Accepts any path-like so both `&str` and `&Path`
/// call sites work unchanged.
pub(crate) fn graph_summary<P: AsRef<Path>>(project_root: P) -> Value {
    let root_str = project_root.as_ref().to_string_lossy();
    let graph_dir = crate::core::property_graph::graph_dir(root_str.as_ref());
    let db_path = graph_dir.join("graph.db");
    let db_path_display = db_path.display().to_string();
    if !db_path.exists() {
        return json!({
            "exists": false,
            "db_path": db_path_display,
            "nodes": null,
            "edges": null
        });
    }
    match crate::core::property_graph::CodeGraph::open(root_str.as_ref()) {
        Ok(g) => json!({
            "exists": true,
            "db_path": g.db_path().display().to_string(),
            "nodes": g.node_count().ok(),
            "edges": g.edge_count().ok()
        }),
        Err(_) => json!({
            "exists": true,
            "db_path": db_path_display,
            "nodes": null,
            "edges": null
        }),
    }
}
