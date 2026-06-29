//! Code-health interconnection fabric (Phase 3 / #1083).
//!
//! Fans complexity hotspots out across the shared data fabric so health becomes
//! a *queryable, cross-linked* signal rather than a leaf report:
//!   - **BM25** gets a `health://complexity/...` chunk per hotspot, so
//!     `ctx_semantic_search "complex functions"` surfaces them.
//!   - The **property graph** gets a `health_hotspot` cross-source edge from the
//!     code file to that URI, so `ctx_read` shows a hotspot hint inline.
//!   - **Knowledge** gets a `code_health` fact, so `ctx_knowledge` can recall a
//!     function's complexity without re-parsing.
//!
//! Crucially this is a *replace-source*, not an append: each pass prunes the
//! previous one from every store first (via [`consolidation::PrunePrior`]), so a
//! resolved hotspot never lingers as a stale signal. Persistence is shared with
//! the provider pipeline — one store-write path, one set of guarantees.

use super::scan::ProjectHealth;
use crate::core::bm25_index::ChunkKind;
use crate::core::consolidation::{self, ConsolidationArtifacts, PrunePrior};
use crate::core::content_chunk::ContentChunk;
use crate::core::graph_index::IndexEdge;
use crate::core::knowledge_provider_extract::ExtractedFact;

/// URI scheme + BM25 prune prefix for health chunks.
const URI_SCHEME: &str = "health://";
/// Resource segment in the `health://<resource>/<id>` URI.
const RESOURCE: &str = "complexity";
/// Cross-source edge kind for the code-file → hotspot link.
const EDGE_KIND: &str = "health_hotspot";
/// Knowledge fact category for complexity facts.
const FACT_CATEGORY: &str = "code_health";
/// Confidence assigned to derived complexity facts (structural, not inferred).
const FACT_CONFIDENCE: f32 = 0.9;
/// Upper bound on per-symbol hotspot edges written to the property graph. Edges
/// are one cheap row each, but a pathological repo shouldn't write unbounded
/// rows from a background task. The worst offenders are kept when truncating.
const MAX_PG_HOTSPOT_EDGES: usize = 200;

/// Stable URI for a hotspot, e.g. `health://complexity/src/foo.rs#bar`.
fn hotspot_uri(item_id: &str) -> String {
    format!("{URI_SCHEME}{RESOURCE}/{item_id}")
}

/// Stable item id for a hotspot: `<file>#<symbol>`.
fn item_id(file: &str, symbol: &str) -> String {
    format!("{file}#{symbol}")
}

/// Build deterministic fabric artifacts from a health report. Empty when the
/// project is clean.
///
/// The surfaces have different coverage by design:
///   - **BM25 chunks + knowledge facts** cover the top-N hotspots in the score
///     (`score.hotspots`) — the searchable/recallable set, kept small so the
///     index and memory don't bloat.
///   - **Property-graph edges** cover *every* over-threshold function (capped at
///     [`MAX_PG_HOTSPOT_EDGES`]), so `ctx_symbol`/`ctx_callgraph` can annotate
///     any hotspot symbol with its cc. Edges are one cheap row each and survive
///     the code-graph mirror (`clear_code_graph` preserves cross-source edges).
///
/// Order follows the (pre-sorted) score and a stable worst-first sort, so
/// repeated builds are byte-identical (#498).
pub fn build_artifacts(health: &ProjectHealth) -> ConsolidationArtifacts {
    let mut artifacts = ConsolidationArtifacts::default();

    // BM25 + knowledge: bounded to the top-N hotspots.
    for h in &health.score.hotspots {
        let id = item_id(&h.file, &h.symbol);
        let title = format!("{} (cc={})", h.symbol, h.cognitive);
        let content = format!(
            "Code-health hotspot: function `{}` in {} (line {}) has cognitive \
             complexity {}, above the navigability threshold. High-complexity \
             functions cost more tokens to read and edit safely; consider \
             extracting nested logic.",
            h.symbol, h.file, h.line, h.cognitive
        );
        artifacts.bm25_chunks.push(ContentChunk::from_provider(
            "health",
            RESOURCE,
            &id,
            &title,
            ChunkKind::Other,
            content,
            vec![h.file.clone()],
            Some(serde_json::json!({
                "file": h.file,
                "symbol": h.symbol,
                "line": h.line,
                "cognitive": h.cognitive,
            })),
        ));
        artifacts.facts.push(ExtractedFact {
            category: FACT_CATEGORY.to_string(),
            key: id,
            value: format!("cognitive complexity {} (line {})", h.cognitive, h.line),
            confidence: FACT_CONFIDENCE,
        });
    }

    // PG edges: every over-threshold function (capped, worst-first).
    for h in pg_hotspots(health) {
        artifacts.edges.push(IndexEdge {
            from: h.file.clone(),
            to: hotspot_uri(&item_id(&h.file, &h.symbol)),
            kind: EDGE_KIND.to_string(),
            weight: h.cognitive as f32,
        });
    }

    artifacts
}

