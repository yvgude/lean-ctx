//! Aggregate baseline-versus-treatment comparison.

use serde::{Deserialize, Serialize};

use super::{baseline::BaselineMeasurement, recommendation::TreatmentMeasurement};

/// Reports whose baseline uses each task's own host model (3.11.0 and later).
pub const SAME_HOST_MODEL_SEMANTICS: &str = "same-host-model/v2";
/// Reports written before 3.11.0 carry no `semantics` field. Their baseline was priced with a
/// fixed reference model, so cost differences may be model price differences and their
/// recommendations may name a model switch. They are kept as records, never shown as current.
pub const LEGACY_SEMANTICS: &str = "legacy-reference-model";

fn legacy_semantics() -> String {
    LEGACY_SEMANTICS.into()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShadowReport {
    #[serde(default = "legacy_semantics")]
    pub semantics: String,
    pub report_id: String,
    pub timestamp: String,
    pub tasks_count: u32,
    pub tasks_accepted: u32,
    pub baseline: AggregateMetrics,
    pub treatment: AggregateMetrics,
    /// Treatment minus baseline; negative values mean the treatment is lower.
    pub delta: DeltaMetrics,
    pub savings: SavingsMetrics,
    pub recommendations: Vec<Recommendation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AggregateMetrics {
    pub total_cost_micros: u64,
    pub total_tokens: u64,
    pub avg_cpao_micros: Option<u64>,
    pub avg_latency_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DeltaMetrics {
    pub cost_micros: i64,
    pub tokens: i64,
    pub cpao_micros: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavingsMetrics {
    pub absolute_micros: u64,
    /// Negative values indicate a cost increase instead of savings.
    pub relative_percent: f64,
    pub tokens_saved: u64,
    pub quality_maintained: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recommendation {
    pub category: String,
    pub description: String,
    pub estimated_savings_micros: u64,
}

pub fn compare(
    baselines: &[BaselineMeasurement],
    treatments: &[TreatmentMeasurement],
) -> ShadowReport {
    let baseline = aggregate(baselines.iter().map(Metric::baseline));
    let treatment = aggregate(treatments.iter().map(Metric::treatment));
    let cost_delta = signed_delta(treatment.total_cost_micros, baseline.total_cost_micros);
    let token_delta = signed_delta(treatment.total_tokens, baseline.total_tokens);
    let baseline_accepted = baselines
        .iter()
        .filter(|item| item.outcome_accepted)
        .count();
    let treatment_accepted = treatments
        .iter()
        .filter(|item| item.outcome_accepted)
        .count();
    let quality_maintained = acceptance_rate(treatment_accepted, treatments.len())
        >= acceptance_rate(baseline_accepted, baselines.len());
    let absolute_micros = baseline
        .total_cost_micros
        .saturating_sub(treatment.total_cost_micros);
    let tokens_saved = baseline.total_tokens.saturating_sub(treatment.total_tokens);
    let savings = SavingsMetrics {
        absolute_micros,
        relative_percent: percentage_savings(
            baseline.total_cost_micros,
            treatment.total_cost_micros,
        ),
        tokens_saved,
        quality_maintained,
    };
    let delta = DeltaMetrics {
        cost_micros: cost_delta,
        tokens: token_delta,
        cpao_micros: match (treatment.avg_cpao_micros, baseline.avg_cpao_micros) {
            (Some(treatment), Some(baseline)) => Some(signed_delta(treatment, baseline)),
            _ => None,
        },
    };
    let recommendations = recommendations(baselines, treatments, &savings);

    ShadowReport {
        semantics: SAME_HOST_MODEL_SEMANTICS.into(),
        report_id: format!(
            "shadow-{}-{}-{}",
            baseline.total_cost_micros, treatment.total_cost_micros, baseline.total_tokens
        ),
        timestamp: "deterministic".into(),
        tasks_count: u32::try_from(baselines.len()).unwrap_or(u32::MAX),
        tasks_accepted: u32::try_from(treatment_accepted).unwrap_or(u32::MAX),
        baseline,
        treatment,
        delta,
        savings,
        recommendations,
    }
}

/// #1905: the baseline is simulated from the same task and outcome signals as
/// the treatment, so the acceptance comparison cannot show a quality change.
pub const BASELINE_NOTE: &str = "Note: the baseline is simulated (uncompressed token counts, same outcome signals), not a second measured run. Cost and token deltas are estimates; the acceptance line is not a measured quality comparison.";

impl ShadowReport {
    /// A stored report from before the same-host-model comparison (see [`LEGACY_SEMANTICS`]).
    pub fn is_legacy(&self) -> bool {
        self.semantics != SAME_HOST_MODEL_SEMANTICS
    }
}

pub const LEGACY_NOTE: &str = "Legacy record: written before reports compared each task with its own host model. Its baseline may be priced with a different reference model, so its cost difference is not a context saving and its recommendations are not current guidance. Kept for history only.";

pub fn format_report(report: &ShadowReport) -> String {
    if report.is_legacy() {
        // Token counts do not depend on the model, so they stay readable; costs and
        // recommendations from the reference-model baseline are withheld.
        return format!(
            "# Shadow Comparison Report (legacy record)\n\n{LEGACY_NOTE}\n\nRecorded tasks: {}. Tokens: baseline {}, treatment {}.\n\n{BASELINE_NOTE}\n\n## Recommendations\n- Withheld: recommendations of legacy records are not current guidance.",
            report.tasks_count, report.baseline.total_tokens, report.treatment.total_tokens,
        );
    }
    let recommendations = if report.recommendations.is_empty() {
        "- No recommendation: treatment did not reduce cost without lowering outcome acceptance."
            .into()
    } else {
        report
            .recommendations
            .iter()
            .map(|item| {
                format!(
                    "- {}: {} (estimated savings: {} micros)",
                    item.category, item.description, item.estimated_savings_micros
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    format!(
        "# Shadow Comparison Report\n\n| Metric | Baseline | Treatment | Delta |\n|---|---:|---:|---:|\n| Cost (micros) | {} | {} | {} |\n| Tokens | {} | {} | {} |\n| Avg CPAO (micros) | {} | {} | {} |\n| Avg latency (ms) | {} | {} | {} |\n\nCost savings: {} micros ({:.2}%). Token savings: {}. Outcome acceptance not below baseline: {}.\n\n{BASELINE_NOTE}\n\n## Recommendations\n{}",
        report.baseline.total_cost_micros,
        report.treatment.total_cost_micros,
        format_delta(report.delta.cost_micros, "saved", "more"),
        report.baseline.total_tokens,
        report.treatment.total_tokens,
        format_delta(report.delta.tokens, "saved", "more"),
        display_cpao(report.baseline.avg_cpao_micros),
        display_cpao(report.treatment.avg_cpao_micros),
        display_cpao_delta(report.delta.cpao_micros),
        report.baseline.avg_latency_ms,
        report.treatment.avg_latency_ms,
        format_delta(
            signed_delta(
                report.treatment.avg_latency_ms,
                report.baseline.avg_latency_ms
            ),
            "faster",
            "slower",
        ),
        report.savings.absolute_micros,
        report.savings.relative_percent,
        report.savings.tokens_saved,
        if report.savings.quality_maintained {
            "yes"
        } else {
            "no"
        },
        recommendations,
    )
}

fn aggregate(items: impl Iterator<Item = Metric>) -> AggregateMetrics {
    let values: Vec<_> = items.collect();
    let accepted: Vec<_> = values.iter().filter(|item| item.accepted).collect();
    AggregateMetrics {
        total_cost_micros: values.iter().map(|item| item.cost).sum(),
        total_tokens: values.iter().map(|item| item.tokens).sum(),
        avg_cpao_micros: (!accepted.is_empty())
            .then(|| accepted.iter().map(|item| item.cost).sum::<u64>() / accepted.len() as u64),
        avg_latency_ms: if values.is_empty() {
            0
        } else {
            values.iter().map(|item| item.duration).sum::<u64>() / values.len() as u64
        },
    }
}

/// LeanCTX never recommends a model: the host chooses it. A cost difference is attributed to
/// context compression only when every task was priced with the same model on both sides, so a
/// price difference between models can never appear as a context saving.
fn recommendations(
    baselines: &[BaselineMeasurement],
    treatments: &[TreatmentMeasurement],
    savings: &SavingsMetrics,
) -> Vec<Recommendation> {
    let same_models = baselines.len() == treatments.len()
        && baselines
            .iter()
            .zip(treatments)
            .all(|(baseline, treatment)| baseline.model == treatment.model);
    if !same_models
        || savings.absolute_micros == 0
        || savings.tokens_saved == 0
        || !savings.quality_maintained
    {
        return Vec::new();
    }
    vec![Recommendation {
        category: "Context compression".into(),
        description: format!(
            "Keep context compression: it removed an estimated {} tokens against the simulated baseline.",
            savings.tokens_saved
        ),
        estimated_savings_micros: savings.absolute_micros,
    }]
}

#[derive(Clone, Copy)]
struct Metric {
    tokens: u64,
    cost: u64,
    duration: u64,
    accepted: bool,
}

impl Metric {
    fn baseline(item: &BaselineMeasurement) -> Self {
        Self::new(
            item.input_tokens,
            item.output_tokens,
            item.total_cost_micros,
            item.duration_ms,
            item.outcome_accepted,
        )
    }

    fn treatment(item: &TreatmentMeasurement) -> Self {
        Self::new(
            item.input_tokens,
            item.output_tokens,
            item.total_cost_micros,
            item.duration_ms,
            item.outcome_accepted,
        )
    }

    fn new(input: u64, output: u64, cost: u64, duration: u64, accepted: bool) -> Self {
        Self {
            tokens: input.saturating_add(output),
            cost,
            duration,
            accepted,
        }
    }
}

fn acceptance_rate(accepted: usize, total: usize) -> f64 {
    if total == 0 {
        1.0
    } else {
        accepted as f64 / total as f64
    }
}

fn percentage_savings(baseline: u64, treatment: u64) -> f64 {
    if baseline == 0 {
        0.0
    } else {
        (baseline as f64 - treatment as f64) * 100.0 / baseline as f64
    }
}

fn signed_delta(treatment: u64, baseline: u64) -> i64 {
    let difference = i128::from(treatment) - i128::from(baseline);
    i64::try_from(difference).unwrap_or_else(|_| {
        if difference.is_negative() {
            i64::MIN
        } else {
            i64::MAX
        }
    })
}

fn format_delta(delta: i64, lower_label: &str, higher_label: &str) -> String {
    match delta.cmp(&0) {
        std::cmp::Ordering::Less => format!("{} {lower_label}", delta.unsigned_abs()),
        std::cmp::Ordering::Greater => format!("{} {higher_label}", delta.unsigned_abs()),
        std::cmp::Ordering::Equal => "unchanged".into(),
    }
}

fn display_cpao(value: Option<u64>) -> String {
    value.map_or_else(|| "n/a".into(), |item| item.to_string())
}

fn display_cpao_delta(value: Option<i64>) -> String {
    value.map_or_else(
        || "n/a".into(),
        |delta| format_delta(delta, "lower", "higher"),
    )
}
