//! `GET /api/admin/timeseries` (enterprise#46) — per-day usage/savings series
//! for the admin dashboard's trend charts.
//!
//! Same window semantics as the usage breakdown (`admin_api::resolve_window`
//! rules): RFC-3339 `from`/`to`, defaulting to the last 30 days. Buckets are
//! UTC days (`date_trunc('day', ts)`); empty days are filled in so charts get
//! a gapless series — an empty day is a real "0", not missing data.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use deadpool_postgres::Pool;
use serde::{Deserialize, Serialize};

use super::admin_api::{AdminState, UsageQuery, resolve_window};

/// One UTC-day bucket of the series.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TimeseriesPoint {
    /// UTC day, `YYYY-MM-DD`.
    pub day: String,
    pub requests: i64,
    pub cost_usd: f64,
    pub saved_usd: f64,
    pub reference_cost_usd: f64,
}

/// Response of `GET /api/admin/timeseries`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TimeseriesResponse {
    pub from: String,
    pub to: String,
    pub points: Vec<TimeseriesPoint>,
}

/// Deterministic per-day rollup; bounds are bound parameters (injection-free).
const TIMESERIES_SQL: &str = "
SELECT date_trunc('day', ts)              AS day,
       count(*)                           AS requests,
       coalesce(sum(cost_usd), 0)         AS cost_usd,
       coalesce(sum(saved_usd), 0)        AS saved_usd,
       coalesce(sum(reference_cost_usd), 0) AS reference_cost_usd
FROM usage_events
WHERE ts >= $1 AND ts <= $2
GROUP BY 1
ORDER BY 1";

pub(super) async fn get_timeseries(
    State(state): State<Arc<AdminState>>,
    Query(q): Query<UsageQuery>,
) -> Response {
    let (from, to) = match resolve_window(q.from.as_deref(), q.to.as_deref()) {
        Ok(w) => w,
        Err(msg) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": msg})),
            )
                .into_response();
        }
    };
    match timeseries(&state.pool, from, to).await {
        Ok(resp) => Json(resp).into_response(),
        Err(e) => {
            tracing::warn!("admin timeseries query failed: {e:#}");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error": "usage store unavailable"})),
            )
                .into_response()
        }
    }
}

/// Runs the rollup and fills day gaps with zero points.
///
/// # Errors
/// Propagates pool/query errors (the handler maps them to 503).
pub async fn timeseries(
    pool: &Pool,
    from: chrono::DateTime<chrono::Utc>,
    to: chrono::DateTime<chrono::Utc>,
) -> anyhow::Result<TimeseriesResponse> {
    let client = pool.get().await?;
    let rows = client.query(TIMESERIES_SQL, &[&from, &to]).await?;
    let measured: Vec<TimeseriesPoint> = rows
        .iter()
        .map(|r| {
            let day: chrono::DateTime<chrono::Utc> = r.get("day");
            TimeseriesPoint {
                day: day.format("%Y-%m-%d").to_string(),
                requests: r.get("requests"),
                cost_usd: r.get("cost_usd"),
                saved_usd: r.get("saved_usd"),
                reference_cost_usd: r.get("reference_cost_usd"),
            }
        })
        .collect();
    Ok(TimeseriesResponse {
        from: from.to_rfc3339(),
        to: to.to_rfc3339(),
        points: fill_gaps(&measured, from, to),
    })
}

/// Produces one point per UTC day in `[from, to]`, taking measured values
/// where present and zeros elsewhere. Pure (unit-tested).
fn fill_gaps(
    measured: &[TimeseriesPoint],
    from: chrono::DateTime<chrono::Utc>,
    to: chrono::DateTime<chrono::Utc>,
) -> Vec<TimeseriesPoint> {
    let mut by_day: std::collections::BTreeMap<String, &TimeseriesPoint> =
        measured.iter().map(|p| (p.day.clone(), p)).collect();
    let mut out = Vec::new();
    let mut day = from.date_naive();
    let last = to.date_naive();
    while day <= last {
        let key = day.format("%Y-%m-%d").to_string();
        out.push(by_day.remove(&key).cloned().unwrap_or(TimeseriesPoint {
            day: key,
            requests: 0,
            cost_usd: 0.0,
            saved_usd: 0.0,
            reference_cost_usd: 0.0,
        }));
        day = day.succ_opt().expect("date within chrono range");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(s)
            .expect("test timestamp")
            .with_timezone(&chrono::Utc)
    }

    #[test]
    fn gaps_are_filled_with_zero_days() {
        let measured = vec![
            TimeseriesPoint {
                day: "2026-07-01".into(),
                requests: 5,
                cost_usd: 1.0,
                saved_usd: 0.5,
                reference_cost_usd: 2.0,
            },
            TimeseriesPoint {
                day: "2026-07-03".into(),
                requests: 2,
                cost_usd: 0.4,
                saved_usd: 0.1,
                reference_cost_usd: 0.9,
            },
        ];
        let filled = fill_gaps(
            &measured,
            ts("2026-07-01T08:00:00Z"),
            ts("2026-07-04T02:00:00Z"),
        );
        let days: Vec<&str> = filled.iter().map(|p| p.day.as_str()).collect();
        assert_eq!(
            days,
            ["2026-07-01", "2026-07-02", "2026-07-03", "2026-07-04"]
        );
        assert_eq!(filled[0].requests, 5);
        assert_eq!(filled[1].requests, 0, "gap day is an explicit zero");
        assert_eq!(filled[2].requests, 2);
        assert_eq!(filled[3].requests, 0);
    }

    #[test]
    fn single_day_window_yields_one_point() {
        let filled = fill_gaps(&[], ts("2026-07-02T00:00:00Z"), ts("2026-07-02T23:59:59Z"));
        assert_eq!(filled.len(), 1);
        assert_eq!(filled[0].day, "2026-07-02");
        assert_eq!(filled[0].requests, 0);
    }

    #[test]
    fn response_shape_round_trips() {
        let resp = TimeseriesResponse {
            from: "2026-07-01T00:00:00+00:00".into(),
            to: "2026-07-02T00:00:00+00:00".into(),
            points: vec![TimeseriesPoint {
                day: "2026-07-01".into(),
                requests: 10,
                cost_usd: 3.2,
                saved_usd: 1.1,
                reference_cost_usd: 5.0,
            }],
        };
        let json = serde_json::to_value(&resp).expect("serializes");
        let parsed: TimeseriesResponse = serde_json::from_value(json).expect("round-trips");
        assert_eq!(parsed, resp);
    }
}
