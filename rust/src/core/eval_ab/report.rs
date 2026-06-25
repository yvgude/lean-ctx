//! Paired report + non-regression gate (#237).
//!
//! Every task is scored under *both* conditions, giving a paired sample of per-task deltas
//! (`lean_ctx − baseline`). From those we compute the mean delta, a **deterministic bootstrap**
//! 95% confidence interval (fixed seed → byte-identical CI on every machine), win/tie/loss
//! counts and pass-rate deltas, then collapse it all into a single [`Verdict`] that drives the
//! CI quality gate.

use serde::{Deserialize, Serialize};

use super::model::ModelFingerprint;

/// Report schema discriminator + version (also guards artifact parsing).
pub const REPORT_KIND: &str = "lean-ctx.eval-ab-report";
pub const REPORT_SCHEMA_VERSION: u32 = 1;

/// Equality tolerance when classifying a task as win/tie/loss.
const EPS: f64 = 1e-9;

/// One task scored under both conditions, with the audit digests for each window + answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PairRecord {
    pub task_id: String,
    pub domain: String,
    pub baseline_value: f64,
    pub lean_ctx_value: f64,
    pub baseline_passed: bool,
    pub lean_ctx_passed: bool,
    pub baseline_tokens: usize,
    pub lean_ctx_tokens: usize,
    pub baseline_context_digest: String,
    pub lean_ctx_context_digest: String,
    pub baseline_answer_digest: String,
    pub lean_ctx_answer_digest: String,
}

impl PairRecord {
    fn delta(&self) -> f64 {
        self.lean_ctx_value - self.baseline_value
    }
}

/// Aggregate statistics over all paired records.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AbStats {
    pub n: usize,
    pub baseline_mean: f64,
    pub lean_ctx_mean: f64,
    pub mean_delta: f64,
    pub ci_low: f64,
    pub ci_high: f64,
    pub wins: usize,
    pub ties: usize,
    pub losses: usize,
    pub baseline_pass_rate: f64,
    pub lean_ctx_pass_rate: f64,
    pub bootstrap_iters: usize,
    pub bootstrap_seed: u64,
    pub noninferiority_margin: f64,
}

/// The headline conclusion of a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// CI lower bound is strictly positive — lean-ctx improves quality.
    Improved,
    /// CI lower bound ≥ −margin — no regression within the tolerated margin.
    NonInferior,
    /// CI lower bound below −margin — a regression the gate must block.
    Regressed,
}

impl Verdict {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Verdict::Improved => "IMPROVED",
            Verdict::NonInferior => "NO REGRESSION",
            Verdict::Regressed => "REGRESSED",
        }
    }

    /// Whether the CI quality gate should pass.
    #[must_use]
    pub fn gate_passes(self) -> bool {
        !matches!(self, Verdict::Regressed)
    }
}

/// Knobs for the statistics + gate. Defaults are deterministic and strict.
#[derive(Debug, Clone, Copy)]
pub struct ReportConfig {
    pub bootstrap_iters: usize,
    pub bootstrap_seed: u64,
    /// How far the CI lower bound may sit below zero and still count as "no regression".
    pub noninferiority_margin: f64,
}

impl Default for ReportConfig {
    fn default() -> Self {
        Self {
            bootstrap_iters: 2000,
            bootstrap_seed: 0x5EED_5EED_5EED_5EED,
            noninferiority_margin: 0.0,
        }
    }
}

/// The full A/B report: provenance, per-task records, aggregate stats and the verdict.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AbReport {
    pub schema_version: u32,
    pub kind: String,
    pub created_at: String,
    pub lean_ctx_version: String,
    pub suite: String,
    pub budget_tokens: usize,
    pub model: ModelFingerprint,
    pub records: Vec<PairRecord>,
    pub stats: AbStats,
    pub verdict: Verdict,
}

