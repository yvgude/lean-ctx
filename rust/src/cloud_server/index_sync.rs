//! Hosted Personal Index buckets (GL #392) — `/api/sync/index*`.
//!
//! Stores one client-side-encrypted bundle per (account, project). The server
//! never sees plaintext: bundles are XChaCha20-Poly1305 ciphertext whose key
//! is HKDF-derived from the account API key, which this backend only stores
//! as a SHA-256 hash. Handlers log sizes and hashes — never payloads
//! (zero-content logging). Contract: `docs/contracts/hosted-personal-index-v1.md`.
//!
//! Quota is **display-first**: a push over quota returns `413 quota_exceeded`
//! with the current usage — it warns and blocks, it never bills.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::auth::AppState;
use super::billing_edge::{hosted_index_quota_mb, require_cloud_sync};
use super::helpers::internal_error;

/// Per-bundle ceiling, enforced via the route-level body limit and re-checked
/// here (defense in depth).
pub(super) const MAX_BUNDLE_BYTES: usize = 64 * 1024 * 1024;

/// Bucket names are vector-namespace hashes: 32 lowercase hex chars (MD5 of
/// the project identity). Anything else is rejected before touching the DB.
fn valid_project_hash(s: &str) -> bool {
    s.len() == 32
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    let digest = h.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        use std::fmt::Write;
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Coarse quota-threshold block, aligned with `billing-plane-v2`'s
/// `StorageMetering` semantics (`lean-ctx-cloud/src/metering.rs`): `none`
/// (no entitlement) → `ok` → `warn` (≥ 50 %) → `critical` (≥ 80 %) → `over`
/// (≥ 100 %). Display-first: this block informs, the only enforcement is the
/// push block in [`put_bundle`].
fn quota_state_block(used_bytes: i64, quota_bytes: i64) -> serde_json::Value {
    if quota_bytes <= 0 {
        return json!({
            "used_bytes": used_bytes,
            "quota_bytes": 0,
            "overage_bytes": 0,
            "percent": serde_json::Value::Null,
            "state": "none",
        });
    }
    let percent = used_bytes as f64 / quota_bytes as f64 * 100.0;
    let state = if percent >= 100.0 {
        "over"
    } else if percent >= 80.0 {
        "critical"
    } else if percent >= 50.0 {
        "warn"
    } else {
        "ok"
    };
    json!({
        "used_bytes": used_bytes,
        "quota_bytes": quota_bytes,
        "overage_bytes": used_bytes.saturating_sub(quota_bytes),
        "percent": (percent * 100.0).round() / 100.0,
        "state": state,
    })
}

async fn account_usage_bytes(state: &AppState, user_id: Uuid) -> Result<i64, (StatusCode, String)> {
    let client = state.pool.get().await.map_err(internal_error)?;
    let row = client
        .query_one(
            "SELECT COALESCE(SUM(size_bytes), 0)::BIGINT FROM index_bundles WHERE user_id=$1",
            &[&user_id],
        )
        .await
        .map_err(internal_error)?;
    Ok(row.get(0))
}

/// `PUT /api/sync/index/{project_hash}` — upsert the encrypted bundle.
pub(super) async fn put_bundle(
    State(state): State<AppState>,
    Path(project_hash): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let (user_id, _email) = require_cloud_sync(&state, &headers).await?;
    super::devices::track(&state, user_id, &headers, "index");
    if !valid_project_hash(&project_hash) {
        return Err((StatusCode::BAD_REQUEST, "invalid project hash".into()));
    }
    if body.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "empty bundle".into()));
    }
    if body.len() > MAX_BUNDLE_BYTES {
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            format!(
                "bundle exceeds the {} MB per-bundle limit",
                MAX_BUNDLE_BYTES / 1_048_576
            ),
        ));
    }

    // Quota check (account-wide, replacing this project's previous bundle).
    let quota_mb = hosted_index_quota_mb(&state, user_id).await;
    let quota_bytes = i64::from(quota_mb) * 1_000_000;
    let used = account_usage_bytes(&state, user_id).await?;
    let client = state.pool.get().await.map_err(internal_error)?;
    let existing: i64 = client
        .query_opt(
            "SELECT size_bytes FROM index_bundles WHERE user_id=$1 AND project_hash=$2",
            &[&user_id, &project_hash],
        )
        .await
        .map_err(internal_error)?
        .map_or(0, |r| r.get(0));
    let projected = used - existing + body.len() as i64;
    if projected > quota_bytes {
        // Display-first: block the push, bill nothing, tell the user exactly
        // where they stand.
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            json!({
                "error": "quota_exceeded",
                "used_bytes": used,
                "quota_mb": quota_mb,
                "bundle_bytes": body.len(),
                "storage": quota_state_block(projected, quota_bytes),
            })
            .to_string(),
        ));
    }

    let sha = sha256_hex(&body);
    let size = body.len() as i64;
    client
        .execute(
            r"
INSERT INTO index_bundles (user_id, project_hash, bytes, size_bytes, sha256, updated_at)
VALUES ($1,$2,$3,$4,$5, NOW())
ON CONFLICT (user_id, project_hash)
DO UPDATE SET bytes=EXCLUDED.bytes, size_bytes=EXCLUDED.size_bytes,
              sha256=EXCLUDED.sha256, updated_at=NOW()
",
            &[&user_id, &project_hash, &body.as_ref(), &size, &sha],
        )
        .await
        .map_err(internal_error)?;

    tracing::info!(
        %user_id,
        project = %project_hash,
        size_bytes = size,
        "index bundle stored"
    );
    Ok(Json(
        json!({ "stored": true, "size_bytes": size, "sha256": sha }),
    ))
}

