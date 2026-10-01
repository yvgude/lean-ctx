//! Paired report + non-regression gate (#237).
//!
//! Every task is scored under *both* conditions, giving a paired sample of per-task deltas
//! (`lean_ctx − baseline`). From those we compute the mean delta, a **deterministic bootstrap**
//! 95% confidence interval (fixed seed → byte-identical CI on every machine), win/tie/loss
//! counts and pass-rate deltas, then collapse it all into a single [`Verdict`] that drives the
//! CI quality gate.
//!
//! The verdict is only as strong as the evidence behind it: every report carries its
//! [`EvidenceTier`] and power status, and an underpowered run is `Inconclusive` unless it
//! already shows a regression.

use serde::{Deserialize, Serialize};

use super::model::ModelFingerprint;
use crate::core::context_quality::EvidenceTier;

/// Report schema discriminator + version (also guards artifact parsing).
/// v2 (additive): `Verdict::Inconclusive`, `evidence_tier`, `power`.
pub const REPORT_KIND: &str = "lean-ctx.eval-ab-report";
pub const REPORT_SCHEMA_VERSION: u32 = 2;

/// Equality tolerance when classifying a task as win/tie/loss.
const EPS: f64 = 1e-9;

/// Below this many paired tasks a bootstrap CI says nothing about quality: a
/// passing verdict only shows the pipeline ran end to end (#1905).
pub const MIN_POWERED_PAIRS: usize = 30;

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
    /// Powered run, CI lower bound strictly positive — lean-ctx improves quality.
    Improved,
    /// Powered run, CI lower bound ≥ −margin — no regression within the tolerated margin.
    NonInferior,
    /// CI lower bound below −margin — a regression the gate must block, at any sample size.
    Regressed,
    /// Too few paired tasks to conclude anything about quality. The pipeline ran and
    /// showed no regression, which is all this verdict says.
    Inconclusive,
}

impl Verdict {
    pub fn label(self) -> &'static str {
        match self {
            Verdict::Improved => "IMPROVED",
            Verdict::NonInferior => "NON-INFERIOR",
            Verdict::Regressed => "REGRESSED",
            Verdict::Inconclusive => "INCONCLUSIVE",
        }
    }

    /// Whether the CI quality gate should pass. Only an observed regression fails it;
    /// an inconclusive run passes the gate but backs no quality claim.
    pub fn gate_passes(self) -> bool {
        !matches!(self, Verdict::Regressed)
    }

    /// The most conservative verdict of a set: a regression dominates, then missing
    /// evidence, then "no regression", then improvement. An empty set is inconclusive.
    pub fn most_conservative(verdicts: impl IntoIterator<Item = Verdict>) -> Verdict {
        let rank = |v: &Verdict| match v {
            Verdict::Regressed => 3,
            Verdict::Inconclusive => 2,
            Verdict::NonInferior => 1,
            Verdict::Improved => 0,
        };
        verdicts
            .into_iter()
            .max_by_key(rank)
            .unwrap_or(Verdict::Inconclusive)
    }
}

/// Whether the run had enough paired tasks for its verdict to mean anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PowerStatus {
    pub pairs: usize,
    pub required_pairs: usize,
    pub powered: bool,
}

impl PowerStatus {
    fn for_pairs(pairs: usize) -> Self {
        Self {
            pairs,
            required_pairs: MIN_POWERED_PAIRS,
            powered: pairs >= MIN_POWERED_PAIRS,
        }
    }
}

/// Knobs for the statistics + gate. Defaults are deterministic and strict.
#[derive(Debug, Clone, Copy)]
pub struct ReportConfig {
    pub bootstrap_iters: usize,
    pub bootstrap_seed: u64,
    /// How far the CI lower bound may sit below zero and still count as "no regression".
    pub noninferiority_margin: f64,
    /// The answers came from a live model call, not a replayed recording. Defaults to
    /// `false` so a caller that forgets to set it gets the weaker evidence tier.
    pub live_model: bool,
}

