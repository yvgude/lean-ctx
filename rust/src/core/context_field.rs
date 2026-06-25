//! Context Field Theory (CFT) -- unified potential function for context items.
//!
//! Combines information-theoretic, graph-based, and history signals into a
//! single scalar potential Phi(i,t) per context item, enabling principled
//! budget allocation and view selection.
//!
//! Scientific basis:
//!   Phi(i,t) = `w_R`*R + `w_S`*S + `w_G`*G + `w_H`*H - `w_C`*C - `w_D`*D
//! where R = task relevance (heat diffusion + `PageRank`),
//!       S = surprise (cross-entropy with Zipfian prior),
//!       G = graph proximity (weighted BFS distance),
//!       H = history signal (bandit feedback),
//!       C = token cost for the active view,
//!       D = redundancy with already-selected items (Jaccard).

use std::collections::HashMap;
use std::fmt;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Shared types used across CFT modules (Ledger, Overlay, Handles, Compiler)
// ---------------------------------------------------------------------------

/// Stable, content-addressed identifier for a context item.
/// Derived from `kind + source_path` so the same file always maps to the
/// same ID within a session, regardless of content changes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ContextItemId(pub String);

impl ContextItemId {
    #[must_use]
    pub fn from_file(path: &str) -> Self {
        Self(format!("file:{path}"))
    }
    #[must_use]
    pub fn from_shell(command: &str) -> Self {
        let hash = crate::core::project_hash::hash_project_root(command);
        Self(format!("shell:{hash}"))
    }
    #[must_use]
    pub fn from_knowledge(category: &str, key: &str) -> Self {
        Self(format!("knowledge:{category}:{key}"))
    }
    #[must_use]
    pub fn from_memory(key: &str) -> Self {
        Self(format!("memory:{key}"))
    }
    #[must_use]
    pub fn from_provider(provider: &str, key: &str) -> Self {
        Self(format!("provider:{provider}:{key}"))
    }
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ContextItemId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextKind {
    File,
    Shell,
    Knowledge,
    Memory,
    Provider,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum ContextState {
    #[default]
    Candidate,
    Included,
    Excluded,
    Pinned,
    Stale,
    Shadowed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewKind {
    Full,
    Signatures,
    Map,
    Diff,
    Aggressive,
    Entropy,
    Lines,
    Reference,
    Handle,
}

impl ViewKind {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Signatures => "signatures",
            Self::Map => "map",
            Self::Diff => "diff",
            Self::Aggressive => "aggressive",
            Self::Entropy => "entropy",
            Self::Lines => "lines",
            Self::Reference => "reference",
            Self::Handle => "handle",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "signatures" => Self::Signatures,
            "map" => Self::Map,
            "diff" => Self::Diff,
            "aggressive" => Self::Aggressive,
            "entropy" => Self::Entropy,
            "lines" => Self::Lines,
            "reference" => Self::Reference,
            "handle" => Self::Handle,
            _ => Self::Full,
        }
    }

    /// Phase-transition ordering: lower index = denser (more tokens).
    #[must_use]
    pub fn density_rank(&self) -> u8 {
        match self {
            Self::Full => 0,
            Self::Aggressive => 1,
            Self::Diff => 2,
            Self::Lines => 3,
            Self::Entropy => 4,
            Self::Signatures => 5,
            Self::Map => 6,
            Self::Reference => 7,
            Self::Handle => 8,
        }
    }
}

/// Token-cost estimates for each available view of a context item.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ViewCosts {
    pub estimates: HashMap<ViewKind, usize>,
}

