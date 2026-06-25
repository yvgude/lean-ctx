//! Self-serve OIDC SSO login (GL #482) — the edge half.
//!
//! The control plane stores each org's `IdP` config (issuer, client id, sealed
//! secret, verified email domain); this module runs the actual Authorization
//! Code + PKCE flow against the `IdP` and turns a valid ID token into the same
//! session credential a password login produces.
//!
//! Flow:
//! 1. `POST /api/auth/sso/start {email}` — domain → org lookup. When SSO is
//!    configured the response carries the `IdP` authorize URL; the state, nonce
//!    and PKCE verifier are stored server-side (hashed state, 10-min TTL).
//! 2. `IdP` redirects to `GET /api/auth/sso/callback?code&state` — the state is
//!    consumed (single use), the code is exchanged (client secret fetched
//!    per-login from the control plane, never cached), and the ID token is
//!    verified: signature against the issuer's JWKS, `iss`, `aud`, `exp`,
//!    `nonce`, and that the email's domain is exactly the org's domain.
//! 3. The user is JIT-provisioned (passwordless, email pre-verified), an API
//!    key is rotated in, and the browser is redirected to the login page with
//!    a one-time handoff code — the key itself never appears in a URL.
//! 4. `POST /api/auth/sso/handoff {code}` — the page swaps the code (single
//!    use, 60-s TTL) for `{api_key, user_id, email}`.
//!
//! Failures redirect to `/login/?sso_error=<reason>` with neutral reasons —
//! nothing distinguishes "wrong signature" from "expired assertion" to a
//! probing caller; details go to server logs only.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect};
use base64::Engine;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::auth::{
    AppState, generate_api_key, generate_token, mark_email_verified, rotate_api_key, sha256_hex,
    upsert_user,
};
use super::config::Config;
use super::helpers::internal_error;

/// Discovery + JWKS documents are cached per issuer for this long. The flow
/// TTLs (10-min login states, 60-s handoff codes) are enforced in SQL, where
/// the rows live.
const DISCOVERY_TTL: Duration = Duration::from_hours(1);

// ── Issuer metadata cache ────────────────────────────────────────────────────

/// issuer → (discovery JSON, JWKS JSON, fetched-at). Both documents are
/// public; caching them keeps per-login latency to the token exchange only.
static ISSUER_CACHE: Mutex<Option<HashMap<String, (Value, Value, Instant)>>> = Mutex::new(None);

fn cached_issuer_meta(issuer: &str) -> Option<(Value, Value)> {
    let guard = match ISSUER_CACHE.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    guard
        .as_ref()
        .and_then(|m| m.get(issuer))
        .filter(|(_, _, at)| at.elapsed() < DISCOVERY_TTL)
        .map(|(d, j, _)| (d.clone(), j.clone()))
}

fn store_issuer_meta(issuer: &str, discovery: Value, jwks: Value) {
    let mut guard = match ISSUER_CACHE.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    guard
        .get_or_insert_with(HashMap::new)
        .insert(issuer.to_string(), (discovery, jwks, Instant::now()));
}

/// Fetch (or reuse) the issuer's discovery document and JWKS. Blocking I/O —
/// call inside `spawn_blocking`.
fn issuer_meta(issuer: &str) -> Result<(Value, Value), String> {
    if let Some(hit) = cached_issuer_meta(issuer) {
        return Ok(hit);
    }
    let discovery_url = format!("{issuer}/.well-known/openid-configuration");
    let discovery = http_get_json(&discovery_url)?;
    // The discovery document must belong to the issuer it was fetched from
    // (OIDC Core §4.3) — a mismatch means a misconfigured or hostile IdP.
    let doc_issuer = discovery
        .get("issuer")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim_end_matches('/');
    if doc_issuer != issuer {
        return Err(format!(
            "discovery issuer mismatch: configured {issuer}, document says {doc_issuer}"
        ));
    }
    let jwks_uri = discovery
        .get("jwks_uri")
        .and_then(Value::as_str)
        .ok_or_else(|| "discovery document has no jwks_uri".to_string())?;
    let jwks = http_get_json(jwks_uri)?;
    store_issuer_meta(issuer, discovery.clone(), jwks.clone());
    Ok((discovery, jwks))
}