impl Default for ReportConfig {
    fn default() -> Self {
        Self {
            bootstrap_iters: 2000,
            bootstrap_seed: 0x5EED_5EED_5EED_5EED,
            noninferiority_margin: 0.0,
            live_model: false,
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
    /// Kind of evidence behind the verdict. `None` only for v1 reports, which predate it.
    #[serde(default)]
    pub evidence_tier: Option<EvidenceTier>,
    /// Sample-size status. `None` only for v1 reports.
    #[serde(default)]
    pub power: Option<PowerStatus>,
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
        let evidence_tier = EvidenceTier::for_model_run(&model, !cfg.live_model);
        let power = PowerStatus::for_pairs(stats.n);
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
            evidence_tier: Some(evidence_tier),
            power: Some(power),
        }
    }

    /// Whether this report backs a statement about task quality on a real model: a
    /// powered, non-regressing run of a model-backed evidence tier.
    pub fn supports_quality_claim(&self) -> bool {
        matches!(self.verdict, Verdict::Improved | Verdict::NonInferior)
            && self.power.is_some_and(|p| p.powered)
            && self
                .evidence_tier
                .is_some_and(EvidenceTier::supports_task_quality_claim)
    }

    /// Pretty JSON for machine consumption / artifact embedding.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    /// Compact human summary for the terminal.
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
        match self.evidence_tier {
            Some(tier) => out.push_str(&format!(
                "EVIDENCE: tier {} ({}), NI margin -{:.3}\n",
                tier.code(),
                tier.label(),
                s.noninferiority_margin
            )),
            None => out.push_str("EVIDENCE: UNSPECIFIED (v1 report)\n"),
        }
        if s.n < MIN_POWERED_PAIRS {
            out.push_str(&format!(
                "POWER:   underpowered — {} paired task(s) < {MIN_POWERED_PAIRS}; this run checks \
                 the pipeline, it cannot show that compression keeps answer quality\n",
                s.n
            ));
        }
        out.push_str(if self.supports_quality_claim() {
            "CLAIM:   supports a task-quality claim for this suite, model and methodology\n"
        } else {
            "CLAIM:   none — this run is a mechanism / regression check only\n"
        });
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
        return Verdict::Inconclusive;
    }
    // An observed regression blocks at any sample size: a small suite cannot show that
    // quality is kept, but it can show that it was lost.
    if stats.ci_low < -cfg.noninferiority_margin - EPS {
        return Verdict::Regressed;
    }
    if stats.n < MIN_POWERED_PAIRS {
        return Verdict::Inconclusive;
    }
    if stats.ci_low > EPS {
        Verdict::Improved
    } else {
        Verdict::NonInferior
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

/// Tiny seedable PRNG (SplitMix64) — keeps the bootstrap CI reproducible without a dependency.
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

    fn fp_real() -> ModelFingerprint {
        ModelFingerprint {
            provider: crate::core::eval_ab::model::PROVIDER_OPENAI.into(),
            endpoint: "http://localhost:11434/v1".into(),
            params: ModelParams::default(),
        }
    }

    fn improving(n: usize) -> Vec<PairRecord> {
        (0..n)
            .map(|i| rec(&i.to_string(), 0.1 * (i % 3) as f64, 0.9))
            .collect()
    }

    #[test]
    fn clear_improvement_is_improved_only_when_powered() {
        let report = AbReport::build(
            "s",
            4000,
            fp(),
            improving(MIN_POWERED_PAIRS),
            ReportConfig::default(),
        );
        assert_eq!(report.verdict, Verdict::Improved, "{:?}", report.stats);
        assert!(report.verdict.gate_passes());

        // Same effect, five pairs: an apparent improvement is not evidence (#1905).
        let report = AbReport::build("s", 4000, fp(), improving(5), ReportConfig::default());
        assert_eq!(report.verdict, Verdict::Inconclusive, "{:?}", report.stats);
        assert!(report.verdict.gate_passes());
        assert!(!report.supports_quality_claim());
    }

    #[test]
    fn empty_run_is_inconclusive_not_non_inferior() {
        let report = AbReport::build("s", 4000, fp(), Vec::new(), ReportConfig::default());
        assert_eq!(report.verdict, Verdict::Inconclusive);
        assert!(!report.supports_quality_claim());
    }

    #[test]
    fn quality_claim_needs_power_and_a_model_backed_tier() {
        let ties = |n: usize| -> Vec<PairRecord> {
            (0..n).map(|i| rec(&i.to_string(), 0.6, 0.6)).collect()
        };
        // Powered replay of a real model: Tier C, claim supported.
        let replay = AbReport::build(
            "s",
            4000,
            fp_real(),
            ties(MIN_POWERED_PAIRS),
            ReportConfig::default(),
        );
        assert_eq!(replay.verdict, Verdict::NonInferior);
        assert_eq!(replay.evidence_tier, Some(EvidenceTier::RecordedRegression));
        assert!(replay.supports_quality_claim());

        // Live call of the same model: Tier D.
        let live = AbReport::build(
            "s",
            4000,
            fp_real(),
            ties(MIN_POWERED_PAIRS),
            ReportConfig {
                live_model: true,
                ..ReportConfig::default()
            },
        );
        assert_eq!(live.evidence_tier, Some(EvidenceTier::LiveTaskEvaluation));

        // Fixture answers handed to both arms: powered tie, still only a mechanism check.
        let fixture = AbReport::build(
            "s",
            4000,
            fp(),
            ties(MIN_POWERED_PAIRS),
            ReportConfig::default(),
        );
        assert_eq!(fixture.verdict, Verdict::NonInferior);
        assert_eq!(fixture.evidence_tier, Some(EvidenceTier::Mechanism));
        assert!(!fixture.supports_quality_claim());
        assert!(fixture.render().contains("CLAIM:   none"));
    }

    #[test]
    fn v1_report_without_tier_parses_and_claims_nothing() {
        let mut v2 = AbReport::build(
            "s",
            4000,
            fp_real(),
            improving(MIN_POWERED_PAIRS),
            ReportConfig::default(),
        );
        let mut json: serde_json::Value = serde_json::from_str(&v2.to_json()).unwrap();
        let obj = json.as_object_mut().unwrap();
        obj.remove("evidence_tier");
        obj.remove("power");
        v2 = serde_json::from_value(json).unwrap();
        assert_eq!(v2.evidence_tier, None);
        assert!(!v2.supports_quality_claim());
        assert!(v2.render().contains("EVIDENCE: UNSPECIFIED"));
    }

    #[test]
    fn most_conservative_verdict_prefers_regression_then_missing_evidence() {
        use Verdict::*;
        assert_eq!(Verdict::most_conservative([Improved, Regressed]), Regressed);
        assert_eq!(
            Verdict::most_conservative([Improved, NonInferior, Inconclusive]),
            Inconclusive
        );
        assert_eq!(
            Verdict::most_conservative([Improved, NonInferior]),
            NonInferior
        );
        assert_eq!(Verdict::most_conservative([]), Inconclusive);
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
    fn identical_scores_are_non_inferior_only_when_powered() {
        let records = vec![rec("1", 0.7, 0.7), rec("2", 0.4, 0.4)];
        let report = AbReport::build("s", 4000, fp(), records, ReportConfig::default());
        assert_eq!(report.verdict, Verdict::Inconclusive);
        assert_eq!(report.stats.ties, 2);
        assert!(report.verdict.gate_passes());

        let records: Vec<_> = (0..MIN_POWERED_PAIRS)
            .map(|i| rec(&i.to_string(), 0.7, 0.7))
            .collect();
        let report = AbReport::build("s", 4000, fp(), records, ReportConfig::default());
        assert_eq!(report.verdict, Verdict::NonInferior);
    }

    #[test]
    fn small_suite_is_labelled_underpowered() {
        let small = vec![rec("1", 0.7, 0.7), rec("2", 0.4, 0.4)];
        let report = AbReport::build("s", 4000, fp(), small, ReportConfig::default());
        assert!(
            report
                .render()
                .contains("POWER:   underpowered — 2 paired task(s) < 30"),
            "{}",
            report.render()
        );

        let large: Vec<_> = (0..MIN_POWERED_PAIRS)
            .map(|i| rec(&i.to_string(), 0.5, 0.5))
            .collect();
        let report = AbReport::build("s", 4000, fp(), large, ReportConfig::default());
        assert!(!report.render().contains("underpowered"));
    }

    #[test]
    fn bootstrap_ci_is_deterministic() {
        let diffs = vec![0.1, 0.3, -0.2, 0.5, 0.0, 0.4];
        let a = bootstrap_ci(&diffs, 1000, 42);
        let b = bootstrap_ci(&diffs, 1000, 42);
        assert_eq!(a, b);
    }
}
