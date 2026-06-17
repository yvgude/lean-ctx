use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::auth::AppState;
use super::billing_edge::require_cloud_sync;
use super::helpers::internal_error;

/// Same ceiling as the knowledge vault — gotcha stores are tiny.
const MAX_VAULT_BYTES: usize = 8 * 1024 * 1024;

#[derive(Deserialize)]
pub(super) struct GotchaEntry {
    pub pattern: String,
    pub fix: String,
    #[serde(default)]
    pub severity: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub occurrences: i64,
    #[serde(default)]
    pub prevented_count: i64,
    #[serde(default)]
    pub confidence: Option<f64>,
}

#[derive(Deserialize)]
pub(super) struct GotchasEnvelope {
    pub gotchas: Vec<GotchaEntry>,
}

#[derive(Serialize)]
pub(super) struct GotchaRow {
    pub pattern: String,
    pub fix: String,
    pub severity: Option<String>,
    pub category: Option<String>,
    pub occurrences: i64,
    pub prevented_count: i64,
    pub confidence: Option<f64>,
}

/// `POST /api/sync/gotchas` — two wire formats on one route (GL #467
/// follow-up, mirroring `/api/sync/knowledge`):
///
/// - `application/octet-stream`: the zero-knowledge vault path. The body is
///   client-side XChaCha20-Poly1305 ciphertext (HKDF domain
///   `gotcha-vault-v1`); the server stores it opaquely and **deletes the
///   account's legacy plaintext rows**.
/// - `application/json` (legacy, deprecated): plaintext upserts.
pub(super) async fn post_gotchas(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let (user_id, _) = require_cloud_sync(&state, &headers).await?;
    super::devices::track(&state, user_id, &headers, "gotchas");

    let content_type = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if content_type.starts_with("application/octet-stream") {
        return post_vault(&state, user_id, &headers, &body).await;
    }

    let body: GotchasEnvelope = serde_json::from_slice(&body)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("invalid JSON body: {e}")))?;
    let client = state.pool.get().await.map_err(internal_error)?;

    for g in &body.gotchas {
        let pattern = g.pattern.trim();
        if pattern.is_empty() {
            continue;
        }
        client
            .execute(
                r"INSERT INTO gotchas (user_id, pattern, fix, severity, category, occurrences, prevented_count, confidence)
                   VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
                   ON CONFLICT (user_id, pattern) DO UPDATE SET
                     fix = EXCLUDED.fix,
                     severity = EXCLUDED.severity,
                     category = EXCLUDED.category,
                     occurrences = EXCLUDED.occurrences,
                     prevented_count = EXCLUDED.prevented_count,
                     confidence = EXCLUDED.confidence,
                     updated_at = NOW()",
                &[
                    &user_id,
                    &pattern,
                    &g.fix,
                    &g.severity,
                    &g.category,
                    &g.occurrences,
                    &g.prevented_count,
                    &g.confidence,
                ],
            )
            .await
            .map_err(internal_error)?;
    }

    Ok(Json(serde_json::json!({"synced": body.gotchas.len()})))
}

/// Store the encrypted gotcha vault blob. Zero-content logging: sizes and
/// hashes only, never payloads.
async fn post_vault(
    state: &AppState,
    user_id: Uuid,
    headers: &HeaderMap,
    body: &Bytes,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if body.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "empty vault blob".into()));
    }
    if body.len() > MAX_VAULT_BYTES {
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("vault exceeds the {} MB limit", MAX_VAULT_BYTES / 1_048_576),
        ));
    }

    let entry_count: i64 = headers
        .get("x-entry-count")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
        .max(0);

    let mut h = Sha256::new();
    h.update(body);
    let sha = crate::core::agent_identity::hex_encode(&h.finalize());

    let client = state.pool.get().await.map_err(internal_error)?;
    client
        .execute(
            r"
INSERT INTO gotcha_blobs (user_id, blob, entry_count, sha256, updated_at)
VALUES ($1,$2,$3,$4, NOW())
ON CONFLICT (user_id)
DO UPDATE SET blob=EXCLUDED.blob, entry_count=EXCLUDED.entry_count,
              sha256=EXCLUDED.sha256, updated_at=NOW()
",
            &[&user_id, &body.as_ref(), &entry_count, &sha],
        )
        .await
        .map_err(internal_error)?;

    // Re-encryption migration: the vault snapshot supersedes every plaintext
    // row this account ever pushed.
    let purged = client
        .execute("DELETE FROM gotchas WHERE user_id=$1", &[&user_id])
        .await
        .map_err(internal_error)?;

    tracing::info!(
        %user_id,
        size_bytes = body.len(),
        entry_count,
        purged_plaintext_rows = purged,
        "gotcha vault stored"
    );
    Ok(Json(serde_json::json!({
        "stored": true,
        "size_bytes": body.len(),
        "entry_count": entry_count,
        "sha256": sha,
        "purged_plaintext_rows": purged,
    })))
}

/// `GET /api/sync/gotchas` — content negotiation like the knowledge route:
/// `Accept: application/octet-stream` returns the encrypted vault blob
/// (404 when the account has none); anything else serves the legacy listing.
pub(super) async fn get_gotchas(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, (StatusCode, String)> {
    let (user_id, _) = require_cloud_sync(&state, &headers).await?;

    let wants_vault = headers
        .get(axum::http::header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("application/octet-stream"));
    if wants_vault {
        let client = state.pool.get().await.map_err(internal_error)?;
        let row = client
            .query_opt(
                "SELECT blob FROM gotcha_blobs WHERE user_id=$1",
                &[&user_id],
            )
            .await
            .map_err(internal_error)?
            .ok_or((
                StatusCode::NOT_FOUND,
                "no gotcha vault for this account yet".to_string(),
            ))?;
        let bytes: Vec<u8> = row.get(0);
        return Ok((
            [(axum::http::header::CONTENT_TYPE, "application/octet-stream")],
            bytes,
        )
            .into_response());
    }
    let client = state.pool.get().await.map_err(internal_error)?;

    let rows = client
        .query(
            r"SELECT pattern, fix, severity, category, occurrences, prevented_count, confidence
               FROM gotchas WHERE user_id = $1
               ORDER BY prevented_count DESC, occurrences DESC LIMIT 200",
            &[&user_id],
        )
        .await
        .map_err(internal_error)?;

    let result: Vec<GotchaRow> = rows
        .iter()
        .map(|r| GotchaRow {
            pattern: r.get(0),
            fix: r.get(1),
            severity: r.get(2),
            category: r.get(3),
            occurrences: r.get(4),
            prevented_count: r.get(5),
            confidence: r.get(6),
        })
        .collect();

    Ok(Json(result).into_response())
}
