use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::Serialize;

use super::auth::AppState;

#[derive(Serialize)]
pub struct LeaderboardEntry {
    pub rank: i64,
    pub display_hash: String,
    pub username: Option<String>,
    pub total_tokens_saved: i64,
    pub badge_flags: i32,
    pub badges: Vec<String>,
}

#[derive(Serialize)]
pub struct TeamLeaderboardEntry {
    pub rank: i64,
    pub team_name: String,
    pub total_tokens_saved: i64,
    pub member_count: i64,
}

pub async fn get_leaderboard(
    State(state): State<AppState>,
) -> Result<Json<Vec<LeaderboardEntry>>, (StatusCode, String)> {
    let client = state.pool.get().await.map_err(internal_error)?;

    let rows = client
        .query(
            r#"
SELECT display_hash, username, total_tokens_saved, badge_flags
FROM user_profiles
WHERE total_tokens_saved > 0
ORDER BY total_tokens_saved DESC
LIMIT 50
"#,
            &[],
        )
        .await
        .map_err(internal_error)?;

    let entries: Vec<LeaderboardEntry> = rows
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let flags: i32 = r.get(3);
            LeaderboardEntry {
                rank: (i + 1) as i64,
                display_hash: r.get(0),
                username: r.get(1),
                total_tokens_saved: r.get(2),
                badge_flags: flags,
                badges: badge_names(flags),
            }
        })
        .collect();

    Ok(Json(entries))
}

pub async fn get_team_leaderboard(
    State(state): State<AppState>,
) -> Result<Json<Vec<TeamLeaderboardEntry>>, (StatusCode, String)> {
    let client = state.pool.get().await.map_err(internal_error)?;

    let rows = client
        .query(
            r#"
SELECT t.name,
       COALESCE(SUM(p.total_tokens_saved), 0) as team_saved,
       COUNT(p.user_id) as member_count
FROM teams t
LEFT JOIN user_profiles p ON p.team_id = t.id
GROUP BY t.id, t.name
HAVING COALESCE(SUM(p.total_tokens_saved), 0) > 0
ORDER BY team_saved DESC
LIMIT 20
"#,
            &[],
        )
        .await
        .map_err(internal_error)?;

    let entries: Vec<TeamLeaderboardEntry> = rows
        .iter()
        .enumerate()
        .map(|(i, r)| TeamLeaderboardEntry {
            rank: (i + 1) as i64,
            team_name: r.get(0),
            total_tokens_saved: r.get(1),
            member_count: r.get(2),
        })
        .collect();

    Ok(Json(entries))
}

fn badge_names(flags: i32) -> Vec<String> {
    let mut out = vec![];
    if flags & (1 << 0) != 0 { out.push("First Save".into()); }
    if flags & (1 << 1) != 0 { out.push("Token Saver 1M".into()); }
    if flags & (1 << 2) != 0 { out.push("Token Saver 10M".into()); }
    if flags & (1 << 3) != 0 { out.push("Token Saver 100M".into()); }
    if flags & (1 << 4) != 0 { out.push("Evangelist".into()); }
    if flags & (1 << 5) != 0 { out.push("Team Builder".into()); }
    if flags & (1 << 6) != 0 { out.push("Early Adopter".into()); }
    if flags & (1 << 7) != 0 { out.push("Contributor".into()); }
    out
}

fn internal_error<E: std::fmt::Display>(e: E) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}