/// The set of hotspots that get a per-symbol PG edge: every over-threshold
/// function across the per-file reports, sorted worst-first and capped. Falls
/// back to `score.hotspots` when per-file detail is absent (e.g. a score-only
/// health value), so callers always get edges for the hotspots they have.
fn pg_hotspots(health: &ProjectHealth) -> Vec<super::Hotspot> {
    let mut all: Vec<super::Hotspot> = if health.files.is_empty() {
        health.score.hotspots.clone()
    } else {
        health
            .files
            .iter()
            .flat_map(|f| f.hotspots.iter().cloned())
            .collect()
    };
    all.sort_by(|a, b| {
        b.cognitive
            .cmp(&a.cognitive)
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.line.cmp(&b.line))
    });
    all.truncate(MAX_PG_HOTSPOT_EDGES);
    all
}

/// Look up the cognitive complexity recorded for `symbol` in the persisted
/// health fabric (the `health_hotspot` cross-source edges written by [`apply`]).
/// Returns the highest cc among same-named hotspots, or `None` when the symbol
/// is not a recorded hotspot or the graph is unavailable. Best-effort + cheap:
/// one SQLite read, no parsing.
pub fn hotspot_cc(root: &str, symbol: &str) -> Option<u32> {
    let pg = crate::core::property_graph::CodeGraph::open(root).ok()?;
    pg.all_cross_source_edges()
        .iter()
        .filter(|e| e.kind == EDGE_KIND)
        .filter_map(|e| {
            let sym = e.to.rsplit('#').next()?;
            (sym == symbol).then_some(e.weight.round() as u32)
        })
        .max()
}

/// The prune spec that makes the fabric a replace-source: evict the prior pass
/// from every store before writing the current one.
fn prune_spec() -> PrunePrior {
    PrunePrior {
        bm25_prefix: Some(URI_SCHEME.to_string()),
        edge_kind: Some(EDGE_KIND.to_string()),
        fact_category: Some(FACT_CATEGORY.to_string()),
    }
}

