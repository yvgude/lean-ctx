//! `FinOps` cost/savings export (GL #402): `CloudZero` CBF, Vantage, FOCUS CSV.
//!
//! Turns the tamper-evident savings ledger into daily cost rows a `FinOps`
//! platform can ingest for showback/chargeback. The ledger is the *only*
//! source — every exported number is backed by a hash-chained event with the
//! model price pinned at recording time (`unit_price_per_m_usd`), so a price
//! change never rewrites history. No separate pricing table to maintain.
//!
//! Savings representation (ADR, per platform):
//! - CBF: `lineitem/type=Discount` rows with negative `cost/cost` — CBF's
//!   documented mechanism for rate reductions.
//! - FOCUS / Vantage: `ChargeCategory=Credit` rows with negative
//!   `BilledCost` — FOCUS's category for granted reductions; keeps Usage
//!   spend clean for budgeting while savings stay drillable.

pub mod aliases;
pub mod cbf;
pub mod focus;
pub mod vantage;

use std::collections::BTreeMap;

/// One day × project × agent × model × tool aggregate from the ledger.
#[derive(Debug, Clone, PartialEq)]
pub struct DailyCostRow {
    /// `YYYY-MM-DD` (UTC, from the event timestamp).
    pub date: String,
    /// Privacy-preserving project identifier (truncated repo hash — the
    /// ledger never stores paths). Map to readable names downstream.
    pub project: String,
    /// Recording agent identity (role attribution).
    pub agent_role: String,
    pub model: String,
    pub tool: String,
    /// Tokens actually sent through lean-ctx (the billed reality).
    pub tokens_actual: u64,
    /// Verified tokens saved (bounce-adjusted).
    pub tokens_saved: u64,
    /// Cost of the actual tokens at the event-pinned model price.
    pub cost_usd: f64,
    /// Verified savings valued at the same pinned price.
    pub savings_usd: f64,
}

/// Inclusive date-range filter, `YYYY-MM-DD` strings (lexicographic compare
/// is correct for ISO dates).
#[derive(Debug, Clone, Default)]
pub struct DateRange {
    pub from: Option<String>,
    pub to: Option<String>,
}

impl DateRange {
    fn contains(&self, date: &str) -> bool {
        if let Some(f) = &self.from
            && date < f.as_str()
        {
            return false;
        }
        if let Some(t) = &self.to
            && date > t.as_str()
        {
            return false;
        }
        true
    }
}

/// Aggregate the ledger into daily rows. Events outside the range (or with
/// a malformed timestamp) are skipped.
#[must_use]
pub fn aggregate(range: &DateRange) -> Vec<DailyCostRow> {
    let Some(path) = crate::core::savings_ledger::store::default_path() else {
        return Vec::new();
    };
    aggregate_events(&crate::core::savings_ledger::store::load(&path), range)
}

fn aggregate_events(
    events: &[crate::core::savings_ledger::SavingsEvent],
    range: &DateRange,
) -> Vec<DailyCostRow> {
    let mut map: BTreeMap<(String, String, String, String, String), DailyCostRow> = BTreeMap::new();

    for ev in events {
        let Some(date) = ev.ts.get(..10) else {
            continue;
        };
        if date.len() != 10 || !range.contains(date) {
            continue;
        }
        let key = (
            date.to_string(),
            ev.repo_hash.clone(),
            ev.agent_id.clone(),
            ev.model_id.clone(),
            ev.tool.clone(),
        );
        let row = map.entry(key).or_insert_with(|| DailyCostRow {
            date: date.to_string(),
            project: ev.repo_hash.clone(),
            agent_role: ev.agent_id.clone(),
            model: ev.model_id.clone(),
            tool: ev.tool.clone(),
            tokens_actual: 0,
            tokens_saved: 0,
            cost_usd: 0.0,
            savings_usd: 0.0,
        });
        let net_saved = ev.saved_tokens.saturating_sub(ev.bounce_adjustment);
        row.tokens_actual += ev.actual_tokens;
        row.tokens_saved += net_saved;
        row.cost_usd += ev.actual_tokens as f64 / 1_000_000.0 * ev.unit_price_per_m_usd;
        row.savings_usd += ev.saved_usd;
    }

    map.into_values().collect()
}

/// RFC 4180 CSV field quoting (quote when the value contains `, " \n`).
pub(crate) fn csv_field(v: &str) -> String {
    if v.contains([',', '"', '\n']) {
        format!("\"{}\"", v.replace('"', "\"\""))
    } else {
        v.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::savings_ledger::SavingsEvent;

    pub(crate) fn event(
        ts: &str,
        repo: &str,
        model: &str,
        actual: u64,
        saved: u64,
    ) -> SavingsEvent {
        SavingsEvent {
            ts: ts.into(),
            tool: "ctx_read".into(),
            model_id: model.into(),
            tokenizer: "o200k_base".into(),
            baseline_tokens: actual + saved,
            actual_tokens: actual,
            saved_tokens: saved,
            bounce_adjustment: 0,
            unit_price_per_m_usd: 2.5,
            saved_usd: saved as f64 / 1_000_000.0 * 2.5,
            repo_hash: repo.into(),
            agent_id: "coder".into(),
            prev_hash: String::new(),
            entry_hash: String::new(),
        }
    }

    #[test]
    fn aggregates_per_day_and_dimensions() {
        let events = vec![
            event("2026-06-01T08:00:00+00:00", "proj_a", "claude", 300, 700),
            event("2026-06-01T09:00:00+00:00", "proj_a", "claude", 100, 900),
            event("2026-06-02T08:00:00+00:00", "proj_a", "claude", 50, 50),
            event("2026-06-01T08:00:00+00:00", "proj_b", "gpt", 10, 90),
        ];
        let rows = aggregate_events(&events, &DateRange::default());
        assert_eq!(rows.len(), 3, "day×project×model groups");

        let a1 = rows
            .iter()
            .find(|r| r.project == "proj_a" && r.date == "2026-06-01")
            .unwrap();
        assert_eq!(a1.tokens_actual, 400);
        assert_eq!(a1.tokens_saved, 1600);
        assert!((a1.cost_usd - 400.0 / 1e6 * 2.5).abs() < 1e-12);
        assert!((a1.savings_usd - 1600.0 / 1e6 * 2.5).abs() < 1e-12);
    }

    #[test]
    fn date_range_filters_inclusively() {
        let events = vec![
            event("2026-06-01T08:00:00+00:00", "p", "m", 1, 1),
            event("2026-06-02T08:00:00+00:00", "p", "m", 1, 1),
            event("2026-06-03T08:00:00+00:00", "p", "m", 1, 1),
        ];
        let range = DateRange {
            from: Some("2026-06-02".into()),
            to: Some("2026-06-02".into()),
        };
        let rows = aggregate_events(&events, &range);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].date, "2026-06-02");
    }

    #[test]
    fn bounce_adjustment_reduces_savings() {
        let mut ev = event("2026-06-01T08:00:00+00:00", "p", "m", 100, 1000);
        ev.bounce_adjustment = 400;
        let rows = aggregate_events(&[ev], &DateRange::default());
        assert_eq!(rows[0].tokens_saved, 600);
    }

    #[test]
    fn csv_field_quotes_specials() {
        assert_eq!(csv_field("plain"), "plain");
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
    }
}
