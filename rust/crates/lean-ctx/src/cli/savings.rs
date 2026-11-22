// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx savings [--period day|week|month|all] [--format table|json|markdown]`.

use lean_ctx::core::{
    savings_ledger,
    value_gate::{self, ValueAssessment},
};

#[rustfmt::skip]
#[derive(serde::Serialize)]
struct Report {
    period: String,
    total_tasks: usize,
    accepted_tasks: usize,
    tokens_processed: u64,
    tokens_saved: u64,
    compression_percent: f64,
    estimated_cost_usd: f64,
    actual_cost_usd: f64,
    total_savings_usd: f64,
    savings_percent: f64,
    cpao_usd: Option<f64>,
    etpao: Option<u64>,
    top_sources: Vec<(String, u64)>,
}

#[rustfmt::skip]
pub(crate) fn cmd_savings_report(args: &[String]) {
    if args
        .iter()
        .any(|arg| matches!(arg.as_str(), "-h" | "--help"))
    {
        usage();
        return;
    }
    let Some((period, format)) = parse(args) else {
        eprintln!("savings: expected --period day|week|month|all and --format table|json|markdown");
        usage();
        std::process::exit(2);
    };
    let report = build_report(
        period,
        savings_ledger::all_events(),
        value_gate::store().recent(usize::MAX),
    );
    match format {
        "json" => println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".into())
        ),
        "markdown" => print_markdown(&report),
        _ => print!("{}", table(&report)),
    }
}

#[rustfmt::skip]
fn build_report(
    period: &str,
    events: Vec<savings_ledger::SavingsEvent>,
    tasks: Vec<ValueAssessment>,
) -> Report {
    let cutoff = days(period).map(|d| lean_ctx_time::UtcDateTime::now() - lean_ctx_time::Duration::days(d));
    let events: Vec<_> = events
        .into_iter()
        .filter(|e| e.mechanism == savings_ledger::MECHANISM_COMPRESSION)
        .filter(|e| in_period(&e.ts, cutoff))
        .collect();
    let tasks: Vec<_> = tasks
        .into_iter()
        .filter(|t| in_period(&t.timestamp, cutoff))
        .collect();
    let raw = events.iter().map(|e| e.baseline_tokens).sum::<u64>();
    let actual = events.iter().map(|e| e.actual_tokens).sum::<u64>();
    let saved = raw.saturating_sub(actual);
    let estimated = events
        .iter()
        .map(|e| e.baseline_tokens as f64 * e.unit_price_per_m_usd / 1e6)
        .sum();
    let actual_cost = events
        .iter()
        .map(|e| e.actual_tokens as f64 * e.unit_price_per_m_usd / 1e6)
        .sum();
    let accepted = tasks.iter().filter(|t| t.outcome_accepted).count();
    let cost_micros = tasks.iter().map(|t| t.cost_micros).sum::<u64>();
    Report {
        period: period.into(),
        total_tasks: tasks.len(),
        accepted_tasks: accepted,
        tokens_processed: raw,
        tokens_saved: saved,
        compression_percent: pct(saved, raw),
        estimated_cost_usd: estimated,
        actual_cost_usd: actual_cost,
        total_savings_usd: estimated - actual_cost,
        savings_percent: pct(saved, raw),
        cpao_usd: (accepted > 0).then_some(cost_micros as f64 / accepted as f64 / 1e6),
        etpao: (accepted > 0).then(|| {
            tasks.iter().map(|t| t.total_tokens).sum::<u64>() / accepted as u64
        }),
        top_sources: top_sources(&events),
    }
}

