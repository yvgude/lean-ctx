//! Spreading activation — associative retrieval over a weighted graph.
//!
//! ## The idea (cognitive science → retrieval)
//!
//! In ACT-R and classic semantic-network models (Collins & Loftus 1975),
//! recall works by *spreading activation*: cue concepts light up, and energy
//! flows along associative links to related concepts, attenuating with distance.
//! Items that many short, strong paths reach end up most activated — i.e. most
//! relevant to the cue.
//!
//! We apply this to code: seed activation at the files/symbols a task names,
//! then spread it across the project graph (imports, calls, co-access). The
//! resulting activation is an associative relevance signal that complements
//! lexical BM25 — it surfaces files that are *structurally* close to the seeds
//! even when they share no query terms.
//!
//! ## Convergence
//!
//! Each node's outgoing edges are fan-out-normalised (they sum to 1), so a node
//! re-emits at most `decay · energy` (with `decay < 1`). Total energy in the
//! system is therefore strictly decreasing, guaranteeing termination; a firing
//! threshold prunes negligible pulses so cost stays near the active frontier.

use std::collections::HashMap;

/// Pulses below this energy are not propagated further (keeps work bounded and
/// the result sparse).
const FIRING_THRESHOLD: f64 = 1e-4;

/// Spread `seeds` over `adjacency` for up to `iterations` hops.
///
/// - `seeds`: initial activation per node (the query cues).
/// - `adjacency`: `node → [(neighbour, weight)]`; weights need not be
///   normalised (we fan-out-normalise internally).
/// - `decay`: per-hop attenuation in `(0, 1)`; smaller ⇒ activation stays more
///   local. Values outside the range are clamped.
///
/// Returns accumulated activation per reached node (including seeds). Nodes not
/// reachable from any seed are absent (implicitly zero).
#[must_use]
pub fn spread(
    seeds: &HashMap<String, f64>,
    adjacency: &HashMap<String, Vec<(String, f64)>>,
    decay: f64,
    iterations: usize,
) -> HashMap<String, f64> {
    let decay = decay.clamp(0.0, 0.999);
    let mut activation: HashMap<String, f64> = seeds.clone();
    let mut frontier: HashMap<String, f64> = seeds.clone();

    for _ in 0..iterations {
        let mut next: HashMap<String, f64> = HashMap::new();
        for (node, &energy) in &frontier {
            if energy < FIRING_THRESHOLD {
                continue;
            }
            let Some(edges) = adjacency.get(node) else {
                continue;
            };
            let total: f64 = edges.iter().map(|(_, w)| w.max(0.0)).sum();
            if total <= 0.0 {
                continue;
            }
            for (nbr, w) in edges {
                let w = w.max(0.0);
                if w <= 0.0 {
                    continue;
                }
                let delta = energy * decay * (w / total);
                if delta >= FIRING_THRESHOLD {
                    *next.entry(nbr.clone()).or_insert(0.0) += delta;
                }
            }
        }
        if next.is_empty() {
            break;
        }
        for (node, e) in &next {
            *activation.entry(node.clone()).or_insert(0.0) += e;
        }
        frontier = next;
    }

    activation
}

/// Convenience: spread from `seeds` and return the top-`k` *non-seed* nodes by
/// activation, strongest first — the files most associatively related to the
/// cues but not already named by them.
#[must_use]
pub fn related_ranked(
    seeds: &HashMap<String, f64>,
    adjacency: &HashMap<String, Vec<(String, f64)>>,
    decay: f64,
    iterations: usize,
    top_k: usize,
) -> Vec<(String, f64)> {
    let activation = spread(seeds, adjacency, decay, iterations);
    let mut ranked: Vec<(String, f64)> = activation
        .into_iter()
        .filter(|(node, _)| !seeds.contains_key(node))
        .collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
    ranked.truncate(top_k);
    ranked
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edge(adj: &mut HashMap<String, Vec<(String, f64)>>, from: &str, to: &str, w: f64) {
        adj.entry(from.to_string())
            .or_default()
            .push((to.to_string(), w));
        adj.entry(to.to_string())
            .or_default()
            .push((from.to_string(), w));
    }

    #[test]
    fn activation_reaches_connected_nodes_only() {
        let mut adj = HashMap::new();
        edge(&mut adj, "a", "b", 1.0);
        edge(&mut adj, "b", "c", 1.0);
        // "island" is disconnected.
        adj.entry("island".to_string()).or_default();

        let seeds = HashMap::from([("a".to_string(), 1.0)]);
        let act = spread(&seeds, &adj, 0.7, 5);

        assert!(act.contains_key("b"));
        assert!(act.contains_key("c"));
        assert!(!act.contains_key("island"));
    }

    #[test]
    fn closer_nodes_get_more_activation() {
        let mut adj = HashMap::new();
        edge(&mut adj, "a", "b", 1.0); // 1 hop from a
        edge(&mut adj, "b", "c", 1.0); // 2 hops from a

        let seeds = HashMap::from([("a".to_string(), 1.0)]);
        let act = spread(&seeds, &adj, 0.7, 5);

        // b (closer) must be more activated than c (farther).
        assert!(act["b"] > act["c"]);
    }

    #[test]
    fn stronger_edges_transmit_more() {
        let mut adj = HashMap::new();
        edge(&mut adj, "seed", "strong", 9.0);
        edge(&mut adj, "seed", "weak", 1.0);

        let seeds = HashMap::from([("seed".to_string(), 1.0)]);
        let act = spread(&seeds, &adj, 0.7, 3);
        assert!(act["strong"] > act["weak"]);
    }

    #[test]
    fn terminates_and_stays_bounded_on_cycles() {
        // A cycle would loop forever without decay + threshold.
        let mut adj = HashMap::new();
        edge(&mut adj, "a", "b", 1.0);
        edge(&mut adj, "b", "c", 1.0);
        edge(&mut adj, "c", "a", 1.0);

        let seeds = HashMap::from([("a".to_string(), 1.0)]);
        let act = spread(&seeds, &adj, 0.9, 1000);
        // Total activation is finite (energy strictly decreases per hop).
        let total: f64 = act.values().sum();
        assert!(total.is_finite());
        assert!(total < 100.0, "energy must not blow up on cycles: {total}");
    }

    #[test]
    fn related_ranked_excludes_seeds() {
        let mut adj = HashMap::new();
        edge(&mut adj, "a", "b", 1.0);
        edge(&mut adj, "a", "c", 1.0);

        let seeds = HashMap::from([("a".to_string(), 1.0)]);
        let ranked = related_ranked(&seeds, &adj, 0.7, 3, 10);
        assert!(ranked.iter().all(|(n, _)| n != "a"));
        assert_eq!(ranked.len(), 2);
    }

    #[test]
    fn empty_seeds_yield_empty_result() {
        let adj = HashMap::new();
        let seeds = HashMap::new();
        assert!(spread(&seeds, &adj, 0.7, 5).is_empty());
    }
}
