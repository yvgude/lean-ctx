//! Personal usage view (`/me`, enterprise#64) — served on the **proxy port**,
//! authenticated by the caller's own gateway key.
//!
//! The admin console (enterprise#45) answers "what does the org spend?"; this
//! surface answers "what did *I* spend and save?". It reuses the same design
//! language and the same `usage_events` store, but every query is scoped to
//! the person resolved from the presented key — nobody sees anybody else's
//! rows, and an org-wide token (no person identity) is refused.
//!
//! Wiring: the proxy compiles this router in under the `gateway-server`
//! feature and mounts it inside its auth middleware. `gateway serve` installs
//! the Postgres pool into [`install_pool`] before the proxy starts; without a
//! store (plain `lean-ctx proxy`) the data endpoint answers 503 and the shell
//! explains what is missing. Fail-open rule untouched: this is a read-only
//! periphery, LLM traffic never depends on it.
//!
//! Auth split (same as the admin console): the static shell is public — every
//! number comes from `GET /api/me/usage`, which sits behind the proxy's
//! Bearer guard and reads the identity tags the guard attached. The key never
//! appears in a URL; the shell keeps it in `sessionStorage`.

use std::sync::OnceLock;

use axum::extract::Query;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Json, Response};
use deadpool_postgres::Pool;
use serde::{Deserialize, Serialize};

use crate::proxy::gateway_identity::GatewayTags;

use super::admin_timeseries::{TimeseriesPoint, fill_gaps};

static USER_POOL: OnceLock<Pool> = OnceLock::new();

/// Installs the process-wide store pool for the personal view. First caller
/// wins (one gateway run-mode per process); later calls return `false`.
pub fn install_pool(pool: Pool) -> bool {
    USER_POOL.set(pool).is_ok()
}

/// Default and maximum query window in days.
const DEFAULT_WINDOW_DAYS: u32 = 30;
const MAX_WINDOW_DAYS: u32 = 365;

// -- Static shell ------------------------------------------------------------

const ME_HTML: &str = include_str!("static/me.html");
const ME_CSS: &str = include_str!("static/me.css");
const ME_JS: &str = include_str!("static/me.js");
/// Shared with the admin console: identical design tokens and components.
const BASE_CSS: &str = include_str!("static/admin.css");
/// Font faces with `/me/static/...` URLs (the proxy port serves no `/static/`).
const ME_FONTS_CSS: &str = include_str!("static/me-fonts.css");
const FONT_INTER_WOFF2: &[u8] = include_bytes!("../dashboard/static/fonts/inter-variable.woff2");
const FONT_JETBRAINS_WOFF2: &[u8] =
    include_bytes!("../dashboard/static/fonts/jetbrains-mono-variable.woff2");
const FONT_SPACE_GROTESK_WOFF2: &[u8] =
    include_bytes!("../dashboard/static/fonts/space-grotesk-variable.woff2");
const VENDOR_CHART_JS: &str = include_str!("../dashboard/static/vendor/chart.umd.min.js");

/// True for the unauthenticated shell paths (`/me` + its static assets). The
/// proxy's auth guard exempts exactly these — the data API stays guarded.
#[must_use]
pub fn is_shell_path(path: &str) -> bool {
    path == "/me" || path.starts_with("/me/static/")
}

/// The personal-view router: static shell + the guarded data endpoint.
/// State-generic so the proxy can merge it regardless of its own state type;
/// no handler here reads router state.
pub fn router<S: Clone + Send + Sync + 'static>() -> axum::Router<S> {
    axum::Router::new()
        .route("/me", axum::routing::get(shell))
        .route("/me/static/base.css", axum::routing::get(base_css))
        .route("/me/static/me.css", axum::routing::get(me_css))
        .route("/me/static/me.js", axum::routing::get(me_js))
        .route("/me/static/fonts/fonts.css", axum::routing::get(fonts_css))
        .route(
            "/me/static/vendor/chart.umd.min.js",
            axum::routing::get(chart_js),
        )
        .route("/me/static/fonts/{file}", axum::routing::get(font_file))
        .route("/api/me/usage", axum::routing::get(me_usage))
        .layer(axum::middleware::from_fn(super::security::security_headers))
}

async fn shell() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        ME_HTML,
    )
}

async fn base_css() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        BASE_CSS,
    )
}

async fn me_css() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], ME_CSS)
}

async fn me_js() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        ME_JS,
    )
}

async fn fonts_css() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        ME_FONTS_CSS,
    )
}

async fn chart_js() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        VENDOR_CHART_JS,
    )
}

