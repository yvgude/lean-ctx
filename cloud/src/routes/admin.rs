use axum::{extract::State, http::StatusCode, Json};
use rusqlite::params;
use serde::Serialize;

use crate::auth;
use crate::db::DbPool;

fn require_admin(db: &DbPool, req: &axum::extract::Request) -> Result<(), StatusCode> {
    let api_key = auth::extract_api_key(req).ok_or(StatusCode::UNAUTHORIZED)?;
    let user = auth::authenticate(db, &api_key).ok_or(StatusCode::UNAUTHORIZED)?;
    if !user.is_admin {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(())
}

#[derive(Serialize)]
pub struct AdminOverview {
    pub total_users: i64,
    pub total_tokens_saved: i64,
    pub total_tool_calls: i64,
    pub total_contributions: i64,
    pub active_users_7d: i64,
    pub cost_saved_usd: f64,
}

pub async fn overview(
    State(db): State<DbPool>,
    req: axum::extract::Request,
) -> Result<Json<AdminOverview>, StatusCode> {
    require_admin(&db, &req)?;

    let conn = db.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let total_users: i64 = conn
        .query_row("SELECT COUNT(*) FROM users", [], |row| row.get(0))
        .unwrap_or(0);

    let total_saved: i64 = conn
        .query_row("SELECT COALESCE(SUM(tokens_saved), 0) FROM stats", [], |row| row.get(0))
        .unwrap_or(0);

    let total_calls: i64 = conn
        .query_row("SELECT COALESCE(SUM(tool_calls), 0) FROM stats", [], |row| row.get(0))
        .unwrap_or(0);

    let total_contributions: i64 = conn
        .query_row("SELECT COUNT(*) FROM collective_data", [], |row| row.get(0))
        .unwrap_or(0);

    let active_7d: i64 = conn
        .query_row(
            "SELECT COUNT(DISTINCT user_id) FROM stats WHERE date >= date('now', '-7 days')",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);

    let cost_saved = total_saved as f64 / 1_000_000.0 * 3.0;

    Ok(Json(AdminOverview {
        total_users,
        total_tokens_saved: total_saved,
        total_tool_calls: total_calls,
        total_contributions,
        active_users_7d: active_7d,
        cost_saved_usd: (cost_saved * 100.0).round() / 100.0,
    }))
}

#[derive(Serialize)]
pub struct AdminUser {
    pub id: String,
    pub email: String,
    pub plan: String,
    pub is_admin: bool,
    pub created_at: String,
    pub tokens_saved: i64,
    pub tool_calls: i64,
    pub last_active: Option<String>,
}

pub async fn users(
    State(db): State<DbPool>,
    req: axum::extract::Request,
) -> Result<Json<Vec<AdminUser>>, StatusCode> {
    require_admin(&db, &req)?;

    let conn = db.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let mut stmt = conn
        .prepare(
            "SELECT u.id, u.email, u.plan, u.is_admin, u.created_at,
                    COALESCE(SUM(s.tokens_saved), 0),
                    COALESCE(SUM(s.tool_calls), 0),
                    MAX(s.date)
             FROM users u
             LEFT JOIN stats s ON u.id = s.user_id
             GROUP BY u.id
             ORDER BY u.created_at DESC",
        )
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let users: Vec<AdminUser> = stmt
        .query_map([], |row| {
            Ok(AdminUser {
                id: row.get(0)?,
                email: row.get(1)?,
                plan: row.get(2)?,
                is_admin: row.get(3)?,
                created_at: row.get(4)?,
                tokens_saved: row.get(5)?,
                tool_calls: row.get(6)?,
                last_active: row.get(7)?,
            })
        })
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .filter_map(|r| r.ok())
        .collect();

    Ok(Json(users))
}

#[derive(Serialize)]
pub struct CollectiveHealth {
    pub total_data_points: i64,
    pub unique_extensions: i64,
    pub recommendations_available: i64,
    pub top_extensions: Vec<ExtSummary>,
}

#[derive(Serialize)]
pub struct ExtSummary {
    pub ext: String,
    pub count: i64,
    pub avg_ratio: f64,
    pub top_mode: String,
}

pub async fn collective(
    State(db): State<DbPool>,
    req: axum::extract::Request,
) -> Result<Json<CollectiveHealth>, StatusCode> {
    require_admin(&db, &req)?;

    let conn = db.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let total: i64 = conn
        .query_row("SELECT COUNT(*) FROM collective_data", [], |row| row.get(0))
        .unwrap_or(0);

    let unique_ext: i64 = conn
        .query_row(
            "SELECT COUNT(DISTINCT file_ext) FROM collective_data WHERE file_ext != 'mixed'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);

    let recs_available: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM (
                SELECT file_ext, size_bucket FROM collective_data
                WHERE file_ext != 'mixed'
                GROUP BY file_ext, size_bucket HAVING COUNT(*) >= 3
            )",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);

    let mut stmt = conn
        .prepare(
            "SELECT file_ext, COUNT(*) as cnt, AVG(compression_ratio), best_mode
             FROM collective_data
             GROUP BY file_ext ORDER BY cnt DESC LIMIT 20",
        )
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let top: Vec<ExtSummary> = stmt
        .query_map([], |row| {
            Ok(ExtSummary {
                ext: row.get(0)?,
                count: row.get(1)?,
                avg_ratio: row.get(2)?,
                top_mode: row.get(3)?,
            })
        })
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .filter_map(|r| r.ok())
        .collect();

    Ok(Json(CollectiveHealth {
        total_data_points: total,
        unique_extensions: unique_ext,
        recommendations_available: recs_available,
        top_extensions: top,
    }))
}

pub async fn make_admin(
    State(db): State<DbPool>,
    req: axum::extract::Request,
) -> Result<Json<serde_json::Value>, StatusCode> {
    require_admin(&db, &req)?;

    let body = axum::body::to_bytes(req.into_body(), 1024)
        .await
        .map_err(|_| StatusCode::BAD_REQUEST)?;

    let payload: serde_json::Value =
        serde_json::from_slice(&body).map_err(|_| StatusCode::BAD_REQUEST)?;

    let email = payload["email"].as_str().ok_or(StatusCode::BAD_REQUEST)?;

    let conn = db.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let affected = conn
        .execute(
            "UPDATE users SET is_admin = 1 WHERE email = ?1",
            params![email],
        )
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if affected == 0 {
        return Err(StatusCode::NOT_FOUND);
    }

    Ok(Json(serde_json::json!({ "message": format!("{email} is now admin") })))
}
