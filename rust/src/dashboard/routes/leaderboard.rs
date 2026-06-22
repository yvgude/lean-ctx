//! Dashboard leaderboard API (#466) — submit this machine's recap to the public
//! board and flip auto-submit, without dropping to `lean-ctx gain --publish`.
//!
//! - `GET  /api/leaderboard/status` → current publish / leaderboard / auto-submit
//!   state ([`wrapped_publish::leaderboard_status`]) the card renders.
//! - `POST /api/leaderboard/submit` → sign + publish the all-time recap with
//!   leaderboard opt-in (optional `{ "name": "handle" }`); returns the permalink.
//! - `POST /api/leaderboard/auto`   → `{ "on": true|false }` flips `[gain]
//!   auto_publish` (and opts in to the board when turning it on).
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
}
