//! Hebbian file co-access graph — "files that fire together, wire together".
//!
//! ## The idea (neuroscience → retrieval)
//!
//! Hebbian theory: synapses between co-active neurons strengthen (long-term
//! potentiation, LTP), while unused ones weaken (long-term depression / the
//! Ebbinghaus forgetting curve). We apply the same rule to files: whenever a
//! task surfaces a set of files *together*, we strengthen the association
//! between every pair; on each update all weights decay slightly, so stale
//! associations fade. Over time the graph learns the project's real working
//! paths — which the static import/AST graph cannot capture.
//!
//! The learned association is an additive retrieval signal: given a file the
//! agent is looking at, [`related`] returns the files history says tend to be
//! touched alongside it.
//!
//! The store is a small per-project JSON file; reads/writes are whole-file and
//! cheap because the graph is pruned aggressively (decay + min-weight + caps).

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Multiplicative decay applied to every edge on each `record` — the forgetting
/// curve. 0.98 ⇒ an association roughly halves after ~34 un-reinforced updates.
const DECAY: f64 = 0.98;
/// Edges weaker than this are pruned (kept the graph small + relevant).
const MIN_WEIGHT: f64 = 0.08;
/// Potentiation increment for a co-access (LTP step).
const LTP_INCREMENT: f64 = 1.0;
/// Cap on neighbours kept per file (strongest retained) to bound memory.
const MAX_NEIGHBORS: usize = 32;
/// Cap on total tracked files; beyond it new files are still recorded but the
/// weakest-degree files are evicted to stay bounded.
const MAX_FILES: usize = 5_000;
/// A single record never associates more than this many files (avoids O(n²)
/// blow-ups when a tool surfaces a huge file set).
const MAX_RECORD_FILES: usize = 24;

/// Persistent, decaying co-access graph for one project.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct CoAccessGraph {
    /// `file → (neighbour → weight)`. Symmetric by construction.
    edges: HashMap<String, HashMap<String, f64>>,
}

impl CoAccessGraph {
    /// Reinforce the mutual association of every pair in `files` (LTP) after
    /// decaying the whole graph one step (global forgetting). Self-pairs and
    /// duplicates are ignored. Bounded work: at most `MAX_RECORD_FILES²` pairs.
    pub fn record(&mut self, files: &[String]) {
        // Distinct, capped input.
        let mut uniq: Vec<&String> = Vec::new();
        for f in files {
            if !f.is_empty() && !uniq.contains(&f) {
                uniq.push(f);
                if uniq.len() >= MAX_RECORD_FILES {
                    break;
                }
            }
        }
        if uniq.len() < 2 {
            return; // nothing to associate
        }

        self.decay_all();

        for i in 0..uniq.len() {
            for j in (i + 1)..uniq.len() {
                self.bump(uniq[i], uniq[j]);
                self.bump(uniq[j], uniq[i]);
            }
        }

        self.prune();
    }

    /// Reinforce the association of `focus` with each file in `others` — a
    /// **star**, not a clique — after one global decay step.
    ///
    /// This is the right model for *streaming* access where one new file enters
    /// the working set (e.g. each `ctx_read`): it associates the newcomer with
    /// the recent set without re-reinforcing the already-known pairs among
    /// `others` (which [`CoAccessGraph::record`] would, biasing toward early files).
    pub fn record_focus(&mut self, focus: &str, others: &[String]) {
        if focus.is_empty() {
            return;
        }
        let mut uniq: Vec<&String> = Vec::new();
        for f in others {
            if !f.is_empty() && f.as_str() != focus && !uniq.contains(&f) {
                uniq.push(f);
                if uniq.len() >= MAX_RECORD_FILES {
                    break;
                }
            }
        }
        if uniq.is_empty() {
            return; // nothing to associate
        }

        self.decay_all();
        for other in uniq {
            self.bump(focus, other);
            self.bump(other, focus);
        }
        self.prune();
    }

    /// Files most strongly associated with `file`, strongest first.
    #[must_use]
    pub fn related(&self, file: &str, top_k: usize) -> Vec<(String, f64)> {
        let Some(neighbours) = self.edges.get(file) else {
            return Vec::new();
        };
        let mut v: Vec<(String, f64)> = neighbours.iter().map(|(k, &w)| (k.clone(), w)).collect();
        v.sort_by(|a, b| b.1.total_cmp(&a.1));
        v.truncate(top_k);
        v
    }

