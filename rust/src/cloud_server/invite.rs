use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde::Serialize;
use uuid::Uuid;

use super::auth::{auth_user, AppState};
use super::profile::ensure_profile;

#[derive(Serialize)]
pub struct InviteResponse {
    pub invite_code: String,
    pub invite_url: String,
}

pub async fn generate_invite(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<InviteResponse>, (StatusCode, String)> {
    let (user_id, _email) = auth_user(&state, &headers).await?;
    let client = state.pool.get().await.map_err(internal_error)?;

    ensure_profile(&client, user_id).await.map_err(internal_error)?;

    let row = client
        .query_one(
            "SELECT invite_code FROM user_profiles WHERE user_id=$1",
            &[&user_id],
        )
        .await
        .map_err(internal_error)?;

    let invite_code: String = row.get(0);
    let invite_url = format!(
        "{}/docs/getting-started?ref={}",
        state.cfg.public_base_url.trim_end_matches('/'),
        invite_code
    );

    Ok(Json(InviteResponse {
        invite_code,
        invite_url,
    }))
}

#[derive(Serialize)]
pub struct InviteInfoResponse {
    pub valid: bool,
    pub inviter_hash: Option<String>,
    pub team_name: Option<String>,
}

pub async fn get_invite_info(
    State(state): State<AppState>,
    Path(code): Path<String>,
) -> Result<Json<InviteInfoResponse>, (StatusCode, String)> {
    let client = state.pool.get().await.map_err(internal_error)?;

    let row = client
        .query_opt(
            "SELECT user_id, display_hash, team_id FROM user_profiles WHERE invite_code=$1",
            &[&code],
        )
        .await
        .map_err(internal_error)?;

    let row = match row {
        Some(r) => r,
        None => {
            return Ok(Json(InviteInfoResponse {
                valid: false,
                inviter_hash: None,
                team_name: None,
            }))
        }
    };

    let display_hash: String = row.get(1);
    let team_id: Option<Uuid> = row.get(2);

    let team_name = if let Some(tid) = team_id {
        client
            .query_opt("SELECT name FROM teams WHERE id=$1", &[&tid])
            .await
            .map_err(internal_error)?
            .map(|r| r.get::<_, String>(0))
    } else {
        None
    };

    Ok(Json(InviteInfoResponse {
        valid: true,
        inviter_hash: Some(display_hash),
        team_name,
    }))
}

pub async fn accept_invite(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(code): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let (user_id, _email) = auth_user(&state, &headers).await?;
    let client = state.pool.get().await.map_err(internal_error)?;

    ensure_profile(&client, user_id).await.map_err(internal_error)?;

    let inviter = client
        .query_opt(
            "SELECT user_id, team_id FROM user_profiles WHERE invite_code=$1",
            &[&code],
        )
        .await
        .map_err(internal_error)?;

    let inviter = match inviter {
        Some(r) => r,
        None => return Err((StatusCode::NOT_FOUND, "Invalid invite code".into())),
    };

    let inviter_id: Uuid = inviter.get(0);
    if inviter_id == user_id {
        return Err((StatusCode::BAD_REQUEST, "Cannot invite yourself".into()));
    }

    let inviter_team_id: Option<Uuid> = inviter.get(1);

    let team_id = match inviter_team_id {
        Some(tid) => tid,
        None => {
            let tid = Uuid::new_v4();
            let inviter_hash: String = client
                .query_one(
                    "SELECT display_hash FROM user_profiles WHERE user_id=$1",
                    &[&inviter_id],
                )
                .await
                .map_err(internal_error)?
                .get(0);

            client
                .execute(
                    "INSERT INTO teams (id, name, owner_id) VALUES ($1, $2, $3)",
                    &[&tid, &format!("Team {inviter_hash}"), &inviter_id],
                )
                .await
                .map_err(internal_error)?;

            client
                .execute(
                    "UPDATE user_profiles SET team_id=$1 WHERE user_id=$2",
                    &[&tid, &inviter_id],
                )
                .await
                .map_err(internal_error)?;

            tid
        }
    };

    client
        .execute(
            "UPDATE user_profiles SET team_id=$1, invited_by=$2 WHERE user_id=$3",
            &[&team_id, &inviter_id, &user_id],
        )
        .await
        .map_err(internal_error)?;

    Ok(Json(serde_json::json!({ "ok": true, "team_id": team_id.to_string() })))
}

fn internal_error<E: std::fmt::Display>(e: E) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}
