//! `GET /api/account/cloud` — the logged-in user's Personal Cloud dashboard.
//!
//! The website's Personal Cloud page reads this single endpoint to decide
//! between the upsell CTA and the live dashboard. It returns:
//! - `cloud_sync` — the entitlement gate (Pro/Team/Enterprise, or an open
//!   deployment) that flips the page from upsell to dashboard,
//! - `plan` — the resolved plan, for the badge + copy,
//! - `buckets` — a privacy-preserving footprint of what this account has synced
//!   (per-bucket row counts + last-synced timestamps),
//! - `buddy` — the synced buddy state, when present,
//! - `last_synced_at` — the most recent sync across every bucket,
//! - `usage` — the dashboard roll-up mirroring the team ROI card: all-time
//!   totals plus a gap-free daily series (cumulative tokens/actions and the
//!   day's mean CEP score, carried forward) built from synced CEP snapshots.
//!
//! The synced *content* never leaves the account; only its shape is surfaced.
//! A failing bucket query degrades to an empty bucket rather than failing the
//! whole dashboard, and accounts without the entitlement get the upsell payload.

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde_json::{Map, Value, json};

use super::auth::{AppState, auth_user};
use super::billing_edge::{cloud_sync_allowed, resolve_plan};
use super::helpers::internal_error;

/// Synced buckets surfaced on the dashboard: `(json key, table, timestamp col)`.
/// All three are compile-time constants — never user input — so interpolating
/// the table/column into the aggregate query is injection-safe.
const BUCKETS: [(&str, &str, &str); 6] = [
    ("knowledge", "knowledge_entries", "updated_at"),
    ("commands", "command_stats", "updated_at"),
    ("cep", "cep_scores", "recorded_at"),
    ("gain", "gain_scores", "recorded_at"),
    ("gotchas", "gotchas", "updated_at"),
    ("feedback", "feedback_thresholds", "updated_at"),
];

pub(super) async fn get_account_cloud(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, String)> {
    let (user_id, _email) = auth_user(&state, &headers).await?;
    let plan = resolve_plan(&state.cfg, user_id).await;

    // No `cloud_sync` entitlement ⇒ the page renders the gated upsell. Still a
    // 200 with the plan so the CTA can tailor its copy.
    if !cloud_sync_allowed(&state.cfg, plan) {
        return Ok(Json(json!({ "cloud_sync": false, "plan": plan.as_str() })));
    }

    let client = state.pool.get().await.map_err(internal_error)?;

    let mut buckets = Map::new();
    let mut latest: Option<DateTime<Utc>> = None;
    for (key, table, ts_col) in BUCKETS {
        let sql = format!("SELECT COUNT(*)::bigint, MAX({ts_col}) FROM {table} WHERE user_id = $1");
        // A missing/locked bucket must never break the dashboard.
        let (mut count, mut last): (i64, Option<DateTime<Utc>>) =
            match client.query_one(&sql, &[&user_id]).await {
                Ok(row) => (row.get(0), row.get(1)),
                Err(_) => (0, None),
            };
        // E2E buckets (GL #467): once an account pushed a vault, its legacy
        // plaintext rows are purged — the count lives in the blob's
        // client-declared metadata instead.
        if count == 0
            && let Some(blob_table) = match key {
                "knowledge" => Some("knowledge_blobs"),
                "gotchas" => Some("gotcha_blobs"),
                _ => None,
            }
        {
            let sql =
                format!("SELECT entry_count, updated_at FROM {blob_table} WHERE user_id = $1");
            if let Ok(Some(row)) = client.query_opt(&sql, &[&user_id]).await {
                count = row.get(0);
                last = row.get(1);
            }
        }
        merge_latest(&mut latest, last);
        buckets.insert(
            key.to_string(),
            json!({ "count": count, "last_synced_at": last.map(|t| t.to_rfc3339()) }),
        );
    }

    let buddy = match client
        .query_opt(
            "SELECT name, species, level, xp, mood, streak, rarity, updated_at \
             FROM buddy_state WHERE user_id = $1",
            &[&user_id],
        )
        .await
    {
        Ok(Some(r)) => {
            let last: Option<DateTime<Utc>> = r.get(7);
            merge_latest(&mut latest, last);
            json!({
                "present": true,
                "name": r.get::<_, Option<String>>(0),
                "species": r.get::<_, Option<String>>(1),
                "level": r.get::<_, i32>(2),
                "xp": r.get::<_, i64>(3),
                "mood": r.get::<_, Option<String>>(4),
                "streak": r.get::<_, i32>(5),
                "rarity": r.get::<_, Option<String>>(6),
                "last_synced_at": last.map(|t| t.to_rfc3339()),
            })
        }
        _ => json!({ "present": false }),
    };

    let usage = load_usage(&client, user_id).await;

    // Hosted Personal Index footprint (GL #392): bucket count + quota usage
    // for the dashboard's quota bar. Sizes only — content is ciphertext.
    let quota_mb = super::billing_edge::hosted_index_quota_mb(&state, user_id).await;
    let hosted_index = match client
        .query_one(
            "SELECT COUNT(*)::bigint, COALESCE(SUM(size_bytes),0)::bigint, MAX(updated_at) \
             FROM index_bundles WHERE user_id = $1",
            &[&user_id],
        )
        .await
    {
        Ok(r) => {
            let last: Option<DateTime<Utc>> = r.get(2);
            merge_latest(&mut latest, last);
            json!({
                "projects": r.get::<_, i64>(0),
                "used_bytes": r.get::<_, i64>(1),
                "quota_mb": quota_mb,
                "last_pushed_at": last.map(|t| t.to_rfc3339()),
            })
        }
        Err(_) => json!({ "projects": 0, "used_bytes": 0, "quota_mb": quota_mb }),
    };

    Ok(Json(json!({
        "cloud_sync": true,
        "plan": plan.as_str(),
        "last_synced_at": latest.map(|t| t.to_rfc3339()),
        "buckets": Value::Object(buckets),
        "buddy": buddy,
        "usage": usage,
        "hosted_index": hosted_index,
    })))
}

