use axum::{extract::State, http::StatusCode, Json};
use serde::Serialize;

use crate::auth;
use crate::db::DbPool;

#[derive(Serialize)]
pub struct ProModel {
    pub file_ext: String,
    pub size_bucket: String,
    pub best_mode: String,
    pub confidence: f64,
    pub sample_count: i64,
}

#[derive(Serialize)]
pub struct ProModelsResponse {
    pub models: Vec<ProModel>,
    pub total_data_points: i64,
    pub improvement_estimate: f64,
}

pub async fn get_models(
    State(db): State<DbPool>,
    req: axum::extract::Request,
) -> Result<Json<ProModelsResponse>, StatusCode> {
    let api_key = auth::extract_api_key(&req).ok_or(StatusCode::UNAUTHORIZED)?;
    let user = auth::authenticate(&db, &api_key).ok_or(StatusCode::UNAUTHORIZED)?;
    auth::require_plan(&user, "pro")?;

    let conn = db.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let total: i64 = conn
        .query_row("SELECT COUNT(*) FROM collective_data", [], |row| row.get(0))
        .unwrap_or(0);

    let mut stmt = conn
        .prepare(
            "SELECT file_ext, size_bucket, best_mode,
                    AVG(compression_ratio) as avg_r, COUNT(*) as cnt
             FROM collective_data
             WHERE file_ext != 'mixed' AND file_ext != 'unknown'
             GROUP BY file_ext, size_bucket
             HAVING cnt >= 3
             ORDER BY cnt DESC
             LIMIT 200",
        )
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let models: Vec<ProModel> = stmt
        .query_map([], |row| {
            let cnt: i64 = row.get(4)?;
            let confidence = (cnt as f64 / (cnt as f64 + 10.0)).min(0.95);
            Ok(ProModel {
                file_ext: row.get(0)?,
                size_bucket: row.get(1)?,
                best_mode: row.get(2)?,
                confidence,
                sample_count: cnt,
            })
        })
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .filter_map(|r| r.ok())
        .collect();

    let improvement = if models.is_empty() {
        0.0
    } else {
        let avg_confidence: f64 =
            models.iter().map(|m| m.confidence).sum::<f64>() / models.len() as f64;
        avg_confidence * 0.25
    };

    Ok(Json(ProModelsResponse {
        models,
        total_data_points: total,
        improvement_estimate: (improvement * 100.0).round() / 100.0,
    }))
}