impl ViewCosts {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&mut self, view: ViewKind, tokens: usize) {
        self.estimates.insert(view, tokens);
    }

    #[must_use]
    pub fn get(&self, view: &ViewKind) -> usize {
        self.estimates.get(view).copied().unwrap_or(0)
    }

    /// Cheapest view that still provides content (excludes Handle).
    #[must_use]
    pub fn cheapest_content_view(&self) -> Option<(ViewKind, usize)> {
        self.estimates
            .iter()
            .filter(|(v, _)| **v != ViewKind::Handle)
            .min_by_key(|&(_, &tokens)| tokens)
            .map(|(&v, &t)| (v, t))
    }

    #[must_use]
    pub fn from_full_tokens(full_tokens: usize) -> Self {
        let mut vc = Self::new();
        vc.set(ViewKind::Full, full_tokens);
        vc.set(ViewKind::Signatures, full_tokens / 5);
        vc.set(ViewKind::Map, full_tokens / 8);
        vc.set(ViewKind::Reference, full_tokens / 20);
        vc.set(ViewKind::Handle, 25);
        vc
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Provenance {
    pub tool: Option<String>,
    pub agent_id: Option<String>,
    pub client_name: Option<String>,
    pub timestamp: Option<String>,
}

// ---------------------------------------------------------------------------
// Context Potential Function
// ---------------------------------------------------------------------------

/// Weights for the potential function components.
/// Adapted via Thompson Sampling (bandit.rs) over time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldWeights {
    pub w_relevance: f64,
    pub w_surprise: f64,
    pub w_graph: f64,
    pub w_history: f64,
    pub w_cost: f64,
    pub w_redundancy: f64,
}

impl Default for FieldWeights {
    fn default() -> Self {
        Self {
            w_relevance: 0.35,
            w_surprise: 0.15,
            w_graph: 0.20,
            w_history: 0.10,
            w_cost: 0.10,
            w_redundancy: 0.10,
        }
    }
}

impl FieldWeights {
    /// Stability-leaning preset (#4): trusts relevance/history, applies a light
    /// cost penalty. Selected when the `conservative` bandit arm wins.
    #[must_use]
    pub fn conservative() -> Self {
        Self {
            w_relevance: 0.45,
            w_surprise: 0.10,
            w_graph: 0.20,
            w_history: 0.15,
            w_cost: 0.05,
            w_redundancy: 0.05,
        }
    }

    /// The default balanced preset (#4); the `balanced` arm maps here.
    #[must_use]
    pub fn balanced() -> Self {
        Self::default()
    }

    /// Compression-leaning preset (#4): heavier cost/surprise weighting so dense
    /// items are favored under pressure. Selected when `aggressive` wins.
    #[must_use]
    pub fn aggressive() -> Self {
        Self {
            w_relevance: 0.30,
            w_surprise: 0.20,
            w_graph: 0.15,
            w_history: 0.05,
            w_cost: 0.20,
            w_redundancy: 0.10,
        }
    }

    /// Map a learned bandit arm to a `FieldWeights` preset (#4). The arm names are
    /// the bandit's own (`conservative`/`balanced`/`aggressive`); unknown names
    /// fall back to balanced. This is what makes the field weights *learned*:
    /// feedback shifts which arm wins, which shifts the weights deterministically.
    #[must_use]
    pub fn from_arm(arm: &crate::core::bandit::BanditArm) -> Self {
        match arm.name.as_str() {
            "conservative" => Self::conservative(),
            "aggressive" => Self::aggressive(),
            _ => Self::balanced(),
        }
    }
}

/// Process-wide learned `FieldWeights` (#4): the bandit-selected weights that
/// [`ContextField::active`] uses, so learning flows into every Phi computation
/// without a disk read per call. `None` until an arm has been chosen on the read
/// path; readers then fall back to the default weights.
static ACTIVE_WEIGHTS: std::sync::RwLock<Option<FieldWeights>> = std::sync::RwLock::new(None);

/// Install the bandit-selected `FieldWeights` as the process-wide active weights
/// (#4). Deterministic given the bandit posterior; called when an arm is chosen.
pub fn set_active_weights(weights: FieldWeights) {
    if let Ok(mut w) = ACTIVE_WEIGHTS.write() {
        *w = Some(weights);
    }
}

/// The current active (learned) `FieldWeights`, or the default when none have been
/// installed yet. Cheap — a single `RwLock` read — so safe on the hot path.
pub fn active_weights() -> FieldWeights {
    ACTIVE_WEIGHTS
        .read()
        .ok()
        .and_then(|w| w.clone())
        .unwrap_or_default()
}

/// Raw signal components for a single context item before combination.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FieldSignals {
    pub relevance: f64,
    pub surprise: f64,
    pub graph_proximity: f64,
    pub history_signal: f64,
    pub token_cost_norm: f64,
    pub redundancy: f64,
}

