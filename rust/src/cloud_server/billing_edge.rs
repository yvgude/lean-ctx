//! Edge client to the private commercial control-plane (`lean-ctx-cloud`).
//!
//! This is the *only* place the open community backend learns an account's paid
//! plan. It calls the private billing service's `/api/billing/entitlements`
//! endpoint with the shared internal key. If the billing service is not
//! configured or unreachable, every account resolves to
//! [`Plan::Free`](crate::core::billing::Plan) — so the open backend runs fully
//! standalone and **no local capability is ever gated** (Local-Free Invariant).

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::core::billing::Plan;

use super::auth::{auth_user, AppState};
use super::config::Config;

/// Resolve a user's effective plan via the private billing service. Any failure
/// (unconfigured, network error, bad response) degrades gracefully to
/// [`Plan::Free`] — the safe default that grants no commercial entitlements.
pub(super) async fn resolve_plan(cfg: &Config, user_id: Uuid) -> Plan {
    let (Some(base), Some(key)) = (
        cfg.billing_base_url.clone(),
        cfg.billing_internal_key.clone(),
    ) else {
        return Plan::Free;
    };

    let url = format!("{base}/api/billing/entitlements/{user_id}");
    let body = tokio::task::spawn_blocking(move || {
        ureq::get(&url)
            .header("X-Internal-Key", &key)
            .call()
            .ok()?
            .into_body()
            .read_to_string()
            .ok()
    })
    .await
    .ok()
    .flatten();

    let Some(body) = body else { return Plan::Free };
    serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|v| v.get("plan").and_then(Value::as_str).map(Plan::parse))
        .unwrap_or(Plan::Free)
}

/// `GET /api/account/entitlements` — the logged-in user's plan and the
/// additive Team/Cloud entitlements it grants.
pub(super) async fn get_account_entitlements(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, String)> {
    let (user_id, _email) = auth_user(&state, &headers).await?;
    let plan = resolve_plan(&state.cfg, user_id).await;
    Ok(Json(json!({
        "plan": plan.as_str(),
        "entitlements": plan.entitlements(),
    })))
}

/// Authenticated server-to-server POST to the private billing service. The shared
/// internal key never leaves the backend. Returns the parsed JSON body, or a
/// `503` when billing is not enabled / `502` when the upstream is unreachable.
async fn billing_post(
    cfg: &Config,
    path: &str,
    payload: Value,
) -> Result<Value, (StatusCode, String)> {
    let (Some(base), Some(key)) = (
        cfg.billing_base_url.clone(),
        cfg.billing_internal_key.clone(),
    ) else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "billing is not enabled on this deployment".to_string(),
        ));
    };

    let url = format!("{base}{path}");
    let bytes = serde_json::to_vec(&payload)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("encode: {e}")))?;

    let text = tokio::task::spawn_blocking(move || {
        ureq::post(&url)
            .header("X-Internal-Key", &key)
            .header("Content-Type", "application/json")
            .send(&bytes)
            .map_err(|e| e.to_string())?
            .into_body()
            .read_to_string()
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("join: {e}")))?
    .map_err(|e| (StatusCode::BAD_GATEWAY, format!("billing upstream: {e}")))?;

    serde_json::from_str::<Value>(&text).map_err(|e| {
        (
            StatusCode::BAD_GATEWAY,
            format!("billing returned non-JSON: {e}"),
        )
    })
}

/// Request body for `POST /api/account/checkout`. `interval` defaults to monthly
/// on the billing side when omitted.
#[derive(Deserialize)]
pub(super) struct CheckoutBody {
    plan: String,
    #[serde(default)]
    interval: Option<String>,
}

/// `POST /api/account/checkout` — start a Stripe Checkout session for the
/// logged-in user and return the hosted `url` to redirect to.
pub(super) async fn post_account_checkout(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CheckoutBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let (user_id, email) = auth_user(&state, &headers).await?;
    let payload = json!({
        "user_id": user_id,
        "email": email,
        "plan": body.plan,
        "interval": body.interval,
    });
    Ok(Json(
        billing_post(&state.cfg, "/api/billing/checkout", payload).await?,
    ))
}

/// `POST /api/account/portal` — open the Stripe billing portal for the logged-in
/// user (manage payment method, invoices, cancel) and return the redirect `url`.
pub(super) async fn post_account_portal(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, String)> {
    let (user_id, _email) = auth_user(&state, &headers).await?;
    let payload = json!({ "user_id": user_id });
    Ok(Json(
        billing_post(&state.cfg, "/api/billing/portal", payload).await?,
    ))
}

// ── Hosted Team server dashboard ──────────────────────────────────────────────
//
// Thin, status-preserving proxies to the private plane's team control endpoints.
// The shared internal key never reaches the browser; the caller is identified by
// their session, so the dashboard can only ever act on its own team instance.

