//! `GET /api/admin/mcp` — the console's window into the tool channel
//! (GL#104). GET-only like every admin endpoint (config changes stay
//! git-reviewed file diffs); mounted behind the gateway's Bearer middleware
//! by `gateway serve`.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use serde::Serialize;

use crate::gateway_server::admin_api::{AdminState, UsageQuery, resolve_window};

/// One registered MCP server, enriched with live inventory counts.
#[derive(Debug, Clone, Serialize)]
pub struct McpServerRow {
    pub id: String,
    pub url: String,
    /// `gateway` when the entry injects an env credential, `caller` when the
    /// upstream is public/unauthenticated from the gateway's perspective.
    pub credential: &'static str,
    pub tools: i64,
    /// Tools whose definition fingerprint changed at least once (rug-pull
    /// signal — observe stage surfaces, M4 enforces).
    pub changed_tools: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct McpToolRow {
    pub server_id: String,
    pub tool: String,
    pub calls: i64,
    pub errors: i64,
    pub persons: i64,
    pub result_tokens: i64,
    pub context_cost_usd: f64,
    pub p50_duration_ms: f64,
    pub max_duration_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct McpInventoryRow {
    pub server_id: String,
    pub tool: String,
    pub schema_sha256: String,
    pub previous_sha256: Option<String>,
    pub change_count: i64,
    pub first_seen: String,
    pub last_seen: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct McpTotals {
    pub calls: i64,
    pub errors: i64,
    pub persons: i64,
    pub result_tokens: i64,
    pub context_cost_usd: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct McpAdminResponse {
    pub from: String,
    pub to: String,
    pub reference_model: Option<String>,
    pub servers: Vec<McpServerRow>,
    pub totals: McpTotals,
    pub tools: Vec<McpToolRow>,
    pub inventory: Vec<McpInventoryRow>,
}

/// `GET /api/admin/mcp?from=&to=` — inventory, per-tool activity and window
/// totals. With no registered servers the endpoint still answers (empty
/// lists), so the console can render its "register a server" empty state.
pub async fn get_mcp(
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

    match assemble(&state, from, to).await {
        Ok(resp) => Json(resp).into_response(),
        Err(e) => {
            tracing::warn!("admin mcp query failed: {e:#}");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error": "mcp store unavailable"})),
            )
                .into_response()
        }
    }
}

async fn assemble(
    state: &AdminState,
    from: chrono::DateTime<chrono::Utc>,
    to: chrono::DateTime<chrono::Utc>,
) -> anyhow::Result<McpAdminResponse> {
    let client = state.pool.get().await?;
    // The tables exist once `serve` ran with a registered server; a console
    // pointed at an older database answers empty rather than 503.
    let _ = super::store::init_schema(&state.pool).await;

    let inventory: Vec<McpInventoryRow> = client
        .query(super::store::INVENTORY_SQL, &[])
        .await?
        .iter()
        .map(|r| McpInventoryRow {
            server_id: r.get("server_id"),
            tool: r.get("tool"),
            schema_sha256: r.get("schema_sha256"),
            previous_sha256: r.get("previous_sha256"),
            change_count: r.get("change_count"),
            first_seen: r.get("first_seen"),
            last_seen: r.get("last_seen"),
        })
        .collect();

    let servers = state
        .mcp_servers
        .iter()
        .map(|s| {
            let tools = inventory.iter().filter(|i| i.server_id == s.id).count() as i64;
            let changed_tools = inventory
                .iter()
                .filter(|i| i.server_id == s.id && i.change_count > 0)
                .count() as i64;
            McpServerRow {
                id: s.id.clone(),
                url: s.url.clone(),
                credential: if s.auth_env.is_some() {
                    "gateway"
                } else {
                    "caller"
                },
                tools,
                changed_tools,
            }
        })
        .collect();

    let t = client
        .query_one(super::store::TOTALS_SQL, &[&from, &to])
        .await?;
    let totals = McpTotals {
        calls: t.get("calls"),
        errors: t.get("errors"),
        persons: t.get("persons"),
        result_tokens: t.get("result_tokens"),
        context_cost_usd: t.get("context_cost_usd"),
    };

    let tools = client
        .query(super::store::TOOL_BREAKDOWN_SQL, &[&from, &to])
        .await?
        .iter()
        .map(|r| McpToolRow {
            server_id: r.get("server_id"),
            tool: r.get("tool"),
            calls: r.get("calls"),
            errors: r.get("errors"),
            persons: r.get("persons"),
            result_tokens: r.get("result_tokens"),
            context_cost_usd: r.get("context_cost_usd"),
            p50_duration_ms: r.get::<_, Option<f64>>("p50_duration_ms").unwrap_or(0.0),
            max_duration_ms: r.get("max_duration_ms"),
        })
        .collect();

    Ok(McpAdminResponse {
        from: from.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        to: to.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        reference_model: state.reference_model.clone(),
        servers,
        totals,
        tools,
        inventory,
    })
}
