// SPDX-License-Identifier: Apache-2.0
//! USD cost calculation and multi-model projection for evidence workflows.

use serde::{Deserialize, Serialize};

use lean_ctx::core::gain::model_pricing::{ModelCost, ModelPricing};

/// Models we project costs for (covers cheap to expensive).
const PROJECTION_MODELS: &[(&str, &str)] = &[
    ("gpt-4o-mini", "GPT-4o Mini"),
    ("gpt-5.4", "GPT-5.4"),
    ("claude-sonnet-4.5", "Claude Sonnet 4.5"),
    ("claude-opus-4.5", "Claude Opus 4.5"),
    ("gemini-2.5-pro", "Gemini 2.5 Pro"),
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CostEstimate {
    pub model: String,
    pub model_label: String,
    pub baseline_input_tokens: usize,
    pub treatment_input_tokens: usize,
    pub baseline_usd: f64,
    pub treatment_usd: f64,
    pub savings_usd: f64,
    pub savings_pct: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct MonthlyProjection {
    pub model: String,
    pub tasks_per_day: u32,
    pub monthly_without_usd: f64,
    pub monthly_with_usd: f64,
    pub monthly_savings_usd: f64,
    pub annual_savings_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CostReport {
    pub per_task_costs: Vec<CostEstimate>,
    pub monthly_projections: Vec<MonthlyProjection>,
    pub summary: CostSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CostSummary {
    pub cheapest_model: String,
    pub most_expensive_model: String,
    pub max_savings_per_task_usd: f64,
    pub avg_savings_pct: f64,
}

/// Calculate cost estimates for all projection models.
pub(crate) fn calculate_cost_report(
    baseline_tokens: usize,
    treatment_tokens: usize,
    output_tokens_estimate: usize,
    tasks_per_day: u32,
) -> CostReport {
    let pricing = ModelPricing::load();
    let mut estimates = Vec::new();

    for &(model_key, label) in PROJECTION_MODELS {
        let quote = pricing.quote(Some(model_key));
        let cost = quote.cost;

        let baseline_usd =
            cost.estimate_usd(baseline_tokens as u64, output_tokens_estimate as u64, 0, 0);
        let treatment_usd =
            cost.estimate_usd(treatment_tokens as u64, output_tokens_estimate as u64, 0, 0);
        let savings_usd = baseline_usd - treatment_usd;
        let savings_pct = if baseline_usd > 0.0 {
            savings_usd / baseline_usd * 100.0
        } else {
            0.0
        };

        estimates.push(CostEstimate {
            model: model_key.to_string(),
            model_label: label.to_string(),
            baseline_input_tokens: baseline_tokens,
            treatment_input_tokens: treatment_tokens,
            baseline_usd,
            treatment_usd,
            savings_usd,
            savings_pct,
        });
    }

    let monthly_projections: Vec<MonthlyProjection> = estimates
        .iter()
        .map(|e| {
            let monthly_without = e.baseline_usd * tasks_per_day as f64 * 30.0;
            let monthly_with = e.treatment_usd * tasks_per_day as f64 * 30.0;
            MonthlyProjection {
                model: e.model.clone(),
                tasks_per_day,
                monthly_without_usd: monthly_without,
                monthly_with_usd: monthly_with,
                monthly_savings_usd: monthly_without - monthly_with,
                annual_savings_usd: (monthly_without - monthly_with) * 12.0,
            }
        })
        .collect();

    let max_savings = estimates
        .iter()
        .map(|e| e.savings_usd)
        .fold(0.0f64, f64::max);
    let avg_pct = if estimates.is_empty() {
        0.0
    } else {
        estimates.iter().map(|e| e.savings_pct).sum::<f64>() / estimates.len() as f64
    };

    let most_expensive = estimates
        .iter()
        .max_by(|a, b| {
            a.savings_usd
                .partial_cmp(&b.savings_usd)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|e| e.model.clone())
        .unwrap_or_default();
    let cheapest = estimates
        .iter()
        .min_by(|a, b| {
            a.savings_usd
                .partial_cmp(&b.savings_usd)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|e| e.model.clone())
        .unwrap_or_default();

    CostReport {
        per_task_costs: estimates,
        monthly_projections,
        summary: CostSummary {
            cheapest_model: cheapest,
            most_expensive_model: most_expensive,
            max_savings_per_task_usd: max_savings,
            avg_savings_pct: avg_pct,
        },
    }
}

/// Format a cost report as a human-readable table.
pub(crate) fn format_cost_table(report: &CostReport) -> String {
    let mut out = String::new();

    out.push_str("╔══════════════════════════════════════════════════════════════════════════╗\n");
    out.push_str("║  Cost Comparison (per task)                                             ║\n");
    out.push_str("╠══════════════════════════════════════════════════════════════════════════╣\n");
    out.push_str("║  Model                  │ Without      │ With lean-ctx │ Savings        ║\n");
    out.push_str("╟─────────────────────────┼──────────────┼───────────────┼────────────────╢\n");

    for e in &report.per_task_costs {
        out.push_str(&format!(
            "║  {:<22} │ ${:<10.4} │ ${:<11.4} │ ${:<6.4} ({:.0}%) ║\n",
            e.model_label, e.baseline_usd, e.treatment_usd, e.savings_usd, e.savings_pct
        ));
    }

    out.push_str("╠══════════════════════════════════════════════════════════════════════════╣\n");
    out.push_str("║  Monthly Projection (100 tasks/day)                                    ║\n");
    out.push_str("╟─────────────────────────┼──────────────┼───────────────┼────────────────╢\n");

    for p in &report.monthly_projections {
        out.push_str(&format!(
            "║  {:<22} │ ${:<10.2} │ ${:<11.2} │ ${:<6.2}/mo     ║\n",
            p.model, p.monthly_without_usd, p.monthly_with_usd, p.monthly_savings_usd
        ));
    }

    out.push_str("╚══════════════════════════════════════════════════════════════════════════╝\n");
    out
}

/// Get the ModelCost for a specific model (for inline calculations).
#[allow(dead_code)]
pub(crate) fn get_model_cost(model: &str) -> ModelCost {
    let pricing = ModelPricing::load();
    pricing.quote(Some(model)).cost
}
