//! Dashboard feedback API — the form that asks what people use lean-ctx for.
//!
//! - `POST /api/feedback` → forward one submission to `api.leanctx.com`.
//!
//! Same-origin proxy, like the leaderboard board (#466): the dashboard CSP pins
//! `connect-src` to `'self'`, so the browser cannot reach `api.leanctx.com`
//! itself. Routing it through here also means the form never has to know an
//! external hostname, and the one place that decides what leaves the machine is
//! this file.
//!
//! What leaves: exactly the four answers the person typed, an optional contact
//! they chose to add, plus the installation id and lean-ctx version so the
//! answers can be read in context. Nothing is derived from their code, paths,
//! prompts or usage, and nothing is sent unless they press the button — this
//! endpoint has no background caller.

use serde::Deserialize;

use super::helpers::json_err;

pub(super) fn handle(
    path: &str,
    _query_str: &str,
    method: &str,
    body: &str,
) -> Option<(&'static str, &'static str, String)> {
    match path {
        "/api/feedback" if method.eq_ignore_ascii_case("POST") => Some(post_feedback(body)),
        "/api/feedback" => Some((
            "405 Method Not Allowed",
            "application/json",
            json_err("POST is required to send feedback"),
        )),
        _ => None,
    }
}

/// Per-field cap, mirroring the server's. Rejecting an over-long answer here
/// costs the person nothing but a message; letting it travel only to be
/// truncated server-side would silently drop the end of what they wrote.
const MAX_ANSWER_CHARS: usize = 2_000;

#[derive(Deserialize, Default)]
struct FeedbackReq {
    #[serde(default)]
    use_case: String,
    #[serde(default)]
    likes_most: String,
    #[serde(default)]
    wishes: String,
    #[serde(default)]
    frequency: String,
    #[serde(default)]
    contact: Option<String>,
}

fn post_feedback(body: &str) -> (&'static str, &'static str, String) {
    let req: FeedbackReq = match serde_json::from_str(body) {
        Ok(r) => r,
        Err(e) => {
            return (
                "400 Bad Request",
                "application/json",
                json_err(&format!("invalid JSON: {e}")),
            );
        }
    };

    let use_case = req.use_case.trim();
    let likes_most = req.likes_most.trim();
    let wishes = req.wishes.trim();

    if use_case.is_empty() && likes_most.is_empty() && wishes.is_empty() {
        return (
            "400 Bad Request",
            "application/json",
            json_err("answer at least one question before sending"),
        );
    }
    for (label, value) in [
        ("use_case", use_case),
        ("likes_most", likes_most),
        ("wishes", wishes),
    ] {
        if value.chars().count() > MAX_ANSWER_CHARS {
            return (
                "400 Bad Request",
                "application/json",
                json_err(&format!(
                    "{label} is longer than {MAX_ANSWER_CHARS} characters"
                )),
            );
        }
    }

    let installation_id = match crate::core::installation_id::get_or_create() {
        Ok(id) => id,
        Err(e) => {
            return (
                "500 Internal Server Error",
                "application/json",
                json_err(&format!("could not read the installation id: {e}")),
            );
        }
    };

    let payload = serde_json::json!({
        "installation_id": installation_id,
        "version": env!("CARGO_PKG_VERSION"),
        "use_case": use_case,
        "likes_most": likes_most,
        "wishes": wishes,
        "frequency": req.frequency.trim(),
        "contact": req.contact.as_deref().map(str::trim).filter(|c| !c.is_empty()),
    });

    match crate::cloud_client::submit_product_feedback(&payload) {
        Ok(message) => (
            "200 OK",
            "application/json",
            serde_json::json!({ "ok": true, "message": message }).to_string(),
        ),
        // The upstream failed, not this request. A 502 with the reason lets the
        // form say "couldn't reach the server" and keep what was typed, instead
        // of reporting a send that never happened. The client's message already
        // names the failure, so it is passed through rather than prefixed again.
        Err(e) => ("502 Bad Gateway", "application/json", json_err(&e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(body: &str) -> &'static str {
        post_feedback(body).0
    }

    #[test]
    fn a_submission_with_no_answers_is_refused_before_it_leaves_the_machine() {
        assert_eq!(
            status(r#"{"use_case":"  ","likes_most":"","wishes":""}"#),
            "400 Bad Request"
        );
        assert_eq!(status("{}"), "400 Bad Request");
    }

    #[test]
    fn malformed_json_is_a_client_error_not_a_crash() {
        assert_eq!(status("not json"), "400 Bad Request");
    }

    /// An over-long answer is refused with a message naming the field, rather
    /// than travelling to be silently truncated at the other end.
    #[test]
    fn an_over_long_answer_names_the_field() {
        let long = "x".repeat(MAX_ANSWER_CHARS + 1);
        let body = serde_json::json!({ "wishes": long }).to_string();
        let (code, _, payload) = post_feedback(&body);
        assert_eq!(code, "400 Bad Request");
        assert!(payload.contains("wishes"), "{payload}");
    }

    /// The cap counts characters: 2000 multi-byte answers are within it.
    #[test]
    fn a_multibyte_answer_is_measured_in_characters() {
        let body = serde_json::json!({ "wishes": "ü".repeat(MAX_ANSWER_CHARS) }).to_string();
        assert_ne!(
            post_feedback(&body).0,
            "400 Bad Request",
            "2000 characters is within the cap regardless of encoding"
        );
    }

    /// Both layers used to prefix the reason, so the caller was told
    /// "could not send feedback: Could not send feedback: …".
    #[test]
    fn an_upstream_failure_is_reported_once() {
        // The lock is not optional: `LEAN_CTX_API_URL` is process-global, and a
        // neighbouring test reading the environment while this one rewrites it
        // is the race #1653 fixed once already.
        let _lock = crate::core::data_dir::test_env_lock();

        // Point the client at a port nothing is listening on so the send fails
        // for a reason that has nothing to do with this request's contents.
        let previous = std::env::var("LEAN_CTX_API_URL").ok();
        crate::test_env::set_var("LEAN_CTX_API_URL", "http://127.0.0.1:9");

        let (code, _, body) = post_feedback(r#"{"wishes":"anything"}"#);

        match previous {
            Some(value) => crate::test_env::set_var("LEAN_CTX_API_URL", value),
            None => crate::test_env::remove_var("LEAN_CTX_API_URL"),
        }

        assert_eq!(code, "502 Bad Gateway");
        assert_eq!(
            body.to_lowercase()
                .matches("could not send feedback")
                .count(),
            1,
            "the reason must be stated once: {body}"
        );
    }

    #[test]
    fn a_get_says_which_verb_is_required() {
        let (code, _, body) = handle("/api/feedback", "", "GET", "").expect("route matches");
        assert_eq!(code, "405 Method Not Allowed");
        assert!(body.contains("POST"), "{body}");
    }

    #[test]
    fn unrelated_paths_are_not_claimed() {
        assert!(handle("/api/stats", "", "POST", "").is_none());
    }
}