/// `GET /api/sync/index/{project_hash}` — download the encrypted bundle.
pub(super) async fn get_bundle(
    State(state): State<AppState>,
    Path(project_hash): Path<String>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let (user_id, _email) = require_cloud_sync(&state, &headers).await?;
    if !valid_project_hash(&project_hash) {
        return Err((StatusCode::BAD_REQUEST, "invalid project hash".into()));
    }
    let client = state.pool.get().await.map_err(internal_error)?;
    let row = client
        .query_opt(
            "SELECT bytes FROM index_bundles WHERE user_id=$1 AND project_hash=$2",
            &[&user_id, &project_hash],
        )
        .await
        .map_err(internal_error)?
        .ok_or((
            StatusCode::NOT_FOUND,
            "no hosted index for this project".to_string(),
        ))?;
    let bytes: Vec<u8> = row.get(0);
    Ok((
        [(axum::http::header::CONTENT_TYPE, "application/octet-stream")],
        bytes,
    ))
}

/// `GET /api/sync/index` — bucket listing + quota usage.
pub(super) async fn list_bundles(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let (user_id, _email) = require_cloud_sync(&state, &headers).await?;
    let quota_mb = hosted_index_quota_mb(&state, user_id).await;
    let client = state.pool.get().await.map_err(internal_error)?;
    let rows = client
        .query(
            "SELECT project_hash, size_bytes, sha256, updated_at
             FROM index_bundles WHERE user_id=$1 ORDER BY updated_at DESC",
            &[&user_id],
        )
        .await
        .map_err(internal_error)?;

    let mut used: i64 = 0;
    let projects: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            let size: i64 = r.get(1);
            used += size;
            let updated_at: chrono::DateTime<chrono::Utc> = r.get(3);
            json!({
                "project_hash": r.get::<_, String>(0),
                "size_bytes": size,
                "sha256": r.get::<_, String>(2),
                "updated_at": updated_at.to_rfc3339(),
            })
        })
        .collect();

    Ok(Json(json!({
        "used_bytes": used,
        "quota_mb": quota_mb,
        "projects": projects,
        // billing-plane-v2-aligned threshold view (same block the team server's
        // /v1/usage carries) — lets clients warn before a push bounces.
        "storage": quota_state_block(used, i64::from(quota_mb) * 1_000_000),
    })))
}

/// `DELETE /api/sync/index/{project_hash}` — free the bucket.
pub(super) async fn delete_bundle(
    State(state): State<AppState>,
    Path(project_hash): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, (StatusCode, String)> {
    let (user_id, _email) = require_cloud_sync(&state, &headers).await?;
    if !valid_project_hash(&project_hash) {
        return Err((StatusCode::BAD_REQUEST, "invalid project hash".into()));
    }
    let client = state.pool.get().await.map_err(internal_error)?;
    let n = client
        .execute(
            "DELETE FROM index_bundles WHERE user_id=$1 AND project_hash=$2",
            &[&user_id, &project_hash],
        )
        .await
        .map_err(internal_error)?;
    if n == 0 {
        return Err((StatusCode::NOT_FOUND, "no such bundle".into()));
    }
    tracing::info!(%user_id, project = %project_hash, "index bundle deleted");
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::valid_project_hash;

    #[test]
    fn project_hash_validation_accepts_md5_hex_only() {
        assert!(valid_project_hash("0123456789abcdef0123456789abcdef"));
        // Too short / long, uppercase, path traversal, non-hex.
        assert!(!valid_project_hash("0123456789abcdef"));
        assert!(!valid_project_hash("0123456789ABCDEF0123456789ABCDEF"));
        assert!(!valid_project_hash("../../../etc/passwd-0123456789ab"));
        assert!(!valid_project_hash("0123456789abcdef0123456789abcdeg"));
        assert!(!valid_project_hash(""));
    }

    /// Threshold semantics must mirror billing-plane-v2's `StorageMetering`
    /// (warn ≥ 50 %, critical ≥ 80 %, over ≥ 100 %, `none` for zero quota) so
    /// Pro and Team surfaces tell one consistent story.
    #[test]
    fn quota_state_block_mirrors_billing_plane_v2_thresholds() {
        let quota = 1_000_000_000_i64; // 1 GB
        for (used, want) in [
            (0_i64, "ok"),
            (499_999_999, "ok"),
            (500_000_000, "warn"),
            (799_999_999, "warn"),
            (800_000_000, "critical"),
            (999_999_999, "critical"),
            (1_000_000_000, "over"),
            (1_500_000_000, "over"),
        ] {
            let block = super::quota_state_block(used, quota);
            assert_eq!(block["state"], want, "used={used}");
            assert_eq!(block["quota_bytes"], quota);
        }
        // Overage figure only appears past the quota.
        let over = super::quota_state_block(1_250_000_000, quota);
        assert_eq!(over["overage_bytes"], 250_000_000_i64);

        // Zero quota = no entitlement: distinct `none` state, null percent.
        let none = super::quota_state_block(123, 0);
        assert_eq!(none["state"], "none");
        assert!(none["percent"].is_null());
    }
}