/// Days covered by `period`; `None` for `all` (no cutoff).
#[rustfmt::skip] fn days(period: &str) -> Option<i64> { match period { "day" => Some(1), "week" => Some(7), "month" => Some(30), _ => None } }
#[rustfmt::skip] fn period_label(period: &str) -> String { days(period).map_or_else(|| "all time".into(), |d| format!("last {d} days")) }
#[rustfmt::skip] fn in_period(timestamp: &str, cutoff: Option<lean_ctx_time::UtcDateTime>) -> bool { cutoff.is_none_or(|cutoff| lean_ctx_time::OffsetDateTime::parse_from_rfc3339(timestamp).map(|t| t.to_utc() >= cutoff).unwrap_or(false)) }
#[rustfmt::skip] fn pct(value: u64, total: u64) -> f64 { if total == 0 { 0.0 } else { value as f64 * 100.0 / total as f64 } }
#[rustfmt::skip]
fn top_sources(events: &[savings_ledger::SavingsEvent]) -> Vec<(String, u64)> {
    let mut out = std::collections::BTreeMap::new();
    for event in events {
        *out.entry(event.tool.clone()).or_insert(0) +=
            event.baseline_tokens.saturating_sub(event.actual_tokens);
    }
    let mut out: Vec<_> = out.into_iter().collect();
    out.sort_by_key(|(_, saved)| std::cmp::Reverse(*saved));
    out.truncate(5);
    out
}
fn parse(args: &[String]) -> Option<(&str, &str)> {
    let mut period = "week";
    let mut format = "table";
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--period" => {
                period = args.get(index + 1)?.as_str();
                index += 2;
            }
            "--format" => {
                format = args.get(index + 1)?.as_str();
                index += 2;
            }
            _ => return None,
        }
    }
    (matches!(period, "day" | "week" | "month" | "all")
        && matches!(format, "table" | "json" | "markdown"))
    .then_some((period, format))
}
fn usage() {
    println!(
        "Show compression savings from the local ledger.\n\nUsage: lean-ctx savings [--period <day|week|month|all>] [--format <table|json|markdown>]\n\nExamples:\n  lean-ctx savings\n  lean-ctx savings --period month\n  lean-ctx savings --period all --format json"
    );
}
#[rustfmt::skip]
fn table(r: &Report) -> String {
    if r.tokens_processed == 0 {
        return format!("LeanCTX Savings Report ({})\nNo compression savings recorded for this period.\n", r.period);
    }
    let mut out = format!(
        "LeanCTX Savings Report ({})\nTotal Tasks: {}\nAccepted Tasks: {} ({:.1}%)\nTokens Processed: {}\nTokens Saved (Compression): {} ({:.1}% reduction)\nEstimated Cost without LeanCTX: ${:.2}\nActual Cost with LeanCTX: ${:.2}\nTotal Savings: ${:.2} ({:.1}%)\nCPAO (Cost per Accepted Outcome): {}\nETPAO (Effective Tokens per Accepted Outcome): {}\nTop 5 Savings Sources:\n",
        period_label(&r.period),
        r.total_tasks,
        r.accepted_tasks,
        pct(r.accepted_tasks as u64, r.total_tasks as u64),
        r.tokens_processed,
        r.tokens_saved,
        r.compression_percent,
        r.estimated_cost_usd,
        r.actual_cost_usd,
        r.total_savings_usd,
        r.savings_percent,
        r.cpao_usd.map_or("n/a".into(), |v| format!("${v:.2}")),
        r.etpao.map_or("n/a".into(), |v| v.to_string())
    );
    for (tool, saved) in &r.top_sources {
        out.push_str(&format!("  {tool}: {saved} tokens\n"));
    }
    out
}
#[rustfmt::skip]
fn print_markdown(r: &Report) {
    if r.tokens_processed == 0 {
        println!(
            "# LeanCTX Savings Report ({})\n\nNo compression savings recorded for this period.",
            r.period
        );
        return;
    }
    println!(
        "# LeanCTX Savings Report ({})\n\n| Metric | Value |\n|---|---|\n| Total Tasks | {} |\n| Accepted Tasks | {} |\n| Tokens Processed | {} |\n| Tokens Saved (Compression) | {} ({:.1}%) |\n| Total Savings | ${:.2} |",
        period_label(&r.period),
        r.total_tasks,
        r.accepted_tasks,
        r.tokens_processed,
        r.tokens_saved,
        r.compression_percent,
        r.total_savings_usd
    );
}

#[cfg(test)] #[rustfmt::skip] mod tests { use super::*;
    #[test] fn test_savings_calculation() {
        assert_eq!((1_000u64).saturating_sub(600), 400);
        assert_eq!(pct(400, 1_000), 40.0);
    }
    #[test] fn test_cost_estimation() {
        assert_eq!(
            lean_ctx::core::value_gate::cost_tracker::calculate_cost(400, 0, 0, "gpt-4o"),
            1_000
        );
    }
    #[test] fn test_period_filter() {
        let now = lean_ctx_time::UtcDateTime::now();
        assert!(in_period(
            &now.to_rfc3339(),
            Some(now - lean_ctx_time::Duration::days(1))
        ));
        assert!(!in_period(
            "2000-01-01T00:00:00Z",
            Some(now - lean_ctx_time::Duration::days(30))
        ));
    }
    #[test] fn test_period_all_has_no_cutoff() {
        assert_eq!(build_report("all", vec![], vec![]).period, "all");
    }
    #[test] fn test_empty_data() {
        let r = build_report("week", vec![], vec![]);
        assert_eq!(r.tokens_processed, 0);
    }
    #[test] fn test_format_table() {
        assert!(table(&build_report("week", vec![], vec![])).contains("No compression savings"));
    }
    #[test] fn test_parse_rejects_missing_values_and_unknown_options() { assert!(parse(&["--period".into()]).is_none()); assert!(parse(&["--unknown".into()]).is_none()); }
    #[test] fn test_format_json() {
        let json = serde_json::to_string(&build_report("week", vec![], vec![])).unwrap();
        assert!(serde_json::from_str::<serde_json::Value>(&json).is_ok());
    }
}
