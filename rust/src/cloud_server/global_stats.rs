use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::Serialize;

use super::auth::AppState;
use super::helpers::internal_error;

#[derive(Serialize)]
pub struct GlobalStatsResponse {
    pub total_tokens_saved: i64,
    pub total_users: i64,
    pub total_contributions: i64,
    pub total_teams: i64,
}

pub async fn get_global_stats(
    State(state): State<AppState>,
) -> Result<Json<GlobalStatsResponse>, (StatusCode, String)> {
    let client = state.pool.get().await.map_err(internal_error)?;

    let tokens_saved: i64 = client
        .query_one(
            "SELECT COALESCE(SUM(total_tokens_saved), 0)::BIGINT FROM user_profiles",
            &[],
        )
        .await
        .map_err(internal_error)?
        .get(0);

    let total_users: i64 = client
        .query_one("SELECT COUNT(*) FROM users", &[])
        .await
        .map_err(internal_error)?
        .get(0);

    let total_contributions: i64 = client
        .query_one("SELECT COUNT(*) FROM contribute_entries", &[])
        .await
        .map_err(internal_error)?
        .get(0);

    Ok(Json(GlobalStatsResponse {
        total_tokens_saved: tokens_saved,
        total_users,
        total_contributions,
        total_teams: 0,
    }))
}
