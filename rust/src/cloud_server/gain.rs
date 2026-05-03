use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::auth::{auth_user, AppState};
use super::helpers::internal_error;

#[derive(Deserialize)]
pub struct GainEntry {
    pub recorded_at: String,
    pub total: f64,
    pub compression: f64,
    pub cost_efficiency: f64,
    pub quality: f64,
    pub consistency: f64,
    #[serde(default)]
    pub trend: Option<String>,
    #[serde(default)]
    pub avoided_usd: Option<f64>,
    #[serde(default)]
    pub tool_spend_usd: Option<f64>,
    #[serde(default)]
    pub model_key: Option<String>,
}

#[derive(Deserialize)]
pub struct GainEnvelope {
    pub scores: Vec<GainEntry>,
}

#[derive(Serialize)]
pub struct GainRow {
    pub recorded_at: String,
    pub total: f64,
    pub compression: f64,
    pub cost_efficiency: f64,
    pub quality: f64,
    pub consistency: f64,
    pub trend: Option<String>,
    pub avoided_usd: Option<f64>,
    pub tool_spend_usd: Option<f64>,
    pub model_key: Option<String>,
}

pub async fn post_gain(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<GainEnvelope>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let (user_id, _) = auth_user(&state, &headers).await?;
    let client = state.pool.get().await.map_err(internal_error)?;

    let mut synced = 0i64;
    for entry in &body.scores {
        let ts: chrono::DateTime<chrono::Utc> = entry
            .recorded_at
            .parse()
            .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid timestamp".into()))?;

        let existing = client
            .query_opt(
                "SELECT 1 FROM gain_scores WHERE user_id=$1 AND recorded_at=$2",
                &[&user_id, &ts],
            )
            .await
            .map_err(internal_error)?;

        if existing.is_none() {
            client
                .execute(
                    r"INSERT INTO gain_scores
                       (id, user_id, recorded_at, total, compression, cost_efficiency, quality, consistency, trend, avoided_usd, tool_spend_usd, model_key)
                       VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)",
                    &[
                        &Uuid::new_v4(),
                        &user_id,
                        &ts,
                        &entry.total,
                        &entry.compression,
                        &entry.cost_efficiency,
                        &entry.quality,
                        &entry.consistency,
                        &entry.trend,
                        &entry.avoided_usd,
                        &entry.tool_spend_usd,
                        &entry.model_key,
                    ],
                )
                .await
                .map_err(internal_error)?;
            synced += 1;
        }
    }

    Ok(Json(serde_json::json!({ "synced": synced })))
}

pub async fn get_gain(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<GainRow>>, (StatusCode, String)> {
    let (user_id, _) = auth_user(&state, &headers).await?;
    let client = state.pool.get().await.map_err(internal_error)?;

    let rows = client
        .query(
            r"SELECT recorded_at, total, compression, cost_efficiency, quality, consistency, trend, avoided_usd, tool_spend_usd, model_key
               FROM gain_scores
               WHERE user_id = $1
               ORDER BY recorded_at DESC
               LIMIT 500",
            &[&user_id],
        )
        .await
        .map_err(internal_error)?;

    let result: Vec<GainRow> = rows
        .iter()
        .map(|r| {
            let ts: chrono::DateTime<chrono::Utc> = r.get(0);
            GainRow {
                recorded_at: ts.to_rfc3339(),
                total: r.get(1),
                compression: r.get(2),
                cost_efficiency: r.get(3),
                quality: r.get(4),
                consistency: r.get(5),
                trend: r.get(6),
                avoided_usd: r.get(7),
                tool_spend_usd: r.get(8),
                model_key: r.get(9),
            }
        })
        .collect();

    Ok(Json(result))
}
