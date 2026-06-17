use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::auth::AppState;
use super::billing_edge::require_cloud_sync;
use super::helpers::internal_error;

/// Vault blobs are tiny compared to index bundles; 8 MB is ~two orders of
/// magnitude above the largest observed knowledge store.
const MAX_VAULT_BYTES: usize = 8 * 1024 * 1024;

#[derive(Deserialize)]
pub(super) struct KnowledgeEnvelope {
    pub entries: Vec<IncomingEntry>,
}

#[derive(Deserialize)]
pub(super) struct IncomingEntry {
    pub category: String,
    pub key: String,
    pub value: String,
    #[serde(rename = "updated_by")]
    pub _updated_by: Option<String>,
    #[serde(rename = "updated_at")]
    pub _updated_at: Option<String>,
}

#[derive(Serialize)]
pub(super) struct OutEntry {
    pub category: String,
    pub key: String,
    pub value: String,
    pub updated_by: String,
    pub updated_at: String,
}

/// `POST /api/sync/knowledge` — two wire formats on one route (GL #467):
///
/// - `application/octet-stream`: the zero-knowledge vault path. The body is
///   client-side XChaCha20-Poly1305 ciphertext; the server stores it opaquely
///   and **deletes the account's legacy plaintext rows** (the vault is a full
///   snapshot, so this is the re-encryption migration).
/// - `application/json` (legacy, deprecated): plaintext entry upserts —
///   kept so pre-vault clients keep working until they upgrade.
pub(super) async fn post_knowledge(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let (user_id, email) = require_cloud_sync(&state, &headers).await?;
    super::devices::track(&state, user_id, &headers, "knowledge");

    let content_type = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if content_type.starts_with("application/octet-stream") {
        return post_vault(&state, user_id, &headers, &body).await;
    }

    let env: KnowledgeEnvelope = serde_json::from_slice(&body)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("invalid JSON body: {e}")))?;
    let mut synced = 0i64;
    for e in env.entries {
        if e.key.trim().is_empty() {
            continue;
        }
        upsert(&state, user_id, &e.category, &e.key, &e.value).await?;
        synced += 1;
    }
    Ok(Json(
        serde_json::json!({ "synced": synced, "updated_by": email }),
    ))
}

/// Store the encrypted vault blob. Zero-content logging: sizes and hashes
/// only, never payloads (the payload is ciphertext anyway — defense in depth).
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

    // Client-declared display metadatum — the server cannot count encrypted
    // entries, and that is the point.
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
INSERT INTO knowledge_blobs (user_id, blob, entry_count, sha256, updated_at)
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
        .execute(
            "DELETE FROM knowledge_entries WHERE user_id=$1",
            &[&user_id],
        )
        .await
        .map_err(internal_error)?;

    tracing::info!(
        %user_id,
        size_bytes = body.len(),
        entry_count,
        purged_plaintext_rows = purged,
        "knowledge vault stored"
    );
    Ok(Json(serde_json::json!({
        "stored": true,
        "size_bytes": body.len(),
        "entry_count": entry_count,
        "sha256": sha,
        "purged_plaintext_rows": purged,
    })))
}

/// `GET /api/sync/knowledge` — content negotiation (GL #467):
/// `Accept: application/octet-stream` returns the encrypted vault blob
/// (404 when the account has none yet); anything else serves the legacy
/// plaintext listing so pre-vault clients keep working.
pub(super) async fn get_knowledge(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, (StatusCode, String)> {
    let (user_id, email) = require_cloud_sync(&state, &headers).await?;

    let wants_vault = headers
        .get(axum::http::header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("application/octet-stream"));
    if wants_vault {
        let client = state.pool.get().await.map_err(internal_error)?;
        let row = client
            .query_opt(
                "SELECT blob FROM knowledge_blobs WHERE user_id=$1",
                &[&user_id],
            )
            .await
            .map_err(internal_error)?
            .ok_or((
                StatusCode::NOT_FOUND,
                "no knowledge vault for this account yet".to_string(),
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
            "SELECT category, key, value, updated_at FROM knowledge_entries WHERE user_id=$1 ORDER BY updated_at DESC",
            &[&user_id],
        )
        .await
        .map_err(internal_error)?;

    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let updated_at: DateTime<Utc> = r.get(3);
        out.push(OutEntry {
            category: r.get(0),
            key: r.get(1),
            value: r.get(2),
            updated_by: email.clone(),
            updated_at: updated_at.to_rfc3339(),
        });
    }
    Ok(Json(out).into_response())
}

async fn upsert(
    state: &AppState,
    user_id: Uuid,
    category: &str,
    key: &str,
    value: &str,
) -> Result<(), (StatusCode, String)> {
    let client = state.pool.get().await.map_err(internal_error)?;
    client
        .execute(
            r"
INSERT INTO knowledge_entries (user_id, category, key, value, updated_at)
VALUES ($1,$2,$3,$4, NOW())
ON CONFLICT (user_id, category, key)
DO UPDATE SET value=EXCLUDED.value, updated_at=NOW()
",
            &[&user_id, &category, &key, &value],
        )
        .await
        .map_err(internal_error)?;
    Ok(())
}