fn http_get_json(url: &str) -> Result<Value, String> {
    if !url.starts_with("https://") {
        return Err(format!("refusing non-https url: {url}"));
    }
    let body = ureq::get(url)
        .header("Accept", "application/json")
        .call()
        .map_err(|e| format!("GET {url}: {e}"))?
        .into_body()
        .read_to_string()
        .map_err(|e| format!("read {url}: {e}"))?;
    serde_json::from_str(&body).map_err(|e| format!("parse {url}: {e}"))
}

// ── Control-plane lookups ────────────────────────────────────────────────────

/// The public half of an org's SSO config, as served by the control plane.
/// (The callback re-fetches `org_id` itself for JIT provisioning.)
pub(super) struct SsoOrg {
    pub issuer: String,
    pub client_id: String,
    pub sso_required: bool,
    pub owner_email: Option<String>,
}

/// Look up a verified SSO config for an email domain. `Ok(None)` covers
/// "no SSO for this domain" *and* "billing plane not configured/reachable" —
/// password login proceeds in both cases (SSO never bricks sign-in).
pub(super) async fn lookup_sso_org(cfg: &Config, domain: &str) -> Result<Option<SsoOrg>, String> {
    let (Some(base), Some(key)) = (
        cfg.billing_base_url.clone(),
        cfg.billing_internal_key.clone(),
    ) else {
        return Ok(None);
    };
    let url = format!("{base}/api/billing/sso/lookup/{domain}");
    let resp = tokio::task::spawn_blocking(move || {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .into();
        let resp = agent
            .get(&url)
            .header("X-Internal-Key", &key)
            .call()
            .map_err(|e| e.to_string())?;
        let status = resp.status().as_u16();
        let body = resp
            .into_body()
            .read_to_string()
            .map_err(|e| e.to_string())?;
        Ok::<_, String>((status, body))
    })
    .await
    .map_err(|e| format!("join: {e}"))?;

    let (status, body) = match resp {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "sso lookup unreachable — treating as no-SSO");
            return Ok(None);
        }
    };
    if status == 404 {
        return Ok(None);
    }
    if status != 200 {
        return Err(format!("sso lookup returned {status}"));
    }
    let v: Value = serde_json::from_str(&body).map_err(|e| format!("sso lookup parse: {e}"))?;
    Ok(Some(SsoOrg {
        issuer: v
            .get("issuer")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        client_id: v
            .get("client_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        sso_required: v
            .get("sso_required")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        owner_email: v
            .get("owner_email")
            .and_then(Value::as_str)
            .map(str::to_string),
    }))
}

/// Whether a password login/registration for `email` must be refused because
/// the org behind its domain enforces SSO. The org owner is exempt — a broken
/// `IdP` must never lock the org out of its own dashboard (break-glass).
pub(super) async fn password_login_blocked(cfg: &Config, email: &str) -> bool {
    let Some(domain) = email.rsplit('@').next().filter(|d| d.contains('.')) else {
        return false;
    };
    match lookup_sso_org(cfg, &domain.to_ascii_lowercase()).await {
        Ok(Some(org)) => org.sso_required && org.owner_email.as_deref() != Some(email),
        _ => false,
    }
}

/// Fetch issuer + client id + decrypted client secret for the token exchange.
/// Called once per callback; the secret lives only on this stack frame.
fn fetch_exchange_secret(
    base: &str,
    key: &str,
    domain: &str,
) -> Result<(String, String, String), String> {
    let url = format!("{base}/api/billing/sso/exchange/{domain}");
    let body = ureq::get(&url)
        .header("X-Internal-Key", key)
        .call()
        .map_err(|e| format!("exchange secret: {e}"))?
        .into_body()
        .read_to_string()
        .map_err(|e| format!("exchange secret read: {e}"))?;
    let v: Value = serde_json::from_str(&body).map_err(|e| format!("exchange parse: {e}"))?;
    let get = |k: &str| -> Result<String, String> {
        v.get(k)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| format!("exchange secret: missing {k}"))
    };
    Ok((get("issuer")?, get("client_id")?, get("client_secret")?))
}

// ── POST /api/auth/sso/start ─────────────────────────────────────────────────

#[derive(Deserialize)]
pub(super) struct StartBody {
    pub email: String,
}

