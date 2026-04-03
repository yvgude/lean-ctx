use axum::{extract::State, http::StatusCode, Json};
use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::auth;
use crate::db::{schema::SharedKnowledgeEntry, DbPool};

#[derive(Deserialize)]
pub struct PushKnowledgeRequest {
    pub entries: Vec<SharedKnowledgeEntry>,
}

#[derive(Serialize)]
pub struct PushKnowledgeResponse {
    pub synced: usize,
}

pub async fn push_knowledge(
    State(db): State<DbPool>,
    req: axum::extract::Request,
) -> Result<Json<PushKnowledgeResponse>, StatusCode> {
    let api_key = auth::extract_api_key(&req).ok_or(StatusCode::UNAUTHORIZED)?;
    let user = auth::authenticate(&db, &api_key).ok_or(StatusCode::UNAUTHORIZED)?;
    auth::require_plan(&user, "team")?;

    let team_id = get_user_team(&db, &user.id).ok_or(StatusCode::FORBIDDEN)?;

    let body = axum::body::to_bytes(req.into_body(), 1024 * 1024)
        .await
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    let payload: PushKnowledgeRequest =
        serde_json::from_slice(&body).map_err(|_| StatusCode::BAD_REQUEST)?;

    let conn = db.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut synced = 0;
    for entry in &payload.entries {
        let result = conn.execute(
            "INSERT INTO shared_knowledge (team_id, category, key, value, updated_by)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(team_id, category, key) DO UPDATE SET
               value = excluded.value,
               updated_by = excluded.updated_by,
               updated_at = datetime('now')",
            params![team_id, entry.category, entry.key, entry.value, user.id],
        );
        if result.is_ok() {
            synced += 1;
        }
    }

    Ok(Json(PushKnowledgeResponse { synced }))
}

pub async fn pull_knowledge(
    State(db): State<DbPool>,
    req: axum::extract::Request,
) -> Result<Json<Vec<SharedKnowledgeEntry>>, StatusCode> {
    let api_key = auth::extract_api_key(&req).ok_or(StatusCode::UNAUTHORIZED)?;
    let user = auth::authenticate(&db, &api_key).ok_or(StatusCode::UNAUTHORIZED)?;
    auth::require_plan(&user, "team")?;

    let team_id = get_user_team(&db, &user.id).ok_or(StatusCode::FORBIDDEN)?;

    let conn = db.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut stmt = conn.prepare(
        "SELECT sk.category, sk.key, sk.value, u.email, sk.updated_at
         FROM shared_knowledge sk
         JOIN users u ON sk.updated_by = u.id
         WHERE sk.team_id = ?1
         ORDER BY sk.updated_at DESC"
    ).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let entries: Vec<SharedKnowledgeEntry> = stmt.query_map(params![team_id], |row| {
        Ok(SharedKnowledgeEntry {
            category: row.get(0)?,
            key: row.get(1)?,
            value: row.get(2)?,
            updated_by: row.get(3)?,
            updated_at: row.get(4)?,
        })
    }).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .filter_map(|r| r.ok())
    .collect();

    Ok(Json(entries))
}

fn get_user_team(db: &DbPool, user_id: &str) -> Option<String> {
    let conn = db.lock().ok()?;
    conn.query_row(
        "SELECT team_id FROM team_members WHERE user_id = ?1 LIMIT 1",
        params![user_id],
        |row| row.get(0),
    ).ok()
}
