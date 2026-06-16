//! Public invite redemption (GL #385) — `POST /api/team/join`.
//!
//! The teammate opening `leanctx.com/join/?code=…` has no account and no
//! session; the only credential is the 256-bit invite code itself. This
//! endpoint forwards the code to the private control plane, which atomically
//! consumes the invite and mints a member token (returned exactly once).
//!
//! Defense in depth, even though 64-hex codes are unguessable:
//! - shape check before any upstream call (64 hex chars),
//! - per-IP sliding-window rate limit (in-memory; restarts only reset it),
//! - one neutral 404 for unknown/expired/revoked/used codes (no probing).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use serde::Deserialize;
use serde_json::Value;

use super::auth::{AppState, sha256_hex};

/// Attempts allowed per client IP within [`WINDOW`].
const MAX_ATTEMPTS: usize = 10;
/// Sliding rate-limit window.
const WINDOW: Duration = Duration::from_hours(1);

/// In-memory attempt log: salted ip-hash → recent attempt instants. Bounded by
/// pruning on every insert; the map only ever holds IPs active in the window.
static ATTEMPTS: Mutex<Option<HashMap<String, Vec<Instant>>>> = Mutex::new(None);

/// Record one attempt and decide whether this client is over the limit.
/// Unknown origin (no forwarded IP header) is allowed through — the front
/// proxy on leanctx.com always sets one, so this only relaxes local dev.
fn rate_limited(ip_hash: Option<String>, now: Instant) -> bool {
    let Some(key) = ip_hash else {
        return false;
    };
    let mut guard = match ATTEMPTS.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    let map = guard.get_or_insert_with(HashMap::new);
    map.retain(|_, hits| {
        hits.retain(|t| now.duration_since(*t) < WINDOW);
        !hits.is_empty()
    });
    let hits = map.entry(key).or_default();
    if hits.len() >= MAX_ATTEMPTS {
        return true;
    }
    hits.push(now);
    false
}

/// Salted hash of the client IP from the front proxy headers; raw IPs are
/// never stored (same construction as the wrapped publisher).
fn client_ip_hash(headers: &HeaderMap, salt: &str) -> Option<String> {
    let ip = headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            headers
                .get("x-real-ip")
                .and_then(|v| v.to_str().ok())
                .map(str::trim)
                .filter(|s| !s.is_empty())
        })?;
    Some(sha256_hex(&format!("{salt}:{ip}")))
}

/// The only code shape the control plane ever mints: 64 hex chars.
fn valid_code_shape(code: &str) -> bool {
    code.len() == 64 && code.bytes().all(|b| b.is_ascii_hexdigit())
}

#[derive(Deserialize)]
pub(super) struct JoinBody {
    code: String,
}

/// `POST /api/team/join` — redeem an invite code for a member token. Public:
/// the caller has no account; the code is the credential. The token in the
/// response is shown exactly once and never retrievable again.
pub(super) async fn post_team_join(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<JoinBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let code = body.code.trim().to_ascii_lowercase();
    if !valid_code_shape(&code) {
        return Err((
            StatusCode::BAD_REQUEST,
            "that does not look like an invite code".into(),
        ));
    }

    if rate_limited(
        client_ip_hash(&headers, &state.cfg.ip_hash_salt),
        Instant::now(),
    ) {
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            "too many attempts — try again later".into(),
        ));
    }

    let (status, json) = super::billing_edge::forward_invite_redeem(&state.cfg, &code).await?;

    if status.is_success() {
        return Ok(Json(json));
    }
    // One neutral message for every dead-code shape (unknown, expired,
    // revoked, used) so the page can render it directly; seat-limit errors
    // pass through with their actionable text.
    if status == StatusCode::NOT_FOUND {
        return Err((
            StatusCode::NOT_FOUND,
            "this invite link is invalid, expired, or already used".into(),
        ));
    }
    let msg = json
        .get("error")
        .and_then(Value::as_str)
        .or_else(|| json.get("message").and_then(Value::as_str))
        .unwrap_or("could not redeem the invite")
        .to_string();
    Err((status, msg))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_shape_is_64_hex() {
        assert!(valid_code_shape(&"a".repeat(64)));
        assert!(valid_code_shape(&"A".repeat(64)));
        assert!(!valid_code_shape(&"a".repeat(63)));
        assert!(!valid_code_shape(&"a".repeat(65)));
        assert!(!valid_code_shape(&"g".repeat(64)));
        assert!(!valid_code_shape(""));
    }

    #[test]
    fn rate_limit_counts_per_ip_within_window() {
        let now = Instant::now();
        let ip = Some("test-ip-hash-rate-limit".to_string());
        for _ in 0..MAX_ATTEMPTS {
            assert!(!rate_limited(ip.clone(), now));
        }
        assert!(rate_limited(ip.clone(), now));
        // A different client is unaffected.
        assert!(!rate_limited(Some("other-ip".into()), now));
        // After the window has fully passed, the slate is clean.
        let later = now + WINDOW + Duration::from_secs(1);
        assert!(!rate_limited(ip, later));
    }

    #[test]
    fn missing_ip_is_never_limited() {
        let now = Instant::now();
        for _ in 0..(MAX_ATTEMPTS * 3) {
            assert!(!rate_limited(None, now));
        }
    }
}