/// Begin an SSO login: answers `{sso:false}` when the email's domain has no
/// verified `IdP`, else stores the flow state and returns the authorize URL.
pub(super) async fn sso_start(
    State(state): State<AppState>,
    Json(body): Json<StartBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let email = body.email.trim().to_lowercase();
    let Some(domain) = email
        .rsplit('@')
        .next()
        .filter(|d| d.contains('.') && !d.is_empty())
        .map(str::to_string)
    else {
        return Err((StatusCode::BAD_REQUEST, "Invalid email".into()));
    };

    let org = lookup_sso_org(&state.cfg, &domain).await.map_err(|e| {
        tracing::error!(error = %e, "sso start lookup failed");
        (
            StatusCode::BAD_GATEWAY,
            "SSO is temporarily unavailable".to_string(),
        )
    })?;
    let Some(org) = org else {
        return Ok(Json(json!({ "sso": false })));
    };

    // Discovery (cached) — needed for the authorization endpoint.
    let issuer = org.issuer.clone();
    let meta = tokio::task::spawn_blocking(move || issuer_meta(&issuer))
        .await
        .map_err(internal_error)?
        .map_err(|e| {
            tracing::error!(error = %e, issuer = %org.issuer, "sso discovery failed");
            (
                StatusCode::BAD_GATEWAY,
                "The identity provider is not reachable right now".to_string(),
            )
        })?;
    let authorize_endpoint = meta
        .0
        .get("authorization_endpoint")
        .and_then(Value::as_str)
        .ok_or((
            StatusCode::BAD_GATEWAY,
            "The identity provider publishes no authorization endpoint".to_string(),
        ))?
        .to_string();

    // Flow state: state (returned by IdP), nonce (bound into the ID token),
    // PKCE verifier (proves the callback belongs to this start).
    let state_token = generate_token();
    let nonce = generate_token();
    let pkce_verifier = generate_token();
    let pkce_challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(pkce_verifier.as_bytes()));

    let client = state.pool.get().await.map_err(internal_error)?;
    // Opportunistic sweep keeps the table tiny without a background job.
    client
        .execute(
            "DELETE FROM sso_login_states WHERE created_at < NOW() - INTERVAL '10 minutes'",
            &[],
        )
        .await
        .map_err(internal_error)?;
    client
        .execute(
            "INSERT INTO sso_login_states (state_sha256, email_domain, nonce, pkce_verifier)
             VALUES ($1, $2, $3, $4)",
            &[&sha256_hex(&state_token), &domain, &nonce, &pkce_verifier],
        )
        .await
        .map_err(internal_error)?;

    let redirect_uri = format!(
        "{}/api/auth/sso/callback",
        state.cfg.api_base_url.trim_end_matches('/')
    );
    let authorize_url = format!(
        "{authorize_endpoint}{}response_type=code&client_id={}&redirect_uri={}&scope={}&state={}&nonce={}&code_challenge={}&code_challenge_method=S256&login_hint={}",
        if authorize_endpoint.contains('?') {
            "&"
        } else {
            "?"
        },
        urlencode(&org.client_id),
        urlencode(&redirect_uri),
        urlencode("openid email profile"),
        urlencode(&state_token),
        urlencode(&nonce),
        urlencode(&pkce_challenge),
        urlencode(&email),
    );

    Ok(Json(json!({ "sso": true, "redirect_url": authorize_url })))
}

// ── GET /api/auth/sso/callback ───────────────────────────────────────────────

#[derive(Deserialize)]
pub(super) struct CallbackQuery {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
}

/// Where failed logins land: the login page with a neutral reason code.
fn error_redirect(cfg: &Config, reason: &str) -> Redirect {
    Redirect::to(&format!(
        "{}/login/?sso_error={reason}",
        cfg.public_base_url.trim_end_matches('/')
    ))
}

