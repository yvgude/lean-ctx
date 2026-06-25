//! QUBO context-selection spike (#10) — research only, never the default.
//!
//! Context selection under a token budget is a quadratic optimization: maximize
//! total salience while penalizing redundancy between co-selected items and
//! staying within budget. That is naturally a QUBO (quadratic unconstrained
//! binary optimization):
//!
//! ```text
//!   minimize  E(x) = -Σ φ_i x_i  +  α Σ_{i<j} sim_ij x_i x_j  +  β·overflow(x)
//!   over      x ∈ {0,1}^n
//! ```
//!
//! where `overflow` is the budget violation. QUBO is the form solved by quantum
//! annealers and their classical analogues (simulated annealing / simulated
//! bifurcation). This module provides a *deterministic* simulated-annealing
//! solver (seeded PRNG — no `getrandom`) plus a benchmark harness comparing it to
//! the production greedy knapsack on quality (φ captured) and tokens.
//!
//! IMPORTANT: this is a benchmark spike gated behind `LEAN_CTX_EXPERIMENTAL_QUBO`.
//! It never changes selection defaults; the greedy compiler remains in charge.
//! Promotion is conditional on a measurable win from the harness below.

use crate::core::entropy::jaccard_similarity;

/// Redundancy penalty weight in the QUBO objective.
const ALPHA: f64 = 0.5;
/// Budget-overflow penalty weight (per token over budget). Large so any feasible
/// solution dominates an infeasible one.
const BETA: f64 = 1.0;
/// Annealing iterations. Fixed for determinism and bounded cost.
const SA_ITERS: usize = 4000;

/// `true` when the experimental QUBO spike is enabled. Off by default — the
/// greedy selector stays the default selection path regardless.
pub fn is_enabled() -> bool {
    matches!(
        std::env::var("LEAN_CTX_EXPERIMENTAL_QUBO")
            .ok()
            .as_deref()
            .map(str::trim),
        Some("1" | "true" | "yes" | "on")
    )
}

/// A candidate item for QUBO selection.
#[derive(Debug, Clone)]
pub struct QuboItem {
    pub id: String,
    pub phi: f64,
    pub tokens: usize,
    /// Content fingerprint for the pairwise redundancy term.
    pub sketch: String,
}

/// Deterministic, reproducible PRNG (`SplitMix64`) — keeps the spike free of
/// `getrandom` so results are byte-stable across runs/machines.
struct SplitMix64(u64);
impl SplitMix64 {
    fn new(seed: u64) -> Self {
        Self(seed)
    }
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / ((1u64 << 53) as f64)
    }
    fn next_below(&mut self, bound: usize) -> usize {
        if bound == 0 {
            0
        } else {
            (self.next_u64() % bound as u64) as usize
        }
    }
}

/// Precomputed pairwise redundancy (upper triangle), so SA energy deltas are
/// cheap. `sim[i][j]` for `i < j`.
fn redundancy_matrix(items: &[QuboItem]) -> Vec<Vec<f64>> {
    let n = items.len();
    let mut sim = vec![vec![0.0; n]; n];
    for i in 0..n {
        for j in (i + 1)..n {
            let s = jaccard_similarity(&items[i].sketch, &items[j].sketch);
            sim[i][j] = s;
            sim[j][i] = s;
        }
    }
    sim
}

fn energy(items: &[QuboItem], sim: &[Vec<f64>], x: &[bool], budget: usize) -> f64 {
    let mut e = 0.0;
    let mut tokens = 0usize;
    for i in 0..items.len() {
        if !x[i] {
            continue;
        }
        e -= items[i].phi;
        tokens += items[i].tokens;
        for j in (i + 1)..items.len() {
            if x[j] {
                e += ALPHA * sim[i][j];
            }
        }
    }
    let overflow = tokens.saturating_sub(budget) as f64;
    e + BETA * overflow
}

