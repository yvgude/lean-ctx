use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;

use super::auth::{auth_user, AppState};
use super::helpers::internal_error;

pub async fn post_buddy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let (user_id, _) = auth_user(&state, &headers).await?;
    let client = state.pool.get().await.map_err(internal_error)?;

    let name = body["name"].as_str().map(std::string::ToString::to_string);
    let species = body["species"]
        .as_str()
        .map(std::string::ToString::to_string);
    let level = body["level"].as_i64().unwrap_or(1) as i32;
    let xp = body["xp"].as_i64().unwrap_or(0);
    let mood = body["mood"].as_str().map(std::string::ToString::to_string);
    let streak = body["streak"]
        .as_i64()
        .or_else(|| body["streak_days"].as_i64())
        .unwrap_or(0) as i32;
    let rarity = body["rarity"]
        .as_str()
        .map(std::string::ToString::to_string);
    let state_json = serde_json::to_string(&body).unwrap_or_default();

    client
        .execute(
            r"INSERT INTO buddy_state (user_id, name, species, level, xp, mood, streak, rarity, state_json)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
               ON CONFLICT (user_id) DO UPDATE SET
                 name = EXCLUDED.name,
                 species = EXCLUDED.species,
                 level = EXCLUDED.level,
                 xp = EXCLUDED.xp,
                 mood = EXCLUDED.mood,
                 streak = EXCLUDED.streak,
                 rarity = EXCLUDED.rarity,
                 state_json = EXCLUDED.state_json,
                 updated_at = NOW()",
            &[
                &user_id, &name, &species, &level, &xp, &mood, &streak, &rarity, &state_json,
            ],
        )
        .await
        .map_err(internal_error)?;

    Ok(Json(serde_json::json!({"ok": true})))
}

pub async fn get_buddy(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let (user_id, _) = auth_user(&state, &headers).await?;
    let client = state.pool.get().await.map_err(internal_error)?;

    let row = client
        .query_opt(
            "SELECT state_json, name, species, level, xp, mood, streak, rarity FROM buddy_state WHERE user_id = $1",
            &[&user_id],
        )
        .await
        .map_err(internal_error)?;

    match row {
        Some(r) => {
            let state_json: Option<String> = r.get(0);
            if let Some(json_str) = state_json {
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&json_str) {
                    return Ok(Json(val));
                }
            }
            Ok(Json(serde_json::json!({
                "name": r.get::<_, Option<String>>(1),
                "species": r.get::<_, Option<String>>(2),
                "level": r.get::<_, i32>(3),
                "xp": r.get::<_, i64>(4),
                "mood": r.get::<_, Option<String>>(5),
                "streak": r.get::<_, i32>(6),
                "rarity": r.get::<_, Option<String>>(7),
            })))
        }
        None => Ok(Json(serde_json::Value::Null)),
    }
}
