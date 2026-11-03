// SPDX-License-Identifier: Apache-2.0
use serde::Serialize;

pub(super) fn handle(
    path: &str,
    _query_str: &str,
    method: &str,
    _body: &str,
) -> Option<(&'static str, &'static str, String)> {
    match (path, method) {
        ("/api/solution", "GET") => Some(solution_response()),
        _ => None,
    }
}

#[derive(Serialize)]
struct SolutionApiResponse {
    enabled: bool,
    intensity: String,
    output_savings: OutputSavings,
    loc: LocMetrics,
    decisions: DecisionMetrics,
    trend_7d: Vec<TrendDay>,
    top_patterns: Vec<PatternMetric>,
}

#[derive(Serialize)]
struct OutputSavings {
    tokens_total: u64,
    tokens_optimized: u64,
    reduction_pct: u8,
}

#[derive(Serialize)]
struct LocMetrics {
    added: u64,
    removed: u64,
    net_reduced: i64,
    reduction_pct: u8,
}

#[derive(Serialize)]
struct DecisionMetrics {
    total: u64,
    stdlib: u64,
    native: u64,
    reuse: u64,
    yagni: u64,
    debt_open: u64,
}

#[derive(Serialize)]
struct TrendDay {
    date: String,
    loc_reduced: i64,
    decisions: u64,
}

#[derive(Serialize)]
struct PatternMetric {
    pattern: String,
    count: u64,
}

fn solution_response() -> (&'static str, &'static str, String) {
    let config = lean_ctx::core::config::Config::load();
    let snapshot = lean_ctx::core::solution_tracker::snapshot();
    let mut top_patterns: Vec<PatternMetric> = snapshot
        .decisions_by_kind
        .iter()
        .filter(|(_, count)| **count > 0)
        .map(|(kind, count)| PatternMetric {
            pattern: kind.clone(),
            count: *count,
        })
        .collect();
    top_patterns.sort_unstable_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then_with(|| left.pattern.cmp(&right.pattern))
    });
    top_patterns.truncate(5);

    let loc_reduction_pct = if snapshot.loc_removed == 0 {
        0
    } else {
        ((snapshot.loc_net_saved.max(0) as u64)
            .saturating_mul(100)
            .saturating_div(snapshot.loc_removed)
            .min(100)) as u8
    };

    let response = SolutionApiResponse {
        enabled: config.solution.enabled,
        intensity: config.solution.effective_intensity().label().to_string(),
        output_savings: OutputSavings {
            tokens_total: snapshot.output_tokens_baseline,
            tokens_optimized: snapshot.output_tokens_actual,
            reduction_pct: snapshot.output_reduction_pct,
        },
        loc: LocMetrics {
            added: snapshot.loc_added,
            removed: snapshot.loc_removed,
            net_reduced: snapshot.loc_net_saved,
            reduction_pct: loc_reduction_pct,
        },
        decisions: DecisionMetrics {
            total: snapshot.decisions_total,
            stdlib: snapshot
                .decisions_by_kind
                .get("stdlib")
                .copied()
                .unwrap_or_default(),
            native: snapshot
                .decisions_by_kind
                .get("native")
                .copied()
                .unwrap_or_default(),
            reuse: snapshot
                .decisions_by_kind
                .get("reuse")
                .copied()
                .unwrap_or_default(),
            yagni: snapshot
                .decisions_by_kind
                .get("yagni")
                .copied()
                .unwrap_or_default(),
            debt_open: snapshot
                .decisions_by_kind
                .get("debt")
                .copied()
                .unwrap_or_default(),
        },
        trend_7d: lean_ctx::core::solution_tracker::trend_7d()
            .iter()
            .map(|(date, decisions, loc)| TrendDay {
                date: date.clone(),
                loc_reduced: *loc,
                decisions: *decisions,
            })
            .collect(),
        top_patterns,
    };

    (
        "200 OK",
        "application/json",
        serde_json::to_string(&response).unwrap_or_else(|_| "{}".to_string()),
    )
}