/// Replace the persisted health fabric for `root` with the current findings.
///
/// Always prunes the prior pass (even when `health` is clean, so a fixed project
/// drops its stale hotspots), then ingests the current artifacts. Best-effort
/// per store; never panics. Safe to call from the background indexer.
pub fn apply(root: &str, health: &ProjectHealth) {
    let artifacts = build_artifacts(health);
    consolidation::apply_artifacts_to_stores(&artifacts, root, &prune_spec());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::code_health::score::{Hotspot, NavigabilityScore};
    use crate::core::knowledge::ProjectKnowledge;
    use crate::core::property_graph::CodeGraph;

    fn score_with(hotspots: Vec<Hotspot>) -> NavigabilityScore {
        NavigabilityScore {
            score: 80,
            total_functions: 10,
            over_threshold: hotspots.len(),
            worst_cognitive: hotspots.iter().map(|h| h.cognitive).max().unwrap_or(0),
            import_cycles: 0,
            estimated_waste_usd: 0.0,
            hotspots,
        }
    }

    fn health_with(hotspots: Vec<Hotspot>) -> ProjectHealth {
        ProjectHealth {
            score: score_with(hotspots),
            files: Vec::new(),
            naming_count: 0,
        }
    }

    fn hotspot(file: &str, symbol: &str, line: usize, cognitive: u32) -> Hotspot {
        Hotspot {
            file: file.to_string(),
            symbol: symbol.to_string(),
            line,
            cognitive,
        }
    }

    #[test]
    fn build_emits_triplet_per_hotspot() {
        let health = health_with(vec![
            hotspot("src/a.rs", "big", 10, 22),
            hotspot("src/b.rs", "huge", 5, 31),
        ]);
        let a = build_artifacts(&health);
        assert_eq!(a.bm25_chunks.len(), 2);
        assert_eq!(a.edges.len(), 2);
        assert_eq!(a.facts.len(), 2);

        assert!(a.bm25_chunks[0].file_path.starts_with(URI_SCHEME));
        assert_eq!(
            a.bm25_chunks[0].file_path,
            "health://complexity/src/a.rs#big"
        );
        assert_eq!(a.facts[0].category, FACT_CATEGORY);
        assert_eq!(a.facts[0].key, "src/a.rs#big");

        // Edges are worst-first; each carries cc as the weight + the hotspot URI.
        assert_eq!(a.edges[0].weight, 31.0, "worst hotspot edge first");
        let edge_a = a.edges.iter().find(|e| e.from == "src/a.rs").unwrap();
        assert_eq!(edge_a.kind, EDGE_KIND);
        assert_eq!(edge_a.weight, 22.0);
        assert_eq!(edge_a.to, "health://complexity/src/a.rs#big");
    }

    #[test]
    fn build_clean_project_is_empty() {
        let a = build_artifacts(&health_with(Vec::new()));
        assert!(a.bm25_chunks.is_empty());
        assert!(a.edges.is_empty());
        assert!(a.facts.is_empty());
    }

    #[test]
    fn build_is_deterministic() {
        let health = health_with(vec![
            hotspot("src/a.rs", "big", 10, 22),
            hotspot("src/b.rs", "huge", 5, 31),
        ]);
        let first = build_artifacts(&health);
        let second = build_artifacts(&health);
        let paths = |a: &ConsolidationArtifacts| {
            a.bm25_chunks
                .iter()
                .map(|c| c.file_path.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(paths(&first), paths(&second));
        assert_eq!(
            first
                .facts
                .iter()
                .map(|f| f.value.clone())
                .collect::<Vec<_>>(),
            second
                .facts
                .iter()
                .map(|f| f.value.clone())
                .collect::<Vec<_>>(),
        );
    }

    /// The core Phase 3 guarantee: a resolved hotspot is pruned from knowledge
    /// and the property graph on the next pass, never lingering as stale signal.
    #[test]
    fn apply_then_clean_prunes_every_store() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_str().unwrap();
        // A real source file keeps the BM25 index non-stale across reloads.
        std::fs::write(dir.path().join("lib.rs"), "pub fn ok() {}\n").unwrap();

        apply(root, &health_with(vec![hotspot("src/a.rs", "big", 10, 22)]));

        let pg = CodeGraph::open(root).unwrap();
        assert_eq!(
            pg.all_cross_source_edges()
                .iter()
                .filter(|e| e.kind == EDGE_KIND)
                .count(),
            1,
            "hotspot edge written"
        );
        let knowledge = ProjectKnowledge::load(root).unwrap();
        assert_eq!(
            knowledge
                .facts
                .iter()
                .filter(|f| f.category == FACT_CATEGORY)
                .count(),
            1,
            "hotspot fact written"
        );

        // Project fixed → clean report must evict the prior pass everywhere.
        apply(root, &health_with(Vec::new()));

        let pg = CodeGraph::open(root).unwrap();
        assert_eq!(
            pg.all_cross_source_edges()
                .iter()
                .filter(|e| e.kind == EDGE_KIND)
                .count(),
            0,
            "resolved hotspot edge pruned"
        );
        let knowledge = ProjectKnowledge::load(root).unwrap();
        assert_eq!(
            knowledge
                .facts
                .iter()
                .filter(|f| f.category == FACT_CATEGORY)
                .count(),
            0,
            "resolved hotspot fact pruned"
        );
    }

    fn file_report(hotspots: Vec<Hotspot>) -> crate::core::code_health::FileReport {
        crate::core::code_health::FileReport {
            file: hotspots.first().map(|h| h.file.clone()).unwrap_or_default(),
            total_functions: hotspots.len(),
            over_threshold: hotspots.len(),
            worst_cognitive: hotspots.iter().map(|h| h.cognitive).max().unwrap_or(0),
            hotspots,
            naming: Vec::new(),
            wasted_tokens: 0,
        }
    }

    /// PG edges give per-symbol coverage of *every* over-threshold function, even
    /// when the score's hotspot list is bounded to fewer (#1084).
    #[test]
    fn pg_edges_cover_all_over_threshold_not_just_score() {
        let top = hotspot("src/a.rs", "worst", 1, 40);
        let health = ProjectHealth {
            // Score keeps only the single worst (bounded display set).
            score: score_with(vec![top.clone()]),
            // Per-file reports carry three over-threshold functions.
            files: vec![
                file_report(vec![top, hotspot("src/a.rs", "mid", 20, 25)]),
                file_report(vec![hotspot("src/b.rs", "low", 3, 18)]),
            ],
            naming_count: 0,
        };
        let a = build_artifacts(&health);
        assert_eq!(a.bm25_chunks.len(), 1, "BM25 follows the bounded score");
        assert_eq!(a.facts.len(), 1, "facts follow the bounded score");
        assert_eq!(
            a.edges.len(),
            3,
            "edges cover every over-threshold function"
        );
        assert_eq!(a.edges[0].weight, 40.0, "worst-first");
    }

    /// `hotspot_cc` recovers a symbol's complexity from the persisted edges —
    /// the lookup `ctx_callgraph risk` uses.
    #[test]
    fn hotspot_cc_reads_back_persisted_edge() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_str().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "pub fn ok() {}\n").unwrap();

        apply(root, &health_with(vec![hotspot("src/a.rs", "big", 10, 22)]));
        assert_eq!(hotspot_cc(root, "big"), Some(22));
        assert_eq!(hotspot_cc(root, "not_a_hotspot"), None);
    }
}