/// Combined potential for a context item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldPotential {
    pub signals: FieldSignals,
    pub phi: f64,
    pub view_costs: ViewCosts,
    pub best_view: ViewKind,
}

/// Token budget parameters for compilation.
#[derive(Debug, Clone, Copy)]
pub struct TokenBudget {
    pub total: usize,
    pub used: usize,
}

impl TokenBudget {
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.total.saturating_sub(self.used)
    }
    #[must_use]
    pub fn utilization(&self) -> f64 {
        if self.total == 0 {
            return 1.0;
        }
        self.used as f64 / self.total as f64
    }
    /// Temperature derived from budget pressure: high pressure = high T.
    /// T in [0.1, 2.0]. At T=0.1 (low pressure), prefer dense views.
    /// At T=2.0 (high pressure), prefer sparse views.
    #[must_use]
    pub fn temperature(&self) -> f64 {
        let u = self.utilization();
        (0.1 + u * 1.9).clamp(0.1, 2.0)
    }
}

/// The Context Field: computes Phi for a set of items given a task context.
pub struct ContextField {
    weights: FieldWeights,
}

impl Default for ContextField {
    fn default() -> Self {
        Self::new()
    }
}

impl ContextField {
    #[must_use]
    pub fn new() -> Self {
        Self {
            weights: FieldWeights::default(),
        }
    }

    #[must_use]
    pub fn with_weights(weights: FieldWeights) -> Self {
        Self { weights }
    }

    /// Construct a field using the process-wide learned `FieldWeights` (#4) when an
    /// arm has been selected, else the defaults. Cheap (a `RwLock` read), so it is
    /// safe to call on the per-read Phi hot path.
    #[must_use]
    pub fn active() -> Self {
        Self {
            weights: active_weights(),
        }
    }

    /// Compute the unified potential Phi(i,t) for a context item.
    ///
    /// All input signals should be normalized to [0, 1] before calling.
    /// The cost and redundancy terms are subtracted (penalty).
    #[must_use]
    pub fn compute_phi(&self, signals: &FieldSignals) -> f64 {
        let w = &self.weights;
        let phi = w.w_relevance * signals.relevance
            + w.w_surprise * signals.surprise
            + w.w_graph * signals.graph_proximity
            + w.w_history * signals.history_signal
            - w.w_cost * signals.token_cost_norm
            - w.w_redundancy * signals.redundancy;
        phi.clamp(0.0, 1.0)
    }

    /// Select the best view for an item given the temperature (budget pressure).
    ///
    /// Uses Boltzmann-weighted view selection:
    ///   `P(view_v` | `item_i`, T) = exp(-C(v) / T) / Z(i, T)
    ///
    /// At low temperature (relaxed budget), denser views are preferred.
    /// At high temperature (tight budget), sparser views are preferred.
    #[must_use]
    pub fn select_view(&self, costs: &ViewCosts, temperature: f64) -> ViewKind {
        if costs.estimates.is_empty() {
            return ViewKind::Full;
        }

        let t = temperature.max(0.01);
        let max_cost = costs.estimates.values().copied().max().unwrap_or(1).max(1) as f64;

        let mut best_view = ViewKind::Full;
        let mut best_score = f64::NEG_INFINITY;

        for (&view, &tokens) in &costs.estimates {
            let normalized_cost = tokens as f64 / max_cost;
            let density_bonus = 1.0 - (f64::from(view.density_rank()) / 8.0);
            // At low T, density_bonus dominates (prefer dense/full views).
            // At high T, the cost penalty dominates (prefer cheap/sparse views).
            let score = density_bonus * (2.0 - t) - normalized_cost * t;
            if score > best_score {
                best_score = score;
                best_view = view;
            }
        }

        best_view
    }

    /// Compute potentials for a batch of items.
    #[must_use]
    pub fn compute_batch(
        &self,
        items: &[(ContextItemId, FieldSignals, ViewCosts)],
        budget: TokenBudget,
    ) -> HashMap<ContextItemId, FieldPotential> {
        let temperature = budget.temperature();
        let mut result = HashMap::new();

        for (id, signals, costs) in items {
            let phi = self.compute_phi(signals);
            let best_view = self.select_view(costs, temperature);
            result.insert(
                id.clone(),
                FieldPotential {
                    signals: signals.clone(),
                    phi,
                    view_costs: costs.clone(),
                    best_view,
                },
            );
        }

        result
    }
}

