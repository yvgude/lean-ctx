//! Dashboard leaderboard API (#466) — submit this machine's recap to the public
//! board, flip auto-submit, and link multiple machines (GH #736), without
//! dropping to `lean-ctx gain --publish` or `--link`.
//!
//! - `GET  /api/leaderboard/status` → current publish / leaderboard / auto-submit
//!   state ([`wrapped_publish::leaderboard_status`]) the card renders.
//! - `POST /api/leaderboard/submit` → sign + publish the all-time recap with
//!   leaderboard opt-in (optional `{ "name": "handle" }`); returns the permalink.
//! - `POST /api/leaderboard/auto`   → `{ "on": true|false }` flips `[gain]
//!   auto_publish` (and opts in to the board when turning it on).
//! - `POST /api/leaderboard/link/start`    → mint a pairing code for this card.
//! - `POST /api/leaderboard/link/complete` → `{ "code": "XXXX-XXXX" }` joins
//!   this card into the initiator's link group (login-less machine linking).
//!
//! Security: like every dashboard mutation, the POSTs are Bearer-token gated and
//! CSRF-`Origin` checked *before* the router runs (see `dashboard/mod.rs`). The
//! submit body is the *same* minimal, whitelisted aggregate the CLI sends — only
//! tokens saved, est. USD, compression rate and the chosen handle, never code,
//! paths or prompts (enforced by `cli::wrapped_publish::build_payload` + the
//! server whitelist).

use serde::Deserialize;

use super::helpers::json_err;
use crate::cli::wrapped_publish;

pub(super) fn handle(
    path: &str,
    _query_str: &str,
    method: &str,
    body: &str,
) -> Option<(&'static str, &'static str, String)> {
    match path {
        "/api/leaderboard" => Some(get_board()),
        "/api/leaderboard/status" => Some(get_status()),
        "/api/leaderboard/submit" if method.eq_ignore_ascii_case("POST") => Some(post_submit(body)),
        "/api/leaderboard/submit" => Some(method_not_allowed("submit a leaderboard entry")),
        "/api/leaderboard/auto" if method.eq_ignore_ascii_case("POST") => Some(post_auto(body)),
        "/api/leaderboard/auto" => Some(method_not_allowed("toggle auto-submit")),
        "/api/leaderboard/link/start" if method.eq_ignore_ascii_case("POST") => {
            Some(post_link_start())
        }
        "/api/leaderboard/link/start" => Some(method_not_allowed("start a machine link")),
        "/api/leaderboard/link/complete" if method.eq_ignore_ascii_case("POST") => {
            Some(post_link_complete(body))
        }
        "/api/leaderboard/link/complete" => Some(method_not_allowed("complete a machine link")),
        _ => None,
    }
}

/// A GET on a mutating endpoint is a client bug; say so explicitly (with the
/// right verb) instead of a generic 404 so the mistake is obvious.
fn method_not_allowed(action: &str) -> (&'static str, &'static str, String) {
    (
        "405 Method Not Allowed",
        "application/json",
        json_err(&format!("use POST to {action}")),
    )
}

/// Same-origin proxy for the public community board (`GET /api/leaderboard`).
/// The dashboard CSP pins `connect-src` to `'self'`, so the browser cannot fetch
/// `api.leanctx.com` directly — we fetch it here and pass the JSON straight
/// through. A 502 (with the upstream error) lets the UI show "couldn't load the
/// board" without breaking the rest of the view.
fn get_board() -> (&'static str, &'static str, String) {
    match crate::cloud_client::fetch_leaderboard() {
        Ok(json) => ("200 OK", "application/json", json.to_string()),
        Err(e) => (
            "502 Bad Gateway",
            "application/json",
            json_err(&format!("could not load leaderboard: {e}")),
        ),
    }
}

fn get_status() -> (&'static str, &'static str, String) {
    let status = wrapped_publish::leaderboard_status();
    serde_json::to_string(&status).map_or_else(
        |e| {
            (
                "500 Internal Server Error",
                "application/json",
                json_err(&format!("failed to serialize leaderboard status: {e}")),
            )
        },
        |body| ("200 OK", "application/json", body),
    )
}