    /// Canonical *undirected* co-access edges `(from, to, weight)` with
    /// `from <= to`, strongest first, weights `>= min_weight`, capped at
    /// `max_edges`. The graph is symmetric by construction, but asymmetric
    /// pruning can leave the two directions with slightly different weights, so
    /// the stronger direction wins. Deterministic order (weight desc, then path).
    #[must_use]
    pub fn canonical_edges(&self, min_weight: f64, max_edges: usize) -> Vec<(String, String, f64)> {
        let mut best: HashMap<(String, String), f64> = HashMap::new();
        for (from, neighbours) in &self.edges {
            for (to, &w) in neighbours {
                if w < min_weight || from == to {
                    continue;
                }
                let key = if from <= to {
                    (from.clone(), to.clone())
                } else {
                    (to.clone(), from.clone())
                };
                let slot = best.entry(key).or_insert(0.0);
                if w > *slot {
                    *slot = w;
                }
            }
        }
        let mut out: Vec<(String, String, f64)> =
            best.into_iter().map(|((a, b), w)| (a, b, w)).collect();
        out.sort_by(|x, y| {
            y.2.total_cmp(&x.2)
                .then_with(|| x.0.cmp(&y.0))
                .then_with(|| x.1.cmp(&y.1))
        });
        out.truncate(max_edges);
        out
    }

    fn bump(&mut self, from: &str, to: &str) {
        let entry = self.edges.entry(from.to_string()).or_default();
        *entry.entry(to.to_string()).or_insert(0.0) += LTP_INCREMENT;
    }

    fn decay_all(&mut self) {
        for neighbours in self.edges.values_mut() {
            for w in neighbours.values_mut() {
                *w *= DECAY;
            }
        }
    }

    fn prune(&mut self) {
        for neighbours in self.edges.values_mut() {
            neighbours.retain(|_, &mut w| w >= MIN_WEIGHT);
            if neighbours.len() > MAX_NEIGHBORS {
                let mut kept: Vec<(String, f64)> =
                    neighbours.iter().map(|(k, &w)| (k.clone(), w)).collect();
                kept.sort_by(|a, b| b.1.total_cmp(&a.1));
                kept.truncate(MAX_NEIGHBORS);
                *neighbours = kept.into_iter().collect();
            }
        }
        self.edges.retain(|_, neighbours| !neighbours.is_empty());

        if self.edges.len() > MAX_FILES {
            // Evict the lowest-degree files (least-connected memories).
            let mut by_degree: Vec<(String, usize)> = self
                .edges
                .iter()
                .map(|(k, n)| (k.clone(), n.len()))
                .collect();
            by_degree.sort_by_key(|(_, d)| *d);
            let evict = self.edges.len() - MAX_FILES;
            for (file, _) in by_degree.into_iter().take(evict) {
                self.edges.remove(&file);
            }
        }
    }
}

// ── Persistence (one small JSON file per project) ──────────────────────────

fn store_path(project_root: &str) -> Option<PathBuf> {
    let normalized = crate::core::graph_index::normalize_project_root(project_root);
    let hash = crate::core::project_hash::hash_project_root(&normalized);
    crate::core::paths::state_dir()
        .ok()
        .map(|d| d.join("cooccurrence").join(format!("{hash}.json")))
}

