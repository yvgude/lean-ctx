use axum::extract::State;
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::Json;
use serde::Serialize;

use super::auth::{auth_user, AppState};
use super::helpers::internal_error;

#[derive(Serialize)]
pub struct ModelsResponse {
    pub models: Vec<ModelRec>,
}

#[derive(Serialize)]
pub struct ModelRec {
    pub file_ext: String,
    pub size_bucket: String,
    pub recommended_mode: String,
    pub confidence: f64,
}

pub async fn get_models(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<ModelsResponse>, (StatusCode, String)> {
    let (_user_id, _email) = auth_user(&state, &headers).await?;

    let client = state.pool.get().await.map_err(internal_error)?;
    let rows = client
        .query(
            r"
SELECT file_ext, size_bucket, best_mode, COUNT(*)::BIGINT AS c
FROM contribute_entries
GROUP BY file_ext, size_bucket, best_mode
",
            &[],
        )
        .await
        .map_err(internal_error)?;

    use std::collections::HashMap;
    let mut by_key: HashMap<(String, String), Vec<(String, i64)>> = HashMap::new();
    for r in rows {
        let file_ext: String = r.get(0);
        let size_bucket: String = r.get(1);
        let mode: String = r.get(2);
        let c: i64 = r.get(3);
        by_key
            .entry((file_ext, size_bucket))
            .or_default()
            .push((mode, c));
    }

    let mut models = Vec::new();
    for ((file_ext, size_bucket), modes) in by_key {
        let total: i64 = modes.iter().map(|(_, c)| *c).sum();
        if total <= 0 {
            continue;
        }
        let mut best: Option<(String, i64)> = None;
        for (m, c) in modes {
            if best.as_ref().is_none_or(|(_, bc)| c > *bc) {
                best = Some((m, c));
            }
        }
        if let Some((recommended_mode, best_count)) = best {
            let confidence = (best_count as f64) / (total as f64);
            models.push(ModelRec {
                file_ext,
                size_bucket,
                recommended_mode,
                confidence,
            });
        }
    }

    Ok(Json(ModelsResponse { models }))
}
