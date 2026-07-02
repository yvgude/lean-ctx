//! Admin usage API (enterprise#20) — the self-hosted gateway's spend/savings
//! breakdown, straight from `usage_events` (Doc 08 §3.3).
//!
//! `GET /api/admin/usage?from=<ISO>&to=<ISO>` returns the person × project ×
//! model × provider cross-join with per-group token/cost/savings sums, plus
//! totals and the seat projection. Runs in the **self-hosted `gateway-server`
//! (OSS, local Postgres)** — "seeing your own instance" is local-free; the
//! multi-tenant managed console is a separate commercial surface.
//!
//! Auth: the router is mounted behind the gateway's Bearer middleware by
//! `gateway serve` (enterprise#10); this module contains no credential logic.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use deadpool_postgres::Pool;
use serde::{Deserialize, Serialize};

/// Days in the projection's reference month. The projection is an
/// *extrapolation for planning*, clearly labeled — not a billing number.
const PROJECTION_MONTH_DAYS: f64 = 30.0;

/// One aggregated row of the person × project × model × provider cross-join.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UsageBreakdownRow {
    pub person: String,
    pub project: String,
    pub model: String,
    pub provider: String,
    pub requests: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cost_usd: f64,
    pub saved_tokens: i64,
    pub saved_usd: f64,
}

/// Aggregate totals + the seat projection over the queried window.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UsageTotals {
    pub requests: i64,
    pub cost_usd: f64,
    pub saved_usd: f64,
    /// Reference (avoided-cost) sum for the window, when a `reference_model`
    /// is configured (enterprise#15/#18); 0.0 otherwise.
    pub reference_cost_usd: f64,
    /// Distinct persons with ≥1 event in the window — the projection divisor.
    pub active_persons: i64,
    /// `saved_usd / active_persons × seats`, scaled to a 30-day month
    /// (enterprise#20, Doc 04): "if every configured seat saved like the
    /// currently active users, this is the monthly org-wide savings".
    /// `None` when no seats are configured or nothing is active — the
    /// cockpit never invents a projection.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub projection_seats: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub projection_usd_per_month: Option<f64>,
}

/// Response of `GET /api/admin/usage`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UsageBreakdownResponse {
    pub from: String,
    pub to: String,
    pub rows: Vec<UsageBreakdownRow>,
    pub totals: UsageTotals,
}

/// Query parameters: ISO-8601 `from`/`to` (defaults: last 30 days up to now).
#[derive(Debug, Clone, Deserialize)]
pub struct UsageQuery {
    pub from: Option<String>,
    pub to: Option<String>,
}

/// Shared state of the admin router: the store pool + deployment parameters
/// (the config/identity slice the status card and dashboard need).
#[derive(Clone)]
pub struct AdminState {
    pub pool: Pool,
    /// Seats for the projection (`[gateway_server].seats`).
    pub seats: Option<u32>,
    /// `[gateway_server].org_label` — branding for the dashboard header.
    pub org_label: Option<String>,
    /// Process start, for the status card's uptime.
    pub started_at: std::time::Instant,
    /// Resolved provider registry snapshot (id/shape/credential presence).
    pub providers: Vec<super::admin_status::ProviderStatus>,
    /// `[proxy.routing].enabled`.
    pub routing_enabled: bool,
    /// `[proxy.baseline].reference_model`.
    pub reference_model: Option<String>,
    /// Effective local shadow rate (USD per MTok).
    pub local_shadow_rate: f64,
}

/// Builds the admin API router. Mounted behind Bearer auth by `gateway serve`.
pub fn router(state: AdminState) -> axum::Router {
    axum::Router::new()
        .route("/api/admin/usage", axum::routing::get(get_usage))
        .route(
            "/api/admin/timeseries",
            axum::routing::get(super::admin_timeseries::get_timeseries),
        )
        .route(
            "/api/admin/status",
            axum::routing::get(super::admin_status::get_status),
        )
        .with_state(Arc::new(state))
}

