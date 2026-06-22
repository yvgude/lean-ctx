//! Dashboard doctor API (#466) — installation health signal + one-click fix.
//!
//! - `GET  /api/doctor`     → the structured three-level health report
//!   ([`crate::doctor::health_report`]) the UI renders as a ✅/⚠/✗ badge.
//! - `POST /api/doctor/fix` → runs every `doctor --fix` repair step in-process
//!   and returns the resulting `SetupReport`.
//!
//! Security: like every dashboard mutation, the POST is Bearer-token gated and
//! CSRF-`Origin` checked *before* the router runs (see `dashboard/mod.rs`). The
//! fix takes no client input — it runs the same fixed repair pipeline as the CLI
//! — so there is nothing to validate beyond requiring the POST method.

use super::helpers::json_err;

pub(super) fn handle(
    path: &str,
    _query_str: &str,
    method: &str,
    _body: &str,
) -> Option<(&'static str, &'static str, String)> {
    match path {
        "/api/doctor/fix" if method.eq_ignore_ascii_case("POST") => Some(post_fix()),
        // A GET here is a client bug; say so explicitly instead of 404 so the
        // mistake is obvious (the fix mutates state and must be POSTed).
        "/api/doctor/fix" => Some((
            "405 Method Not Allowed",
            "application/json",
            json_err("use POST to run doctor --fix"),
        )),
        "/api/doctor" => Some(get_doctor()),
        _ => None,
    }
}

fn get_doctor() -> (&'static str, &'static str, String) {
    let report = crate::doctor::health_report();
    serde_json::to_string(&report).map_or_else(
        |e| {
            (
                "500 Internal Server Error",
                "application/json",
                json_err(&format!("failed to serialize doctor report: {e}")),
            )
        },
        |body| ("200 OK", "application/json", body),
    )
}

fn post_fix() -> (&'static str, &'static str, String) {
    match crate::doctor::run_fix_report() {
        Ok(report) => serde_json::to_string(&report).map_or_else(
            |e| {
                (
                    "500 Internal Server Error",
                    "application/json",
                    json_err(&format!("failed to serialize fix report: {e}")),
                )
            },
            |body| ("200 OK", "application/json", body),
        ),
        Err(e) => (
            "500 Internal Server Error",
            "application/json",
            json_err(&format!("doctor --fix failed: {e}")),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_doctor_returns_structured_health_json() {
        let (status, mime, body) = handle("/api/doctor", "", "GET", "").expect("route matches");
        assert_eq!(status, "200 OK");
        assert_eq!(mime, "application/json");
        let v: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");
        assert!(
            matches!(v["level"].as_str(), Some("good" | "warnings" | "issues")),
            "level must be one of the three badge states, got {:?}",
            v["level"]
        );
        assert!(v["total"].as_u64().is_some(), "total must be present");
        assert!(v["checks"].is_array(), "checks must be an array");
    }

    #[test]
    fn fix_route_rejects_get_with_405() {
        let (status, _mime, body) =
            handle("/api/doctor/fix", "", "GET", "").expect("route matches");
        assert_eq!(status, "405 Method Not Allowed");
        assert!(
            body.contains("POST"),
            "the 405 must hint at the right method"
        );
    }

    #[test]
    fn unrelated_paths_pass_through() {
        assert!(handle("/api/stats", "", "GET", "").is_none());
        assert!(handle("/api/doctorx", "", "GET", "").is_none());
        assert!(handle("/", "", "GET", "").is_none());
    }
}
