// SPDX-License-Identifier: Apache-2.0

//! The installation's own daily usage record, sent as `usage_history`.
//!
//! `stats.json` (what `lean-ctx gain` shows) already counts every compressed
//! operation, MCP or shell hook, per local calendar day. Sending its recent
//! days lets usage and savings be charted from before telemetry existed and
//! for installations that never call an MCP tool. Counts only: no command
//! names, paths or content.

use crate::core::stats::{DayStats, StatsStore};
use crate::core::telemetry_v2::{
    LifetimeUsage, MAX_COUNT, MAX_HISTORY_DAYS, MAX_TOKENS, UsageDay, UsageHistoryMetrics,
};

/// History for the batch, or `None` when the installation never recorded use.
/// Summed over the user's data directories, like `lean-ctx gain`: the
/// installation ID is shared across them, so the record must be too.
pub(super) fn usage_history(today: lean_ctx_time::Date) -> Option<UsageHistoryMetrics> {
    usage_history_from(&crate::core::stats::load_for_display(), today)
}

pub(super) fn usage_history_from(
    store: &StatsStore,
    today: lean_ctx_time::Date,
) -> Option<UsageHistoryMetrics> {
    let horizon = today - lean_ctx_time::Duration::days(MAX_HISTORY_DAYS as i64 - 1);
    // Local days may run one ahead of UTC; anything later is a clock error.
    let latest = today + lean_ctx_time::Duration::days(1);
    let mut days: Vec<UsageDay> = store
        .daily
        .iter()
        .filter_map(|day| {
            let date = lean_ctx_time::parse_date(&day.date, "%Y-%m-%d").ok()?;
            (date >= horizon && date <= latest && day.commands > 0).then(|| usage_day(date, day))
        })
        .collect();
    days.sort_by(|a, b| a.date.cmp(&b.date));
    days.dedup_by(|later, earlier| later.date == earlier.date);
    if days.len() > MAX_HISTORY_DAYS {
        days.drain(..days.len() - MAX_HISTORY_DAYS);
    }
    if days.is_empty() && store.total_commands == 0 {
        return None;
    }
    Some(UsageHistoryMetrics {
        days,
        lifetime: LifetimeUsage {
            commands: store.total_commands.min(MAX_COUNT),
            original_tokens: store.total_input_tokens.min(MAX_TOKENS),
            delivered_tokens: store.total_output_tokens.min(MAX_TOKENS),
            first_use_month: store
                .first_use
                .as_deref()
                .and_then(|stamp| lean_ctx_time::OffsetDateTime::parse_from_rfc3339(stamp).ok())
                .map(|stamp| stamp.format("%Y-%m").to_string()),
        },
    })
}

fn usage_day(date: lean_ctx_time::Date, day: &DayStats) -> UsageDay {
    UsageDay {
        date: date.strftime("%Y-%m-%d").to_string(),
        commands: day.commands.min(MAX_COUNT),
        original_tokens: day.input_tokens.min(MAX_TOKENS),
        delivered_tokens: day.output_tokens.min(MAX_TOKENS),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(date: &str, commands: u64, input: u64, output: u64) -> DayStats {
        DayStats {
            date: date.into(),
            commands,
            input_tokens: input,
            output_tokens: output,
            ..DayStats::default()
        }
    }

    fn today() -> lean_ctx_time::Date {
        lean_ctx_time::Date::new(2026, 10, 8).unwrap()
    }

    #[test]
    fn history_keeps_the_retention_window_in_order_without_identifying_detail() {
        let store = StatsStore {
            total_commands: 120,
            total_input_tokens: 90_000,
            total_output_tokens: 20_000,
            first_use: Some("2026-03-14T09:12:44+02:00".into()),
            daily: vec![
                day("2026-01-02", 5, 100, 50),
                day("2026-07-11", 0, 0, 0),
                day("2026-10-08", 7, 900, 300),
                day("2026-07-12", 3, 400, 100),
                day("not-a-date", 9, 9, 9),
                day("2026-10-10", 1, 1, 1),
            ],
            ..StatsStore::default()
        };
        let history = usage_history_from(&store, today()).expect("history");
        let dates: Vec<_> = history.days.iter().map(|day| day.date.as_str()).collect();
        // Outside the 90 days, empty, malformed and far-future days are dropped.
        assert_eq!(dates, ["2026-07-12", "2026-10-08"]);
        assert_eq!(history.days[1].original_tokens, 900);
        assert_eq!(history.days[1].delivered_tokens, 300);
        assert_eq!(history.lifetime.commands, 120);
        assert_eq!(history.lifetime.first_use_month.as_deref(), Some("2026-03"));
        let event = crate::core::telemetry_v2::TelemetryEventV2::UsageHistory(history);
        let wire = serde_json::to_string(&event).unwrap();
        assert!(
            wire.starts_with(r#"{"name":"usage_history","metrics":{"days":["#),
            "{wire}"
        );
        assert!(!wire.contains("T09"), "only the month of first use is sent");
    }

    #[test]
    fn unused_installation_sends_no_history() {
        assert!(usage_history_from(&StatsStore::default(), today()).is_none());
    }
}
