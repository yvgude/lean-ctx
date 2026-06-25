//! Edge client to the private commercial control-plane (`lean-ctx-cloud`).
//!
//! This is the *only* place the open community backend learns an account's paid
//! plan. It calls the private billing service's `/api/billing/entitlements`
//! endpoint with the shared internal key. If the billing service is not
//! configured or unreachable, every account resolves to
//! [`Plan::Free`](crate::core::billing::Plan) — so the open backend runs fully
//! standalone and **no local capability is ever gated** (Local-Free Invariant).

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, PoisonError};
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::core::billing::Plan;

use super::auth::{AppState, auth_user};
use super::config::Config;

/// Resolve a user's effective plan via the private billing service. Any failure
/// (unconfigured, network error, bad response) degrades gracefully to
/// [`Plan::Free`] — the safe default that grants no commercial entitlements.
pub(super) async fn resolve_plan(cfg: &Config, user_id: Uuid) -> Plan {
    resolve_entitlements_raw(cfg, user_id)
        .await
        .and_then(|v| v.get("plan").and_then(Value::as_str).map(Plan::parse))
        .unwrap_or(Plan::Free)
}

// ── Entitlements cache (GL #785) ──────────────────────────────────────────────
//
// A per-user, in-memory cache of the billing plane's entitlements payload. It
// exists for one reason: a brief billing-service outage must never downgrade a
// *paying* account. Without it, `resolve_entitlements_raw` degrades to `Free` on
// any failure, so a single blip would 402 paying Pro users on every
// `/api/sync/*` request (fail-closed against people who pay us).
//
// Policy (mirrors the supporters-wall cache below):
// - fresh within `ENTITLEMENTS_CACHE_TTL` ⇒ serve cached (also shields the plane
//   from per-request traffic),
// - otherwise refetch; on success refresh the slot,
// - on upstream failure ⇒ serve the last value regardless of age (a stale plan
//   beats a wrong downgrade). Only an account never seen before falls through to
//   `Free`, exactly as it did before this cache existed.
//
// Memory is bounded: once the map passes `ENTITLEMENTS_CACHE_MAX`, entries older
// than `ENTITLEMENTS_STALE_RETAIN` are pruned — they could only ever serve as a
// very old stale fallback.

/// How long a fetched entitlements payload counts as fresh.
const ENTITLEMENTS_CACHE_TTL: Duration = Duration::from_mins(1);
/// Soft cap on distinct cached accounts before pruning kicks in.
const ENTITLEMENTS_CACHE_MAX: usize = 50_000;
/// On overflow, evict entries older than this (kept only as stale fallback).
const ENTITLEMENTS_STALE_RETAIN: Duration = Duration::from_hours(1);

struct CachedEntitlements {
    at: Instant,
    value: Value,
}

type EntitlementsCacheSlot = Mutex<HashMap<Uuid, CachedEntitlements>>;
static ENTITLEMENTS_CACHE: LazyLock<EntitlementsCacheSlot> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// The cached payload for `user_id` if it was stored less than
/// `ENTITLEMENTS_CACHE_TTL` before `now`. `now` is injected so expiry is
/// unit-testable without sleeping.
fn entitlements_cache_fresh(
    slot: &EntitlementsCacheSlot,
    user_id: Uuid,
    now: Instant,
) -> Option<Value> {
    let guard = slot.lock().unwrap_or_else(PoisonError::into_inner);
    guard
        .get(&user_id)
        .filter(|e| now.duration_since(e.at) < ENTITLEMENTS_CACHE_TTL)
        .map(|e| e.value.clone())
}

/// The last cached payload for `user_id` regardless of age — the stale fallback
/// served when the billing plane is unreachable (a stale plan beats a wrong
/// downgrade to Free for a paying account).
fn entitlements_cache_any(slot: &EntitlementsCacheSlot, user_id: Uuid) -> Option<Value> {
    let guard = slot.lock().unwrap_or_else(PoisonError::into_inner);
    guard.get(&user_id).map(|e| e.value.clone())
}

/// Store a freshly fetched payload, restarting the TTL window at `now` and
/// pruning very old entries if the map has grown past its soft cap.
fn entitlements_cache_store(
    slot: &EntitlementsCacheSlot,
    user_id: Uuid,
    now: Instant,
    value: &Value,
) {
    let mut guard = slot.lock().unwrap_or_else(PoisonError::into_inner);
    guard.insert(
        user_id,
        CachedEntitlements {
            at: now,
            value: value.clone(),
        },
    );
    if guard.len() > ENTITLEMENTS_CACHE_MAX {
        prune_entitlements_cache(&mut guard, now);
    }
}

/// Drop entries older than `ENTITLEMENTS_STALE_RETAIN` relative to `now`. They
/// could only ever serve as a very old stale fallback, so evicting them bounds
/// memory without affecting fresh hits or recent stale fallbacks.
fn prune_entitlements_cache(map: &mut HashMap<Uuid, CachedEntitlements>, now: Instant) {
    map.retain(|_, e| now.duration_since(e.at) < ENTITLEMENTS_STALE_RETAIN);
}

