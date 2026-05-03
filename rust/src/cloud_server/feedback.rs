use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde::{Deserialize, Serialize};

use super::auth::{auth_user, AppState};
use super::helpers::internal_error;

#[derive(Deserialize)]
pub struct FeedbackEntry {
    pub language: String,
    pub entropy: f64,
    pub jaccard: f64,
    #[serde(default)]
    pub sample_count: i32,
    #[serde(default)]
    pub avg_efficiency: f64,
}

#[derive(Serialize)]
pub struct FeedbackOut {
    pub language: String,
    pub entropy: f64,
    pub jaccard: f64,
    pub sample_count: i32,
    pub avg_efficiency: f64,
}

pub async fn post_feedback(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Vec<FeedbackEntry>>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let (user_id, _) = auth_user(&state, &headers).await?;
    let client = state.pool.get().await.map_err(internal_error)?;

    let mut count = 0u32;
    for entry in &body {
        client
            .execute(
                r"INSERT INTO feedback_thresholds (user_id, language, entropy, jaccard, sample_count, avg_efficiency)
                   VALUES ($1, $2, $3, $4, $5, $6)
                   ON CONFLICT (user_id, language) DO UPDATE SET
                     entropy = EXCLUDED.entropy,
                     jaccard = EXCLUDED.jaccard,
                     sample_count = EXCLUDED.sample_count,
                     avg_efficiency = EXCLUDED.avg_efficiency,
                     updated_at = NOW()",
                &[
                    &user_id,
                    &entry.language,
                    &entry.entropy,
                    &entry.jaccard,
                    &entry.sample_count,
                    &entry.avg_efficiency,
                ],
            )
            .await
            .map_err(internal_error)?;
        count += 1;
    }

    Ok(Json(serde_json::json!({"synced": count})))
}

pub async fn get_feedback(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<FeedbackOut>>, (StatusCode, String)> {
    let (user_id, _) = auth_user(&state, &headers).await?;
    let client = state.pool.get().await.map_err(internal_error)?;

    let rows = client
        .query(
            "SELECT language, entropy, jaccard, sample_count, avg_efficiency FROM feedback_thresholds WHERE user_id = $1 ORDER BY language",
            &[&user_id],
        )
        .await
        .map_err(internal_error)?;

    let out: Vec<FeedbackOut> = rows
        .iter()
        .map(|r| FeedbackOut {
            language: r.get(0),
            entropy: r.get(1),
            jaccard: r.get(2),
            sample_count: r.get(3),
            avg_efficiency: r.get(4),
        })
        .collect();

    Ok(Json(out))
}