/// The `IdP`'s redirect target. Every validation failure lands on the login
/// page with a neutral `sso_error`; only success mints a handoff code.
pub(super) async fn sso_callback(
    State(state): State<AppState>,
    Query(q): Query<CallbackQuery>,
) -> impl IntoResponse {
    if let Some(e) = q.error.as_deref() {
        tracing::info!(idp_error = %e, "sso callback: idp returned error");
        return error_redirect(&state.cfg, "idp_denied");
    }
    let (Some(code), Some(state_token)) = (q.code.as_deref(), q.state.as_deref()) else {
        return error_redirect(&state.cfg, "missing_params");
    };

    // Consume the flow state — single use, freshness enforced in SQL.
    let row = match state.pool.get().await {
        Ok(client) => client
            .query_opt(
                "DELETE FROM sso_login_states
                 WHERE state_sha256 = $1 AND created_at > NOW() - INTERVAL '10 minutes'
                 RETURNING email_domain, nonce, pkce_verifier",
                &[&sha256_hex(state_token)],
            )
            .await
            .ok()
            .flatten(),
        Err(_) => None,
    };
    let Some(row) = row else {
        return error_redirect(&state.cfg, "expired");
    };
    let domain: String = row.get(0);
    let expected_nonce: String = row.get(1);
    let pkce_verifier: String = row.get(2);

    let (Some(base), Some(key)) = (
        state.cfg.billing_base_url.clone(),
        state.cfg.billing_internal_key.clone(),
    ) else {
        return error_redirect(&state.cfg, "unavailable");
    };

    // Blocking half: secret fetch, token exchange, ID-token verification.
    let code = code.to_string();
    let domain_for_exchange = domain.clone();
    let redirect_uri = format!(
        "{}/api/auth/sso/callback",
        state.cfg.api_base_url.trim_end_matches('/')
    );
    let exchanged = tokio::task::spawn_blocking(move || {
        exchange_and_verify(
            &base,
            &key,
            &domain_for_exchange,
            &code,
            &redirect_uri,
            &pkce_verifier,
            &expected_nonce,
        )
    })
    .await;

    let (email, org_id) = match exchanged {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, %domain, "sso callback verification failed");
            return error_redirect(&state.cfg, "verify_failed");
        }
        Err(e) => {
            tracing::error!(error = %e, "sso callback join error");
            return error_redirect(&state.cfg, "unavailable");
        }
    };

    // The asserted email must belong to the org's verified domain — an IdP
    // can only ever sign in addresses under the domain it proved to own.
    if email.rsplit('@').next().map(str::to_ascii_lowercase) != Some(domain.clone()) {
        tracing::warn!(%domain, "sso callback: email outside org domain");
        return error_redirect(&state.cfg, "verify_failed");
    }

    // JIT user: passwordless, email pre-verified by the IdP.
    let (user_id, _created) = match upsert_user(&state.pool, &email, None).await {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(error = %e, "sso callback: upsert user failed");
            return error_redirect(&state.cfg, "unavailable");
        }
    };
    if let Err(e) = mark_email_verified(&state.pool, user_id).await {
        tracing::warn!(error = %e, "sso callback: mark verified failed");
    }

    let api_key = generate_api_key();
    if let Err(e) = rotate_api_key(&state.pool, user_id, &sha256_hex(&api_key)).await {
        tracing::error!(error = %e, "sso callback: rotate key failed");
        return error_redirect(&state.cfg, "unavailable");
    }

    // JIT org membership on the control plane (fire-and-forget — a hiccup
    // here must not break the sign-in; entitlements catch up next login).
    {
        let cfg = state.cfg.clone();
        let email = email.clone();
        tokio::spawn(async move {
            jit_membership(&cfg, org_id, user_id, &email).await;
        });
    }

    // One-time handoff: the api key never rides in a URL.
    let handoff = generate_token();
    let stored = match state.pool.get().await {
        Ok(client) => client
            .execute(
                "INSERT INTO sso_handoff_codes (code_sha256, user_id, api_key, email)
                 VALUES ($1, $2, $3, $4)",
                &[&sha256_hex(&handoff), &user_id, &api_key, &email],
            )
            .await
            .is_ok(),
        Err(_) => false,
    };
    if !stored {
        return error_redirect(&state.cfg, "unavailable");
    }

    Redirect::to(&format!(
        "{}/login/?sso_handoff={handoff}",
        state.cfg.public_base_url.trim_end_matches('/')
    ))
}