async fn font_file(axum::extract::Path(file): axum::extract::Path<String>) -> Response {
    let bytes: &'static [u8] = match file.as_str() {
        "inter-variable.woff2" => FONT_INTER_WOFF2,
        "jetbrains-mono-variable.woff2" => FONT_JETBRAINS_WOFF2,
        "space-grotesk-variable.woff2" => FONT_SPACE_GROTESK_WOFF2,
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    ([(header::CONTENT_TYPE, "font/woff2")], bytes).into_response()
}

// -- Data endpoint -----------------------------------------------------------

/// Query parameters of `GET /api/me/usage`.
#[derive(Debug, Clone, Deserialize)]
pub struct MeQuery {
    /// Window length in days (default 30, clamped to `1..=365`).
    pub days: Option<u32>,
}

/// One aggregated model row of the personal breakdown.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MeModelRow {
    pub model: String,
    pub provider: String,
    pub requests: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cost_usd: f64,
    pub saved_usd: f64,
}

/// One aggregated project row of the personal breakdown.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MeProjectRow {
    pub project: String,
    pub requests: i64,
    pub cost_usd: f64,
    pub saved_usd: f64,
}

/// One aggregated MCP tool row of the personal breakdown (GL#104): the tools
/// this person called through `/mcp/{server}` and what that context costs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MeToolRow {
    pub server_id: String,
    pub tool: String,
    pub calls: i64,
    pub result_tokens: i64,
    pub context_cost_usd: f64,
}

/// Personal aggregate totals over the queried window.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MeTotals {
    pub requests: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cost_usd: f64,
    pub saved_tokens: i64,
    pub saved_usd: f64,
    /// Avoided-cost reference sum (0.0 without a configured baseline).
    pub reference_cost_usd: f64,
    /// Requests the active router rewrote (`routed_from IS NOT NULL`).
    pub routed_requests: i64,
}

/// Response of `GET /api/me/usage`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MeUsageResponse {
    /// The person this data belongs to (pseudonymized when GDPR mode is on —
    /// the same form the store holds, so what you see is what is stored).
    pub person: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub org_label: Option<String>,
    pub version: String,
    pub from: String,
    pub to: String,
    pub totals: MeTotals,
    pub by_model: Vec<MeModelRow>,
    pub by_project: Vec<MeProjectRow>,
    /// MCP tools this person used (empty without MCP traffic — the shell
    /// hides the section entirely then).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<MeToolRow>,
    pub days: Vec<TimeseriesPoint>,
}

/// Person-scoped totals. Window bounds and person are bound parameters;
/// everything else is static SQL (deterministic, injection-free).
const ME_TOTALS_SQL: &str = "
SELECT count(*)                                            AS requests,
       coalesce(sum(input_tokens), 0)::BIGINT              AS input_tokens,
       coalesce(sum(output_tokens), 0)::BIGINT             AS output_tokens,
       coalesce(sum(cost_usd), 0)                          AS cost_usd,
       coalesce(sum(saved_tokens), 0)::BIGINT              AS saved_tokens,
       coalesce(sum(saved_usd), 0)                         AS saved_usd,
       coalesce(sum(reference_cost_usd), 0)                AS reference_cost_usd,
       count(*) FILTER (WHERE routed_from IS NOT NULL)     AS routed_requests
FROM usage_events
WHERE ts >= $1 AND ts <= $2 AND person = $3";

const ME_BY_MODEL_SQL: &str = "
SELECT model, provider,
       count(*)                   AS requests,
       sum(input_tokens)::BIGINT  AS input_tokens,
       sum(output_tokens)::BIGINT AS output_tokens,
       sum(cost_usd)              AS cost_usd,
       sum(saved_usd)             AS saved_usd
FROM usage_events
WHERE ts >= $1 AND ts <= $2 AND person = $3
GROUP BY model, provider
ORDER BY cost_usd DESC";

const ME_BY_PROJECT_SQL: &str = "
SELECT project,
       count(*)      AS requests,
       sum(cost_usd) AS cost_usd,
       sum(saved_usd) AS saved_usd
FROM usage_events
WHERE ts >= $1 AND ts <= $2 AND person = $3
GROUP BY project
ORDER BY cost_usd DESC";

const ME_TIMESERIES_SQL: &str = "
SELECT date_trunc('day', ts)                AS day,
       count(*)                             AS requests,
       coalesce(sum(cost_usd), 0)           AS cost_usd,
       coalesce(sum(saved_usd), 0)          AS saved_usd,
       coalesce(sum(reference_cost_usd), 0) AS reference_cost_usd
FROM usage_events
WHERE ts >= $1 AND ts <= $2 AND person = $3
GROUP BY 1
ORDER BY 1";

