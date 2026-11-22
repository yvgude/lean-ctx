// SPDX-License-Identifier: Apache-2.0
//! `cloud.members.list`: the members of the team this instance is linked
//! to, as LeanCTX Cloud lists them, as a Local Read kind.
//!
//! The engine sends `GET /v1/members` with its instance token and passes the
//! Cloud's `items` and `next_cursor` through, with one change: an email is
//! kept only on the caller's own row. When the engine knows the linked
//! account's `user_id` (from the instance's card), only the row of that
//! user keeps its email. Before the first card it knows no id, and the
//! Cloud marks the caller's row by nothing else, so an email is kept only
//! when exactly one row carries one; an answer with more is passed on with
//! every email removed. Nothing is written.
//!
//! - `cloud.members.list`: `GET /v1/local/cloud/members`, CLI
//!   `lean-ctx cloud members list --json`.
//!
//! Wired as one `local_read::Part`: its routes, kinds and commands are
//! chained into the route table and the capabilities builder.

use serde::Serialize;
use serde_json::Value;

use lean_ctx_cloud::transport::CloudClient;
use lean_ctx_cloud::{CloudError, CloudStore};

use super::routes::{Method, Project, Route};
use super::{Answer, Code, Failure, Via};

/// The kind of the team members' document.
pub(crate) const KIND: &str = "cloud.members.list";

/// This part's routes (Route as in `routes.rs`).
pub(crate) const ROUTES: &[Route] = &[Route {
    kind: KIND,
    method: Method::Get,
    path: "/v1/local/cloud/members",
    cli: &["cloud", "members", "list", "--json"],
    action: &["list"],
    params: &[],
    project: Project::None,
    names: None,
}];

/// This part's document kinds.
pub(crate) const KINDS: &[&str] = &[KIND];

/// Command path → CLI verbs, as `capabilities::COMMANDS`.
pub(crate) const COMMANDS: &[(&str, &[&str])] = &[("cloud.members", &["list"])];

/// The largest Cloud answer read.
const MAX_ANSWER_BYTES: usize = 512 * 1024;

/// The Cloud's members of the team.
#[derive(Debug, Serialize)]
pub(crate) struct MembersList {
    /// `"cloud"`: LeanCTX Cloud holds the team.
    pub source: &'static str,
    /// The Cloud's member rows, an email only on the caller's own.
    pub items: Vec<Value>,
    /// The Cloud's cursor of the next page, `null` on the last one.
    pub next_cursor: Value,
}

/// The document of the Cloud's answer `body`.
/// `caller` is the linked account's `user_id`, when known.
pub(crate) fn members_doc(body: &Value, caller: Option<&str>) -> Result<MembersList, Failure> {
    let mut items = body
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| {
            Failure::from(CloudError::Protocol(
                "the members answer has no items".to_owned(),
            ))
        })?;
    keep_only_the_callers_email(&mut items, caller);
    let next_cursor = match body.get("next_cursor") {
        Some(cursor @ Value::String(_)) => cursor.clone(),
        _ => Value::Null,
    };
    Ok(MembersList {
        source: "cloud",
        items,
        next_cursor,
    })
}

/// Removes `user.email` from every row but the caller's. With the caller's
/// `user_id` known, that is the row of that id. Without it, the Cloud gives
/// the caller's own row its email and no other, so exactly one email is
/// kept; a second email means the rows cannot tell whose it is, and none is
/// passed on.
fn keep_only_the_callers_email(items: &mut [Value], caller: Option<&str>) {
    let single = items_with_email(items) <= 1;
    let keep = |row: &Value| match caller {
        Some(id) => row["user"]["id"].as_str() == Some(id),
        None => single,
    };
    let kept: Vec<bool> = items.iter().map(keep).collect();
    for (row, kept) in items.iter_mut().zip(kept) {
        if kept {
            continue;
        }
        if let Some(user) = row.get_mut("user").and_then(Value::as_object_mut) {
            user.remove("email");
        }
    }
}

/// How many rows carry a `user.email`.
fn items_with_email(items: &[Value]) -> usize {
    items
        .iter()
        .filter(|row| row["user"].get("email").is_some())
        .count()
}

/// Ask for the members with `client`, answering the status and body even
/// when that is an error. A `401` is an error, so the session mints a new
/// token once and then reports it.
pub(crate) fn send_list(client: &CloudClient) -> Result<(u16, Value), CloudError> {
    let (status, answer) = client.send_answer("GET", "/members", None, MAX_ANSWER_BYTES)?;
    if status == 401 {
        return Err(CloudError::Unauthorized);
    }
    Ok((status, answer))
}