/// The raw entitlements payload from the private billing service (plan,
/// entitlements, org membership — GL #468). Cached per user with a stale-on-error
/// fallback (GL #785), so a billing blip never downgrades a paying account.
/// `None` only when billing is unconfigured, or the account has never been seen
/// and the plane is currently unreachable — callers then degrade to
/// [`Plan::Free`] exactly like before.
async fn resolve_entitlements_raw(cfg: &Config, user_id: Uuid) -> Option<Value> {
    let (Some(base), Some(key)) = (
        cfg.billing_base_url.clone(),
        cfg.billing_internal_key.clone(),
    ) else {
        return None;
    };

    let now = Instant::now();
    if let Some(cached) = entitlements_cache_fresh(&ENTITLEMENTS_CACHE, user_id, now) {
        return Some(cached);
    }

    let url = format!("{base}/api/billing/entitlements/{user_id}");
    let fetched = tokio::task::spawn_blocking(move || {
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
    .flatten()
    .and_then(|body| serde_json::from_str::<Value>(&body).ok());

    match fetched {
        Some(value) => {
            entitlements_cache_store(&ENTITLEMENTS_CACHE, user_id, now, &value);
            Some(value)
        }
        // Billing unreachable / bad response: serve the last known plan so a blip
        // never downgrades a paying account. Never-seen accounts fall to Free.
        None => entitlements_cache_any(&ENTITLEMENTS_CACHE, user_id),
    }
}

/// Billing-side account deletion (GL #535): cancels any live subscription
/// immediately and purges the user's billing rows.
///
/// Tri-state by design:
/// - billing not configured → `Ok(None)` (nothing to delete — standalone deploy)
/// - billing reachable + 2xx → `Ok(Some(response))`
/// - anything else → `Err(502)` — the caller MUST abort the account deletion,
///   otherwise a paid subscription could keep charging a deleted account.
pub(super) async fn billing_delete_account(
    cfg: &Config,
    user_id: Uuid,
) -> Result<Option<Value>, (StatusCode, String)> {
    let (Some(base), Some(key)) = (
        cfg.billing_base_url.clone(),
        cfg.billing_internal_key.clone(),
    ) else {
        return Ok(None);
    };

    let url = format!("{base}/api/billing/account/{user_id}");
    let body = tokio::task::spawn_blocking(move || {
        ureq::delete(&url)
            .header("X-Internal-Key", &key)
            .call()
            .map_err(|e| e.to_string())?
            .into_body()
            .read_to_string()
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("join: {e}")))?
    .map_err(|e| {
        (
            StatusCode::BAD_GATEWAY,
            format!("billing deletion failed — account NOT deleted, please retry: {e}"),
        )
    })?;

    Ok(Some(serde_json::from_str(&body).unwrap_or(Value::Null)))
}

/// Whether this deployment leaves cloud sync **ungated** for everyone: either no
/// commercial plane is wired (`billing_base_url` unset), or the operator
/// explicitly opted out (`LEANCTX_CLOUD_SYNC_OPEN=1`). leanctx.com has neither,
/// so sync there is gated to the `cloud_sync` entitlement.
fn sync_is_open(cfg: &Config) -> bool {
    cfg.sync_open || cfg.billing_base_url.is_none()
}

/// Pure cloud-sync gate policy, factored out so it is unit-testable without a
/// DB/billing round-trip. Sync is allowed when the deployment does not gate it
/// at all, or when the caller's plan grants the `cloud_sync` entitlement
/// (Pro/Team/Enterprise). Free and Supporter are denied on a gated deployment.
pub(super) fn cloud_sync_allowed(cfg: &Config, plan: Plan) -> bool {
    sync_is_open(cfg) || plan.entitlements().cloud_sync
}

/// Authenticate the caller **and** require the `cloud_sync` entitlement before a
/// `/api/sync/*` handler proceeds. Returns the same `(user_id, email)` tuple as
/// [`auth_user`], so a call site is a drop-in swap.
///
/// Gating only applies where a commercial plane is actually wired:
/// - **No billing configured** (`billing_base_url` unset) ⇒ open. The community
///   backend runs standalone and sync stays fully usable — nothing is gated
///   without an explicit paid plane (Local-Free Invariant).
/// - **`LEANCTX_CLOUD_SYNC_OPEN=1`** ⇒ open. Operator opt-out for self-hosters
///   who run billing for other reasons but want sync free for everyone.
/// - **Otherwise** ⇒ the account must resolve to a plan whose entitlements grant
///   `cloud_sync` (Pro/Team/Enterprise). Free/Supporter get `402 Payment Required`.
pub(super) async fn require_cloud_sync(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<(Uuid, String), (StatusCode, String)> {
    let (user_id, email) = auth_user(state, headers).await?;

    // Resolve the paid plan only where sync is actually gated; open deployments
    // short-circuit without a billing round-trip. `Plan::Free` is a safe stand-in
    // there — `cloud_sync_allowed` returns `true` via its open checks regardless.
    let plan = if sync_is_open(&state.cfg) {
        Plan::Free
    } else {
        resolve_plan(&state.cfg, user_id).await
    };

    if cloud_sync_allowed(&state.cfg, plan) {
        return Ok((user_id, email));
    }

    Err((
        StatusCode::PAYMENT_REQUIRED,
        format!(
            "cloud sync requires lean-ctx Pro (current plan: {}). \
             Run `lean-ctx upgrade` to enable hosted cross-device sync.",
            plan.as_str()
        ),
    ))
}

/// The account's hosted-index quota in MB (GL #392). Paid plans use their
/// `hosted_index_mb` entitlement (Pro: 1000). Open deployments — no billing
/// plane wired, or sync explicitly opened — get a 1000 MB default so the
/// feature works standalone without ever paying (Local-Free Invariant: the
/// hosted bucket is additive, the local index is never gated).
pub(super) async fn hosted_index_quota_mb(state: &AppState, user_id: Uuid) -> u32 {
    if !sync_is_open(&state.cfg) {
        let quota = resolve_plan(&state.cfg, user_id)
            .await
            .entitlements()
            .hosted_index_mb;
        if quota > 0 {
            return quota;
        }
    }
    1_000
}

/// `GET /api/account/entitlements` — the logged-in user's plan, the additive
/// Team/Cloud entitlements it grants, and the org membership (GL #468) the
/// plan may be inherited through (`org: null` for solo accounts).
pub(super) async fn get_account_entitlements(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, String)> {
    let (user_id, _email) = auth_user(&state, &headers).await?;
    let raw = resolve_entitlements_raw(&state.cfg, user_id).await;
    let plan = raw
        .as_ref()
        .and_then(|v| v.get("plan").and_then(Value::as_str).map(Plan::parse))
        .unwrap_or(Plan::Free);
    let org = raw
        .as_ref()
        .and_then(|v| v.get("org").cloned())
        .unwrap_or(Value::Null);
    // Subscription lifecycle (GL #535): lets the dashboard show a scheduled
    // cancellation ("ends on July 10 — resume anytime") instead of silence.
    let subscription = raw
        .as_ref()
        .and_then(|v| v.get("subscription").cloned())
        .unwrap_or(Value::Null);
    Ok(Json(json!({
        "plan": plan.as_str(),
        "entitlements": plan.entitlements(),
        "org": org,
        "subscription": subscription,
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
            // Body methods (POST default, PATCH for partial updates, PUT for
            // full settings replacement). All carry the caller's JSON unchanged.
            _ => {
                let bytes = serde_json::to_vec(&payload.unwrap_or_else(|| json!({})))
                    .map_err(|e| e.to_string())?;
                let builder = match method {
                    "PATCH" => agent.patch(&url),
                    "PUT" => agent.put(&url),
                    _ => agent.post(&url),
                };
                builder
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

/// `GET /api/account/team/savings` — the logged-in owner's aggregated team
/// savings roll-up (net tokens + USD saved, per member and per model). Returns
/// `savings_available:false` until the hosted server has received at least one
/// signed batch, or `provisioned:false` when no team server exists yet.
pub(super) async fn get_account_team_savings(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, String)> {
    let (user_id, _email) = auth_user(&state, &headers).await?;
    let (status, json) = billing_forward(
        &state.cfg,
        "GET",
        format!("/api/billing/team/{user_id}/savings"),
        None,
    )
    .await?;
    finish(status, json)
}

/// `GET /api/account/team/savings/member/{signer}` — per-member drilldown
/// (GL #389): the signer's own 90-day cumulative series plus model/tool
/// breakdowns. 404 when the signer never reported a batch. The signer id is
/// the truncated public key from `summary.by_member[].signer` (URL-safe by
/// construction; anything else is rejected upstream).
pub(super) async fn get_account_team_savings_member(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(signer): axum::extract::Path<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let (user_id, _email) = auth_user(&state, &headers).await?;
    // Tight allowlist before the id is embedded in an upstream URL.
    if signer.is_empty()
        || signer.len() > 64
        || !signer
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err((StatusCode::BAD_REQUEST, "invalid signer id".into()));
    }
    let (status, json) = billing_forward(
        &state.cfg,
        "GET",
        format!("/api/billing/team/{user_id}/savings/member/{signer}"),
        None,
    )
    .await?;
    finish(status, json)
}

/// Internal GET against the billing plane for the digest job (GL #386) —
/// no user session involved, the job acts on the server's own behalf.
/// `None` when billing is unconfigured or unreachable.
pub(super) async fn forward_for_digest(cfg: &Config, path: String) -> Option<(u16, Value)> {
    match billing_forward(cfg, "GET", path, None).await {
        Ok((status, json)) => Some((status.as_u16(), json)),
        Err(_) => None,
    }
}

/// Request body for team settings (GL #388).
#[derive(Deserialize)]
pub(super) struct TeamSettingsBody {
    #[serde(default, rename = "roiWebhookUrl")]
    roi_webhook_url: Option<String>,
}

/// `PUT /api/account/team/settings` — owner-tunable team-server settings
/// (currently the weekly ROI webhook URL, GL #388). Validation and the
/// config re-render happen in the control plane; this edge only forwards.
pub(super) async fn put_account_team_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<TeamSettingsBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let (user_id, _email) = auth_user(&state, &headers).await?;
    let (status, json) = billing_forward(
        &state.cfg,
        "PUT",
        format!("/api/billing/team/{user_id}/settings"),
        Some(json!({ "roiWebhookUrl": body.roi_webhook_url })),
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

// ── Invite links (GL #385) ────────────────────────────────────────────────────

/// Request body for `POST /api/account/team/invites`.
#[derive(Deserialize)]
pub(super) struct InviteBody {
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    role: Option<String>,
}

/// `POST /api/account/team/invites` — mint a one-time invite link for the
/// logged-in owner's team. The code is returned exactly once; the dashboard
/// turns it into `https://leanctx.com/join/?code=…`.
pub(super) async fn post_account_team_invite(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<InviteBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let (user_id, _email) = auth_user(&state, &headers).await?;
    let (status, json) = billing_forward(
        &state.cfg,
        "POST",
        format!("/api/billing/team/{user_id}/invites"),
        Some(json!({ "label": body.label, "role": body.role })),
    )
    .await?;
    finish(status, json)
}

/// `GET /api/account/team/invites` — the owner's invite audit list
/// (pending / used / revoked / expired; never the codes themselves).
pub(super) async fn get_account_team_invites(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, String)> {
    let (user_id, _email) = auth_user(&state, &headers).await?;
    let (status, json) = billing_forward(
        &state.cfg,
        "GET",
        format!("/api/billing/team/{user_id}/invites"),
        None,
    )
    .await?;
    finish(status, json)
}

/// `DELETE /api/account/team/invites/{invite_id}` — revoke a pending invite.
pub(super) async fn delete_account_team_invite(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(invite_id): Path<Uuid>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let (user_id, _email) = auth_user(&state, &headers).await?;
    let (status, json) = billing_forward(
        &state.cfg,
        "DELETE",
        format!("/api/billing/team/{user_id}/invites/{invite_id}"),
        None,
    )
    .await?;
    if status == StatusCode::NO_CONTENT {
        return Ok(Json(json!({ "revoked": true })));
    }
    finish(status, json)
}

/// Forward an invite redemption to the control plane on behalf of the (login-
/// less) teammate. Used by the public join endpoint (`team_join.rs`).
pub(super) async fn forward_invite_redeem(
    cfg: &Config,
    code: &str,
) -> Result<(StatusCode, Value), (StatusCode, String)> {
    billing_forward(
        cfg,
        "POST",
        "/api/billing/invites/redeem".to_string(),
        Some(json!({ "code": code })),
    )
    .await
}

// ── Org SSO settings (GL #482) ────────────────────────────────────────────────

/// Request body for `PUT /api/account/org/sso`.
#[derive(Deserialize)]
pub(super) struct OrgSsoBody {
    email_domain: String,
    issuer: String,
    client_id: String,
    #[serde(default)]
    client_secret: Option<String>,
}

/// `GET /api/account/org/sso` — the caller's org SSO config (never the secret).
pub(super) async fn get_account_org_sso(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, String)> {
    let (user_id, _email) = auth_user(&state, &headers).await?;
    let (status, json) = billing_forward(
        &state.cfg,
        "GET",
        format!("/api/billing/org/{user_id}/sso"),
        None,
    )
    .await?;
    finish(status, json)
}

/// `PUT /api/account/org/sso` — create/update the org's `IdP` configuration.
pub(super) async fn put_account_org_sso(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<OrgSsoBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let (user_id, _email) = auth_user(&state, &headers).await?;
    let (status, json) = billing_forward(
        &state.cfg,
        "PUT",
        format!("/api/billing/org/{user_id}/sso"),
        Some(json!({
            "email_domain": body.email_domain,
            "issuer": body.issuer,
            "client_id": body.client_id,
            "client_secret": body.client_secret,
        })),
    )
    .await?;
    finish(status, json)
}

/// `POST /api/account/org/sso/verify` — run the DNS-TXT domain check.
pub(super) async fn post_account_org_sso_verify(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, String)> {
    let (user_id, _email) = auth_user(&state, &headers).await?;
    let (status, json) = billing_forward(
        &state.cfg,
        "POST",
        format!("/api/billing/org/{user_id}/sso/verify"),
        Some(json!({})),
    )
    .await?;
    finish(status, json)
}

/// Request body for `PUT /api/account/org/sso/required`.
#[derive(Deserialize)]
pub(super) struct OrgSsoRequiredBody {
    sso_required: bool,
}

/// `PUT /api/account/org/sso/required` — toggle SSO enforcement.
pub(super) async fn put_account_org_sso_required(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<OrgSsoRequiredBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let (user_id, _email) = auth_user(&state, &headers).await?;
    let (status, json) = billing_forward(
        &state.cfg,
        "PUT",
        format!("/api/billing/org/{user_id}/sso/required"),
        Some(json!({ "sso_required": body.sso_required })),
    )
    .await?;
    finish(status, json)
}

/// `DELETE /api/account/org/sso` — remove the `IdP` configuration.
pub(super) async fn delete_account_org_sso(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, String)> {
    let (user_id, _email) = auth_user(&state, &headers).await?;
    let (status, json) = billing_forward(
        &state.cfg,
        "DELETE",
        format!("/api/billing/org/{user_id}/sso"),
        None,
    )
    .await?;
    if status == StatusCode::NO_CONTENT {
        return Ok(Json(json!({ "removed": true })));
    }
    finish(status, json)
}

/// Read-side query for the org audit log (GL #484). Mirrors the control-plane
/// contract; all three are optional.
#[derive(Deserialize)]
pub(super) struct AuditQuery {
    #[serde(default)]
    before: Option<i64>,
    #[serde(default)]
    limit: Option<i64>,
    #[serde(default)]
    event: Option<String>,
}

/// Build the sanitized upstream query string. `before`/`limit` are numeric and
/// safe; `event` is allowlisted to our `snake_case` event ids so nothing
/// untrusted is ever spliced into the upstream URL.
fn build_audit_query(q: &AuditQuery) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(b) = q.before
        && b > 0
    {
        parts.push(format!("before={b}"));
    }
    if let Some(l) = q.limit {
        parts.push(format!("limit={}", l.clamp(1, 200)));
    }
    if let Some(ev) = q.event.as_deref() {
        let ev = ev.trim();
        if !ev.is_empty()
            && ev.len() <= 48
            && ev.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
        {
            parts.push(format!("event={ev}"));
        }
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("?{}", parts.join("&"))
    }
}

/// `GET /api/account/org/audit` — the owner's paginated governance audit log.
pub(super) async fn get_account_org_audit(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<AuditQuery>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let (user_id, _email) = auth_user(&state, &headers).await?;
    let qs = build_audit_query(&q);
    let (status, json) = billing_forward(
        &state.cfg,
        "GET",
        format!("/api/billing/org/{user_id}/audit{qs}"),
        None,
    )
    .await?;
    finish(status, json)
}

/// `GET /api/account/org/audit/export.csv` — the owner's audit log as a CSV
/// download. The control plane renders the CSV; this edge streams it through
/// with the right headers (the body is not JSON, so it bypasses `finish`).
pub(super) async fn get_account_org_audit_export(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, (StatusCode, String)> {
    let (user_id, _email) = auth_user(&state, &headers).await?;
    let (status, body) = billing_forward_text(
        &state.cfg,
        format!("/api/billing/org/{user_id}/audit/export.csv"),
    )
    .await?;
    if !status.is_success() {
        return Err((status, "audit export failed".to_string()));
    }
    Ok((
        [
            (header::CONTENT_TYPE, "text/csv; charset=utf-8"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"leanctx-audit-log.csv\"",
            ),
        ],
        body,
    )
        .into_response())
}

// ── ctxpkg registry publisher self-service (GL #406) ─────────────────────────
//
// Namespace + publish-token management for the logged-in account. Thin
// status-preserving proxies to the private plane; publish/download themselves
// never touch this edge — they go straight to the registry via ctxpkg.com.

/// Request body for `PUT /api/account/registry/namespace`.
#[derive(Deserialize)]
pub(super) struct RegistryNamespaceBody {
    namespace: String,
    /// Claim on behalf of an org (GL #524) — requires owner/admin there.
    #[serde(default)]
    org_id: Option<String>,
}

/// `PUT /api/account/registry/namespace` — claim the account's publisher
/// namespace on the ctxpkg registry (permanent in v1).
pub(super) async fn put_account_registry_namespace(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<RegistryNamespaceBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let (user_id, _email) = auth_user(&state, &headers).await?;
    let (status, json) = billing_forward(
        &state.cfg,
        "PUT",
        format!("/api/billing/registry/{user_id}/namespace"),
        Some(json!({ "namespace": body.namespace, "org_id": body.org_id })),
    )
    .await?;
    finish(status, json)
}

/// `GET /api/account/registry` — publisher profile: namespace + token list
/// (metadata only; plaintext tokens are shown exactly once at mint time).
pub(super) async fn get_account_registry(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, String)> {
    let (user_id, _email) = auth_user(&state, &headers).await?;
    let (status, json) = billing_forward(
        &state.cfg,
        "GET",
        format!("/api/billing/registry/{user_id}"),
        None,
    )
    .await?;
    finish(status, json)
}

/// Request body for `POST /api/account/registry/tokens`.
#[derive(Deserialize, Default)]
pub(super) struct RegistryTokenBody {
    #[serde(default)]
    label: Option<String>,
    /// `publish` (default) or `read` — read tokens are install-only (GL #524).
    #[serde(default)]
    scope: Option<String>,
}

/// `POST /api/account/registry/tokens` — mint a `ctxp_…` publish token or a
/// `ctxr_…` read-only install token.
pub(super) async fn post_account_registry_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<RegistryTokenBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let (user_id, _email) = auth_user(&state, &headers).await?;
    let (status, json) = billing_forward(
        &state.cfg,
        "POST",
        format!("/api/billing/registry/{user_id}/tokens"),
        Some(json!({ "label": body.label, "scope": body.scope })),
    )
    .await?;
    finish(status, json)
}

/// `DELETE /api/account/registry/tokens/{token_id}` — revoke a publish token.
pub(super) async fn delete_account_registry_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(token_id): Path<i64>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let (user_id, _email) = auth_user(&state, &headers).await?;
    let (status, json) = billing_forward(
        &state.cfg,
        "DELETE",
        format!("/api/billing/registry/{user_id}/tokens/{token_id}"),
        None,
    )
    .await?;
    finish(status, json)
}

/// Request body for `PUT /api/account/registry/price`.
#[derive(Deserialize)]
pub(super) struct RegistryPriceBody {
    name: String,
    /// `0` / absent clears the price (the pack becomes free again).
    #[serde(default)]
    price_cents: Option<i32>,
}

/// `PUT /api/account/registry/price` — set or clear a package price on the
/// account's namespace (Paid Packs v0, GL #529).
pub(super) async fn put_account_registry_price(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<RegistryPriceBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let (user_id, _email) = auth_user(&state, &headers).await?;
    let (status, json) = billing_forward(
        &state.cfg,
        "PUT",
        format!("/api/billing/registry/{user_id}/price"),
        Some(json!({ "name": body.name, "price_cents": body.price_cents })),
    )
    .await?;
    finish(status, json)
}

/// Request body for `POST /api/account/registry/buy`.
#[derive(Deserialize)]
pub(super) struct RegistryBuyBody {
    namespace: String,
    name: String,
}

/// `POST /api/account/registry/buy` — start a Stripe Checkout for a paid
/// pack; returns the hosted checkout URL (GL #529).
pub(super) async fn post_account_registry_buy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<RegistryBuyBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let (user_id, email) = auth_user(&state, &headers).await?;
    let (status, json) = billing_forward(
        &state.cfg,
        "POST",
        format!("/api/billing/registry/{user_id}/buy"),
        Some(json!({
            "namespace": body.namespace,
            "name": body.name,
            "email": email,
        })),
    )
    .await?;
    finish(status, json)
}

/// Request body for `POST /api/account/registry/domains`.
#[derive(Deserialize)]
pub(super) struct RegistryDomainBody {
    domain: String,
}

/// `POST /api/account/registry/domains` — register a domain for Verified
/// Publisher and receive the DNS-TXT challenge (GL #516).
pub(super) async fn post_account_registry_domain(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<RegistryDomainBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let (user_id, _email) = auth_user(&state, &headers).await?;
    let (status, json) = billing_forward(
        &state.cfg,
        "POST",
        format!("/api/billing/registry/{user_id}/domains"),
        Some(json!({ "domain": body.domain })),
    )
    .await?;
    finish(status, json)
}

/// `POST /api/account/registry/domains/{domain_id}/verify` — trigger the
/// DNS-TXT check; flips the publisher to verified on success.
pub(super) async fn post_account_registry_domain_verify(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(domain_id): Path<i64>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let (user_id, _email) = auth_user(&state, &headers).await?;
    let (status, json) = billing_forward(
        &state.cfg,
        "POST",
        format!("/api/billing/registry/{user_id}/domains/{domain_id}/verify"),
        None,
    )
    .await?;
    finish(status, json)
}

/// `DELETE /api/account/registry/domains/{domain_id}` — remove a domain.
pub(super) async fn delete_account_registry_domain(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(domain_id): Path<i64>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let (user_id, _email) = auth_user(&state, &headers).await?;
    let (status, json) = billing_forward(
        &state.cfg,
        "DELETE",
        format!("/api/billing/registry/{user_id}/domains/{domain_id}"),
        None,
    )
    .await?;
    finish(status, json)
}

/// Like [`billing_forward`] but returns the raw upstream body unparsed — used
/// for the CSV export, whose body is `text/csv`, not JSON.
async fn billing_forward_text(
    cfg: &Config,
    path: String,
) -> Result<(StatusCode, String), (StatusCode, String)> {
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
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .into();
        let resp = agent
            .get(&url)
            .header("X-Internal-Key", &key)
            .call()
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
    let status = StatusCode::from_u16(code).unwrap_or(StatusCode::BAD_GATEWAY);
    Ok((status, text))
}

// ── Public supporters wall ────────────────────────────────────────────────────

/// How long a fetched supporters payload stays fresh before the next request
/// re-validates against the private plane. The wall changes rarely; 5 minutes
/// keeps it lively while shielding the plane from per-pageview traffic.
const SUPPORTERS_CACHE_TTL: Duration = Duration::from_mins(5);
/// Plaintext clamp for a supporter's display name.
const SUPPORTER_NAME_MAX: usize = 80;
/// Plaintext clamp for a supporter's optional message.
const SUPPORTER_MESSAGE_MAX: usize = 140;
/// Clamp for short metadata strings (tier / currency / RFC 3339 timestamp).
const SUPPORTER_META_MAX: usize = 40;

/// The last sanitized supporters payload and when it was fetched. Process-wide
/// because the wall is global (not per-user), so a single slot suffices.
type SupportersCacheSlot = Mutex<Option<(Instant, Value)>>;
static SUPPORTERS_CACHE: SupportersCacheSlot = Mutex::new(None);

/// The cached wall, if it was stored less than `SUPPORTERS_CACHE_TTL` before
/// `now`. `now` is injected so expiry is unit-testable without sleeping.
fn supporters_cache_fresh(slot: &SupportersCacheSlot, now: Instant) -> Option<Value> {
    let guard = slot.lock().unwrap_or_else(PoisonError::into_inner);
    guard
        .as_ref()
        .filter(|(at, _)| now.duration_since(*at) < SUPPORTERS_CACHE_TTL)
        .map(|(_, v)| v.clone())
}

/// Store a freshly sanitized wall payload, restarting the TTL window at `now`.
fn supporters_cache_store(slot: &SupportersCacheSlot, now: Instant, value: &Value) {
    *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some((now, value.clone()));
}

/// The last stored wall regardless of age — the stale fallback served when the
/// private plane is unreachable (a stale wall beats a broken one).
fn supporters_cache_last(slot: &SupportersCacheSlot) -> Option<Value> {
    slot.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .map(|(_, v)| v.clone())
}

/// Clamp supporter-provided free text to plain text (defense in depth — the
/// website renders via `textContent`, this protects every other consumer):
/// HTML tags are dropped, control characters and runs of whitespace collapse to
/// a single space, and the result is cut at `max` characters (char-boundary
/// safe for any UTF-8 input).
fn sanitize_supporter_text(raw: &str, max: usize) -> String {
    // Pass 1: drop tag-shaped `<…>` runs, neutralize control characters. A `<`
    // only opens a tag when followed by a letter, `/` or `!`, so prose like
    // "i <3 rust" survives.
    let mut plain = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '<'
            && matches!(chars.peek(), Some(n) if n.is_ascii_alphabetic() || *n == '/' || *n == '!')
        {
            for tag_char in chars.by_ref() {
                if tag_char == '>' {
                    break;
                }
            }
            plain.push(' ');
            continue;
        }
        plain.push(if c.is_control() { ' ' } else { c });
    }

    // Pass 2: collapse whitespace, trim, clamp to `max` characters.
    let mut out = String::with_capacity(plain.len().min(max * 4));
    let mut count = 0usize;
    let mut last_was_space = true; // swallows leading whitespace
    for c in plain.chars() {
        let c = if c.is_whitespace() { ' ' } else { c };
        if c == ' ' && last_was_space {
            continue;
        }
        last_was_space = c == ' ';
        out.push(c);
        count += 1;
        if count == max {
            break;
        }
    }
    while out.ends_with(' ') {
        out.pop();
    }
    out
}

/// Rebuild the upstream supporters payload whitelist-style: only the documented
/// fields survive (anything else the plane might ever leak is dropped), every
/// free-text field is clamped to plain text, and `count` is recomputed instead
/// of trusted.
fn sanitize_supporters_payload(raw: &Value) -> Value {
    let supporters: Vec<Value> = raw
        .get("supporters")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|entry| {
                    if !entry.is_object() {
                        return None;
                    }
                    let text = |field: &str, max: usize| {
                        sanitize_supporter_text(
                            entry.get(field).and_then(Value::as_str).unwrap_or(""),
                            max,
                        )
                    };
                    let message = text("message", SUPPORTER_MESSAGE_MAX);
                    Some(json!({
                        "name": text("name", SUPPORTER_NAME_MAX),
                        "message": if message.is_empty() { Value::Null } else { Value::String(message) },
                        "tier": text("tier", SUPPORTER_META_MAX),
                        "amount_cents": entry.get("amount_cents").and_then(Value::as_i64).unwrap_or(0).max(0),
                        "currency": text("currency", SUPPORTER_META_MAX),
                        "created_at": text("created_at", SUPPORTER_META_MAX),
                    }))
                })
                .collect()
        })
        .unwrap_or_default();

    json!({ "count": supporters.len(), "supporters": supporters })
}

/// `GET /api/supporters` — the public supporters wall (no auth). Proxies the
/// private plane's read model with the shared internal key (which never reaches
/// the browser), sanitizes every supporter field to clamped plain text, and
/// serves from a 5-minute in-memory cache so page views don't hammer the plane.
///
/// Failure ladder:
/// - billing unconfigured ⇒ `200` with an empty wall (a standalone community
///   backend has no supporters read-model; the website still renders),
/// - upstream unreachable but a wall was fetched before ⇒ the stale copy,
/// - upstream unreachable and nothing cached ⇒ `503 {"error":"supporters_unavailable"}`.
pub(super) async fn get_supporters(State(state): State<AppState>) -> Response {
    let (Some(base), Some(key)) = (
        state.cfg.billing_base_url.clone(),
        state.cfg.billing_internal_key.clone(),
    ) else {
        return Json(json!({ "supporters": [], "count": 0 })).into_response();
    };

    let now = Instant::now();
    if let Some(fresh) = supporters_cache_fresh(&SUPPORTERS_CACHE, now) {
        return Json(fresh).into_response();
    }

    let url = format!("{base}/api/billing/supporters");
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

    match body.and_then(|b| serde_json::from_str::<Value>(&b).ok()) {
        Some(raw) => {
            let clean = sanitize_supporters_payload(&raw);
            supporters_cache_store(&SUPPORTERS_CACHE, now, &clean);
            Json(clean).into_response()
        }
        None => match supporters_cache_last(&SUPPORTERS_CACHE) {
            Some(stale) => Json(stale).into_response(),
            None => (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "error": "supporters_unavailable" })),
            )
                .into_response(),
        },
    }
}