/// How far back the daily usage series reaches. Matches the team dashboard's
/// longest range toggle (90d) so both charts tell the same story.
const SERIES_DAYS: i64 = 90;

/// One aggregated day of synced CEP activity, before cumulation.
struct DayRow {
    day: NaiveDate,
    tokens: i64,
    calls: i64,
    score: f64,
}

/// Build the usage roll-up from this account's synced CEP snapshots: all-time
/// totals plus a gap-free daily series over the last [`SERIES_DAYS`] days.
/// Tokens/actions are cumulative (like the team ROI series); the CEP score is
/// the day's mean, carried forward over days without sessions. Returns a
/// payload with `available: false` when nothing has been synced yet — the page
/// then keeps its setup-focused empty state instead of a dead chart.
async fn load_usage(client: &deadpool_postgres::Client, user_id: uuid::Uuid) -> Value {
    let Ok(totals) = client
        .query_one(
            "SELECT COALESCE(SUM(tokens_saved),0)::bigint, \
                    COALESCE(SUM(tool_calls),0)::bigint, \
                    COUNT(*)::bigint \
             FROM cep_scores WHERE user_id = $1",
            &[&user_id],
        )
        .await
    else {
        return json!({ "available": false });
    };
    let (total_tokens, total_calls, snapshots): (i64, i64, i64) =
        (totals.get(0), totals.get(1), totals.get(2));
    if snapshots == 0 {
        return json!({ "available": false });
    }

    // Tokens/actions saved *before* the window seed the cumulative series, so
    // the chart's left edge starts at the account's real all-time level.
    let window_start = (Utc::now() - Duration::days(SERIES_DAYS)).date_naive();
    let (mut run_tokens, mut run_calls): (i64, i64) = match client
        .query_one(
            "SELECT COALESCE(SUM(tokens_saved),0)::bigint, \
                    COALESCE(SUM(tool_calls),0)::bigint \
             FROM cep_scores WHERE user_id = $1 AND recorded_at::date < $2",
            &[&user_id, &window_start],
        )
        .await
    {
        Ok(r) => (r.get(0), r.get(1)),
        Err(_) => (0, 0),
    };

    let days: Vec<DayRow> = match client
        .query(
            "SELECT recorded_at::date AS day, \
                    COALESCE(SUM(tokens_saved),0)::bigint, \
                    COALESCE(SUM(tool_calls),0)::bigint, \
                    AVG(score)::float8 \
             FROM cep_scores WHERE user_id = $1 AND recorded_at::date >= $2 \
             GROUP BY day ORDER BY day",
            &[&user_id, &window_start],
        )
        .await
    {
        Ok(rows) => rows
            .iter()
            .map(|r| DayRow {
                day: r.get(0),
                tokens: r.get(1),
                calls: r.get(2),
                score: r.get::<_, Option<f64>>(3).unwrap_or(0.0),
            })
            .collect(),
        Err(_) => Vec::new(),
    };

    // Last session's mean score (most recent day with data) for the KPI.
    let current_score = days.last().map(|d| d.score);

    let series = fill_series(&days, &mut run_tokens, &mut run_calls);

    json!({
        "available": true,
        "totals": {
            "tokens_saved": total_tokens,
            "agent_actions": total_calls,
            "sessions": snapshots,
            "cep_score": current_score,
        },
        "series": series,
    })
}

