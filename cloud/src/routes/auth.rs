use axum::{extract::State, http::StatusCode, Json};
use serde::{Deserialize, Serialize};

use crate::auth::{generate_api_key, hash_api_key};
use crate::db::{queries, DbPool};

#[derive(Deserialize)]
pub struct RegisterRequest {
    pub email: String,
}

#[derive(Serialize)]
pub struct RegisterResponse {
    pub api_key: String,
    pub user_id: String,
    pub message: String,
}

#[derive(Serialize)]
pub struct ErrorResponse {
    pub error: String,
}

pub async fn register(
    State(db): State<DbPool>,
    Json(req): Json<RegisterRequest>,
) -> Result<Json<RegisterResponse>, (StatusCode, Json<ErrorResponse>)> {
    let email = req.email.trim().to_lowercase();

    if !email.contains('@') || !email.contains('.') {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse { error: "Invalid email address".to_string() }),
        ));
    }

    if let Some(existing) = queries::find_user_by_email(&db, &email) {
        return Ok(Json(RegisterResponse {
            api_key: existing.api_key,
            user_id: existing.id,
            message: "Existing account found. API key returned.".to_string(),
        }));
    }

    let api_key = generate_api_key();
    let api_key_hash = hash_api_key(&api_key);

    match queries::create_user(&db, &email, &api_key, &api_key_hash) {
        Ok(user) => Ok(Json(RegisterResponse {
            api_key,
            user_id: user.id,
            message: "Account created. Save your API key — it won't be shown again.".to_string(),
        })),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse { error: format!("Failed to create user: {e}") }),
        )),
    }
}

pub async fn me(
    State(db): State<DbPool>,
    req: axum::extract::Request,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let api_key = crate::auth::extract_api_key(&req).ok_or(StatusCode::UNAUTHORIZED)?;
    let user = crate::auth::authenticate(&db, &api_key).ok_or(StatusCode::UNAUTHORIZED)?;

    Ok(Json(serde_json::json!({
        "id": user.id,
        "email": user.email,
        "plan": user.plan,
        "created_at": user.created_at,
    })))
}