/// An empty body is a valid "submit with my saved handle" request, so `name`
/// defaults to `None` rather than being required.
#[derive(Deserialize, Default)]
struct SubmitReq {
    #[serde(default)]
    name: Option<String>,
}

fn post_submit(body: &str) -> (&'static str, &'static str, String) {
    let req: SubmitReq = if body.trim().is_empty() {
        SubmitReq::default()
    } else {
        match serde_json::from_str(body) {
            Ok(r) => r,
            Err(e) => {
                return (
                    "400 Bad Request",
                    "application/json",
                    json_err(&format!("invalid JSON: {e}")),
                );
            }
        }
    };

    match wrapped_publish::submit_leaderboard(req.name.as_deref()) {
        Ok(card) => {
            let body = serde_json::json!({
                "ok": true,
                "url": card.url,
                "id": card.id,
            })
            .to_string();
            ("200 OK", "application/json", body)
        }
        // "Nothing to publish yet" is a client-state problem (no savings), not an
        // upstream failure — distinguish it so the UI can word the message right.
        Err(e) if e.starts_with("Nothing to publish") => {
            ("409 Conflict", "application/json", json_err(&e))
        }
        Err(e) => (
            "502 Bad Gateway",
            "application/json",
            json_err(&format!("leaderboard submit failed: {e}")),
        ),
    }
}

#[derive(Deserialize)]
struct AutoReq {
    on: bool,
}

fn post_auto(body: &str) -> (&'static str, &'static str, String) {
    let req: AutoReq = match serde_json::from_str(body) {
        Ok(r) => r,
        Err(e) => {
            return (
                "400 Bad Request",
                "application/json",
                json_err(&format!(
                    "invalid JSON (expected {{\"on\":true|false}}): {e}"
                )),
            );
        }
    };

    match wrapped_publish::set_auto_submit(req.on) {
        // Echo the fresh state so the UI repaints from the source of truth.
        Ok(()) => get_status(),
        Err(e) => (
            "500 Internal Server Error",
            "application/json",
            json_err(&e),
        ),
    }
}

// ─── Login-less machine linking (GH #736) ─────────────────────────────────────

/// Mint a pairing code for this machine’s card. The dashboard proxies the cloud
/// API so the browser never reaches `api.leanctx.com` directly (CSP `connect-src`
/// = `'self'`). Auth is the card’s `edit_token`, not the dashboard bearer token.
fn post_link_start() -> (&'static str, &'static str, String) {
    match wrapped_publish::link_start() {
        Ok(minted) => {
            let body = serde_json::json!({
                "ok": true,
                "code": minted.code,
                "expires_in_secs": minted.expires_in_secs,
            })
            .to_string();
            ("200 OK", "application/json", body)
        }
        Err(e) if e.contains("No published card") => {
            ("409 Conflict", "application/json", json_err(&e))
        }
        Err(e) => (
            "502 Bad Gateway",
            "application/json",
            json_err(&format!("link start failed: {e}")),
        ),
    }
}

#[derive(Deserialize)]
struct LinkCompleteReq {
    code: String,
}