/// Expand sparse day rows into a gap-free cumulative series from the first
/// active day through today. Scores carry forward across silent days so the
/// line stays continuous; cumulative counters simply hold their level.
fn fill_series(days: &[DayRow], run_tokens: &mut i64, run_calls: &mut i64) -> Vec<Value> {
    let Some(first) = days.first() else {
        return Vec::new();
    };
    let today = Utc::now().date_naive();
    let mut by_day = std::collections::HashMap::new();
    for d in days {
        by_day.insert(d.day, d);
    }

    let mut out = Vec::new();
    let mut score = first.score;
    let mut cursor = first.day;
    while cursor <= today {
        if let Some(d) = by_day.get(&cursor) {
            *run_tokens += d.tokens;
            *run_calls += d.calls;
            score = d.score;
        }
        out.push(json!({
            "date": cursor.format("%Y-%m-%d").to_string(),
            "net_saved_tokens": *run_tokens,
            "total_events": *run_calls,
            "score": score,
        }));
        cursor += Duration::days(1);
    }
    out
}

/// Keep the most recent of the running maximum and a candidate timestamp.
fn merge_latest(latest: &mut Option<DateTime<Utc>>, candidate: Option<DateTime<Utc>>) {
    if let Some(ts) = candidate {
        *latest = Some(latest.map_or(ts, |cur| cur.max(ts)));
    }
}

#[cfg(test)]
mod tests {
    use super::{DayRow, fill_series, merge_latest};
    use chrono::{Duration, TimeZone, Utc};

    #[test]
    fn merge_latest_keeps_the_most_recent_non_null() {
        let mut latest = None;
        let seed = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let older = Utc.with_ymd_and_hms(2025, 6, 1, 0, 0, 0).unwrap();
        let newer = Utc.with_ymd_and_hms(2026, 6, 9, 0, 0, 0).unwrap();

        merge_latest(&mut latest, Some(seed)); // first value seeds the max
        assert_eq!(latest, Some(seed));
        merge_latest(&mut latest, None); // None never lowers the max
        assert_eq!(latest, Some(seed));
        merge_latest(&mut latest, Some(older)); // an older bucket does not win
        assert_eq!(latest, Some(seed));
        merge_latest(&mut latest, Some(newer)); // a newer bucket advances it
        assert_eq!(latest, Some(newer));
    }

    #[test]
    fn fill_series_is_gap_free_cumulative_and_carries_scores() {
        let today = Utc::now().date_naive();
        let d0 = today - Duration::days(4);
        let d2 = today - Duration::days(2);
        let days = vec![
            DayRow {
                day: d0,
                tokens: 100,
                calls: 10,
                score: 0.5,
            },
            DayRow {
                day: d2,
                tokens: 50,
                calls: 5,
                score: 0.7,
            },
        ];

        // 1000/100 synced before the window seed the running totals.
        let (mut tokens, mut calls) = (1000i64, 100i64);
        let series = fill_series(&days, &mut tokens, &mut calls);

        // d0..=today inclusive, no gaps.
        assert_eq!(series.len(), 5);
        assert_eq!(series[0]["date"], d0.format("%Y-%m-%d").to_string());

        // Day 0 adds onto the seed; the silent day 1 holds the level.
        assert_eq!(series[0]["net_saved_tokens"], 1100);
        assert_eq!(series[1]["net_saved_tokens"], 1100);
        assert_eq!(series[1]["score"], 0.5); // carried forward

        // Day 2 adds again and updates the score; trailing days hold it.
        assert_eq!(series[2]["net_saved_tokens"], 1150);
        assert_eq!(series[2]["total_events"], 115);
        assert_eq!(series[4]["score"], 0.7);
    }

    #[test]
    fn fill_series_handles_no_active_days() {
        let (mut tokens, mut calls) = (0i64, 0i64);
        assert!(fill_series(&[], &mut tokens, &mut calls).is_empty());
    }
}