/// The GROUP BY over `usage_events` (Doc 08 §3.3). Window bounds are bound
/// parameters; everything else is static SQL (deterministic, injection-free).
const USAGE_BREAKDOWN_SQL: &str = "
SELECT person, project, model, provider,
       count(*)                    AS requests,
       sum(input_tokens)::BIGINT   AS input_tokens,
       sum(output_tokens)::BIGINT  AS output_tokens,
       sum(cost_usd)               AS cost_usd,
       sum(saved_tokens)::BIGINT   AS saved_tokens,
       sum(saved_usd)              AS saved_usd
FROM usage_events
WHERE ts >= $1 AND ts <= $2
GROUP BY person, project, model, provider
ORDER BY cost_usd DESC";

const USAGE_TOTALS_SQL: &str = "
SELECT count(*)                     AS requests,
       coalesce(sum(cost_usd), 0)   AS cost_usd,
       coalesce(sum(saved_usd), 0)  AS saved_usd,
       coalesce(sum(reference_cost_usd), 0) AS reference_cost_usd,
       count(DISTINCT person)       AS active_persons
FROM usage_events
WHERE ts >= $1 AND ts <= $2";

async fn get_usage(State(state): State<Arc<AdminState>>, Query(q): Query<UsageQuery>) -> Response {
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

    match usage_breakdown(&state.pool, from, to, state.seats).await {
        Ok(resp) => Json(resp).into_response(),
        Err(e) => {
            tracing::warn!("admin usage query failed: {e:#}");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error": "usage store unavailable"})),
            )
                .into_response()
        }
    }
}

/// Parses the window, defaulting to the last 30 days ending now. Rejects an
/// inverted window instead of silently returning an empty result.
pub(super) fn resolve_window(
    from: Option<&str>,
    to: Option<&str>,
) -> Result<(chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>), String> {
    let parse = |s: &str, which: &str| {
        chrono::DateTime::parse_from_rfc3339(s)
            .map(|d| d.with_timezone(&chrono::Utc))
            .map_err(|e| format!("invalid `{which}` timestamp (RFC 3339 expected): {e}"))
    };
    let to_ts = match to {
        Some(s) => parse(s, "to")?,
        None => chrono::Utc::now(),
    };
    let from_ts = match from {
        Some(s) => parse(s, "from")?,
        None => to_ts - chrono::Duration::days(30),
    };
    if from_ts > to_ts {
        return Err("`from` must not be after `to`".into());
    }
    Ok((from_ts, to_ts))
}

/// Runs the cross-join + totals queries and assembles the response.
///
/// # Errors
/// Propagates pool/query errors (the handler maps them to 503).
pub async fn usage_breakdown(
    pool: &Pool,
    from: chrono::DateTime<chrono::Utc>,
    to: chrono::DateTime<chrono::Utc>,
    seats: Option<u32>,
) -> anyhow::Result<UsageBreakdownResponse> {
    let client = pool.get().await?;

    let rows = client
        .query(USAGE_BREAKDOWN_SQL, &[&from, &to])
        .await?
        .iter()
        .map(|r| UsageBreakdownRow {
            person: r.get("person"),
            project: r.get("project"),
            model: r.get("model"),
            provider: r.get("provider"),
            requests: r.get("requests"),
            input_tokens: r.get("input_tokens"),
            output_tokens: r.get("output_tokens"),
            cost_usd: r.get("cost_usd"),
            saved_tokens: r.get("saved_tokens"),
            saved_usd: r.get("saved_usd"),
        })
        .collect();

    let t = client.query_one(USAGE_TOTALS_SQL, &[&from, &to]).await?;
    let totals = build_totals(
        t.get("requests"),
        t.get("cost_usd"),
        t.get("saved_usd"),
        t.get("reference_cost_usd"),
        t.get("active_persons"),
        seats,
        to - from,
    );

    Ok(UsageBreakdownResponse {
        from: from.to_rfc3339(),
        to: to.to_rfc3339(),
        rows,
        totals,
    })
}