impl AbReport {
    /// Computes stats + verdict over the records and assembles the report.
    pub fn build(
        suite: impl Into<String>,
        budget_tokens: usize,
        model: ModelFingerprint,
        records: Vec<PairRecord>,
        cfg: ReportConfig,
    ) -> Self {
        let stats = compute_stats(&records, cfg);
        let verdict = verdict_for(&stats, cfg);
        Self {
            schema_version: REPORT_SCHEMA_VERSION,
            kind: REPORT_KIND.to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            lean_ctx_version: env!("CARGO_PKG_VERSION").to_string(),
            suite: suite.into(),
            budget_tokens,
            model,
            records,
            stats,
            verdict,
        }
    }

    /// Pretty JSON for machine consumption / artifact embedding.
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    /// Compact human summary for the terminal.
    #[must_use]
    pub fn render(&self) -> String {
        let s = &self.stats;
        let mut out = String::new();
        out.push_str(&format!("Suite:   {}\n", self.suite));
        out.push_str(&format!(
            "Model:   {} ({}, temp={}, seed={})\n",
            self.model.params.model,
            self.model.provider,
            self.model.params.temperature,
            self.model.params.seed
        ));
        out.push_str(&format!(
            "Budget:  {} tokens / condition\n",
            self.budget_tokens
        ));
        out.push_str(&format!("Tasks:   {}\n\n", s.n));
        out.push_str(&format!(
            "Mean score   baseline={:.3}  lean-ctx={:.3}  Δ={:+.3}\n",
            s.baseline_mean, s.lean_ctx_mean, s.mean_delta
        ));
        out.push_str(&format!(
            "Pass rate    baseline={:.0}%   lean-ctx={:.0}%\n",
            s.baseline_pass_rate * 100.0,
            s.lean_ctx_pass_rate * 100.0
        ));
        out.push_str(&format!(
            "Δ 95% CI     [{:+.3}, {:+.3}]  ({} bootstrap, seed {:#x})\n",
            s.ci_low, s.ci_high, s.bootstrap_iters, s.bootstrap_seed
        ));
        out.push_str(&format!(
            "Win/Tie/Loss {} / {} / {}\n\n",
            s.wins, s.ties, s.losses
        ));
        out.push_str(&format!("VERDICT: {}\n", self.verdict.label()));
        out
    }
}

fn mean(values: impl Iterator<Item = f64>) -> f64 {
    let mut sum = 0.0;
    let mut count = 0usize;
    for v in values {
        sum += v;
        count += 1;
    }
    if count == 0 { 0.0 } else { sum / count as f64 }
}

fn compute_stats(records: &[PairRecord], cfg: ReportConfig) -> AbStats {
    let n = records.len();
    let baseline_mean = mean(records.iter().map(|r| r.baseline_value));
    let lean_ctx_mean = mean(records.iter().map(|r| r.lean_ctx_value));
    let diffs: Vec<f64> = records.iter().map(PairRecord::delta).collect();
    let mean_delta = mean(diffs.iter().copied());

    let (mut wins, mut ties, mut losses) = (0usize, 0usize, 0usize);
    for d in &diffs {
        if *d > EPS {
            wins += 1;
        } else if *d < -EPS {
            losses += 1;
        } else {
            ties += 1;
        }
    }

    let (ci_low, ci_high) = bootstrap_ci(&diffs, cfg.bootstrap_iters, cfg.bootstrap_seed);

    AbStats {
        n,
        baseline_mean,
        lean_ctx_mean,
        mean_delta,
        ci_low,
        ci_high,
        wins,
        ties,
        losses,
        baseline_pass_rate: mean(
            records
                .iter()
                .map(|r| f64::from(u8::from(r.baseline_passed))),
        ),
        lean_ctx_pass_rate: mean(
            records
                .iter()
                .map(|r| f64::from(u8::from(r.lean_ctx_passed))),
        ),
        bootstrap_iters: cfg.bootstrap_iters,
        bootstrap_seed: cfg.bootstrap_seed,
        noninferiority_margin: cfg.noninferiority_margin,
    }
}