/// Request body for `POST /api/supporters/checkout`: the chosen monthly
/// contribution in USD minor units (cents).
#[derive(Deserialize)]
pub(super) struct SupporterCheckoutBody {
    #[serde(default)]
    amount_cents: i64,
}

/// `POST /api/supporters/checkout` — start a no-account, custom-amount Supporter
/// subscription and return the hosted Stripe `url`. Public: supporting needs no
/// login. The amount is clamped here (defense in depth) and again on the private
/// plane; a 503/502 lets the website fall back to a fixed preset Payment Link.
pub(super) async fn post_supporter_checkout(
    State(state): State<AppState>,
    Json(body): Json<SupporterCheckoutBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let amount = body.amount_cents.clamp(100, 100_000);
    Ok(Json(
        billing_post(
            &state.cfg,
            "/api/billing/supporters/checkout",
            json!({ "amount_cents": amount }),
        )
        .await?,
    ))
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

// ── Team seats, hosted-index storage & managed connectors ─────────────────────
//
// Same thin-proxy pattern as the team roster above: authenticate the owner by
// their session, forward to the private plane with the internal key, and preserve
// the upstream status. Request bodies are passed through unchanged (the plane owns
// validation), so the edge never duplicates the seat/connector schema.

/// `POST /api/account/team/seats` — change the team's seat count (written straight
/// to the Stripe subscription, prorated). Body `{ "seats": N }`; returns the
/// refreshed team payload so the dashboard re-renders in one round-trip.
pub(super) async fn post_account_team_seats(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let (user_id, _email) = auth_user(&state, &headers).await?;
    let (status, json) = billing_forward(
        &state.cfg,
        "POST",
        format!("/api/billing/team/{user_id}/seats"),
        Some(body),
    )
    .await?;
    finish(status, json)
}

/// `GET /api/account/team/storage` — hosted retrieval-index footprint + metering.
/// `available:false` until a team server is provisioned and reports storage.
pub(super) async fn get_account_team_storage(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, String)> {
    let (user_id, _email) = auth_user(&state, &headers).await?;
    let (status, json) = billing_forward(
        &state.cfg,
        "GET",
        format!("/api/billing/team/{user_id}/storage"),
        None,
    )
    .await?;
    finish(status, json)
}

/// `GET /api/account/team/connectors` — the secret-free managed-connector roster,
/// each merged with its latest live sync status.
pub(super) async fn get_account_team_connectors(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, String)> {
    let (user_id, _email) = auth_user(&state, &headers).await?;
    let (status, json) = billing_forward(
        &state.cfg,
        "GET",
        format!("/api/billing/team/{user_id}/connectors"),
        None,
    )
    .await?;
    finish(status, json)
}

/// `POST /api/account/team/connectors` — create a managed connector. The plaintext
/// provider secret is forwarded once to the plane (encrypted at rest there) and is
/// never stored or echoed by the edge. 400 from the plane on validation / limit.
pub(super) async fn post_account_team_connector(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let (user_id, _email) = auth_user(&state, &headers).await?;
    let (status, json) = billing_forward(
        &state.cfg,
        "POST",
        format!("/api/billing/team/{user_id}/connectors"),
        Some(body),
    )
    .await?;
    finish(status, json)
}

/// `PATCH /api/account/team/connectors/{connector_id}` — pause/resume a connector.
/// Body `{ "enabled": bool }`. The plane returns 204 No Content, so surface a
/// small JSON ack the dashboard can treat as success.
pub(super) async fn patch_account_team_connector(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(connector_id): Path<String>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let (user_id, _email) = auth_user(&state, &headers).await?;
    let (status, json) = billing_forward(
        &state.cfg,
        "PATCH",
        format!("/api/billing/team/{user_id}/connectors/{connector_id}"),
        Some(body),
    )
    .await?;
    if status.is_success() {
        return Ok(Json(json!({ "updated": true })));
    }
    finish(status, json)
}

/// `DELETE /api/account/team/connectors/{connector_id}` — remove a connector and
/// redeploy. The plane returns 204 No Content; surface a JSON ack.
pub(super) async fn delete_account_team_connector(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(connector_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let (user_id, _email) = auth_user(&state, &headers).await?;
    let (status, json) = billing_forward(
        &state.cfg,
        "DELETE",
        format!("/api/billing/team/{user_id}/connectors/{connector_id}"),
        None,
    )
    .await?;
    if status.is_success() {
        return Ok(Json(json!({ "deleted": true })));
    }
    finish(status, json)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `Config` carrying only the two knobs the sync gate reads. `billing`
    /// toggles whether a commercial plane is wired; `sync_open` is the operator
    /// opt-out (`LEANCTX_CLOUD_SYNC_OPEN`).
    fn cfg(billing: bool, sync_open: bool) -> Config {
        Config {
            bind_host: "127.0.0.1".into(),
            bind_port: 8088,
            public_base_url: String::new(),
            api_base_url: String::new(),
            database_url: String::new(),
            ip_hash_salt: String::new(),
            smtp_host: None,
            smtp_port: None,
            smtp_username: None,
            smtp_password: None,
            smtp_from: None,
            billing_base_url: billing.then(|| "https://billing.example".to_string()),
            billing_internal_key: billing.then(|| "internal-key".to_string()),
            sync_open,
        }
    }

    #[test]
    fn gated_deployment_blocks_free_and_supporter_only() {
        // leanctx.com: billing wired, no operator opt-out → the gate is live.
        let gated = cfg(true, false);
        // Free and Supporter lack `cloud_sync` ⇒ denied (handler returns 402).
        assert!(!cloud_sync_allowed(&gated, Plan::Free));
        assert!(!cloud_sync_allowed(&gated, Plan::Supporter));
        // Pro and every superset grant `cloud_sync` ⇒ allowed.
        assert!(cloud_sync_allowed(&gated, Plan::Pro));
        assert!(cloud_sync_allowed(&gated, Plan::Team));
        assert!(cloud_sync_allowed(&gated, Plan::Enterprise));
    }

    #[test]
    fn no_billing_plane_never_gates_sync() {
        // A self-hosted community backend without billing keeps sync fully usable
        // for every logged-in user (Local-Free Invariant) — even Free.
        let open = cfg(false, false);
        assert!(sync_is_open(&open));
        assert!(cloud_sync_allowed(&open, Plan::Free));
    }

    #[test]
    fn operator_opt_out_opens_sync_even_with_billing() {
        // Self-host with billing wired but LEANCTX_CLOUD_SYNC_OPEN=1 → sync free
        // for everyone, regardless of plan.
        let opt_out = cfg(true, true);
        assert!(sync_is_open(&opt_out));
        assert!(cloud_sync_allowed(&opt_out, Plan::Free));
    }

    // ── Supporters wall: sanitization ─────────────────────────────────────────

    #[test]
    fn supporter_text_strips_html_and_control_chars() {
        let dirty = "Eve <script>alert('x')</script>\u{0007}\n<b>!</b>";
        assert_eq!(
            sanitize_supporter_text(dirty, SUPPORTER_NAME_MAX),
            "Eve alert('x') !"
        );
        // A bare `<` that doesn't open a tag is normal prose and survives.
        assert_eq!(
            sanitize_supporter_text("i <3 rust & you", SUPPORTER_MESSAGE_MAX),
            "i <3 rust & you"
        );
    }

    #[test]
    fn supporter_text_clamps_length_on_char_boundaries() {
        // Multi-byte input must clamp by characters, not bytes (no panics, no
        // split code points). 90 'ä' → exactly 80 chars.
        let long_name = "ä".repeat(90);
        let clamped = sanitize_supporter_text(&long_name, SUPPORTER_NAME_MAX);
        assert_eq!(clamped.chars().count(), SUPPORTER_NAME_MAX);

        let long_message = "m".repeat(500);
        assert_eq!(
            sanitize_supporter_text(&long_message, SUPPORTER_MESSAGE_MAX).len(),
            SUPPORTER_MESSAGE_MAX
        );

        // Whitespace runs (incl. tabs/newlines) collapse and ends are trimmed.
        assert_eq!(
            sanitize_supporter_text("  a \t\t b\n\nc  ", SUPPORTER_NAME_MAX),
            "a b c"
        );
    }

    #[test]
    fn supporters_payload_is_whitelisted_and_recounted() {
        let raw = json!({
            "supporters": [
                {
                    "name": "<b>Ada</b>",
                    "message": "",
                    "tier": "Sponsor",
                    "amount_cents": 2500,
                    "currency": "usd",
                    "created_at": "2026-05-01T10:00:00Z",
                    "email": "leak@example.com"
                },
                "not-an-object"
            ],
            "count": 99
        });

        let clean = sanitize_supporters_payload(&raw);
        // Non-object entries are dropped and `count` is recomputed, not trusted.
        assert_eq!(clean["count"], 1);
        assert_eq!(clean["supporters"].as_array().map(Vec::len), Some(1));

        let s = &clean["supporters"][0];
        assert_eq!(s["name"], "Ada");
        // Empty message normalizes to null so clients can simply skip it.
        assert!(s["message"].is_null());
        assert_eq!(s["tier"], "Sponsor");
        assert_eq!(s["amount_cents"], 2500);
        assert_eq!(s["currency"], "usd");
        assert_eq!(s["created_at"], "2026-05-01T10:00:00Z");
        // Unknown upstream fields never pass the edge.
        assert!(s.get("email").is_none());
    }

    #[test]
    fn supporters_payload_handles_malformed_upstream_shapes() {
        // No `supporters` array at all → an empty, well-formed wall.
        let clean = sanitize_supporters_payload(&json!({ "unexpected": true }));
        assert_eq!(clean["count"], 0);
        assert_eq!(clean["supporters"].as_array().map(Vec::len), Some(0));
    }

    // ── Supporters wall: cache ────────────────────────────────────────────────

    #[test]
    fn supporters_cache_hit_expiry_and_stale_fallback() {
        let slot: SupportersCacheSlot = Mutex::new(None);
        let t0 = Instant::now();

        // Empty cache: neither fresh nor stale.
        assert!(supporters_cache_fresh(&slot, t0).is_none());
        assert!(supporters_cache_last(&slot).is_none());

        let wall = json!({ "count": 1, "supporters": [{ "name": "Ada" }] });
        supporters_cache_store(&slot, t0, &wall);

        // Fresh within the TTL window (just before expiry).
        let just_before = (t0 + SUPPORTERS_CACHE_TTL)
            .checked_sub(Duration::from_secs(1))
            .unwrap();
        assert_eq!(
            supporters_cache_fresh(&slot, just_before),
            Some(wall.clone())
        );

        // At/after the TTL the entry no longer counts as fresh…
        assert!(supporters_cache_fresh(&slot, t0 + SUPPORTERS_CACHE_TTL).is_none());
        // …but stays available as the stale fallback for upstream outages.
        assert_eq!(supporters_cache_last(&slot), Some(wall.clone()));

        // Storing again restarts the TTL window.
        let t1 = t0 + SUPPORTERS_CACHE_TTL + Duration::from_secs(10);
        supporters_cache_store(&slot, t1, &wall);
        assert_eq!(supporters_cache_fresh(&slot, t1), Some(wall));
    }

    // ── Entitlements cache (GL #785) ──────────────────────────────────────────

    #[test]
    fn entitlements_cache_fresh_then_expiry_then_stale_fallback() {
        let slot: EntitlementsCacheSlot = Mutex::new(HashMap::new());
        let uid = Uuid::new_v4();
        let t0 = Instant::now();

        // Cold cache: neither fresh nor stale.
        assert!(entitlements_cache_fresh(&slot, uid, t0).is_none());
        assert!(entitlements_cache_any(&slot, uid).is_none());

        let pro = json!({ "plan": "pro", "entitlements": { "cloud_sync": true } });
        entitlements_cache_store(&slot, uid, t0, &pro);

        // Fresh just before the TTL window closes.
        let just_before = (t0 + ENTITLEMENTS_CACHE_TTL)
            .checked_sub(Duration::from_secs(1))
            .unwrap();
        assert_eq!(
            entitlements_cache_fresh(&slot, uid, just_before),
            Some(pro.clone())
        );

        // At/after the TTL it is no longer fresh…
        assert!(entitlements_cache_fresh(&slot, uid, t0 + ENTITLEMENTS_CACHE_TTL).is_none());
        // …but survives as the stale fallback served during a billing outage.
        assert_eq!(entitlements_cache_any(&slot, uid), Some(pro));
    }

    #[test]
    fn entitlements_cache_stale_fallback_is_per_user() {
        let slot: EntitlementsCacheSlot = Mutex::new(HashMap::new());
        let seen = Uuid::new_v4();
        let never_seen = Uuid::new_v4();
        let t0 = Instant::now();

        entitlements_cache_store(&slot, seen, t0, &json!({ "plan": "pro" }));

        // A previously-seen payer keeps Pro during an outage; an account we have
        // never resolved has nothing to fall back to → caller degrades to Free.
        assert_eq!(
            entitlements_cache_any(&slot, seen),
            Some(json!({ "plan": "pro" }))
        );
        assert!(entitlements_cache_any(&slot, never_seen).is_none());
    }

    #[test]
    fn prune_entitlements_cache_evicts_only_very_old_entries() {
        let mut map: HashMap<Uuid, CachedEntitlements> = HashMap::new();
        let t0 = Instant::now();
        let later = t0 + ENTITLEMENTS_STALE_RETAIN + Duration::from_secs(1);

        let old = Uuid::new_v4();
        let recent = Uuid::new_v4();
        map.insert(
            old,
            CachedEntitlements {
                at: t0,
                value: json!({ "plan": "team" }),
            },
        );
        map.insert(
            recent,
            CachedEntitlements {
                at: later,
                value: json!({ "plan": "pro" }),
            },
        );

        prune_entitlements_cache(&mut map, later);

        assert!(
            !map.contains_key(&old),
            "entries past the stale-retain window are dropped"
        );
        assert!(map.contains_key(&recent), "recent entries are kept");
    }
}