/// Pure projection math (unit-tested): per-active-person savings × seats,
/// normalized from the window length to a 30-day month.
fn build_totals(
    requests: i64,
    cost_usd: f64,
    saved_usd: f64,
    reference_cost_usd: f64,
    active_persons: i64,
    seats: Option<u32>,
    window: chrono::Duration,
) -> UsageTotals {
    let window_days = window.num_seconds() as f64 / 86_400.0;
    let projection = seats
        .filter(|_| active_persons > 0 && window_days > 0.0)
        .map(|s| {
            #[allow(clippy::cast_precision_loss)]
            let per_person_per_month =
                saved_usd / active_persons as f64 / window_days * PROJECTION_MONTH_DAYS;
            per_person_per_month * f64::from(s)
        });
    UsageTotals {
        requests,
        cost_usd,
        saved_usd,
        reference_cost_usd,
        active_persons,
        projection_seats: seats.filter(|_| projection.is_some()),
        projection_usd_per_month: projection,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projection_scales_per_person_savings_to_seats_and_month() {
        // 10 active persons saved $500 over a 15-day window → $100/person/month;
        // 800 seats → $80k/month.
        let t = build_totals(
            1_000,
            2_000.0,
            500.0,
            3_000.0,
            10,
            Some(800),
            chrono::Duration::days(15),
        );
        assert_eq!(t.projection_seats, Some(800));
        let p = t.projection_usd_per_month.expect("projection");
        assert!((p - 80_000.0).abs() < 1e-6, "got {p}");
    }

    #[test]
    fn projection_absent_without_seats_or_activity() {
        // No seats configured → no projection, ever.
        let t = build_totals(10, 1.0, 1.0, 0.0, 5, None, chrono::Duration::days(30));
        assert_eq!(t.projection_usd_per_month, None);
        assert_eq!(t.projection_seats, None);
        // Seats configured but zero active persons → nothing to extrapolate from.
        let t = build_totals(0, 0.0, 0.0, 0.0, 0, Some(800), chrono::Duration::days(30));
        assert_eq!(t.projection_usd_per_month, None);
        assert_eq!(t.projection_seats, None, "seats hidden when unusable");
    }

    #[test]
    fn window_defaults_and_validation() {
        let (from, to) = resolve_window(None, None).expect("default window");
        assert!((to - from).num_days() == 30);

        let (from, to) = resolve_window(Some("2026-07-01T00:00:00Z"), Some("2026-07-31T23:59:59Z"))
            .expect("explicit window");
        assert_eq!(from.to_rfc3339(), "2026-07-01T00:00:00+00:00");
        assert!(to > from);

        assert!(resolve_window(Some("not-a-date"), None).is_err());
        assert!(
            resolve_window(Some("2026-08-01T00:00:00Z"), Some("2026-07-01T00:00:00Z")).is_err(),
            "inverted window must be rejected"
        );
    }

    #[test]
    fn response_serializes_stably() {
        // The response shape is a client contract (Doc 08 §3.3) — pin it.
        let resp = UsageBreakdownResponse {
            from: "2026-07-01T00:00:00+00:00".into(),
            to: "2026-07-31T23:59:59+00:00".into(),
            rows: vec![UsageBreakdownRow {
                person: "alice@example.com".into(),
                project: "billing".into(),
                model: "claude-sonnet-4-5".into(),
                provider: "Anthropic".into(),
                requests: 1240,
                input_tokens: 9_000_000,
                output_tokens: 480_000,
                cost_usd: 312.40,
                saved_tokens: 3_100_000,
                saved_usd: 210.11,
            }],
            totals: build_totals(
                1240,
                312.40,
                210.11,
                522.51,
                1,
                Some(800),
                chrono::Duration::days(30),
            ),
        };
        let json = serde_json::to_value(&resp).expect("serializes");
        assert_eq!(json["rows"][0]["person"], "alice@example.com");
        assert_eq!(json["totals"]["active_persons"], 1);
        assert!(json["totals"]["projection_usd_per_month"].is_f64());
        let parsed: UsageBreakdownResponse = serde_json::from_value(json).expect("round-trips");
        assert_eq!(parsed, resp);
    }
}
