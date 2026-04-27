use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::auth::{auth_user, AppState};
use super::helpers::internal_error;

#[derive(Deserialize)]
pub struct CepEntry {
    pub recorded_at: String,
    pub score: f64,
    #[serde(default)]
    pub cache_hit_rate: Option<f64>,
    #[serde(default)]
    pub mode_diversity: Option<f64>,
    #[serde(default)]
    pub compression_rate: Option<f64>,
    #[serde(default)]
    pub tool_calls: Option<i64>,
    #[serde(default)]
    pub tokens_saved: Option<i64>,
    #[serde(default)]
    pub complexity: Option<f64>,
}

#[derive(Deserialize)]
pub struct CepEnvelope {
    pub scores: Vec<CepEntry>,
}

#[derive(Serialize)]
pub struct CepRow {
    pub recorded_at: String,
    pub score: f64,
    pub cache_hit_rate: Option<f64>,
    pub mode_diversity: Option<f64>,
    pub compression_rate: Option<f64>,
    pub tool_calls: Option<i64>,
    pub tokens_saved: Option<i64>,
    pub complexity: Option<f64>,
}

pub async fn post_cep(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CepEnvelope>,
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
                "SELECT 1 FROM cep_scores WHERE user_id=$1 AND recorded_at=$2",
                &[&user_id, &ts],
            )
            .await
            .map_err(internal_error)?;

        if existing.is_none() {
            client
                .execute(
                    r"INSERT INTO cep_scores (id, user_id, recorded_at, score, cache_hit_rate, mode_diversity, compression_rate, tool_calls, tokens_saved, complexity)
                       VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
                    &[
                        &Uuid::new_v4(),
                        &user_id,
                        &ts,
                        &entry.score,
                        &entry.cache_hit_rate,
                        &entry.mode_diversity,
                        &entry.compression_rate,
                        &entry.tool_calls,
                        &entry.tokens_saved,
                        &entry.complexity,
                    ],
                )
                .await
                .map_err(internal_error)?;
            synced += 1;
        }
    }

    Ok(Json(serde_json::json!({"synced": synced})))
}

pub async fn get_cep(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<CepRow>>, (StatusCode, String)> {
    let (user_id, _) = auth_user(&state, &headers).await?;
    let client = state.pool.get().await.map_err(internal_error)?;

    let rows = client
        .query(
            r"SELECT recorded_at, score, cache_hit_rate, mode_diversity, compression_rate, tool_calls, tokens_saved, complexity
               FROM cep_scores WHERE user_id = $1
               ORDER BY recorded_at DESC LIMIT 500",
            &[&user_id],
        )
        .await
        .map_err(internal_error)?;

    let result: Vec<CepRow> = rows
        .iter()
        .map(|r| {
            let ts: chrono::DateTime<chrono::Utc> = r.get(0);
            CepRow {
                recorded_at: ts.to_rfc3339(),
                score: r.get(1),
                cache_hit_rate: r.get(2),
                mode_diversity: r.get(3),
                compression_rate: r.get(4),
                tool_calls: r.get(5),
                tokens_saved: r.get(6),
                complexity: r.get(7),
            }
        })
        .collect();

    Ok(Json(result))
}