/// `GET /api/me/usage?days=N` — the caller's own usage, keyed by the identity
/// the proxy auth guard attached. Refuses tokens without a person identity:
/// the personal view exists exactly for per-person keys (enterprise#11).
async fn me_usage(
    tags: Option<axum::Extension<GatewayTags>>,
    Query(q): Query<MeQuery>,
) -> Response {
    let Some(person) = tags.as_ref().and_then(|t| t.person.clone()) else {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "error": "this view needs a personal gateway key (it identifies you); \
                          ask your admin for one: lean-ctx gateway keys add --person <you>"
            })),
        )
            .into_response();
    };
    let Some(pool) = USER_POOL.get() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": "usage store not configured on this gateway (DATABASE_URL unset)"
            })),
        )
            .into_response();
    };
    let team = tags.and_then(|t| t.0.team);
    let days = q
        .days
        .unwrap_or(DEFAULT_WINDOW_DAYS)
        .clamp(1, MAX_WINDOW_DAYS);
    let to = chrono::Utc::now();
    let from = to - chrono::Duration::days(i64::from(days));

    match personal_usage(pool, &person, team, from, to).await {
        Ok(resp) => Json(resp).into_response(),
        Err(e) => {
            tracing::warn!("personal usage query failed: {e:#}");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error": "usage store unavailable"})),
            )
                .into_response()
        }
    }
}

