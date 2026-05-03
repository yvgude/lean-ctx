use axum::extract::State;
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::auth::{auth_user, AppState};
use super::helpers::internal_error;

#[derive(Deserialize)]
pub struct KnowledgeEnvelope {
    pub entries: Vec<IncomingEntry>,
}

#[derive(Deserialize)]
pub struct IncomingEntry {
    pub category: String,
    pub key: String,
    pub value: String,
    #[allow(dead_code)]
    pub updated_by: Option<String>,
    #[allow(dead_code)]
    pub updated_at: Option<String>,
}

#[derive(Serialize)]
pub struct OutEntry {
    pub category: String,
    pub key: String,
    pub value: String,
    pub updated_by: String,
    pub updated_at: String,
}

pub async fn post_knowledge(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(env): Json<KnowledgeEnvelope>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let (user_id, email) = auth_user(&state, &headers).await?;
    let mut synced = 0i64;
    for e in env.entries {
        if e.key.trim().is_empty() {
            continue;
        }
        upsert(&state, user_id, &e.category, &e.key, &e.value).await?;
        synced += 1;
    }
    Ok(Json(
        serde_json::json!({ "synced": synced, "updated_by": email }),
    ))
}

pub async fn get_knowledge(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<OutEntry>>, (StatusCode, String)> {
    let (user_id, email) = auth_user(&state, &headers).await?;
    let client = state.pool.get().await.map_err(internal_error)?;
    let rows = client
        .query(
            "SELECT category, key, value, updated_at FROM knowledge_entries WHERE user_id=$1 ORDER BY updated_at DESC",
            &[&user_id],
        )
        .await
        .map_err(internal_error)?;

    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let updated_at: DateTime<Utc> = r.get(3);
        out.push(OutEntry {
            category: r.get(0),
            key: r.get(1),
            value: r.get(2),
            updated_by: email.clone(),
            updated_at: updated_at.to_rfc3339(),
        });
    }
    Ok(Json(out))
}

async fn upsert(
    state: &AppState,
    user_id: Uuid,
    category: &str,
    key: &str,
    value: &str,
) -> Result<(), (StatusCode, String)> {
    let client = state.pool.get().await.map_err(internal_error)?;
    client
        .execute(
            r"
INSERT INTO knowledge_entries (user_id, category, key, value, updated_at)
VALUES ($1,$2,$3,$4, NOW())
ON CONFLICT (user_id, category, key)
DO UPDATE SET value=EXCLUDED.value, updated_at=NOW()
",
            &[&user_id, &category, &key, &value],
        )
        .await
        .map_err(internal_error)?;
    Ok(())
}