/// Forward a team control call to the private plane, preserving the upstream
/// status so the dashboard can surface 404 (no instance yet) / 400 (seat limit)
/// distinctly. Errors only for unset billing (503) or an unreachable plane (502).
async fn billing_forward(
    cfg: &Config,
    method: &'static str,
    path: String,
    payload: Option<Value>,
) -> Result<(StatusCode, Value), (StatusCode, String)> {
    let (Some(base), Some(key)) = (
        cfg.billing_base_url.clone(),
        cfg.billing_internal_key.clone(),
    ) else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "billing is not enabled on this deployment".to_string(),
        ));
    };

    let url = format!("{base}{path}");
    let (code, text) = tokio::task::spawn_blocking(move || -> Result<(u16, String), String> {
        // Read non-2xx as a normal response so the upstream status is preserved.
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .into();
        let resp = match method {
            "GET" => agent.get(&url).header("X-Internal-Key", &key).call(),
            "DELETE" => agent.delete(&url).header("X-Internal-Key", &key).call(),
            _ => {
                let bytes = serde_json::to_vec(&payload.unwrap_or_else(|| json!({})))
                    .map_err(|e| e.to_string())?;
                agent
                    .post(&url)
                    .header("X-Internal-Key", &key)
                    .header("Content-Type", "application/json")
                    .send(&bytes)
            }
        }
        .map_err(|e| e.to_string())?;
        let code = resp.status().as_u16();
        let body = resp
            .into_body()
            .read_to_string()
            .map_err(|e| e.to_string())?;
        Ok((code, body))
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("join: {e}")))?
    .map_err(|e| (StatusCode::BAD_GATEWAY, format!("billing upstream: {e}")))?;

    let json = serde_json::from_str::<Value>(&text).unwrap_or(Value::Null);
    let status = StatusCode::from_u16(code).unwrap_or(StatusCode::BAD_GATEWAY);
    Ok((status, json))
}

/// Turn a forwarded `(status, body)` into a handler result, propagating the
/// upstream error message on non-2xx so the dashboard can display it.
fn finish(status: StatusCode, json: Value) -> Result<Json<Value>, (StatusCode, String)> {
    if status.is_success() {
        return Ok(Json(json));
    }
    let msg = json
        .get("error")
        .and_then(Value::as_str)
        .or_else(|| json.get("message").and_then(Value::as_str))
        .unwrap_or("team request failed")
        .to_string();
    Err((status, msg))
}

/// Request body for issuing a team member token.
#[derive(Deserialize)]
pub(super) struct MemberBody {
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    label: Option<String>,
}

/// `GET /api/account/team` — the logged-in owner's hosted team server status and
/// member roster (no secrets). `provisioned:false` until a Team plan deploys one.
pub(super) async fn get_account_team(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, String)> {
    let (user_id, _email) = auth_user(&state, &headers).await?;
    let (status, json) = billing_forward(
        &state.cfg,
        "GET",
        format!("/api/billing/team/{user_id}"),
        None,
    )
    .await?;
    finish(status, json)
}

/// `POST /api/account/team/owner-token` — (re)issue the owner token, returned
/// exactly once. Rotates any prior owner credential and redeploys the server.
pub(super) async fn post_account_team_owner_token(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, String)> {
    let (user_id, _email) = auth_user(&state, &headers).await?;
    let (status, json) = billing_forward(
        &state.cfg,
        "POST",
        format!("/api/billing/team/{user_id}/owner-token"),
        Some(json!({})),
    )
    .await?;
    finish(status, json)
}

/// `POST /api/account/team/members` — issue a seat-limited member token (returned
/// once). 400 from the plane when the plan's seat limit is reached.
pub(super) async fn post_account_team_member(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<MemberBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let (user_id, _email) = auth_user(&state, &headers).await?;
    let (status, json) = billing_forward(
        &state.cfg,
        "POST",
        format!("/api/billing/team/{user_id}/tokens"),
        Some(json!({ "role": body.role, "label": body.label })),
    )
    .await?;
    finish(status, json)
}

/// `DELETE /api/account/team/members/{token_id}` — revoke a member token and
/// redeploy. The owner token cannot be revoked (the plane rejects it).
pub(super) async fn delete_account_team_member(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(token_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let (user_id, _email) = auth_user(&state, &headers).await?;
    let (status, json) = billing_forward(
        &state.cfg,
        "DELETE",
        format!("/api/billing/team/{user_id}/tokens/{token_id}"),
        None,
    )
    .await?;
    if status.is_success() {
        return Ok(Json(json!({ "revoked": true })));
    }
    finish(status, json)
}
