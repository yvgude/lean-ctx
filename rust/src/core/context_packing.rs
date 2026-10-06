//! Submodular context packing — choosing *which* items to include under a
//! budget so the selected set maximises coverage of what the task cares about.
//!
//! ## Why submodular?
//!
//! Picking a set of context items to maximise term coverage is the classic
//! **maximum coverage** problem. Coverage is *monotone submodular*: each added
//! item helps, but with diminishing returns (a second file covering the same
//! terms adds little). For such objectives the simple **greedy** algorithm —
//! repeatedly take the item with the largest marginal gain per unit cost — is
//! provably within a `1 − 1/e ≈ 0.63` factor of the optimum (Nemhauser,
//! Wolsey & Fisher 1978). That guarantee, plus `O(n·k)` cost, is exactly what
//! we want for online context assembly.
//!
//! This module is deliberately generic and side-effect free: callers map their
//! domain objects (symbol bodies, ranked files, chunks) onto [`CoverageItem`]s
//! and get back the indices to keep, in selection order.

use std::collections::HashSet;

/// A candidate for inclusion: the set of weighted terms it covers and the
/// budget cost of including it (e.g. its token count).
#[derive(Debug, Clone)]
pub(crate) struct CoverageItem {
    /// Distinct terms this item covers. Weights come from `term_weight`.
    pub terms: HashSet<String>,
    /// Budget consumed if this item is selected (must be ≥ 1).
    pub cost: usize,
}

/// Greedy submodular maximisation of weighted term coverage under `budget`.
///
/// Returns the indices of selected items in selection order. Items whose cost
/// alone exceeds the remaining budget are skipped. `term_weight` lets callers
/// prioritise rarer / more salient terms (uniform weight ⇒ plain coverage).
///
/// Guarantees: with unit costs this is the standard greedy max-coverage with a
/// `1 − 1/e` approximation ratio. With costs it uses marginal-gain-per-cost,
/// the cost-effective greedy heuristic.
pub(crate) fn greedy_max_coverage<F>(
    items: &[CoverageItem],
    budget: usize,
    term_weight: F,
) -> Vec<usize>
where
    F: Fn(&str) -> f64,
{
    greedy_scored_coverage(items, budget, term_weight, |_| 1.0)
}

/// The same host-owned budget/coverage selection with optional bounded item
/// priorities. Scores never supply content, identities or additional authority.
pub(crate) fn greedy_scored_coverage<F, S>(
    items: &[CoverageItem],
    budget: usize,
    term_weight: F,
    item_score: S,
) -> Vec<usize>
where
    F: Fn(&str) -> f64,
    S: Fn(usize) -> f64,
{
    let mut covered: HashSet<&str> = HashSet::new();
    let mut selected: Vec<usize> = Vec::new();
    let mut remaining: Vec<usize> = (0..items.len()).collect();
    let mut spent = 0usize;

    while !remaining.is_empty() && spent < budget {
        let mut best: Option<(usize, usize, f64)> = None; // (pos_in_remaining, item_idx, gain_per_cost)

        for (pos, &idx) in remaining.iter().enumerate() {
            let item = &items[idx];
            let priority = item_score(idx);
            if item.cost == 0
                || item.cost > budget - spent
                || !priority.is_finite()
                || priority <= 0.0
            {
                continue;
            }
            // Marginal gain = weight of newly covered terms.
            let gain: f64 = item
                .terms
                .iter()
                .filter(|t| !covered.contains(t.as_str()))
                .map(|t| term_weight(t))
                .sum();
            if gain <= 0.0 {
                continue;
            }
            let gain_per_cost = gain * priority / item.cost as f64;
            let take = match best {
                Some((_, _, best_gpc)) => gain_per_cost > best_gpc,
                None => true,
            };
            if take {
                best = Some((pos, idx, gain_per_cost));
            }
        }

        match best {
            Some((pos, idx, _)) => {
                for t in &items[idx].terms {
                    covered.insert(t.as_str());
                }
                spent += items[idx].cost;
                selected.push(idx);
                remaining.swap_remove(pos);
            }
            // No remaining item adds positive marginal coverage within budget.
            None => break,
        }
    }

    selected
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(terms: &[&str], cost: usize) -> CoverageItem {
        CoverageItem {
            terms: terms.iter().map(|s| (*s).to_string()).collect(),
            cost,
        }
    }

    #[test]
    fn picks_complementary_items_over_redundant_ones() {
        // A and B together cover {x,y,z,w}; C is fully redundant with A.
        let items = vec![
            item(&["x", "y"], 1), // A
            item(&["z", "w"], 1), // B
            item(&["x", "y"], 1), // C (redundant with A)
        ];
        let sel = greedy_max_coverage(&items, 2, |_| 1.0);
        assert_eq!(sel.len(), 2);
        assert!(sel.contains(&0) || sel.contains(&2)); // one of the {x,y} items
        assert!(sel.contains(&1)); // the complementary {z,w} item is always taken
        assert!(
            !(sel.contains(&0) && sel.contains(&2)),
            "must not pick both redundant items"
        );
    }

    #[test]
    fn respects_budget() {
        let items = vec![item(&["a"], 1), item(&["b"], 1), item(&["c"], 1)];
        let sel = greedy_max_coverage(&items, 2, |_| 1.0);
        assert_eq!(sel.len(), 2);
    }

    #[test]
    fn skips_items_that_add_no_new_coverage() {
        let items = vec![item(&["a", "b"], 1), item(&["a"], 1)];
        let sel = greedy_max_coverage(&items, 5, |_| 1.0);
        assert_eq!(sel, vec![0], "second item is fully subsumed → skipped");
    }

    #[test]
    fn term_weight_prioritises_rare_terms() {
        // Item 0 covers a common term; item 1 covers a rare (high-weight) one.
        let items = vec![item(&["common"], 1), item(&["rare"], 1)];
        let weight = |t: &str| if t == "rare" { 10.0 } else { 1.0 };
        let sel = greedy_max_coverage(&items, 1, weight);
        assert_eq!(sel, vec![1], "rare/high-weight term wins the single slot");
    }

    #[test]
    fn empty_input_yields_empty_selection() {
        let sel = greedy_max_coverage(&[], 5, |_| 1.0);
        assert!(sel.is_empty());
    }

    #[test]
    fn zero_cost_items_are_skipped() {
        let items = vec![item(&["a"], 0), item(&["b"], 1)];
        let sel = greedy_max_coverage(&items, 5, |_| 1.0);
        assert_eq!(sel, vec![1]);
    }

    #[test]
    fn scores_change_priority_without_overriding_budget_or_finiteness() {
        let items = vec![item(&["a"], 1), item(&["b"], 1), item(&["c"], usize::MAX)];
        let selected =
            greedy_scored_coverage(&items, 2, |_| 1.0, |index| [0.1, 0.9, f64::INFINITY][index]);
        assert_eq!(selected, vec![1, 0]);
        let selected =
            greedy_scored_coverage(&items, 1, |_| 1.0, |index| [f64::NAN, 0.9, 1.0][index]);
        assert_eq!(selected, vec![1]);
    }
}