/// Token exchange + full ID-token validation. Returns `(email, org_id)`.
fn exchange_and_verify(
    billing_base: &str,
    internal_key: &str,
    domain: &str,
    code: &str,
    redirect_uri: &str,
    pkce_verifier: &str,
    expected_nonce: &str,
) -> Result<(String, Uuid), String> {
    let (issuer, client_id, client_secret) =
        fetch_exchange_secret(billing_base, internal_key, domain)?;

    // org_id rides along from the lookup so the async half can JIT-provision.
    let org_id = {
        let url = format!("{billing_base}/api/billing/sso/lookup/{domain}");
        let body = ureq::get(&url)
            .header("X-Internal-Key", internal_key)
            .call()
            .map_err(|e| format!("org lookup: {e}"))?
            .into_body()
            .read_to_string()
            .map_err(|e| format!("org lookup read: {e}"))?;
        serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|v| {
                v.get("org_id")
                    .and_then(Value::as_str)
                    .and_then(|s| Uuid::parse_str(s).ok())
            })
            .ok_or("org lookup: missing org_id")?
    };

    let (discovery, jwks) = issuer_meta(&issuer)?;
    let token_endpoint = discovery
        .get("token_endpoint")
        .and_then(Value::as_str)
        .ok_or("discovery: no token_endpoint")?;

    let form = format!(
        "grant_type=authorization_code&code={}&redirect_uri={}&client_id={}&client_secret={}&code_verifier={}",
        urlencode(code),
        urlencode(redirect_uri),
        urlencode(&client_id),
        urlencode(&client_secret),
        urlencode(pkce_verifier),
    );
    let body = ureq::post(token_endpoint)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .send(form.as_bytes())
        .map_err(|e| format!("token exchange: {e}"))?
        .into_body()
        .read_to_string()
        .map_err(|e| format!("token exchange read: {e}"))?;
    let tokens: Value =
        serde_json::from_str(&body).map_err(|e| format!("token exchange parse: {e}"))?;
    let id_token = tokens
        .get("id_token")
        .and_then(Value::as_str)
        .ok_or("token response has no id_token")?;

    let claims = verify_id_token(id_token, &jwks, &issuer, &client_id)?;

    let nonce = claims.get("nonce").and_then(Value::as_str).unwrap_or("");
    if nonce != expected_nonce {
        return Err("nonce mismatch".into());
    }
    let email = claims
        .get("email")
        .and_then(Value::as_str)
        .map(str::to_lowercase)
        .ok_or("id token has no email claim")?;
    // `email_verified:false` is an explicit IdP statement — reject. A missing
    // claim is common (Entra) and acceptable: the IdP authenticated the user.
    if claims.get("email_verified").and_then(Value::as_bool) == Some(false) {
        return Err("idp reports email_verified=false".into());
    }
    Ok((email, org_id))
}

/// Validate signature + iss/aud/exp of an ID token against a JWKS document.
fn verify_id_token(
    id_token: &str,
    jwks: &Value,
    issuer: &str,
    client_id: &str,
) -> Result<Value, String> {
    use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};

    let header = decode_header(id_token).map_err(|e| format!("jwt header: {e}"))?;
    let kid = header.kid.as_deref();

    let keys = jwks
        .get("keys")
        .and_then(Value::as_array)
        .ok_or("jwks has no keys")?;
    // Prefer the kid match; a single-key JWKS may omit kid entirely.
    let jwk = keys
        .iter()
        .find(|k| kid.is_some() && k.get("kid").and_then(Value::as_str) == kid)
        .or_else(|| (keys.len() == 1).then(|| &keys[0]))
        .ok_or("no matching jwk for token kid")?;

    let kty = jwk.get("kty").and_then(Value::as_str).unwrap_or("");
    let (decoding_key, algorithm) = match kty {
        "RSA" => {
            let n = jwk
                .get("n")
                .and_then(Value::as_str)
                .ok_or("rsa jwk missing n")?;
            let e = jwk
                .get("e")
                .and_then(Value::as_str)
                .ok_or("rsa jwk missing e")?;
            (
                DecodingKey::from_rsa_components(n, e).map_err(|e| format!("rsa key: {e}"))?,
                header.alg,
            )
        }
        "EC" => {
            let x = jwk
                .get("x")
                .and_then(Value::as_str)
                .ok_or("ec jwk missing x")?;
            let y = jwk
                .get("y")
                .and_then(Value::as_str)
                .ok_or("ec jwk missing y")?;
            (
                DecodingKey::from_ec_components(x, y).map_err(|e| format!("ec key: {e}"))?,
                header.alg,
            )
        }
        other => return Err(format!("unsupported jwk kty {other}")),
    };
    // Only asymmetric algorithms are acceptable for ID tokens; anything else
    // (HS*, none) is an attack, not a configuration.
    if !matches!(
        algorithm,
        Algorithm::RS256
            | Algorithm::RS384
            | Algorithm::RS512
            | Algorithm::PS256
            | Algorithm::PS384
            | Algorithm::PS512
            | Algorithm::ES256
            | Algorithm::ES384
    ) {
        return Err(format!("refusing token alg {algorithm:?}"));
    }

    let mut validation = Validation::new(algorithm);
    validation.set_issuer(&[issuer]);
    validation.set_audience(&[client_id]);
    validation.leeway = 60;

    let data = decode::<Value>(id_token, &decoding_key, &validation)
        .map_err(|e| format!("jwt verify: {e}"))?;
    Ok(data.claims)
}