/// Solve the selection QUBO with deterministic simulated annealing. Returns the
/// indices of selected items. Seeded by a stable hash of the problem so the
/// result is reproducible. Registers activity for `introspect cognition`.
#[must_use]
pub fn select(items: &[QuboItem], budget: usize) -> Vec<usize> {
    crate::core::introspect::tick("qubo_select");
    let n = items.len();
    if n == 0 {
        return Vec::new();
    }
    let sim = redundancy_matrix(items);

    // Start from the greedy feasible solution — a good basin for SA to refine.
    let mut x = greedy_mask(items, budget);
    let mut best = x.clone();
    let mut best_e = energy(items, &sim, &x, budget);
    let mut cur_e = best_e;

    let mut rng = SplitMix64::new(problem_seed(items, budget));
    for k in 0..SA_ITERS {
        // Geometric temperature schedule from 1.0 → ~0.01.
        let t = (1.0 - (k as f64 / SA_ITERS as f64)).mul_add(0.99, 0.01);
        let i = rng.next_below(n);
        x[i] = !x[i];
        let new_e = energy(items, &sim, &x, budget);
        let delta = new_e - cur_e;
        if delta <= 0.0 || rng.next_f64() < (-delta / t).exp() {
            cur_e = new_e;
            if new_e < best_e {
                best_e = new_e;
                best.clone_from(&x);
            }
        } else {
            x[i] = !x[i]; // reject: revert
        }
    }

    best.iter()
        .enumerate()
        .filter_map(|(i, &on)| on.then_some(i))
        .collect()
}

/// Greedy feasible selection (efficiency = φ/token, descending) used both as the
/// SA seed and as the benchmark baseline (mirrors the production compiler).
fn greedy_mask(items: &[QuboItem], budget: usize) -> Vec<bool> {
    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_by(|&a, &b| {
        let ea = items[a].phi / items[a].tokens.max(1) as f64;
        let eb = items[b].phi / items[b].tokens.max(1) as f64;
        eb.partial_cmp(&ea)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| items[a].id.cmp(&items[b].id))
    });
    let mut mask = vec![false; items.len()];
    let mut used = 0usize;
    for i in order {
        if used + items[i].tokens <= budget {
            mask[i] = true;
            used += items[i].tokens;
        }
    }
    mask
}

/// Stable per-problem seed so SA is reproducible.
fn problem_seed(items: &[QuboItem], budget: usize) -> u64 {
    let mut h = budget as u64;
    for it in items {
        h ^= it.id.bytes().fold(1469598103934665603u64, |acc, b| {
            (acc ^ u64::from(b)).wrapping_mul(1099511628211)
        });
        h = h.wrapping_mul(0x100000001b3).wrapping_add(it.tokens as u64);
    }
    h
}

/// Result of a QUBO-vs-greedy benchmark run.
#[derive(Debug, Clone)]
pub struct BenchReport {
    pub items: usize,
    pub budget: usize,
    pub greedy_phi: f64,
    pub greedy_tokens: usize,
    pub qubo_phi: f64,
    pub qubo_tokens: usize,
}

impl BenchReport {
    /// Total φ captured, relative gain of QUBO over greedy (can be negative).
    #[must_use]
    pub fn phi_gain_pct(&self) -> f64 {
        if self.greedy_phi <= 0.0 {
            return 0.0;
        }
        (self.qubo_phi - self.greedy_phi) / self.greedy_phi * 100.0
    }

    #[must_use]
    pub fn format(&self) -> String {
        format!(
            "QUBO spike (experimental, greedy stays default)\n\
             items={}  budget={}\n\
             greedy: phi={:.3} tokens={}\n\
             qubo:   phi={:.3} tokens={}\n\
             phi gain: {:+.1}%",
            self.items,
            self.budget,
            self.greedy_phi,
            self.greedy_tokens,
            self.qubo_phi,
            self.qubo_tokens,
            self.phi_gain_pct(),
        )
    }
}