// ---------------------------------------------------------------------------
// Signal extraction helpers (bridge to existing modules)
// ---------------------------------------------------------------------------

/// Normalize a relevance score from `task_relevance.rs` to [0, 1].
#[must_use]
pub fn normalize_relevance(score: f64, max_score: f64) -> f64 {
    if max_score <= 0.0 {
        return 0.0;
    }
    (score / max_score).clamp(0.0, 1.0)
}

/// Normalize a surprise score from surprise.rs to [0, 1].
/// Surprise range is typically 5.0 (common) to 17.0+ (rare).
#[must_use]
pub fn normalize_surprise(surprise: f64) -> f64 {
    ((surprise - 5.0) / 12.0).clamp(0.0, 1.0)
}

/// Normalize graph proximity (inverse of distance) to [0, 1].
/// Distance 0 = same file = 1.0, distance N = 1/(1+N).
#[must_use]
pub fn normalize_graph_proximity(distance: usize) -> f64 {
    1.0 / (1.0 + distance as f64)
}

/// Normalize token cost relative to budget.
#[must_use]
pub fn normalize_token_cost(tokens: usize, budget_total: usize) -> f64 {
    if budget_total == 0 {
        return 1.0;
    }
    (tokens as f64 / budget_total as f64).clamp(0.0, 1.0)
}

/// Compute efficiency ratio: Phi per token.
/// Used by the greedy knapsack in the compiler.
#[must_use]
pub fn efficiency(phi: f64, tokens: usize) -> f64 {
    if tokens == 0 {
        return phi;
    }
    phi / tokens as f64
}

/// Default MMR trade-off: how much relevance (Phi) is weighted against
/// non-redundancy during integration-aware selection (#5). 0.7 keeps relevance
/// dominant while still penalizing near-duplicates.
pub const MMR_LAMBDA: f64 = 0.7;

/// Maximal Marginal Relevance score (#5): reward relevance (`phi`) but penalize
/// redundancy with the already-selected set (`max_similarity`). This makes
/// selection *integration-aware* in the IIT sense — a context package gains more
/// from a complementary item than from a near-duplicate of one it already holds.
/// Deterministic: a pure function of its inputs, no sampling.
#[must_use]
pub fn mmr_score(phi: f64, max_similarity: f64, lambda: f64) -> f64 {
    let l = lambda.clamp(0.0, 1.0);
    l * phi - (1.0 - l) * max_similarity.clamp(0.0, 1.0)
}

