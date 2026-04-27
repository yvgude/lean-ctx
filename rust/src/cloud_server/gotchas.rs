use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde::{Deserialize, Serialize};

use super::auth::{auth_user, AppState};
use super::helpers::internal_error;

#[derive(Deserialize)]
pub struct GotchaEntry {
    pub pattern: String,
    pub fix: String,
    #[serde(default)]
    pub severity: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub occurrences: i64,
    #[serde(default)]
    pub prevented_count: i64,
    #[serde(default)]
    pub confidence: Option<f64>,
}

#[derive(Deserialize)]
pub struct GotchasEnvelope {
    pub gotchas: Vec<GotchaEntry>,
}

#[derive(Serialize)]
pub struct GotchaRow {
    pub pattern: String,
    pub fix: String,
    pub severity: Option<String>,
    pub category: Option<String>,
    pub occurrences: i64,
    pub prevented_count: i64,
    pub confidence: Option<f64>,
}

pub async fn post_gotchas(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<GotchasEnvelope>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let (user_id, _) = auth_user(&state, &headers).await?;
    let client = state.pool.get().await.map_err(internal_error)?;

    for g in &body.gotchas {
        let pattern = g.pattern.trim();
        if pattern.is_empty() {
            continue;
        }
        client
            .execute(
                r"INSERT INTO gotchas (user_id, pattern, fix, severity, category, occurrences, prevented_count, confidence)
                   VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
                   ON CONFLICT (user_id, pattern) DO UPDATE SET
                     fix = EXCLUDED.fix,
                     severity = EXCLUDED.severity,
                     category = EXCLUDED.category,
                     occurrences = EXCLUDED.occurrences,
                     prevented_count = EXCLUDED.prevented_count,
                     confidence = EXCLUDED.confidence,
                     updated_at = NOW()",
                &[
                    &user_id,
                    &pattern,
                    &g.fix,
                    &g.severity,
                    &g.category,
                    &g.occurrences,
                    &g.prevented_count,
                    &g.confidence,
                ],
            )
            .await
            .map_err(internal_error)?;
    }

    Ok(Json(serde_json::json!({"synced": body.gotchas.len()})))
}

pub async fn get_gotchas(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<GotchaRow>>, (StatusCode, String)> {
    let (user_id, _) = auth_user(&state, &headers).await?;
    let client = state.pool.get().await.map_err(internal_error)?;

    let rows = client
        .query(
            r"SELECT pattern, fix, severity, category, occurrences, prevented_count, confidence
               FROM gotchas WHERE user_id = $1
               ORDER BY prevented_count DESC, occurrences DESC LIMIT 200",
            &[&user_id],
        )
        .await
        .map_err(internal_error)?;

    let result: Vec<GotchaRow> = rows
        .iter()
        .map(|r| GotchaRow {
            pattern: r.get(0),
            fix: r.get(1),
            severity: r.get(2),
            category: r.get(3),
            occurrences: r.get(4),
            prevented_count: r.get(5),
            confidence: r.get(6),
        })
        .collect();

    Ok(Json(result))
}
