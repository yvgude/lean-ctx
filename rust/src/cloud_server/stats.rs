use axum::extract::State;
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::Json;
use chrono::NaiveDate;
use serde::Deserialize;
use uuid::Uuid;

use super::auth::{auth_user, AppState};

#[derive(Deserialize)]
pub struct StatsEnvelope {
    pub stats: Vec<StatsEntry>,
}

#[derive(Deserialize)]
pub struct StatsEntry {
    pub date: String,
    pub tokens_original: i64,
    pub tokens_compressed: i64,
    pub tokens_saved: i64,
    pub tool_calls: i64,
    pub cache_hits: i64,
    pub cache_misses: i64,
}

#[derive(serde::Serialize)]
pub struct StatsOutEntry {
    pub date: String,
    pub tokens_original: i64,
    pub tokens_compressed: i64,
    pub tokens_saved: i64,
    pub tool_calls: i64,
    pub cache_hits: i64,
    pub cache_misses: i64,
    pub updated_at: String,
}

pub async fn get_stats(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<StatsOutEntry>>, (StatusCode, String)> {
    let (user_id, _email) = auth_user(&state, &headers).await?;
    let client = state.pool.get().await.map_err(internal_error)?;
    let rows = client
        .query(
            r#"
SELECT date, tokens_original, tokens_compressed, tokens_saved, tool_calls, cache_hits, cache_misses, updated_at
FROM stats_daily
WHERE user_id=$1
ORDER BY date DESC
LIMIT 120
"#,
            &[&user_id],
        )
        .await
        .map_err(internal_error)?;

    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let date: NaiveDate = r.get(0);
        let updated_at: chrono::DateTime<chrono::Utc> = r.get(7);
        out.push(StatsOutEntry {
            date: date.format("%Y-%m-%d").to_string(),
            tokens_original: r.get(1),
            tokens_compressed: r.get(2),
            tokens_saved: r.get(3),
            tool_calls: r.get(4),
            cache_hits: r.get(5),
            cache_misses: r.get(6),
            updated_at: updated_at.to_rfc3339(),
        });
    }

    Ok(Json(out))
}

pub async fn post_stats(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(env): Json<StatsEnvelope>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let (user_id, _email) = auth_user(&state, &headers).await?;
    for entry in env.stats {
        upsert_daily(&state, user_id, entry).await?;
    }

    let client = state.pool.get().await.map_err(internal_error)?;
    super::profile::ensure_profile(&client, user_id)
        .await
        .map_err(internal_error)?;
    super::profile::recalculate_tokens(&client, user_id)
        .await
        .map_err(internal_error)?;

    Ok(Json(serde_json::json!({ "message": "Synced" })))
}

async fn upsert_daily(
    state: &AppState,
    user_id: Uuid,
    entry: StatsEntry,
) -> Result<(), (StatusCode, String)> {
    let date = NaiveDate::parse_from_str(entry.date.trim(), "%Y-%m-%d")
        .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid date".into()))?;
    let client = state.pool.get().await.map_err(internal_error)?;
    client
        .execute(
            r#"
INSERT INTO stats_daily
  (user_id, date, tokens_original, tokens_compressed, tokens_saved, tool_calls, cache_hits, cache_misses, updated_at)
VALUES
  ($1,$2,$3,$4,$5,$6,$7,$8, NOW())
ON CONFLICT (user_id, date)
DO UPDATE SET
  tokens_original=EXCLUDED.tokens_original,
  tokens_compressed=EXCLUDED.tokens_compressed,
  tokens_saved=EXCLUDED.tokens_saved,
  tool_calls=EXCLUDED.tool_calls,
  cache_hits=EXCLUDED.cache_hits,
  cache_misses=EXCLUDED.cache_misses,
  updated_at=NOW()
"#,
            &[
                &user_id,
                &date,
                &entry.tokens_original,
                &entry.tokens_compressed,
                &entry.tokens_saved,
                &entry.tool_calls,
                &entry.cache_hits,
                &entry.cache_misses,
            ],
        )
        .await
        .map_err(internal_error)?;
    Ok(())
}

fn internal_error<E: std::fmt::Display>(e: E) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

