use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde::{Deserialize, Serialize};

use super::auth::{auth_user, AppState};
use super::helpers::internal_error;

#[derive(Deserialize)]
pub struct CommandEntry {
    pub command: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub count: i64,
    #[serde(default)]
    pub input_tokens: i64,
    #[serde(default)]
    pub output_tokens: i64,
    #[serde(default)]
    pub tokens_saved: i64,
}

#[derive(Deserialize)]
pub struct CommandsEnvelope {
    pub commands: Vec<CommandEntry>,
}

#[derive(Serialize)]
pub struct CommandRow {
    pub command: String,
    pub source: String,
    pub count: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub tokens_saved: i64,
}

pub async fn post_commands(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CommandsEnvelope>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let (user_id, _) = auth_user(&state, &headers).await?;
    let client = state.pool.get().await.map_err(internal_error)?;

    for cmd in &body.commands {
        let command = cmd.command.trim();
        if command.is_empty() {
            continue;
        }
        let source = if cmd.source.is_empty() {
            "unknown"
        } else {
            &cmd.source
        };
        client
            .execute(
                r"INSERT INTO command_stats (user_id, command, source, count, input_tokens, output_tokens, tokens_saved)
                   VALUES ($1, $2, $3, $4, $5, $6, $7)
                   ON CONFLICT (user_id, command) DO UPDATE SET
                     source = EXCLUDED.source,
                     count = EXCLUDED.count,
                     input_tokens = EXCLUDED.input_tokens,
                     output_tokens = EXCLUDED.output_tokens,
                     tokens_saved = EXCLUDED.tokens_saved,
                     updated_at = NOW()",
                &[
                    &user_id,
                    &command,
                    &source,
                    &cmd.count,
                    &cmd.input_tokens,
                    &cmd.output_tokens,
                    &cmd.tokens_saved,
                ],
            )
            .await
            .map_err(internal_error)?;
    }

    Ok(Json(serde_json::json!({"synced": body.commands.len()})))
}

pub async fn get_commands(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<CommandRow>>, (StatusCode, String)> {
    let (user_id, _) = auth_user(&state, &headers).await?;
    let client = state.pool.get().await.map_err(internal_error)?;

    let rows = client
        .query(
            r"SELECT command, source, count, input_tokens, output_tokens, tokens_saved
               FROM command_stats WHERE user_id = $1
               ORDER BY tokens_saved DESC LIMIT 200",
            &[&user_id],
        )
        .await
        .map_err(internal_error)?;

    let result: Vec<CommandRow> = rows
        .iter()
        .map(|r| CommandRow {
            command: r.get(0),
            source: r.get(1),
            count: r.get(2),
            input_tokens: r.get(3),
            output_tokens: r.get(4),
            tokens_saved: r.get(5),
        })
        .collect();

    Ok(Json(result))
}
