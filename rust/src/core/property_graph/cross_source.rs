//! Cross-source edges — lateral links between code files and external sources
//! (issues, PRs, DB schemas, wiki pages) discovered via the provider pipeline.
//!
//! Stored in a dedicated table rather than the code node/edge model (#682) so
//! external URIs never pollute the File-node catalog and the exact provider
//! relation kind and weight survive round-trips — which the `cross_source_hints`
//! consumer relies on for weighting and relation labels.

use rusqlite::{Connection, params};

use crate::core::graph_index::IndexEdge;

pub(super) fn upsert(
    conn: &Connection,
    from: &str,
    to: &str,
    kind: &str,
    weight: f32,
) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO cross_source_edges (from_path, to_path, kind, weight)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(from_path, to_path, kind) DO UPDATE SET
            weight = MAX(weight, excluded.weight)",
        params![from, to, kind, f64::from(weight)],
    )?;
    Ok(())
}

pub(super) fn all(conn: &Connection) -> anyhow::Result<Vec<IndexEdge>> {
    let mut stmt =
        conn.prepare("SELECT from_path, to_path, kind, weight FROM cross_source_edges")?;
    let edges = stmt
        .query_map([], |row| {
            Ok(IndexEdge {
                from: row.get(0)?,
                to: row.get(1)?,
                kind: row.get(2)?,
                weight: row.get::<_, f64>(3)? as f32,
            })
        })?
        .filter_map(std::result::Result::ok)
        .collect();
    Ok(edges)
}

pub(super) fn count(conn: &Connection) -> anyhow::Result<usize> {
    let c: i64 = conn.query_row("SELECT COUNT(*) FROM cross_source_edges", [], |row| {
        row.get(0)
    })?;
    Ok(c as usize)
}

/// Delete every cross-source edge of a given `kind`. Lets a recomputed source
/// (the code-health fabric) evict its prior pass so resolved hotspots don't
/// linger as stale `health_hotspot` hints. Returns the number of rows removed.
pub(super) fn delete_by_kind(conn: &Connection, kind: &str) -> anyhow::Result<usize> {
    let removed = conn.execute(
        "DELETE FROM cross_source_edges WHERE kind = ?1",
        params![kind],
    )?;
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::super::CodeGraph;

    #[test]
    fn upsert_keeps_higher_weight_and_round_trips_kind() {
        let g = CodeGraph::open_in_memory().unwrap();
        g.upsert_cross_source_edge("src/auth.rs", "github://issues/42", "mentions", 1.0)
            .unwrap();
        // Same triple, lower weight must not downgrade.
        g.upsert_cross_source_edge("src/auth.rs", "github://issues/42", "mentions", 0.5)
            .unwrap();
        // Same triple, higher weight upgrades.
        g.upsert_cross_source_edge("src/auth.rs", "github://issues/42", "mentions", 1.5)
            .unwrap();
        // Distinct kind is a separate edge.
        g.upsert_cross_source_edge("src/db.rs", "postgres://schemas/sessions", "queries", 1.2)
            .unwrap();

        let mut edges = g.all_cross_source_edges();
        edges.sort_by(|a, b| a.to.cmp(&b.to));
        assert_eq!(edges.len(), 2);
        assert_eq!(g.cross_source_edge_count().unwrap(), 2);

        let issue = edges.iter().find(|e| e.to.contains("issues/42")).unwrap();
        assert_eq!(issue.kind, "mentions");
        assert!((issue.weight - 1.5).abs() < f32::EPSILON, "weight upgraded");

        let schema = edges.iter().find(|e| e.kind == "queries").unwrap();
        assert!((schema.weight - 1.2).abs() < 1e-6);
    }

    #[test]
    fn empty_when_no_cross_source_edges() {
        let g = CodeGraph::open_in_memory().unwrap();
        assert!(g.all_cross_source_edges().is_empty());
        assert_eq!(g.cross_source_edge_count().unwrap(), 0);
    }

    #[test]
    fn delete_by_kind_removes_only_that_kind() {
        let g = CodeGraph::open_in_memory().unwrap();
        g.upsert_cross_source_edge("src/a.rs", "health://complexity/a", "health_hotspot", 22.0)
            .unwrap();
        g.upsert_cross_source_edge("src/b.rs", "health://complexity/b", "health_hotspot", 31.0)
            .unwrap();
        g.upsert_cross_source_edge("src/a.rs", "github://issues/42", "mentions", 1.0)
            .unwrap();

        let removed = g
            .delete_cross_source_edges_by_kind("health_hotspot")
            .unwrap();
        assert_eq!(removed, 2, "both hotspot edges removed");
        assert_eq!(g.cross_source_edge_count().unwrap(), 1);
        assert!(
            g.all_cross_source_edges()
                .iter()
                .all(|e| e.kind == "mentions"),
            "unrelated provider edge survives"
        );

        // Deleting an absent kind is a no-op.
        assert_eq!(
            g.delete_cross_source_edges_by_kind("health_hotspot")
                .unwrap(),
            0
        );
    }
}