fn captured(items: &[QuboItem], idx: &[usize]) -> (f64, usize) {
    idx.iter().fold((0.0, 0usize), |(p, t), &i| {
        (p + items[i].phi, t + items[i].tokens)
    })
}

/// Run the QUBO-vs-greedy benchmark on a problem. Pure and deterministic.
#[must_use]
pub fn benchmark(items: &[QuboItem], budget: usize) -> BenchReport {
    let greedy: Vec<usize> = greedy_mask(items, budget)
        .iter()
        .enumerate()
        .filter_map(|(i, &on)| on.then_some(i))
        .collect();
    let qubo = select(items, budget);
    let (greedy_phi, greedy_tokens) = captured(items, &greedy);
    let (qubo_phi, qubo_tokens) = captured(items, &qubo);
    BenchReport {
        items: items.len(),
        budget,
        greedy_phi,
        greedy_tokens,
        qubo_phi,
        qubo_tokens,
    }
}

/// A deterministic synthetic problem for the CLI harness: clusters of redundant
/// items plus unique high-φ items, so QUBO's redundancy awareness can show.
#[must_use]
pub fn synthetic_problem() -> (Vec<QuboItem>, usize) {
    let mut items = Vec::new();
    // Three near-duplicate clusters (same sketch) of medium φ.
    for cluster in 0..3 {
        for k in 0..3 {
            items.push(QuboItem {
                id: format!("dup{cluster}_{k}"),
                phi: 0.6,
                tokens: 300,
                sketch: format!("cluster {cluster} shared redundant content body"),
            });
        }
    }
    // Unique high-φ items with genuinely distinct content (no shared words, so
    // the redundancy term reflects only the intended duplicate clusters).
    let unique_sketches = [
        "kepler orbital mechanics ellipse perihelion",
        "ribosome translation codon peptide synthesis",
        "byzantine consensus quorum fault tolerance",
        "monsoon humidity evaporation precipitation cycle",
    ];
    for (u, sketch) in unique_sketches.iter().enumerate() {
        items.push(QuboItem {
            id: format!("uniq{u}"),
            phi: 0.8,
            tokens: 300,
            sketch: (*sketch).to_string(),
        });
    }
    (items, 1500)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items() -> Vec<QuboItem> {
        synthetic_problem().0
    }

    #[test]
    fn disabled_by_default() {
        // Spike must be opt-in; greedy stays default. Serialize env access through
        // the shared test lock so this never races other env-reading tests.
        let _lock = crate::core::data_dir::test_env_lock();
        crate::test_env::remove_var("LEAN_CTX_EXPERIMENTAL_QUBO");
        assert!(!is_enabled());
    }

    #[test]
    fn selection_respects_budget() {
        let it = items();
        let budget = 1500;
        let sel = select(&it, budget);
        let (_, tokens) = captured(&it, &sel);
        assert!(tokens <= budget, "QUBO must not exceed budget: {tokens}");
    }

    #[test]
    fn selection_is_deterministic() {
        // Determinism contract (#498): seeded SA → identical selection each run.
        let it = items();
        let a = select(&it, 1500);
        let b = select(&it, 1500);
        assert_eq!(a, b, "seeded SA must be reproducible");
    }

    #[test]
    fn benchmark_runs_and_reports() {
        let (it, budget) = synthetic_problem();
        let report = benchmark(&it, budget);
        assert_eq!(report.items, it.len());
        assert!(report.greedy_phi > 0.0);
        assert!(report.qubo_phi > 0.0);
        // Both stay within budget.
        assert!(report.greedy_tokens <= budget);
        assert!(report.qubo_tokens <= budget);
        // Report formats without panicking.
        assert!(report.format().contains("QUBO spike"));
    }

    #[test]
    fn empty_problem_selects_nothing() {
        assert!(select(&[], 1000).is_empty());
    }
}