/// The body of a members answer, or its refusal with its meaning kept:
/// `step_up_required` and `members_read_instance` are named in
/// `error.reason`.
pub(crate) fn listed(status: u16, body: Value) -> Result<Value, Failure> {
    if (200..300).contains(&status) {
        return Ok(body);
    }
    Err(refusal(status, &body))
}

/// The Local Read refusal of a Cloud error answer about the team's members.
pub(crate) fn refusal(status: u16, body: &Value) -> Failure {
    if let Some(refusal) = super::decisions_part::policy_refusal::from_answer(body) {
        return refusal;
    }
    let code = body["error"]["code"].as_str().unwrap_or_default();
    match code {
        "unauthorized" | "lease_expired" | "instance_revoked" => Failure::new(
            Code::NotConnected,
            "LeanCTX Cloud no longer accepts this machine's connection; run `lean-ctx cloud connect`",
        ),
        "step_up_required" => Failure::new(
            Code::Forbidden,
            "LeanCTX Cloud asks the person to confirm this on the LeanCTX Cloud web",
        )
        .with_reason("step_up_required"),
        "plan_required" => {
            Failure::new(Code::PlanRequired, "the team's plan does not include this")
        }
        "forbidden" | "forbidden_right" => Failure::new(
            Code::Forbidden,
            "the person this machine is linked to may not see the team's members",
        ),
        "not_found" => Failure::new(Code::NotFound, "LeanCTX Cloud does not know this team"),
        "upgrade_required" => super::decisions_part::upgrade_refusal(body),
        _ if status == 429 || code == "rate_limited" || code == "members_read_instance" => {
            let message = match body["error"]["retry_after_s"].as_u64() {
                Some(s) => format!(
                    "LeanCTX Cloud asks to retry in {} s",
                    s.min(lean_ctx_cloud::error::RETRY_AFTER_CAP_S)
                ),
                None => "LeanCTX Cloud asks to retry later".to_owned(),
            };
            let held = Failure::new(Code::RateLimited, message);
            if code == "members_read_instance" {
                held.with_reason("members_read_instance")
            } else {
                held
            }
        }
        _ if status == 402 => {
            Failure::new(Code::PlanRequired, "the team's plan does not include this")
        }
        _ if status == 403 => Failure::new(
            Code::Forbidden,
            "the person this machine is linked to may not see the team's members",
        ),
        _ if status == 404 => Failure::new(Code::NotFound, "LeanCTX Cloud does not know this"),
        _ if status == 409 => Failure::new(Code::Conflict, "LeanCTX Cloud refused this (409)"),
        _ if status == 408 || status >= 500 => Failure::new(
            Code::Unavailable,
            format!("LeanCTX Cloud could not answer ({status})"),
        ),
        _ => Failure::new(Code::Internal, format!("LeanCTX Cloud answered {status}")),
    }
}

/// Ask the connected instance's Cloud for the team's members.
fn list() -> Result<MembersList, Failure> {
    let store = CloudStore::open()?;
    let caller = lean_ctx_cloud::consent::broker::linked_user_id(&store);
    let instance = store.instance()?.ok_or(CloudError::NotConnected)?;
    let config = lean_ctx::core::config::Config::load().cloud.clone();
    let trust = lean_ctx_cloud::root_cert::trust_root(&store, &config, &instance.url)?;
    let session = lean_ctx_cloud::auth::Session::load(store, trust)?;
    let (status, answer) = session.with_client(send_list)?;
    members_doc(&listed(status, answer)?, caller.as_deref())
}

/// The released document of `kind`, for the CLI or the socket.
pub(crate) fn answer(kind: &str, _args: &[String], via: Via) -> Answer {
    match kind {
        KIND => super::answer_released("ctx_knowledge", "", kind, via, list),
        other => Answer::failure(&Failure::invalid(format!("unknown kind {other}"))),
    }
}