fn verdict_for(stats: &AbStats, cfg: ReportConfig) -> Verdict {
    if stats.n == 0 {
        return Verdict::NonInferior;
    }
    if stats.ci_low > EPS {
        Verdict::Improved
    } else if stats.ci_low >= -cfg.noninferiority_margin - EPS {
        Verdict::NonInferior
    } else {
        Verdict::Regressed
    }
}

/// Deterministic percentile bootstrap of the mean of `diffs` (paired deltas).
fn bootstrap_ci(diffs: &[f64], iters: usize, seed: u64) -> (f64, f64) {
    let n = diffs.len();
    if n == 0 || iters == 0 {
        return (0.0, 0.0);
    }
    let mut rng = SplitMix64::new(seed);
    let mut means: Vec<f64> = Vec::with_capacity(iters);
    for _ in 0..iters {
        let mut sum = 0.0;
        for _ in 0..n {
            sum += diffs[rng.below(n)];
        }
        means.push(sum / n as f64);
    }
    means.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    (percentile(&means, 2.5), percentile(&means, 97.5))
}

/// Nearest-rank percentile of a pre-sorted slice.
fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = (p / 100.0 * (sorted.len() as f64 - 1.0)).round() as usize;
    sorted[rank.min(sorted.len() - 1)]
}

/// Tiny seedable PRNG (`SplitMix64`) — keeps the bootstrap CI reproducible without a dependency.
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

    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::eval_ab::model::ModelParams;

    fn rec(id: &str, base: f64, lean: f64) -> PairRecord {
        PairRecord {
            task_id: id.into(),
            domain: "qa".into(),
            baseline_value: base,
            lean_ctx_value: lean,
            baseline_passed: base >= 0.5,
            lean_ctx_passed: lean >= 0.5,
            baseline_tokens: 100,
            lean_ctx_tokens: 100,
            baseline_context_digest: "a".into(),
            lean_ctx_context_digest: "b".into(),
            baseline_answer_digest: "c".into(),
            lean_ctx_answer_digest: "d".into(),
        }
    }

    fn fp() -> ModelFingerprint {
        ModelFingerprint {
            provider: "recorded".into(),
            endpoint: "rec".into(),
            params: ModelParams::default(),
        }
    }

    #[test]
    fn clear_improvement_is_verdict_improved() {
        let records = vec![
            rec("1", 0.0, 1.0),
            rec("2", 0.0, 1.0),
            rec("3", 0.2, 0.9),
            rec("4", 0.1, 1.0),
            rec("5", 0.0, 0.8),
        ];
        let report = AbReport::build("s", 4000, fp(), records, ReportConfig::default());
        assert_eq!(report.verdict, Verdict::Improved, "{:?}", report.stats);
        assert!(report.verdict.gate_passes());
        assert_eq!(report.stats.wins, 5);
    }

    #[test]
    fn clear_regression_is_blocked() {
        let records = vec![
            rec("1", 1.0, 0.0),
            rec("2", 1.0, 0.0),
            rec("3", 0.9, 0.1),
            rec("4", 1.0, 0.2),
        ];
        let report = AbReport::build("s", 4000, fp(), records, ReportConfig::default());
        assert_eq!(report.verdict, Verdict::Regressed);
        assert!(!report.verdict.gate_passes());
    }

    #[test]
    fn identical_scores_are_non_inferior() {
        let records = vec![rec("1", 0.7, 0.7), rec("2", 0.4, 0.4)];
        let report = AbReport::build("s", 4000, fp(), records, ReportConfig::default());
        assert_eq!(report.verdict, Verdict::NonInferior);
        assert_eq!(report.stats.ties, 2);
        assert!(report.verdict.gate_passes());
    }

    #[test]
    fn bootstrap_ci_is_deterministic() {
        let diffs = vec![0.1, 0.3, -0.2, 0.5, 0.0, 0.4];
        let a = bootstrap_ci(&diffs, 1000, 42);
        let b = bootstrap_ci(&diffs, 1000, 42);
        assert_eq!(a, b);
    }
}
