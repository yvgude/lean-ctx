//! Device overview (GL #387) — `/api/account/devices`.
//!
//! Every authenticated sync push may carry an `X-Device-Label` header (the
//! client's hostname). We upsert a `(user, label)` row as a fire-and-forget
//! side effect so the dashboard can show "which of my machines synced when".
//! Labels are display metadata only: they never participate in auth, quota,
//! or billing decisions, and a user can forget a device at any time.

use axum::Json;
use axum::extract::{Path as AxumPath, State};
use axum::http::{HeaderMap, StatusCode};
use serde::Serialize;
use uuid::Uuid;

use super::auth::{AppState, auth_user};
use super::helpers::internal_error;

/// Maximum accepted label length (characters). Hostnames are ≤253 by RFC but
/// anything beyond this is noise for a dashboard list.
const MAX_LABEL_CHARS: usize = 64;

/// Normalize a client-supplied device label: trim, cap length, and require
/// every char to be printable non-control. Returns `None` when nothing
/// usable remains — the push is then simply not tracked (never an error).
#[must_use]
pub(super) fn sanitize_label(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.chars().any(char::is_control) {
        return None;
    }
    Some(trimmed.chars().take(MAX_LABEL_CHARS).collect())
}

/// Record "this device just pushed `surface`" without blocking or failing the
/// actual sync: spawned as a background task, errors are logged and dropped.
pub(super) fn track(state: &AppState, user_id: Uuid, headers: &HeaderMap, surface: &'static str) {
    let Some(label) = headers
        .get("x-device-label")
        .and_then(|v| v.to_str().ok())
        .and_then(sanitize_label)
    else {
        return;
    };
    let state = state.clone();
    tokio::spawn(async move {
        let client = match state.pool.get().await {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!(error = %e, "device track: pool unavailable");
                return;
            }
        };
        if let Err(e) = client
            .execute(
                r"
INSERT INTO devices (user_id, device_label, first_seen, last_seen, last_surface, sync_count)
VALUES ($1, $2, NOW(), NOW(), $3, 1)
ON CONFLICT (user_id, device_label)
DO UPDATE SET last_seen = NOW(), last_surface = EXCLUDED.last_surface,
              sync_count = devices.sync_count + 1
",
                &[&user_id, &label, &surface],
            )
            .await
        {
            tracing::debug!(error = %e, "device track: upsert failed");
        }
    });
}

#[derive(Serialize)]
pub(super) struct DeviceRow {
    pub label: String,
    pub first_seen: String,
    pub last_seen: String,
    pub last_surface: Option<String>,
    pub sync_count: i64,
}

/// `GET /api/account/devices` — the authenticated user's device list, most
/// recently active first.
pub(super) async fn list_devices(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let (user_id, _email) = auth_user(&state, &headers).await?;
    let client = state.pool.get().await.map_err(internal_error)?;
    let rows = client
        .query(
            "SELECT device_label, first_seen, last_seen, last_surface, sync_count
             FROM devices WHERE user_id=$1 ORDER BY last_seen DESC LIMIT 50",
            &[&user_id],
        )
        .await
        .map_err(internal_error)?;

    let devices: Vec<DeviceRow> = rows
        .iter()
        .map(|r| {
            let first: chrono::DateTime<chrono::Utc> = r.get(1);
            let last: chrono::DateTime<chrono::Utc> = r.get(2);
            DeviceRow {
                label: r.get(0),
                first_seen: first.to_rfc3339(),
                last_seen: last.to_rfc3339(),
                last_surface: r.get(3),
                sync_count: r.get(4),
            }
        })
        .collect();

    Ok(Json(serde_json::json!({ "devices": devices })))
}

/// `DELETE /api/account/devices/{label}` — forget one device row. Idempotent:
/// deleting an unknown label still returns 200 (the end state is identical).
pub(super) async fn forget_device(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(label): AxumPath<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let (user_id, _email) = auth_user(&state, &headers).await?;
    let Some(label) = sanitize_label(&label) else {
        return Err((StatusCode::BAD_REQUEST, r#"{"error":"bad_label"}"#.into()));
    };
    let client = state.pool.get().await.map_err(internal_error)?;
    let n = client
        .execute(
            "DELETE FROM devices WHERE user_id=$1 AND device_label=$2",
            &[&user_id, &label],
        )
        .await
        .map_err(internal_error)?;

    Ok(Json(serde_json::json!({ "forgotten": n > 0 })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_trimmed_and_capped() {
        assert_eq!(sanitize_label("  mbp-yves  ").as_deref(), Some("mbp-yves"));
        let long = "x".repeat(200);
        assert_eq!(sanitize_label(&long).map(|l| l.chars().count()), Some(64));
    }

    #[test]
    fn empty_and_control_labels_are_rejected() {
        assert_eq!(sanitize_label(""), None);
        assert_eq!(sanitize_label("   "), None);
        assert_eq!(sanitize_label("host\nname"), None);
        assert_eq!(sanitize_label("bell\x07"), None);
    }

    #[test]
    fn unicode_hostnames_survive() {
        // macOS lets users name machines with arbitrary unicode.
        assert_eq!(
            sanitize_label("Yves' MacBook Pro").as_deref(),
            Some("Yves' MacBook Pro")
        );
        assert_eq!(
            sanitize_label("café-séjour").as_deref(),
            Some("café-séjour")
        );
    }
}
