use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use uuid::Uuid;

use super::auth::AppState;
use super::helpers::internal_error;

#[derive(Deserialize)]
pub struct ContributeEnvelope {
    pub entries: Vec<Entry>,
}

#[derive(Deserialize)]
pub struct Entry {
    pub file_ext: String,
    pub size_bucket: String,
    pub best_mode: String,
    pub compression_ratio: f64,
}

pub async fn post_contribute(
    State(state): State<AppState>,
    Json(env): Json<ContributeEnvelope>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let client = state.pool.get().await.map_err(internal_error)?;
    let mut inserted = 0i64;

    for e in env.entries {
        let file_ext = e.file_ext.trim().to_string();
        let size_bucket = e.size_bucket.trim().to_string();
        let best_mode = e.best_mode.trim().to_string();
        if file_ext.is_empty() || size_bucket.is_empty() || best_mode.is_empty() {
            continue;
        }

        let id = Uuid::new_v4();
        client
            .execute(
                "INSERT INTO contribute_entries (id, file_ext, size_bucket, best_mode, compression_ratio) VALUES ($1,$2,$3,$4,$5)",
                &[&id, &file_ext, &size_bucket, &best_mode, &e.compression_ratio],
            )
            .await
            .map_err(internal_error)?;
        inserted += 1;
    }

    Ok(Json(
        serde_json::json!({ "message": format!("Contributed {inserted} entries") }),
    ))
}