/// Complete a machine link using a pairing code from another machine. The
/// dashboard sends `{ "code": "XXXX-XXXX" }` and this proxies the cloud API.
fn post_link_complete(body: &str) -> (&'static str, &'static str, String) {
    let req: LinkCompleteReq = match serde_json::from_str(body) {
        Ok(r) => r,
        Err(e) => {
            return (
                "400 Bad Request",
                "application/json",
                json_err(&format!(
                    "invalid JSON (expected {{\"code\":\"XXXX-XXXX\"}}): {e}"
                )),
            );
        }
    };

    match wrapped_publish::link_complete(&req.code) {
        Ok(()) => {
            let body = serde_json::json!({ "ok": true, "linked": true }).to_string();
            ("200 OK", "application/json", body)
        }
        Err(e) if e.contains("No published card") => {
            ("409 Conflict", "application/json", json_err(&e))
        }
        Err(e) if e.contains("invalid or expired") => {
            ("410 Gone", "application/json", json_err(&e))
        }
        Err(e) => (
            "502 Bad Gateway",
            "application/json",
            json_err(&format!("link complete failed: {e}")),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_returns_expected_shape() {
        let (status, mime, body) =
            handle("/api/leaderboard/status", "", "GET", "").expect("route matches");
        assert_eq!(status, "200 OK");
        assert_eq!(mime, "application/json");
        let v: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");
        // The four bool/flag fields the card always renders must be present and
        // correctly typed regardless of this machine's publish history.
        assert!(v["published"].is_boolean(), "published must be a bool");
        assert!(v["on_leaderboard"].is_boolean(), "on_leaderboard bool");
        assert!(v["auto_submit"].is_boolean(), "auto_submit must be a bool");
        // display_name / url / last_published_at are nullable — keys must exist.
        for key in ["display_name", "url", "last_published_at"] {
            assert!(v.get(key).is_some(), "status must carry '{key}'");
        }
    }

    #[test]
    fn submit_rejects_get_with_405() {
        let (status, _mime, body) =
            handle("/api/leaderboard/submit", "", "GET", "").expect("route matches");
        assert_eq!(status, "405 Method Not Allowed");
        assert!(
            body.contains("POST"),
            "the 405 must hint at the right method"
        );
    }

    #[test]
    fn auto_rejects_get_with_405() {
        let (status, _mime, _body) =
            handle("/api/leaderboard/auto", "", "GET", "").expect("route matches");
        assert_eq!(status, "405 Method Not Allowed");
    }

    #[test]
    fn auto_rejects_invalid_json_with_400() {
        let (status, _mime, body) =
            handle("/api/leaderboard/auto", "", "POST", "not json").expect("route matches");
        assert_eq!(status, "400 Bad Request");
        let v: serde_json::Value = serde_json::from_str(&body).expect("valid JSON error");
        assert!(v["error"].is_string(), "400 must carry an error message");
    }

    #[test]
    fn submit_rejects_malformed_json_with_400() {
        // A non-empty, non-JSON body is a client error — must not reach the network.
        let (status, _mime, _body) =
            handle("/api/leaderboard/submit", "", "POST", "{bad").expect("route matches");
        assert_eq!(status, "400 Bad Request");
    }

    /// The board proxy is wired and degrades gracefully: pointed at an
    /// unreachable upstream it returns a well-formed 502 JSON instead of
    /// hanging or panicking (no real network needed — the connection refuses
    /// immediately).
    #[test]
    fn board_proxy_is_wired_and_degrades_to_502() {
        let _env_lock = crate::core::data_dir::test_env_lock();
        crate::test_env::set_var("LEAN_CTX_API_URL", "http://127.0.0.1:1");
        let res = handle("/api/leaderboard", "", "GET", "");
        crate::test_env::remove_var("LEAN_CTX_API_URL");
        let (status, mime, body) = res.expect("route matches /api/leaderboard");
        assert_eq!(mime, "application/json");
        assert_eq!(status, "502 Bad Gateway");
        let v: serde_json::Value = serde_json::from_str(&body).expect("valid JSON error");
        assert!(v["error"].is_string(), "502 must carry an error message");
    }

    #[test]
    fn unrelated_paths_pass_through() {
        assert!(handle("/api/stats", "", "GET", "").is_none());
        assert!(handle("/api/leaderboardx", "", "GET", "").is_none());
        assert!(handle("/", "", "GET", "").is_none());
    }

    #[test]
    fn link_start_rejects_get_with_405() {
        let (status, _mime, body) =
            handle("/api/leaderboard/link/start", "", "GET", "").expect("route matches");
        assert_eq!(status, "405 Method Not Allowed");
        assert!(body.contains("POST"), "405 must hint at POST");
    }

    #[test]
    fn link_complete_rejects_get_with_405() {
        let (status, _mime, _body) =
            handle("/api/leaderboard/link/complete", "", "GET", "").expect("route matches");
        assert_eq!(status, "405 Method Not Allowed");
    }

    #[test]
    fn link_complete_rejects_invalid_json_with_400() {
        let (status, _mime, body) =
            handle("/api/leaderboard/link/complete", "", "POST", "not json")
                .expect("route matches");
        assert_eq!(status, "400 Bad Request");
        let v: serde_json::Value = serde_json::from_str(&body).expect("valid JSON error");
        assert!(v["error"].is_string(), "400 must carry an error message");
    }
}
