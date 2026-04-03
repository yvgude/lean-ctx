use axum::{extract::State, http::StatusCode, Json};
use serde::{Deserialize, Serialize};

use crate::auth;
use crate::db::{queries, schema::DayStats, DbPool};

#[derive(Deserialize)]
pub struct SyncStatsRequest {
    pub stats: Vec<DayStats>,
}

#[derive(Serialize)]
pub struct SyncStatsResponse {
    pub synced: usize,
    pub message: String,
}

pub async fn upload_stats(
    State(db): State<DbPool>,
    req: axum::extract::Request,
) -> Result<Json<SyncStatsResponse>, StatusCode> {
    let api_key = auth::extract_api_key(&req).ok_or(StatusCode::UNAUTHORIZED)?;
    let user = auth::authenticate(&db, &api_key).ok_or(StatusCode::UNAUTHORIZED)?;
    auth::require_plan(&user, "pro")?;

    let body = axum::body::to_bytes(req.into_body(), 1024 * 1024)
        .await
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    let payload: SyncStatsRequest =
        serde_json::from_slice(&body).map_err(|_| StatusCode::BAD_REQUEST)?;

    let mut synced = 0;
    for stat in &payload.stats {
        if queries::upsert_stats(&db, &user.id, stat).is_ok() {
            synced += 1;
        }
    }

    Ok(Json(SyncStatsResponse {
        synced,
        message: format!("{synced} day(s) synced successfully"),
    }))
}

#[derive(Deserialize)]
pub struct StatsQuery {
    pub days: Option<i64>,
}

pub async fn get_stats(
    State(db): State<DbPool>,
    axum::extract::Query(query): axum::extract::Query<StatsQuery>,
    req: axum::extract::Request,
) -> Result<Json<Vec<DayStats>>, StatusCode> {
    let api_key = auth::extract_api_key(&req).ok_or(StatusCode::UNAUTHORIZED)?;
    let user = auth::authenticate(&db, &api_key).ok_or(StatusCode::UNAUTHORIZED)?;
    auth::require_plan(&user, "pro")?;

    let days = query.days.unwrap_or(30);
    let stats = queries::get_stats(&db, &user.id, days);
    Ok(Json(stats))
}

#[derive(Serialize)]
pub struct StatsSummary {
    pub total_tokens_saved: i64,
    pub total_tool_calls: i64,
    pub avg_compression_ratio: f64,
    pub cost_saved_usd: f64,
    pub days_active: usize,
}

pub async fn get_summary(
    State(db): State<DbPool>,
    req: axum::extract::Request,
) -> Result<Json<StatsSummary>, StatusCode> {
    let api_key = auth::extract_api_key(&req).ok_or(StatusCode::UNAUTHORIZED)?;
    let user = auth::authenticate(&db, &api_key).ok_or(StatusCode::UNAUTHORIZED)?;
    auth::require_plan(&user, "pro")?;

    let stats = queries::get_stats(&db, &user.id, 365);

    let total_saved: i64 = stats.iter().map(|s| s.tokens_saved).sum();
    let total_original: i64 = stats.iter().map(|s| s.tokens_original).sum();
    let total_calls: i64 = stats.iter().map(|s| s.tool_calls).sum();
    let ratio = if total_original > 0 {
        1.0 - (total_original - total_saved) as f64 / total_original as f64
    } else {
        0.0
    };
    let cost_saved = total_saved as f64 / 1_000_000.0 * 2.50;

    Ok(Json(StatsSummary {
        total_tokens_saved: total_saved,
        total_tool_calls: total_calls,
        avg_compression_ratio: ratio,
        cost_saved_usd: (cost_saved * 100.0).round() / 100.0,
        days_active: stats.len(),
    }))
}
