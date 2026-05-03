use axum::extract::{Form, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use chrono::{DateTime, Duration, Utc};
use deadpool_postgres::Pool;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::auth::{auth_user, constant_time_eq, generate_token, sha256_hex, AppState};
use super::helpers::internal_error;

#[derive(Debug, Deserialize)]
pub struct RegisterClientBody {
    pub client_name: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RegisterClientResponse {
    pub client_id: String,
    pub client_secret: String,
    pub token_endpoint: String,
    pub registration_endpoint: String,
    pub token_endpoint_auth_method: String,
}

pub async fn register_client(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<RegisterClientBody>,
) -> Result<Json<RegisterClientResponse>, (StatusCode, String)> {
    let (user_id, _email) = auth_user(&state, &headers).await?;

    let client_id = Uuid::new_v4();
    let client_secret = generate_token();
    let secret_sha = sha256_hex(&client_secret);
    let name = body
        .client_name
        .unwrap_or_else(|| "lean-ctx-connector".to_string());

    let client = state.pool.get().await.map_err(internal_error)?;
    client
        .execute(
            "INSERT INTO oauth_clients (client_id, user_id, client_name, client_secret_sha256) VALUES ($1, $2, $3, $4)",
            &[&client_id, &user_id, &name, &secret_sha],
        )
        .await
        .map_err(internal_error)?;

    Ok(Json(RegisterClientResponse {
        client_id: client_id.to_string(),
        client_secret,
        token_endpoint: format!(
            "{}/oauth/token",
            state.cfg.api_base_url.trim_end_matches('/')
        ),
        registration_endpoint: format!(
            "{}/oauth/register",
            state.cfg.api_base_url.trim_end_matches('/')
        ),
        token_endpoint_auth_method: "client_secret_post".to_string(),
    }))
}

#[derive(Debug, Deserialize)]
pub struct TokenRequestBody {
    pub grant_type: String,
    pub client_id: String,
    pub client_secret: String,
}

#[derive(Debug, Serialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub token_type: String,
    pub expires_in: i64,
}

fn access_token_ttl_secs() -> i64 {
    std::env::var("LEANCTX_CLOUD_OAUTH_TOKEN_TTL_SECS")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .map_or(3600, |n| n.clamp(60, 86_400))
}

pub async fn token(
    State(state): State<AppState>,
    Form(body): Form<TokenRequestBody>,
) -> Result<Json<TokenResponse>, (StatusCode, String)> {
    if body.grant_type.trim() != "client_credentials" {
        return Err((
            StatusCode::BAD_REQUEST,
            "unsupported grant_type (expected client_credentials)".to_string(),
        ));
    }

    let client_id = Uuid::parse_str(body.client_id.trim())
        .map_err(|_| (StatusCode::BAD_REQUEST, "invalid client_id".to_string()))?;

    let client = state.pool.get().await.map_err(internal_error)?;
    let row = client
        .query_opt(
            "SELECT user_id, client_secret_sha256, revoked_at FROM oauth_clients WHERE client_id=$1",
            &[&client_id],
        )
        .await
        .map_err(internal_error)?;

    let Some(row) = row else {
        return Err((
            StatusCode::UNAUTHORIZED,
            "invalid client credentials".to_string(),
        ));
    };

    let user_id: Uuid = row.get(0);
    let secret_sha: String = row.get(1);
    let revoked_at: Option<DateTime<Utc>> = row.get(2);
    if revoked_at.is_some() {
        return Err((StatusCode::UNAUTHORIZED, "client revoked".to_string()));
    }

    let provided_sha = sha256_hex(body.client_secret.trim());
    if !constant_time_eq(secret_sha.as_bytes(), provided_sha.as_bytes()) {
        return Err((
            StatusCode::UNAUTHORIZED,
            "invalid client credentials".to_string(),
        ));
    }

    let access_token = generate_token();
    let token_sha = sha256_hex(&access_token);
    let ttl = access_token_ttl_secs();
    let expires_at = Utc::now() + Duration::seconds(ttl);

    client
        .execute(
            "INSERT INTO oauth_access_tokens (token_sha256, user_id, client_id, expires_at, last_used_at) VALUES ($1, $2, $3, $4, NOW())",
            &[&token_sha, &user_id, &client_id, &expires_at],
        )
        .await
        .map_err(internal_error)?;

    Ok(Json(TokenResponse {
        access_token,
        token_type: "Bearer".to_string(),
        expires_in: ttl,
    }))
}

pub async fn lookup_access_token(
    pool: &Pool,
    token_sha: &str,
) -> anyhow::Result<Option<(Uuid, String)>> {
    let client = pool.get().await?;
    let row = client
        .query_opt(
            "SELECT t.user_id, u.email \
             FROM oauth_access_tokens t \
             JOIN users u ON u.id=t.user_id \
             WHERE t.token_sha256=$1 \
               AND t.revoked_at IS NULL \
               AND t.expires_at > NOW()",
            &[&token_sha],
        )
        .await?;

    if let Some(r) = row {
        let user_id: Uuid = r.get(0);
        let email: String = r.get(1);
        let _ = client
            .execute(
                "UPDATE oauth_access_tokens SET last_used_at=NOW() WHERE token_sha256=$1",
                &[&token_sha],
            )
            .await;
        Ok(Some((user_id, email)))
    } else {
        Ok(None)
    }
}