/// Load the co-access graph for `project_root` (empty if none / unreadable).
#[must_use]
pub fn load(project_root: &str) -> CoAccessGraph {
    let Some(path) = store_path(project_root) else {
        return CoAccessGraph::default();
    };
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save(project_root: &str, graph: &CoAccessGraph) {
    let Some(path) = store_path(project_root) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string(graph) {
        let _ = std::fs::write(&path, json);
    }
}

/// Record that `files` were accessed together for one task, persisting the
/// reinforced graph. No-op for fewer than two distinct files.
pub fn record_access(project_root: &str, files: &[String]) {
    if files.len() < 2 {
        return;
    }
    let mut graph = load(project_root);
    graph.record(files);
    save(project_root, &graph);
}

/// Files historically co-accessed with `file`, strongest association first.
#[must_use]
pub fn related(project_root: &str, file: &str, top_k: usize) -> Vec<(String, f64)> {
    load(project_root).related(file, top_k)
}

// ── Traversal-edge surface (gated, repo-relative) ──────────────────────────
//
// These wrappers (used by ctx_read / ctx_semantic_search / the dashboard) honor
// the `[graph] traversal_edges` config and normalize paths to the repo-relative
// form the code graph uses, so learned edges line up with static edges (#289).

/// Whether traversal (co-access) edges are enabled (`[graph] traversal_edges`).
#[must_use]
pub fn traversal_enabled() -> bool {
    crate::core::config::Config::load().graph.traversal_edges
}

/// Normalize an absolute-or-relative path to the repo-relative form used as the
/// co-access / graph key (e.g. `/repo/src/a.rs` → `src/a.rs`).
fn to_repo_rel(path: &str, project_root: &str) -> String {
    let p = path.replace('\\', "/");
    let root = project_root.trim_end_matches('/').replace('\\', "/");
    if !root.is_empty() {
        let prefix = format!("{root}/");
        if let Some(rest) = p.strip_prefix(&prefix) {
            return rest.to_string();
        }
    }
    p.trim_start_matches('/').to_string()
}

/// Record a *streaming* co-access: the just-touched `focus` file against the
/// recent working set `others` (star association). Paths are normalized to
/// repo-relative. No-op when traversal edges are disabled or there is nothing
/// to associate. Persisted.
pub fn record_focus_access(project_root: &str, focus: &str, others: &[String]) {
    if !traversal_enabled() {
        return;
    }
    let focus_rel = to_repo_rel(focus, project_root);
    if focus_rel.is_empty() {
        return;
    }
    let others_rel: Vec<String> = others
        .iter()
        .map(|o| to_repo_rel(o, project_root))
        .filter(|o| !o.is_empty() && o != &focus_rel)
        .collect();
    if others_rel.is_empty() {
        return;
    }
    let mut graph = load(project_root);
    graph.record_focus(&focus_rel, &others_rel);
    save(project_root, &graph);
}

/// Record that a *set* of files was surfaced together (e.g. search results)
/// after normalizing to repo-relative. No-op when disabled or <2 distinct files.
pub fn record_set_access(project_root: &str, files: &[String]) {
    if !traversal_enabled() {
        return;
    }
    let rel: Vec<String> = files
        .iter()
        .map(|f| to_repo_rel(f, project_root))
        .filter(|f| !f.is_empty())
        .collect();
    record_access(project_root, &rel);
}

/// Canonical undirected co-access edges for `project_root` (strongest first),
/// for dashboard overlay and graph folding. Empty when traversal edges are off.
#[must_use]
pub fn export_edges(
    project_root: &str,
    min_weight: f64,
    max_edges: usize,
) -> Vec<(String, String, f64)> {
    if !traversal_enabled() {
        return Vec::new();
    }
    load(project_root).canonical_edges(min_weight, max_edges)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn co_access_strengthens_association() {
        let mut g = CoAccessGraph::default();
        g.record(&["a.rs".into(), "b.rs".into()]);
        let rel = g.related("a.rs", 5);
        assert_eq!(rel.len(), 1);
        assert_eq!(rel[0].0, "b.rs");
        assert!(rel[0].1 > 0.0);
    }

    #[test]
    fn repeated_co_access_outweighs_single() {
        let mut g = CoAccessGraph::default();
        for _ in 0..5 {
            g.record(&["x.rs".into(), "y.rs".into()]);
        }
        g.record(&["x.rs".into(), "z.rs".into()]);
        let rel = g.related("x.rs", 5);
        // y was reinforced 5×, z once → y must rank first.
        assert_eq!(rel[0].0, "y.rs");
        assert!(rel.iter().any(|(f, _)| f == "z.rs"));
        assert!(rel[0].1 > rel.iter().find(|(f, _)| f == "z.rs").unwrap().1);
    }

    #[test]
    fn weak_associations_are_pruned_by_decay() {
        let mut g = CoAccessGraph::default();
        g.record(&["a.rs".into(), "b.rs".into()]);
        // Hammer an unrelated pair so the a–b edge decays below MIN_WEIGHT.
        for _ in 0..400 {
            g.record(&["c.rs".into(), "d.rs".into()]);
        }
        assert!(
            g.related("a.rs", 5).is_empty(),
            "decayed association should be pruned"
        );
        assert!(!g.related("c.rs", 5).is_empty());
    }

    #[test]
    fn single_file_record_is_noop() {
        let mut g = CoAccessGraph::default();
        g.record(&["lonely.rs".into()]);
        assert!(g.related("lonely.rs", 5).is_empty());
    }

    #[test]
    fn association_is_symmetric() {
        let mut g = CoAccessGraph::default();
        g.record(&["one.rs".into(), "two.rs".into()]);
        assert_eq!(g.related("one.rs", 5)[0].0, "two.rs");
        assert_eq!(g.related("two.rs", 5)[0].0, "one.rs");
    }

    #[test]
    fn serializes_round_trip() {
        // Deterministic: exercises the persistence *format* (the on-disk path
        // uses this same serde round-trip) without touching the process-global
        // data-dir env var, which other tests mutate concurrently.
        let mut g = CoAccessGraph::default();
        g.record(&["alpha.rs".into(), "beta.rs".into()]);
        let json = serde_json::to_string(&g).unwrap();
        let restored: CoAccessGraph = serde_json::from_str(&json).unwrap();
        let rel = restored.related("alpha.rs", 5);
        assert_eq!(rel.len(), 1);
        assert_eq!(rel[0].0, "beta.rs");
    }

    #[test]
    fn neighbours_are_capped() {
        let mut g = CoAccessGraph::default();
        // Pair one hub file with many distinct others across separate records
        // so its neighbour set exceeds the cap before pruning.
        for i in 0..(MAX_NEIGHBORS + 20) {
            g.record(&["hub.rs".into(), format!("f{i}.rs")]);
        }
        assert!(g.related("hub.rs", 1000).len() <= MAX_NEIGHBORS);
    }

    #[test]
    fn record_focus_is_a_star_not_a_clique() {
        let mut g = CoAccessGraph::default();
        // `new.rs` enters a working set of {a.rs, b.rs}.
        g.record_focus("new.rs", &["a.rs".into(), "b.rs".into()]);
        // It associates with both members of the set...
        assert_eq!(g.related("new.rs", 5).len(), 2);
        // ...but a.rs and b.rs are NOT associated with each other (star, not clique).
        assert!(g.related("a.rs", 5).iter().all(|(f, _)| f != "b.rs"));
        assert_eq!(g.related("a.rs", 5)[0].0, "new.rs");
    }

    #[test]
    fn record_focus_ignores_self_and_empty() {
        let mut g = CoAccessGraph::default();
        g.record_focus("x.rs", &["x.rs".into(), String::new()]);
        assert!(g.related("x.rs", 5).is_empty());
    }

    #[test]
    fn canonical_edges_are_undirected_and_sorted() {
        let mut g = CoAccessGraph::default();
        for _ in 0..3 {
            g.record(&["a.rs".into(), "b.rs".into()]);
        }
        g.record(&["a.rs".into(), "c.rs".into()]);
        let edges = g.canonical_edges(0.0, 10);
        // a–b appears once (undirected), not as both a→b and b→a.
        let ab = edges
            .iter()
            .filter(|(f, t, _)| (f == "a.rs" && t == "b.rs") || (f == "b.rs" && t == "a.rs"))
            .count();
        assert_eq!(ab, 1);
        // Canonical `from <= to` ordering.
        assert!(edges.iter().all(|(f, t, _)| f <= t));
        // Strongest first: a–b (3×) ranks before a–c (1×).
        assert_eq!((edges[0].0.as_str(), edges[0].1.as_str()), ("a.rs", "b.rs"));
    }

    #[test]
    fn canonical_edges_respect_min_weight_and_cap() {
        let mut g = CoAccessGraph::default();
        g.record(&["a.rs".into(), "b.rs".into()]);
        assert!(
            g.canonical_edges(100.0, 10).is_empty(),
            "min_weight filters all"
        );
        assert!(g.canonical_edges(0.0, 0).is_empty(), "cap 0 yields nothing");
    }

    #[test]
    fn to_repo_rel_strips_project_root() {
        assert_eq!(to_repo_rel("/repo/src/a.rs", "/repo"), "src/a.rs");
        assert_eq!(to_repo_rel("/repo/src/a.rs", "/repo/"), "src/a.rs");
        assert_eq!(to_repo_rel("src/a.rs", "/repo"), "src/a.rs");
        assert_eq!(to_repo_rel("/other/x.rs", "/repo"), "other/x.rs");
    }
}
