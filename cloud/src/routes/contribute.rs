use axum::{extract::State, http::StatusCode, Json};
use serde::{Deserialize, Serialize};

use crate::db::{queries, schema::CollectiveEntry, DbPool};

#[derive(Deserialize)]
pub struct ContributeRequest {
    pub entries: Vec<CollectiveEntry>,
}

#[derive(Serialize)]
pub struct ContributeResponse {
    pub accepted: usize,
    pub message: String,
}

pub async fn contribute(
    State(db): State<DbPool>,
    Json(req): Json<ContributeRequest>,
) -> Result<Json<ContributeResponse>, StatusCode> {
    if req.entries.is_empty() {
        return Ok(Json(ContributeResponse {
            accepted: 0,
            message: "No data to contribute".to_string(),
        }));
    }

    if req.entries.len() > 1000 {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }

    for entry in &req.entries {
        if entry.file_ext.len() > 10
            || entry.size_bucket.len() > 20
            || entry.best_mode.len() > 20
        {
            return Err(StatusCode::BAD_REQUEST);
        }
    }

    let count = queries::insert_collective_data(&db, &req.entries)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(ContributeResponse {
        accepted: count,
        message: format!("{count} entries contributed. Thank you for improving lean-ctx for everyone!"),
    }))
}

#[derive(Serialize)]
pub struct CollectiveStats {
    pub total_entries: i64,
    pub top_extensions: Vec<ExtensionStat>,
}

#[derive(Serialize)]
pub struct ExtensionStat {
    pub ext: String,
    pub count: i64,
    pub avg_ratio: f64,
    pub best_mode: String,
}

pub async fn collective_stats(
    State(db): State<DbPool>,
) -> Result<Json<CollectiveStats>, StatusCode> {
    let conn = db.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let total: i64 = conn
        .query_row("SELECT COUNT(*) FROM collective_data", [], |row| row.get(0))
        .unwrap_or(0);

    let mut stmt = conn
        .prepare(
            "SELECT file_ext, COUNT(*) as cnt, AVG(compression_ratio) as avg_r, best_mode
             FROM collective_data GROUP BY file_ext ORDER BY cnt DESC LIMIT 10",
        )
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let top: Vec<ExtensionStat> = stmt
        .query_map([], |row| {
            Ok(ExtensionStat {
                ext: row.get(0)?,
                count: row.get(1)?,
                avg_ratio: row.get(2)?,
                best_mode: row.get(3)?,
            })
        })
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .filter_map(|r| r.ok())
        .collect();

    Ok(Json(CollectiveStats {
        total_entries: total,
        top_extensions: top,
    }))
}