/// `lean-ctx cloud members list --json`: the Local Read document; `false`
/// for every other command line.
pub(crate) fn try_run(command: &str, rest: &[String]) -> bool {
    let words: Vec<&str> = rest.iter().map(String::as_str).take(2).collect();
    if command != "cloud" || words != ["members", "list"] || !super::wants_json(rest) {
        return false;
    }
    super::print(&answer(KIND, &rest[1..], Via::Cli));
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn words(w: &[&str]) -> Vec<String> {
        w.iter().map(|s| (*s).to_owned()).collect()
    }

    /// One HTTP exchange on a loopback port: the request as text, and the
    /// answer `status` with `body` (as LeanCTX Cloud sends it).
    fn serve_once(status: u16, body: &str) -> (String, std::thread::JoinHandle<String>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let body = body.to_owned();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = stream.read(&mut chunk).unwrap();
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
                if String::from_utf8_lossy(&buf).contains("\r\n\r\n") {
                    break;
                }
            }
            let answer = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nCloud-Protocol: 2.2\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(answer.as_bytes()).unwrap();
            String::from_utf8_lossy(&buf).into_owned()
        });
        (origin, handle)
    }

    #[test]
    fn members_are_dispatched_routed_and_listed_as_a_cli_verb() {
        assert!(super::super::part_of(KIND).is_some(), "{KIND} has no part");
        let route = super::super::routes::all()
            .find(|r| r.kind == KIND)
            .expect("routed");
        assert!(matches!(route.method, Method::Get));
        assert!(matches!(route.project, Project::None));
        let text = super::super::routes::table_json().to_string();
        assert!(text.contains("/v1/local/cloud/members"), "{text}");
        assert!(
            super::super::PARTS
                .iter()
                .flat_map(|p| p.commands.iter())
                .any(|(path, verbs)| *path == "cloud.members" && *verbs == ["list"]),
            "cloud.members list is a CLI verb, and the only one"
        );
    }

    #[test]
    fn the_clouds_members_pass_through() {
        let (origin, server) = serve_once(
            200,
            r#"{"items":[{"user":{"id":"usr_1","name":"Ada","email":"ada@example.com"},"role":"owner","via":"owner","joined_at":"2026-11-01T00:00:00Z","instances_connected":1,"instances":2},{"user":{"id":"usr_2","name":"Bob"},"role":"member","via":"invite","joined_at":"2026-11-02T00:00:00Z","instances_connected":0,"instances":0}],"next_cursor":null}"#,
        );
        let client = CloudClient::new(&origin).unwrap();
        let (status, answer) = send_list(&client).expect("answered");
        let request = server.join().unwrap();
        assert!(
            request.starts_with("GET /v1/members HTTP/1.1\r\n"),
            "{request}"
        );
        let doc = members_doc(&listed(status, answer).expect("listed"), None).expect("doc");
        let value: Value = serde_json::from_str(&super::super::document(KIND, &doc)).unwrap();
        assert_eq!(value["contract"], "local-read/1");
        assert_eq!(value["kind"], KIND);
        assert_eq!(value["source"], "cloud");
        assert_eq!(value["items"][0]["user"]["email"], "ada@example.com");
        assert!(value["items"][1]["user"].get("email").is_none());
        assert_eq!(value["items"][1]["via"], "invite");
        assert_eq!(value["items"][0]["via"], "owner");
        assert_eq!(value["items"][0]["instances"], 2);
        assert_eq!(value["next_cursor"], Value::Null);
    }

    #[test]
    fn only_a_single_email_is_kept_as_the_callers_own() {
        let row = |id: &str, email: Option<&str>| {
            let mut user = json!({"id": id, "name": id});
            if let Some(email) = email {
                user["email"] = json!(email);
            }
            json!({"user": user, "role": "member", "via": "invite"})
        };
        let one = members_doc(
            &json!({
                "items": [row("usr_1", Some("ada@example.com")), row("usr_2", None)],
                "next_cursor": null
            }),
            None,
        )
        .expect("doc");
        assert_eq!(one.items[0]["user"]["email"], "ada@example.com");
        assert!(one.items[1]["user"].get("email").is_none());

        let many = members_doc(
            &json!({
                "items": [
                    row("usr_1", Some("ada@example.com")),
                    row("usr_2", Some("bob@example.com")),
                    row("usr_3", None)
                ],
                "next_cursor": null
            }),
            None,
        )
        .expect("doc");
        assert_eq!(many.items.len(), 3);
        for item in &many.items {
            assert!(item["user"].get("email").is_none(), "{item}");
            assert!(item["user"]["id"].is_string(), "{item}");
        }
    }

    #[test]
    fn a_known_caller_keeps_only_the_email_of_their_own_row() {
        let row = |id: &str, email: &str| json!({"user": {"id": id, "name": id, "email": email}, "role": "member"});
        let rows = json!({
            "items": [row("usr_1", "ada@example.com"), row("usr_2", "bob@example.com")],
            "next_cursor": null
        });
        let doc = members_doc(&rows, Some("usr_2")).expect("doc");
        assert!(doc.items[0]["user"].get("email").is_none());
        assert_eq!(doc.items[1]["user"]["email"], "bob@example.com");

        // A single email on another person's row is not the caller's.
        let other = json!({"items": [row("usr_1", "ada@example.com")], "next_cursor": null});
        let doc = members_doc(&other, Some("usr_2")).expect("doc");
        assert!(doc.items[0]["user"].get("email").is_none());
    }

    #[test]
    fn an_answer_without_items_is_refused() {
        let failure = members_doc(&json!({"next_cursor": null}), None).expect_err("no items");
        assert_eq!(failure.code, Code::Internal);
    }

    #[test]
    fn cloud_refusals_keep_their_meaning() {
        let refused = |status: u16, body: Value| listed(status, body).expect_err("refused");
        let step_up = refused(403, json!({"error": {"code": "step_up_required"}}));
        assert_eq!(step_up.code, Code::Forbidden);
        assert_eq!(step_up.reason, Some("step_up_required"));
        assert_eq!(
            refused(402, json!({"error": {"code": "plan_required"}})).code,
            Code::PlanRequired
        );
        assert_eq!(refused(402, json!({})).code, Code::PlanRequired);
        assert_eq!(refused(409, json!({})).code, Code::Conflict);
        // The list names no invite refusals.
        assert_eq!(
            refused(409, json!({"error": {"code": "already_member"}})).reason,
            None
        );
        assert_eq!(
            refused(403, json!({"error": {"code": "forbidden"}})).code,
            Code::Forbidden
        );
        assert_eq!(refused(403, json!({})).code, Code::Forbidden);
        assert_eq!(
            refused(403, json!({"error": {"code": "instance_revoked"}})).code,
            Code::NotConnected
        );
        let held = refused(
            429,
            json!({"error": {"code": "rate_limited", "retry_after_s": 30}}),
        );
        assert_eq!(held.code, Code::RateLimited);
        assert_eq!(held.message, "LeanCTX Cloud asks to retry in 30 s");
        assert_eq!(refused(503, json!("<html>")).code, Code::Unavailable);
        // The members route's own refusals (p37 API.md, GET /v1/members).
        let upgrade = refused(
            426,
            json!({"error": {"code": "upgrade_required", "min_instance": "3.12.7"}}),
        );
        assert_eq!(upgrade.code, Code::Internal);
        assert!(
            upgrade.message.contains("lean-ctx 3.12.7 or newer"),
            "{}",
            upgrade.message
        );
        let instance = refused(
            429,
            json!({"error": {"code": "members_read_instance", "retry_after_s": 5}}),
        );
        assert_eq!(instance.code, Code::RateLimited);
        assert_eq!(instance.reason, Some("members_read_instance"));
        assert_eq!(
            refused(404, json!({"error": {"code": "not_found"}})).code,
            Code::NotFound
        );
        assert_eq!(refused(404, json!({})).code, Code::NotFound);
        assert_eq!(
            refused(
                403,
                json!({"error": {"code": "policy_denied", "member": "sharing.allowed_vaults"}})
            )
            .code,
            Code::PolicyDenied
        );
    }

    #[test]
    fn a_401_is_unauthorized_so_the_session_mints_a_new_token() {
        let (origin, server) = serve_once(401, r#"{"error":{"code":"unauthorized"}}"#);
        let client = CloudClient::new(&origin).unwrap();
        let error = send_list(&client).expect_err("unauthorized");
        server.join().unwrap();
        assert!(matches!(error, CloudError::Unauthorized), "{error:?}");
    }

    #[test]
    fn without_a_connection_nothing_is_sent_and_the_answer_is_not_connected() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        assert_eq!(list().expect_err("no instance").code, Code::NotConnected);
    }

    #[test]
    fn only_the_members_list_command_line_is_taken() {
        assert!(
            !try_run("cloud", &words(&["members", "list"])),
            "needs --json"
        );
        assert!(!try_run("cloud", &words(&["members", "invite", "--json"])));
        assert!(!try_run("cloud", &words(&["members", "revoke", "--json"])));
        assert!(!try_run("members", &words(&["list", "--json"])));
    }
}