/// Compute real signals for a file path using existing scoring modules.
/// Bridges CFT with the information-theoretic, graph-based, and history
/// subsystems already in lean-ctx.
#[must_use]
pub fn compute_signals_for_path(
    path: &str,
    task: Option<&str>,
    file_content: Option<&str>,
    budget_total: usize,
    full_tokens: usize,
) -> (FieldSignals, ViewCosts) {
    let mut signals = FieldSignals::default();

    let heatmap = super::heatmap::HeatMap::load();
    let heat_entry = heatmap.entries.get(path);

    // R(i,t): Task relevance via keyword overlap + heatmap frequency
    if let Some(task_desc) = task {
        let (_, keywords) = super::task_relevance::parse_task_hints(task_desc);
        let path_lower = path.to_lowercase();
        let keyword_hits = keywords
            .iter()
            .filter(|kw| path_lower.contains(&kw.to_lowercase()))
            .count();
        let keyword_score = (keyword_hits as f64 * 0.3).min(1.0);
        let freq_score = heat_entry.map_or(0.0, |e| (f64::from(e.access_count) / 10.0).min(1.0));
        signals.relevance = normalize_relevance(keyword_score + freq_score, 2.0);
    } else {
        let freq = heat_entry.map_or(0.0, |e| f64::from(e.access_count));
        signals.relevance = normalize_relevance(freq, 10.0);
    }

    // S(i): Surprise from cross-entropy with Zipfian prior
    if let Some(content) = file_content {
        let surprise_val = super::surprise::line_surprise(content);
        signals.surprise = normalize_surprise(surprise_val);
    }

    // G(i,t): Graph proximity heuristic from path depth
    // (property graph queries require a Connection not available here)
    let depth = path.matches('/').count();
    signals.graph_proximity = normalize_graph_proximity(depth);

    // H(i): History signal from heatmap access count
    let access_count = heat_entry.map_or(0, |e| e.access_count);
    signals.history_signal = (f64::from(access_count) / 20.0).min(1.0);

    // C(i,v): Normalized token cost relative to budget
    signals.token_cost_norm = normalize_token_cost(full_tokens, budget_total);

    // D(i): Redundancy — initialized at 0, refined during compilation pass
    signals.redundancy = 0.0;

    let view_costs = ViewCosts::from_full_tokens(full_tokens);
    (signals, view_costs)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phi_increases_with_relevance() {
        let field = ContextField::new();
        let low = field.compute_phi(&FieldSignals {
            relevance: 0.2,
            ..Default::default()
        });
        let high = field.compute_phi(&FieldSignals {
            relevance: 0.9,
            ..Default::default()
        });
        assert!(high > low, "higher relevance should yield higher phi");
    }

    #[test]
    fn phi_decreases_with_cost() {
        let field = ContextField::new();
        let cheap = field.compute_phi(&FieldSignals {
            relevance: 0.5,
            token_cost_norm: 0.1,
            ..Default::default()
        });
        let expensive = field.compute_phi(&FieldSignals {
            relevance: 0.5,
            token_cost_norm: 0.9,
            ..Default::default()
        });
        assert!(cheap > expensive, "higher cost should reduce phi");
    }

    #[test]
    fn phi_decreases_with_redundancy() {
        let field = ContextField::new();
        let unique = field.compute_phi(&FieldSignals {
            relevance: 0.5,
            redundancy: 0.0,
            ..Default::default()
        });
        let redundant = field.compute_phi(&FieldSignals {
            relevance: 0.5,
            redundancy: 0.9,
            ..Default::default()
        });
        assert!(unique > redundant, "redundancy should reduce phi");
    }

    #[test]
    fn phi_is_clamped_to_unit_interval() {
        let field = ContextField::new();
        let phi = field.compute_phi(&FieldSignals {
            relevance: 1.0,
            surprise: 1.0,
            graph_proximity: 1.0,
            history_signal: 1.0,
            token_cost_norm: 0.0,
            redundancy: 0.0,
        });
        assert!(phi <= 1.0);
        assert!(phi >= 0.0);
    }

    #[test]
    fn view_selection_prefers_dense_at_low_temperature() {
        let field = ContextField::new();
        let costs = ViewCosts::from_full_tokens(5000);
        let view = field.select_view(&costs, 0.1);
        assert_eq!(
            view,
            ViewKind::Full,
            "low temperature (relaxed budget) should prefer full view"
        );
    }

    #[test]
    fn view_selection_prefers_sparse_at_high_temperature() {
        let field = ContextField::new();
        let costs = ViewCosts::from_full_tokens(5000);
        let view = field.select_view(&costs, 2.0);
        assert_ne!(
            view,
            ViewKind::Full,
            "high temperature (tight budget) should prefer sparser view"
        );
    }

    #[test]
    fn budget_temperature_scales_with_utilization() {
        let low = TokenBudget {
            total: 10000,
            used: 1000,
        };
        let high = TokenBudget {
            total: 10000,
            used: 9000,
        };
        assert!(
            high.temperature() > low.temperature(),
            "higher utilization should increase temperature"
        );
    }

    #[test]
    fn normalize_surprise_maps_range() {
        assert!((normalize_surprise(5.0) - 0.0).abs() < 0.01);
        assert!((normalize_surprise(17.0) - 1.0).abs() < 0.01);
        assert!((normalize_surprise(11.0) - 0.5).abs() < 0.01);
    }

    #[test]
    fn normalize_graph_proximity_inverse_distance() {
        assert!((normalize_graph_proximity(0) - 1.0).abs() < f64::EPSILON);
        assert!((normalize_graph_proximity(1) - 0.5).abs() < f64::EPSILON);
        assert!(normalize_graph_proximity(10) < 0.15);
    }

    #[test]
    fn efficiency_ratio_is_phi_per_token() {
        let e = efficiency(0.8, 400);
        assert!((e - 0.002).abs() < 0.0001);
    }

    #[test]
    fn field_weights_from_arm_maps_presets() {
        // #4: each bandit arm name maps to its distinct FieldWeights preset.
        let mut bandit = crate::core::bandit::ThresholdBandit::default();
        let con = FieldWeights::from_arm(&bandit.arms[0]); // conservative
        let agg = FieldWeights::from_arm(&bandit.arms[2]); // aggressive
        assert!(
            con.w_relevance > agg.w_relevance,
            "conservative trusts relevance more"
        );
        assert!(
            agg.w_cost > con.w_cost,
            "aggressive penalizes cost more (denser)"
        );
        let _ = bandit.choose_arm();
    }

    #[test]
    fn learned_weights_shift_after_feedback() {
        // #4: training the bandit toward "aggressive" makes the deterministically
        // chosen arm map to the aggressive FieldWeights preset.
        let mut bandit = crate::core::bandit::ThresholdBandit::default();
        for _ in 0..20 {
            bandit.update("aggressive", true);
        }
        for _ in 0..20 {
            bandit.update("conservative", false);
        }
        let arm = bandit.arms[bandit.best_arm_idx_by_mean()].clone();
        let learned = FieldWeights::from_arm(&arm);
        let expected = FieldWeights::aggressive();
        assert!((learned.w_cost - expected.w_cost).abs() < f64::EPSILON);
    }

    #[test]
    fn active_weights_default_is_balanced() {
        // With nothing installed, active_weights falls back to the default preset.
        // (Set explicitly first to avoid cross-test global leakage, then restore.)
        set_active_weights(FieldWeights::default());
        let w = active_weights();
        let d = FieldWeights::default();
        assert!((w.w_relevance - d.w_relevance).abs() < f64::EPSILON);
    }

    #[test]
    fn mmr_penalizes_redundancy() {
        // #5: a redundant high-Phi item scores lower than a unique one.
        let unique = mmr_score(0.8, 0.0, MMR_LAMBDA);
        let redundant = mmr_score(0.8, 0.9, MMR_LAMBDA);
        assert!(
            unique > redundant,
            "redundant item ({redundant}) must score below unique ({unique})"
        );
    }

    #[test]
    fn mmr_lambda_one_is_pure_relevance() {
        // λ=1 ignores redundancy entirely → equals Phi.
        assert!((mmr_score(0.6, 0.9, 1.0) - 0.6).abs() < f64::EPSILON);
    }

    #[test]
    fn context_item_id_stable() {
        let a = ContextItemId::from_file("src/main.rs");
        let b = ContextItemId::from_file("src/main.rs");
        assert_eq!(a, b);
    }

    #[test]
    fn view_costs_from_full() {
        let vc = ViewCosts::from_full_tokens(5000);
        assert_eq!(vc.get(&ViewKind::Full), 5000);
        assert_eq!(vc.get(&ViewKind::Signatures), 1000);
        assert_eq!(vc.get(&ViewKind::Map), 625);
        assert_eq!(vc.get(&ViewKind::Handle), 25);
    }

    #[test]
    fn batch_compute_produces_results_for_all_items() {
        let field = ContextField::new();
        let items = vec![
            (
                ContextItemId::from_file("a.rs"),
                FieldSignals {
                    relevance: 0.8,
                    ..Default::default()
                },
                ViewCosts::from_full_tokens(2000),
            ),
            (
                ContextItemId::from_file("b.rs"),
                FieldSignals {
                    relevance: 0.3,
                    ..Default::default()
                },
                ViewCosts::from_full_tokens(500),
            ),
        ];
        let budget = TokenBudget {
            total: 10000,
            used: 2000,
        };
        let results = field.compute_batch(&items, budget);
        assert_eq!(results.len(), 2);
        assert!(results.contains_key(&ContextItemId::from_file("a.rs")));
        assert!(results.contains_key(&ContextItemId::from_file("b.rs")));
    }
}