/// Runs the person-scoped queries and assembles the response.
///
/// # Errors
/// Propagates pool/query errors (the handler maps them to 503).
pub async fn personal_usage(
    pool: &Pool,
    person: &str,
    team: Option<String>,
    from: chrono::DateTime<chrono::Utc>,
    to: chrono::DateTime<chrono::Utc>,
) -> anyhow::Result<MeUsageResponse> {
    let client = pool.get().await?;

    let t = client
        .query_one(ME_TOTALS_SQL, &[&from, &to, &person])
        .await?;
    let totals = MeTotals {
        requests: t.get("requests"),
        input_tokens: t.get("input_tokens"),
        output_tokens: t.get("output_tokens"),
        cost_usd: t.get("cost_usd"),
        saved_tokens: t.get("saved_tokens"),
        saved_usd: t.get("saved_usd"),
        reference_cost_usd: t.get("reference_cost_usd"),
        routed_requests: t.get("routed_requests"),
    };

    let by_model = client
        .query(ME_BY_MODEL_SQL, &[&from, &to, &person])
        .await?
        .iter()
        .map(|r| MeModelRow {
            model: r.get("model"),
            provider: r.get("provider"),
            requests: r.get("requests"),
            input_tokens: r.get("input_tokens"),
            output_tokens: r.get("output_tokens"),
            cost_usd: r.get("cost_usd"),
            saved_usd: r.get("saved_usd"),
        })
        .collect();

    let by_project = client
        .query(ME_BY_PROJECT_SQL, &[&from, &to, &person])
        .await?
        .iter()
        .map(|r| MeProjectRow {
            project: r.get("project"),
            requests: r.get("requests"),
            cost_usd: r.get("cost_usd"),
            saved_usd: r.get("saved_usd"),
        })
        .collect();

    // MCP tool usage (GL#104). A gateway that never served MCP traffic has no
    // mcp_events table — that is an empty section, not an error.
    let tools = client
        .query(super::mcp::store::ME_TOOLS_SQL, &[&from, &to, &person])
        .await
        .map(|rows| {
            rows.iter()
                .map(|r| MeToolRow {
                    server_id: r.get("server_id"),
                    tool: r.get("tool"),
                    calls: r.get("calls"),
                    result_tokens: r.get("result_tokens"),
                    context_cost_usd: r.get("context_cost_usd"),
                })
                .collect()
        })
        .unwrap_or_default();

    let measured: Vec<TimeseriesPoint> = client
        .query(ME_TIMESERIES_SQL, &[&from, &to, &person])
        .await?
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

    let cfg = crate::core::config::Config::load();
    Ok(MeUsageResponse {
        person: person.to_string(),
        team,
        org_label: cfg.gateway_server.org_label.clone(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        from: from.to_rfc3339(),
        to: to.to_rfc3339(),
        totals,
        by_model,
        by_project,
        tools,
        days: fill_gaps(&measured, from, to),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_paths_cover_exactly_the_public_surface() {
        assert!(is_shell_path("/me"));
        assert!(is_shell_path("/me/static/me.js"));
        assert!(is_shell_path("/me/static/fonts/inter-variable.woff2"));
        // The data API and everything else stay guarded.
        assert!(!is_shell_path("/api/me/usage"));
        assert!(!is_shell_path("/me2"));
        assert!(!is_shell_path("/mex/static/a.js"));
        assert!(!is_shell_path("/v1/messages"));
    }

    #[test]
    fn embedded_assets_are_nonempty_and_wired() {
        assert!(ME_HTML.contains("<!doctype html"));
        assert!(
            ME_HTML.contains("/me/static/me.js"),
            "shell must load the app script"
        );
        assert!(
            ME_HTML.contains("/me/static/base.css"),
            "shell must reuse the console design system"
        );
        assert!(
            ME_JS.contains("/api/me/usage"),
            "app must talk to the guarded API"
        );
        assert!(
            ME_FONTS_CSS.contains("/me/static/fonts/"),
            "font faces must resolve on the proxy port"
        );
        assert!(!ME_CSS.is_empty());
        assert!(!VENDOR_CHART_JS.is_empty());
    }

    #[test]
    fn shell_never_embeds_credentials() {
        for needle in ["Bearer ", "gk-", "LEAN_CTX_PROXY_TOKEN="] {
            assert!(!ME_HTML.contains(needle), "me.html must not embed {needle}");
        }
        assert!(
            !ME_JS.contains("localStorage.setItem('leanctx-me-key'"),
            "key must live in sessionStorage, not persist in localStorage"
        );
    }

    #[test]
    fn window_days_are_clamped() {
        for (input, expected) in [
            (None, DEFAULT_WINDOW_DAYS),
            (Some(0), 1),
            (Some(7), 7),
            (Some(9999), MAX_WINDOW_DAYS),
        ] {
            let days = input
                .unwrap_or(DEFAULT_WINDOW_DAYS)
                .clamp(1, MAX_WINDOW_DAYS);
            assert_eq!(days, expected, "input {input:?}");
        }
    }

    #[test]
    fn response_shape_round_trips() {
        // The response is a client contract for the /me shell — pin it.
        let resp = MeUsageResponse {
            person: "alice@zuehlke.com".into(),
            team: Some("platform".into()),
            org_label: Some("Zühlke Engineering AG".into()),
            version: "3.8.18".into(),
            from: "2026-06-03T00:00:00+00:00".into(),
            to: "2026-07-03T00:00:00+00:00".into(),
            totals: MeTotals {
                requests: 412,
                input_tokens: 9_000_000,
                output_tokens: 310_000,
                cost_usd: 84.12,
                saved_tokens: 2_400_000,
                saved_usd: 41.90,
                reference_cost_usd: 190.55,
                routed_requests: 96,
            },
            by_model: vec![MeModelRow {
                model: "zuehlke/fast".into(),
                provider: "foundry".into(),
                requests: 96,
                input_tokens: 1_000_000,
                output_tokens: 50_000,
                cost_usd: 4.20,
                saved_usd: 12.80,
            }],
            by_project: vec![MeProjectRow {
                project: "checkout".into(),
                requests: 412,
                cost_usd: 84.12,
                saved_usd: 41.90,
            }],
            tools: vec![MeToolRow {
                server_id: "github".into(),
                tool: "get_issue".into(),
                calls: 31,
                result_tokens: 128_000,
                context_cost_usd: 0.32,
            }],
            days: vec![],
        };
        let json = serde_json::to_value(&resp).expect("serializes");
        assert_eq!(json["person"], "alice@zuehlke.com");
        assert_eq!(json["totals"]["routed_requests"], 96);
        assert_eq!(json["by_model"][0]["model"], "zuehlke/fast");
        let parsed: MeUsageResponse = serde_json::from_value(json).expect("round-trips");
        assert_eq!(parsed, resp);
    }

    #[tokio::test]
    async fn me_usage_refuses_identityless_tokens() {
        // Org token (no person) → 403; absent tags → 403.
        for tags in [
            None,
            Some(axum::Extension(GatewayTags::default())),
            Some(axum::Extension(GatewayTags {
                person: None,
                team: None,
                project: Some("side-quest".into()),
            })),
        ] {
            let resp = me_usage(tags, Query(MeQuery { days: Some(7) })).await;
            assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        }
    }

    #[tokio::test]
    async fn me_usage_without_store_is_503() {
        // A personal key but no installed pool (plain proxy mode) → 503 with
        // a actionable error, never a panic. (No pool is installed in unit
        // tests — OnceLock stays empty.)
        let tags = Some(axum::Extension(GatewayTags {
            person: Some("alice".into()),
            team: None,
            project: None,
        }));
        let resp = me_usage(tags, Query(MeQuery { days: None })).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
