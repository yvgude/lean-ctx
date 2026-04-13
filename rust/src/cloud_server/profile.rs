use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::auth::{auth_user, AppState};

const BADGE_FIRST_SAVE: i32 = 1 << 0;
const BADGE_SAVER_1M: i32 = 1 << 1;
const BADGE_SAVER_10M: i32 = 1 << 2;
const BADGE_SAVER_100M: i32 = 1 << 3;
const BADGE_EVANGELIST: i32 = 1 << 4;
const BADGE_TEAM_BUILDER: i32 = 1 << 5;
const BADGE_EARLY_ADOPTER: i32 = 1 << 6;
const BADGE_CONTRIBUTOR: i32 = 1 << 7;

#[derive(Serialize)]
pub struct ProfileResponse {
    pub display_hash: String,
    pub username: Option<String>,
    pub total_tokens_saved: i64,
    pub badge_flags: i32,
    pub badges: Vec<String>,
    pub invite_code: String,
    pub team: Option<TeamInfo>,
    pub rank: i64,
}

#[derive(Serialize)]
pub struct TeamInfo {
    pub id: String,
    pub name: String,
    pub total_tokens_saved: i64,
    pub member_count: i64,
    pub is_owner: bool,
}

pub async fn get_profile(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<ProfileResponse>, (StatusCode, String)> {
    let (user_id, _email) = auth_user(&state, &headers).await?;
    let client = state.pool.get().await.map_err(internal_error)?;

    ensure_profile(&client, user_id).await.map_err(internal_error)?;
    recalculate_tokens(&client, user_id).await.map_err(internal_error)?;
    recalculate_badges(&client, user_id).await.map_err(internal_error)?;

    let row = client
        .query_one(
            "SELECT display_hash, username, total_tokens_saved, badge_flags, invite_code, team_id FROM user_profiles WHERE user_id=$1",
            &[&user_id],
        )
        .await
        .map_err(internal_error)?;

    let display_hash: String = row.get(0);
    let username: Option<String> = row.get(1);
    let total_tokens_saved: i64 = row.get(2);
    let badge_flags: i32 = row.get(3);
    let invite_code: String = row.get(4);
    let team_id: Option<Uuid> = row.get(5);

    let rank = client
        .query_one(
            "SELECT COUNT(*)+1 FROM user_profiles WHERE total_tokens_saved > $1",
            &[&total_tokens_saved],
        )
        .await
        .map_err(internal_error)?
        .get::<_, i64>(0);

    let team = if let Some(tid) = team_id {
        fetch_team_info(&client, tid, user_id).await.map_err(internal_error)?
    } else {
        None
    };

    Ok(Json(ProfileResponse {
        display_hash,
        username,
        total_tokens_saved,
        badge_flags,
        badges: badge_names(badge_flags),
        invite_code,
        team,
        rank,
    }))
}

#[derive(Deserialize)]
pub struct PatchProfileBody {
    pub username: Option<String>,
}

pub async fn patch_profile(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<PatchProfileBody>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let (user_id, _email) = auth_user(&state, &headers).await?;
    let client = state.pool.get().await.map_err(internal_error)?;

    ensure_profile(&client, user_id).await.map_err(internal_error)?;

    if let Some(name) = &body.username {
        let name = name.trim();
        if name.len() > 32 {
            return Err((StatusCode::BAD_REQUEST, "Username too long (max 32)".into()));
        }
        let val: Option<&str> = if name.is_empty() { None } else { Some(name) };
        client
            .execute(
                "UPDATE user_profiles SET username=$1 WHERE user_id=$2",
                &[&val, &user_id],
            )
            .await
            .map_err(internal_error)?;
    }

    Ok(Json(serde_json::json!({ "ok": true })))
}

pub async fn leave_team(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let (user_id, _email) = auth_user(&state, &headers).await?;
    let client = state.pool.get().await.map_err(internal_error)?;

    let row = client
        .query_opt(
            "SELECT team_id FROM user_profiles WHERE user_id=$1",
            &[&user_id],
        )
        .await
        .map_err(internal_error)?;

    let team_id: Option<Uuid> = row.and_then(|r| r.get(0));
    let team_id = match team_id {
        Some(t) => t,
        None => return Ok(Json(serde_json::json!({ "ok": true, "message": "Not in a team" }))),
    };

    client
        .execute(
            "UPDATE user_profiles SET team_id=NULL WHERE user_id=$1",
            &[&user_id],
        )
        .await
        .map_err(internal_error)?;

    let is_owner = client
        .query_opt(
            "SELECT 1 FROM teams WHERE id=$1 AND owner_id=$2",
            &[&team_id, &user_id],
        )
        .await
        .map_err(internal_error)?
        .is_some();

    let remaining = client
        .query_one(
            "SELECT COUNT(*) FROM user_profiles WHERE team_id=$1",
            &[&team_id],
        )
        .await
        .map_err(internal_error)?
        .get::<_, i64>(0);

    if remaining == 0 {
        client
            .execute("DELETE FROM teams WHERE id=$1", &[&team_id])
            .await
            .map_err(internal_error)?;
    } else if is_owner {
        client
            .execute(
                "UPDATE teams SET owner_id=(SELECT user_id FROM user_profiles WHERE team_id=$1 ORDER BY created_at ASC LIMIT 1) WHERE id=$1",
                &[&team_id],
            )
            .await
            .map_err(internal_error)?;
    }

    Ok(Json(serde_json::json!({ "ok": true })))
}

pub async fn rename_team(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<RenameTeamBody>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let (user_id, _email) = auth_user(&state, &headers).await?;
    let client = state.pool.get().await.map_err(internal_error)?;

    let name = body.name.trim().to_string();
    if name.is_empty() || name.len() > 48 {
        return Err((StatusCode::BAD_REQUEST, "Team name must be 1-48 chars".into()));
    }

    let updated = client
        .execute(
            "UPDATE teams SET name=$1 WHERE owner_id=$2",
            &[&name, &user_id],
        )
        .await
        .map_err(internal_error)?;

    if updated == 0 {
        return Err((StatusCode::FORBIDDEN, "Not a team owner".into()));
    }

    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct RenameTeamBody {
    pub name: String,
}

pub(crate) async fn ensure_profile(
    client: &deadpool_postgres::Object,
    user_id: Uuid,
) -> anyhow::Result<()> {
    let exists = client
        .query_opt(
            "SELECT 1 FROM user_profiles WHERE user_id=$1",
            &[&user_id],
        )
        .await?;
    if exists.is_some() {
        return Ok(());
    }

    let display_hash = make_display_hash(user_id);
    let invite_code = make_invite_code();

    client
        .execute(
            "INSERT INTO user_profiles (user_id, display_hash, invite_code) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
            &[&user_id, &display_hash, &invite_code],
        )
        .await?;
    Ok(())
}

pub(crate) async fn recalculate_tokens(
    client: &deadpool_postgres::Object,
    user_id: Uuid,
) -> anyhow::Result<()> {
    client
        .execute(
            r#"
UPDATE user_profiles SET total_tokens_saved = COALESCE(
  (SELECT SUM(tokens_saved) FROM stats_daily WHERE user_id=$1), 0
) WHERE user_id=$1
"#,
            &[&user_id],
        )
        .await?;
    Ok(())
}

async fn recalculate_badges(
    client: &deadpool_postgres::Object,
    user_id: Uuid,
) -> anyhow::Result<()> {
    let row = client
        .query_one(
            "SELECT total_tokens_saved FROM user_profiles WHERE user_id=$1",
            &[&user_id],
        )
        .await?;
    let total: i64 = row.get(0);

    let invite_count: i64 = client
        .query_one(
            "SELECT COUNT(*) FROM user_profiles WHERE invited_by=$1",
            &[&user_id],
        )
        .await?
        .get(0);

    let team_member_count: i64 = client
        .query_one(
            "SELECT COALESCE((SELECT COUNT(*) FROM user_profiles p JOIN teams t ON p.team_id=t.id WHERE t.owner_id=$1), 0)",
            &[&user_id],
        )
        .await?
        .get(0);

    let contribution_count: i64 = client
        .query_one(
            "SELECT COALESCE((SELECT COUNT(*) FROM contribute_entries WHERE device_hash=(SELECT display_hash FROM user_profiles WHERE user_id=$1)), 0)",
            &[&user_id],
        )
        .await
        .map(|r| r.get::<_, i64>(0))
        .unwrap_or(0);

    let user_created: chrono::DateTime<chrono::Utc> = client
        .query_one("SELECT created_at FROM users WHERE id=$1", &[&user_id])
        .await?
        .get(0);

    let mut flags: i32 = 0;
    if total > 0 {
        flags |= BADGE_FIRST_SAVE;
    }
    if total >= 1_000_000 {
        flags |= BADGE_SAVER_1M;
    }
    if total >= 10_000_000 {
        flags |= BADGE_SAVER_10M;
    }
    if total >= 100_000_000 {
        flags |= BADGE_SAVER_100M;
    }
    if invite_count >= 3 {
        flags |= BADGE_EVANGELIST;
    }
    if team_member_count >= 3 {
        flags |= BADGE_TEAM_BUILDER;
    }
    let cutoff = chrono::NaiveDate::from_ymd_opt(2026, 6, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc();
    if user_created < cutoff {
        flags |= BADGE_EARLY_ADOPTER;
    }
    if contribution_count >= 100 {
        flags |= BADGE_CONTRIBUTOR;
    }

    client
        .execute(
            "UPDATE user_profiles SET badge_flags=$1 WHERE user_id=$2",
            &[&flags, &user_id],
        )
        .await?;
    Ok(())
}

async fn fetch_team_info(
    client: &deadpool_postgres::Object,
    team_id: Uuid,
    user_id: Uuid,
) -> anyhow::Result<Option<TeamInfo>> {
    let row = client
        .query_opt("SELECT id, name, owner_id FROM teams WHERE id=$1", &[&team_id])
        .await?;
    let row = match row {
        Some(r) => r,
        None => return Ok(None),
    };

    let owner_id: Uuid = row.get(2);

    let agg = client
        .query_one(
            "SELECT COALESCE(SUM(total_tokens_saved),0), COUNT(*) FROM user_profiles WHERE team_id=$1",
            &[&team_id],
        )
        .await?;

    Ok(Some(TeamInfo {
        id: team_id.to_string(),
        name: row.get(1),
        total_tokens_saved: agg.get(0),
        member_count: agg.get(1),
        is_owner: owner_id == user_id,
    }))
}

fn make_display_hash(user_id: Uuid) -> String {
    let mut h = Sha256::new();
    h.update(user_id.as_bytes());
    let hash = hex::encode(h.finalize());
    format!("lctx_{}", &hash[..6])
}

fn make_invite_code() -> String {
    let bytes: [u8; 4] = rand::random();
    hex::encode(bytes)
}

fn badge_names(flags: i32) -> Vec<String> {
    let mut out = vec![];
    if flags & BADGE_FIRST_SAVE != 0 {
        out.push("First Save".into());
    }
    if flags & BADGE_SAVER_1M != 0 {
        out.push("Token Saver 1M".into());
    }
    if flags & BADGE_SAVER_10M != 0 {
        out.push("Token Saver 10M".into());
    }
    if flags & BADGE_SAVER_100M != 0 {
        out.push("Token Saver 100M".into());
    }
    if flags & BADGE_EVANGELIST != 0 {
        out.push("Evangelist".into());
    }
    if flags & BADGE_TEAM_BUILDER != 0 {
        out.push("Team Builder".into());
    }
    if flags & BADGE_EARLY_ADOPTER != 0 {
        out.push("Early Adopter".into());
    }
    if flags & BADGE_CONTRIBUTOR != 0 {
        out.push("Contributor".into());
    }
    out
}

fn internal_error<E: std::fmt::Display>(e: E) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}
