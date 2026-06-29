//! Persisted project health (`health.json`) + stale-guarded background refresh.
//!
//! The engine computes the [`NavigabilityScore`] once per index build and stores
//! it next to the graph index. Session-start and other surfaces then *read* it
//! (no re-parse), realizing the plan's "compute once, fan-out everywhere".
//!
//! The refresh is gated by a fingerprint of the indexed source set, so a touch
//! with no byte change never triggers a recompute (mirrors the index's own
//! content-hash reuse).

use super::scan::scan_project;
use super::score::NavigabilityScore;
use crate::core::graph_index::ProjectIndex;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const HEALTH_FILE: &str = "health.json";

/// Hotspots retained in the persisted score.
const TOP_HOTSPOTS: usize = 10;

/// Hotspots shown in the (budgeted) session-start block.
const SESSION_HOTSPOTS: usize = 3;

/// Project health persisted next to the graph index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedHealth {
    /// Fingerprint of the indexed source set; recompute only when it changes.
    pub fingerprint: String,
    pub threshold: u32,
    pub naming_count: usize,
    pub score: NavigabilityScore,
}

fn health_path(root: &str) -> Option<PathBuf> {
    Some(ProjectIndex::index_dir(root)?.join(HEALTH_FILE))
}

/// Load the persisted health for `root`, or `None` when absent/unreadable.
pub fn load(root: &str) -> Option<PersistedHealth> {
    let bytes = std::fs::read(health_path(root)?).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Fingerprint of the indexed file set (path + content hash). Stable + cheap so
/// it can gate the recompute without re-reading any source.
fn fingerprint(index: &ProjectIndex) -> String {
    let mut entries: Vec<String> = index
        .files
        .values()
        .map(|f| format!("{}:{}", f.path, f.hash))
        .collect();
    entries.sort();
    blake3::hash(entries.join("\n").as_bytes())
        .to_hex()
        .to_string()
}

/// Recompute + persist health only when the indexed source set changed. Safe to
/// call from the background indexer (off the hot path); never panics.
pub fn refresh_if_stale(root: &str, index: &ProjectIndex) {
    let Some(path) = health_path(root) else {
        return;
    };
    let fp = fingerprint(index);
    if load(root).is_some_and(|existing| existing.fingerprint == fp) {
        return;
    }

    let threshold = crate::core::config::Config::load()
        .code_health
        .cognitive_threshold;
    let health = scan_project(Path::new(root), threshold, None, TOP_HOTSPOTS);

    // Phase 3: fan the (top-N) hotspots out across BM25 / property graph /
    // knowledge as a replace-source, so health is queryable + cross-linked and
    // resolved hotspots are pruned. Done before the score is moved below.
    super::fabric::apply(root, &health);

    let persisted = PersistedHealth {
        fingerprint: fp,
        threshold,
        naming_count: health.naming_count,
        score: health.score,
    };

    if let Ok(json) = serde_json::to_vec_pretty(&persisted) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, json);
    }
}

/// Compact, deterministic session-start block. Empty when there is no persisted
/// health or the project is clean (no hotspots), so it never adds noise.
pub fn format_session_block(root: &str) -> String {
    load(root)
        .map(|h| render_session_block(&h))
        .unwrap_or_default()
}

/// Pure renderer for the session-start block (deterministic, #498-safe). Empty
/// when the project is clean.
fn render_session_block(health: &PersistedHealth) -> String {
    let s = &health.score;
    if s.hotspots.is_empty() {
        return String::new();
    }

    let mut out = String::from("--- CODE HEALTH (top hotspots) ---\n");
    out.push_str(&format!(
        "navigability {}/100 · {} fn over cc>{} · worst {}\n",
        s.score, s.over_threshold, health.threshold, s.worst_cognitive
    ));
    for h in s.hotspots.iter().take(SESSION_HOTSPOTS) {
        out.push_str(&format!(
            "- {}:{} {} cc={}\n",
            h.file, h.line, h.symbol, h.cognitive
        ));
    }
    out.push_str("(ctx_quality / lean-ctx health for full report)\n---");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::code_health::score::Hotspot;

    fn persisted(hotspots: Vec<Hotspot>) -> PersistedHealth {
        PersistedHealth {
            fingerprint: "fp".into(),
            threshold: 15,
            naming_count: 0,
            score: NavigabilityScore {
                score: 70,
                total_functions: 20,
                over_threshold: hotspots.len(),
                worst_cognitive: hotspots.iter().map(|h| h.cognitive).max().unwrap_or(0),
                import_cycles: 0,
                estimated_waste_usd: 0.0,
                hotspots,
            },
        }
    }

    #[test]
    fn session_block_empty_when_clean() {
        assert!(render_session_block(&persisted(Vec::new())).is_empty());
    }

    #[test]
    fn session_block_is_deterministic_and_caps_hotspots() {
        let hs: Vec<Hotspot> = (0..5)
            .map(|i| Hotspot {
                file: format!("src/f{i}.rs"),
                symbol: format!("fn{i}"),
                line: i + 1,
                cognitive: 30 - i as u32,
            })
            .collect();
        let ph = persisted(hs);

        let a = render_session_block(&ph);
        let b = render_session_block(&ph);
        assert_eq!(a, b, "session block must be byte-stable (#498)");
        assert!(a.contains("navigability 70/100"));
        // Only SESSION_HOTSPOTS lines, sorted worst-first as persisted.
        assert_eq!(a.matches("\n- ").count(), SESSION_HOTSPOTS);
        assert!(a.contains("src/f0.rs:1 fn0 cc=30"));
    }

    #[test]
    fn missing_health_yields_empty_block() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_string_lossy().to_string();
        assert!(format_session_block(&root).is_empty());
    }
}