/// Ensure the org membership on the control plane after a successful login.
async fn jit_membership(cfg: &Config, org_id: Uuid, user_id: Uuid, email: &str) {
    let (Some(base), Some(key)) = (
        cfg.billing_base_url.clone(),
        cfg.billing_internal_key.clone(),
    ) else {
        return;
    };
    let payload = json!({ "org_id": org_id, "user_id": user_id, "email": email });
    let Ok(body) = serde_json::to_vec(&payload) else {
        return;
    };
    let _ = tokio::task::spawn_blocking(move || {
        let result = ureq::post(format!("{base}/api/billing/sso/jit").as_str())
            .header("X-Internal-Key", &key)
            .header("Content-Type", "application/json")
            .send(&body);
        if let Err(e) = result {
            tracing::warn!(error = %e, "sso jit membership failed");
        }
    })
    .await;
}

// ── POST /api/auth/sso/handoff ───────────────────────────────────────────────

#[derive(Deserialize)]
pub(super) struct HandoffBody {
    pub code: String,
}

/// Swap a one-time handoff code for the session credentials. Single use with
/// a 60-second window; afterwards the code is gone either way.
pub(super) async fn sso_handoff(
    State(state): State<AppState>,
    Json(body): Json<HandoffBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let code = body.code.trim();
    if code.len() != 64 || !code.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err((StatusCode::BAD_REQUEST, "Invalid handoff code".into()));
    }

    let client = state.pool.get().await.map_err(internal_error)?;
    // Sweep, then consume — expired codes are unredeemable even if the sweep
    // and the redeem race (the WHERE clause re-checks freshness).
    client
        .execute(
            "DELETE FROM sso_handoff_codes WHERE created_at < NOW() - INTERVAL '60 seconds'",
            &[],
        )
        .await
        .map_err(internal_error)?;
    let row = client
        .query_opt(
            "DELETE FROM sso_handoff_codes
             WHERE code_sha256 = $1 AND created_at > NOW() - INTERVAL '60 seconds'
             RETURNING user_id, api_key, email",
            &[&sha256_hex(code)],
        )
        .await
        .map_err(internal_error)?;
    let Some(row) = row else {
        return Err((
            StatusCode::UNAUTHORIZED,
            "This sign-in link has expired — please try again".into(),
        ));
    };

    let user_id: Uuid = row.get(0);
    let api_key: String = row.get(1);
    let email: String = row.get(2);
    Ok(Json(json!({
        "api_key": api_key,
        "user_id": user_id,
        "email": email,
        "email_verified": true,
    })))
}

/// Percent-encode a query-string component (RFC 3986 unreserved set).
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urlencode_covers_reserved_chars() {
        assert_eq!(urlencode("abc-_.~XYZ09"), "abc-_.~XYZ09");
        assert_eq!(urlencode("a b&c=d"), "a%20b%26c%3Dd");
        assert_eq!(urlencode("https://x/y?z"), "https%3A%2F%2Fx%2Fy%3Fz");
        assert_eq!(urlencode("ü"), "%C3%BC");
    }

    #[test]
    fn pkce_challenge_is_base64url_of_sha256() {
        // RFC 7636 appendix B test vector.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(verifier.as_bytes()));
        assert_eq!(challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn id_token_verification_rejects_garbage_and_alg_confusion() {
        let jwks = json!({ "keys": [{ "kty": "RSA", "kid": "k1", "n": "AQAB", "e": "AQAB" }] });
        assert!(verify_id_token("not.a.jwt", &jwks, "https://iss", "cid").is_err());

        // An HS256 token must be refused even before signature checks.
        let hs_header = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(r#"{"alg":"HS256","typ":"JWT","kid":"k1"}"#);
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r"{}");
        let forged = format!("{hs_header}.{payload}.c2ln");
        let err = verify_id_token(&forged, &jwks, "https://iss", "cid").unwrap_err();
        assert!(err.contains("refusing token alg"), "got: {err}");
    }

    #[test]
    fn email_domain_extraction_is_strict() {
        // Mirrors the callback's containment check.
        let email = "user@sub.acme.com";
        assert_eq!(
            email.rsplit('@').next().map(str::to_ascii_lowercase),
            Some("sub.acme.com".to_string())
        );
    }
}
